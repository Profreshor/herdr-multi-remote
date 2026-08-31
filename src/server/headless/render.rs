use super::*;

impl HeadlessServer {
    fn focused_pane_graphics_demand(&self) -> bool {
        self.app
            .state
            .active
            .and_then(|ws_idx| self.app.state.workspaces.get(ws_idx))
            .and_then(crate::workspace::Workspace::focused_pane_id)
            .is_some_and(|pane_id| self.app.pane_graphics.active_for_pane(pane_id))
    }

    pub(super) fn stream_host_mouse_capture_mode(&mut self) {
        let endpoint_shell_mouse = self.app.state.popup_pane.is_some()
            || self
                .app
                .state
                .focused_pane_requests_mouse_capture_from(&self.app.terminal_runtimes);
        let pixel_mouse_requested = self
            .clients
            .values()
            .any(|client| client.is_shell_client() && client.pixel_mouse);
        let shell_sgr_pixels = pixel_mouse_requested
            && self.focused_pane_graphics_demand()
            && self
                .app
                .state
                .active
                .and_then(|ws_idx| {
                    self.app
                        .state
                        .workspaces
                        .get(ws_idx)
                        .and_then(crate::workspace::Workspace::focused_pane_id)
                        .and_then(|pane_id| {
                            self.app.state.runtime_for_pane_in_workspace(
                                &self.app.terminal_runtimes,
                                ws_idx,
                                pane_id,
                            )
                        })
                })
                .is_some_and(crate::terminal::TerminalRuntime::sgr_pixel_mouse_enabled);
        let requested = self
            .clients
            .iter()
            .filter_map(|(&client_id, client)| match &client.mode {
                ClientConnectionMode::ClientShell => Some((
                    client_id,
                    client.shell_mouse_capture || endpoint_shell_mouse,
                    shell_sgr_pixels && client.pixel_mouse,
                )),
                ClientConnectionMode::TerminalAttach { terminal_id } => {
                    let runtime = self.runtime_for_terminal_id_string(terminal_id);
                    let child_requests_mouse = runtime
                        .is_some_and(crate::terminal::TerminalRuntime::mouse_reporting_enabled);
                    let sgr_pixels = child_requests_mouse
                        && client.pixel_mouse
                        && runtime
                            .is_some_and(crate::terminal::TerminalRuntime::sgr_pixel_mouse_enabled);
                    Some((client_id, child_requests_mouse, sgr_pixels))
                }
                ClientConnectionMode::TerminalPending
                | ClientConnectionMode::TerminalObserve { .. } => None,
            })
            .collect::<Vec<_>>();

        let mut broken_clients = Vec::new();
        for (client_id, enabled, sgr_pixels) in requested {
            let Some(client) = self.clients.get_mut(&client_id) else {
                continue;
            };
            if client.host_mouse_capture_active == Some(enabled)
                && client.host_sgr_pixels_active == Some(sgr_pixels)
            {
                continue;
            }
            let Some(writer) = &client.writer else {
                continue;
            };
            let serialized = match Self::frame_server_message(&ServerMessage::MouseCapture {
                enabled,
                sgr_pixels,
            }) {
                Ok(framed) => framed,
                Err(err) => {
                    warn!(err = %err, "failed to serialize mouse capture mode for client");
                    continue;
                }
            };
            if writer.control.send(serialized).is_err() {
                debug!(
                    client_id,
                    "client writer channel closed during mouse capture update"
                );
                broken_clients.push(client_id);
                continue;
            }
            client.host_mouse_capture_active = Some(enabled);
            client.host_sgr_pixels_active = Some(sgr_pixels);
        }

        for client_id in broken_clients {
            self.remove_client_and_resize_if_needed(client_id);
        }
    }

    pub(super) fn stream_direct_terminal_keyboard_mode(&mut self) {
        let shell_report_all = {
            let runtime = if self.app.state.popup_pane.is_some() {
                self.app.popup_runtime()
            } else {
                self.app.state.active.and_then(|workspace_index| {
                    self.app
                        .state
                        .focused_runtime_in_workspace(&self.app.terminal_runtimes, workspace_index)
                })
            };
            runtime.is_some_and(|runtime| {
                let protocol = runtime.keyboard_protocol();
                protocol.reports_all_keys()
                    || (protocol.reports_event_types() && runtime.modify_other_keys_level() > 0)
            })
        };
        let serialized_shell =
            Self::frame_server_message(&ServerMessage::ClientShellKeyboardReportAll {
                enabled: shell_report_all,
            });
        let mut broken_clients = Vec::new();
        for (&client_id, client) in &mut self.clients {
            if !client.is_shell_client()
                || client.host_keyboard_report_all_active == Some(shell_report_all)
            {
                continue;
            }
            let Some(writer) = &client.writer else {
                continue;
            };
            let Ok(serialized) = serialized_shell.as_ref() else {
                warn!("failed to serialize client shell keyboard report-all mode");
                break;
            };
            if writer.control.send(serialized.clone()).is_err() {
                broken_clients.push(client_id);
                continue;
            }
            client.host_keyboard_report_all_active = Some(shell_report_all);
        }

        let requested = self
            .clients
            .iter()
            .filter_map(|(&client_id, client)| {
                let ClientConnectionMode::TerminalAttach { terminal_id } = &client.mode else {
                    return None;
                };
                let (flags, modify_other_keys_level) = self
                    .runtime_for_terminal_id_string(terminal_id)
                    .map_or((0, 0), |runtime| {
                        let flags = match runtime.keyboard_protocol() {
                            crate::input::KeyboardProtocol::Legacy => 0,
                            crate::input::KeyboardProtocol::Kitty { flags } => flags,
                        };
                        (flags, runtime.modify_other_keys_level())
                    });
                Some((client_id, flags, modify_other_keys_level))
            })
            .collect::<Vec<_>>();

        for (client_id, flags, modify_other_keys_level) in requested {
            let Some(client) = self.clients.get_mut(&client_id) else {
                continue;
            };
            if client.host_keyboard_protocol_active == Some((flags, modify_other_keys_level)) {
                continue;
            }
            let Some(writer) = &client.writer else {
                continue;
            };
            let serialized =
                match Self::frame_server_message(&ServerMessage::DirectTerminalKeyboardProtocol {
                    flags,
                    modify_other_keys_level,
                }) {
                    Ok(framed) => framed,
                    Err(err) => {
                        warn!(err = %err, "failed to serialize direct terminal keyboard mode");
                        continue;
                    }
                };
            if writer.control.send(serialized).is_err() {
                debug!(
                    client_id,
                    "client writer channel closed during direct terminal keyboard update"
                );
                broken_clients.push(client_id);
                continue;
            }
            client.host_keyboard_protocol_active = Some((flags, modify_other_keys_level));
        }

        for client_id in broken_clients {
            self.remove_client_and_resize_if_needed(client_id);
        }
    }

    pub(super) fn has_pending_presentation_work(
        &self,
        needs_full_render: bool,
        needs_graphics_render: bool,
    ) -> bool {
        needs_full_render || needs_graphics_render || self.app.render_dirty.has_immediate_work()
    }

    pub(super) fn sync_immediate_pty_sources(&self) {
        let (has_app_target, direct_terminal_targets) = self.pty_render_targets();
        let mut pane_ids = if has_app_target {
            self.app.state.app_surface_pane_ids()
        } else {
            HashSet::new()
        };
        if !direct_terminal_targets.is_empty() {
            for workspace in &self.app.state.workspaces {
                for tab in &workspace.tabs {
                    pane_ids.extend(tab.panes.iter().filter_map(|(&pane_id, pane)| {
                        direct_terminal_targets
                            .contains(pane.attached_terminal_id.as_str())
                            .then_some(pane_id)
                    }));
                }
            }
            if let Some(popup) = &self.app.state.popup_pane {
                if direct_terminal_targets.contains(popup.terminal_id.as_str()) {
                    pane_ids.insert(popup.pane_id);
                }
            }
        }
        self.app.render_dirty.set_immediate_pty_sources(pane_ids);
    }

    fn pty_render_targets(&self) -> (bool, HashSet<&str>) {
        let mut has_app_target = false;
        let mut direct_terminal_targets = HashSet::new();
        for client in self
            .clients
            .values()
            .filter(|client| client.writer.is_some())
        {
            match &client.mode {
                ClientConnectionMode::ClientShell => has_app_target = true,
                ClientConnectionMode::TerminalAttach { terminal_id }
                | ClientConnectionMode::TerminalObserve { terminal_id } => {
                    direct_terminal_targets.insert(terminal_id.as_str());
                }
                ClientConnectionMode::TerminalPending => {}
            }
        }
        (has_app_target, direct_terminal_targets)
    }

    fn pty_source_visible_to_render_targets(
        &self,
        pane_id: crate::layout::PaneId,
        has_app_target: bool,
        direct_terminal_targets: &HashSet<&str>,
    ) -> bool {
        let terminal_id = self.terminal_id_for_pane(pane_id);
        (has_app_target && (terminal_id.is_none() || self.app_surface_contains_pane(pane_id)))
            || terminal_id.is_none_or(|source| direct_terminal_targets.contains(source.as_str()))
    }

    pub(super) fn pty_sources_visible_to_any_render_target(
        &self,
        sources: &HashSet<crate::layout::PaneId>,
    ) -> bool {
        let (has_app_target, direct_terminal_targets) = self.pty_render_targets();
        if !has_app_target && direct_terminal_targets.is_empty() {
            return false;
        }

        sources.iter().copied().any(|pane_id| {
            self.pty_source_visible_to_render_targets(
                pane_id,
                has_app_target,
                &direct_terminal_targets,
            )
        })
    }

    fn terminal_id_for_pane(
        &self,
        pane_id: crate::layout::PaneId,
    ) -> Option<&crate::terminal::TerminalId> {
        if let Some(popup) = self
            .app
            .state
            .popup_pane
            .as_ref()
            .filter(|popup| popup.pane_id == pane_id)
        {
            return Some(&popup.terminal_id);
        }
        self.app
            .find_pane(pane_id)
            .map(|(_, pane)| &pane.attached_terminal_id)
    }

    fn app_surface_contains_pane(&self, pane_id: crate::layout::PaneId) -> bool {
        if self
            .app
            .state
            .popup_pane
            .as_ref()
            .is_some_and(|popup| popup.pane_id == pane_id)
        {
            return true;
        }
        let Some(workspace) = self
            .app
            .state
            .active
            .and_then(|ws_idx| self.app.state.workspaces.get(ws_idx))
        else {
            return false;
        };
        let Some(tab) = workspace.active_tab() else {
            return false;
        };
        if !tab.panes.contains_key(&pane_id) {
            return false;
        }
        !tab.zoomed || tab.layout.focused() == pane_id
    }

    pub(super) fn render_and_stream(&mut self) {
        let full_started = crate::render_prof::timer();
        let render_targets = render_targets(&self.clients, self.foreground_client_id);

        if render_targets.is_empty() {
            let (cols, rows) = self.effective_size;
            let area = Rect::new(0, 0, cols, rows);
            let resize_panes = self.app.state.view.pane_infos.is_empty();
            if resize_panes {
                crate::ui::compute_view_with_runtime_registry(
                    &mut self.app.state,
                    &self.app.terminal_runtimes,
                    area,
                );
            } else {
                crate::ui::compute_view_without_resizing_panes(
                    &mut self.app.state,
                    &self.app.terminal_runtimes,
                    area,
                );
            }
            self.app.full_redraw_pending = false;
            crate::render_prof::duration_since("full_render.total", full_started);
            debug!(
                cols,
                rows, resize_panes, "updated geometry with no attached clients"
            );
            return;
        }

        let shell_snapshot_template = render_targets
            .iter()
            .any(|(_, _, _, _, mode)| matches!(mode, ClientConnectionMode::ClientShell))
            .then(|| client_shell_snapshot(&self.app, &self.client_shell_boot_id, 0, None));
        let mut broken_clients: Vec<u64> = Vec::new();
        let mut deferred_frame = false;
        for (client_id, (cols, rows), cell_size, is_foreground, mode) in render_targets {
            let area = Rect::new(0, 0, cols, rows);
            let mut shell_projection_revision = 0;
            if matches!(mode, ClientConnectionMode::ClientShell) {
                let Some(client) = self.clients.get_mut(&client_id) else {
                    continue;
                };
                let Some(mut candidate) = shell_snapshot_template.clone() else {
                    continue;
                };
                candidate.config_diagnostic = if client.shell_uses_endpoint_keybindings {
                    self.server_config_diagnostic.clone()
                } else {
                    self.server_config_diagnostic_without_keybindings.clone()
                };
                candidate.revision = client.shell_projection_revision;
                if client.shell_snapshot.as_ref() != Some(&candidate) {
                    client.shell_projection_revision =
                        client.shell_projection_revision.saturating_add(1);
                    candidate.revision = client.shell_projection_revision;
                    let message = ServerMessage::ClientShellSnapshot(Box::new(candidate.clone()));
                    let framed = match Self::frame_server_message(&message) {
                        Ok(framed) => framed,
                        Err(err) => {
                            warn!(client_id, err = %err, "failed to frame client shell replacement");
                            broken_clients.push(client_id);
                            continue;
                        }
                    };
                    let Some(writer) = client.writer.as_ref() else {
                        broken_clients.push(client_id);
                        continue;
                    };
                    if writer.control.send(framed).is_err() {
                        broken_clients.push(client_id);
                        continue;
                    }
                    client.shell_snapshot = Some(candidate);
                }
                shell_projection_revision = client.shell_projection_revision;
            }
            let shell_graphics_delivery = self
                .clients
                .get(&client_id)
                .map(|client| client.shell_graphics_delivery.clone())
                .unwrap_or_default();
            let mut surface_parts = None;
            let frame = match mode {
                ClientConnectionMode::ClientShell => {
                    let render_started = crate::render_prof::timer();
                    let render_cell_size = if cell_size.is_known() {
                        cell_size
                    } else {
                        crate::kitty_graphics::HostCellSize::default()
                    };
                    let crate::server::client_shell::RenderedPaneSurface {
                        frame,
                        panes,
                        splits,
                        popup,
                        graphics,
                        graphics_delivery: next_graphics_delivery,
                    } = render_client_shell_pane_surface(
                        &mut self.app,
                        area,
                        is_foreground,
                        render_cell_size,
                        &shell_graphics_delivery,
                        client_id,
                    );
                    crate::render_prof::duration_since(
                        "full_render.render_tab_surface_virtual",
                        render_started,
                    );
                    surface_parts = Some((panes, splits, popup, graphics, next_graphics_delivery));
                    frame
                }
                ClientConnectionMode::TerminalPending => continue,
                ClientConnectionMode::TerminalAttach { terminal_id }
                | ClientConnectionMode::TerminalObserve { terminal_id } => {
                    let Some(runtime) = self.runtime_for_terminal_id_string(&terminal_id) else {
                        self.send_to_client(
                            client_id,
                            ServerMessage::ServerShutdown {
                                reason: Some(format!(
                                    "terminal attach ended: terminal {terminal_id} not found"
                                )),
                            },
                        );
                        broken_clients.push(client_id);
                        continue;
                    };
                    let render_started = crate::render_prof::timer();
                    let (buffer, cursor) =
                        crate::server::render_stream::render_terminal_virtual(runtime, area);
                    crate::render_prof::duration_since(
                        "full_render.render_terminal_virtual",
                        render_started,
                    );
                    let hyperlinks_started = crate::render_prof::timer();
                    let hyperlinks = runtime.visible_hyperlinks(area);
                    crate::render_prof::duration_since(
                        "full_render.visible_hyperlinks",
                        hyperlinks_started,
                    );
                    let frame_started = crate::render_prof::timer();
                    let frame = FrameData::from_ratatui_buffer_with_hyperlinks(
                        &buffer,
                        cursor,
                        &hyperlinks,
                    );
                    crate::render_prof::duration_since("full_render.frame_build", frame_started);
                    frame
                }
            };

            let Some(client) = self.clients.get_mut(&client_id) else {
                continue;
            };
            let Some(writer) = client.writer.as_ref().cloned() else {
                crate::render_prof::event("full_render.writer_missing");
                continue;
            };
            let has_graphics = surface_parts
                .as_ref()
                .is_some_and(|(_, _, _, graphics, _)| {
                    !graphics.assets.is_empty()
                        || !graphics.placements.is_empty()
                        || !graphics.retained_assets.is_empty()
                });
            let mut next_shell_graphics_delivery = None;
            let prepared = if let Some((panes, splits, popup, graphics, delivery)) = surface_parts {
                next_shell_graphics_delivery = Some(delivery);
                client
                    .render_state
                    .prepare_pane_surface(protocol::PaneSurfaceFrame {
                        boot_id: self.client_shell_boot_id.clone(),
                        projection_revision: shell_projection_revision,
                        frame,
                        panes,
                        splits,
                        popup,
                        graphics,
                    })
            } else {
                client.render_state.prepare_frame(frame)
            };
            let Some(mut prepared) = prepared else {
                client.clear_deferred_render();
                crate::render_prof::event("full_render.skip_identical");
                continue;
            };
            let max = if has_graphics {
                MAX_GRAPHICS_FRAME_SIZE
            } else {
                crate::protocol::MAX_FRAME_SIZE
            };
            let mut shell_assets_deferred = false;
            let serialized = match Self::frame_server_message_with_max(prepared.message(), max) {
                Ok(frame) => frame,
                Err(protocol::FramingError::Oversized { claimed, max }) if has_graphics => {
                    warn!(
                        client_id,
                        claimed, max, "dropping graphics assets from oversized pane surface"
                    );
                    if !prepared.strip_pane_surface_assets() {
                        crate::render_prof::event("full_render.serialize_oversized");
                        continue;
                    }
                    next_shell_graphics_delivery = None;
                    shell_assets_deferred = true;
                    match Self::frame_server_message(prepared.message()) {
                        Ok(framed) => framed,
                        Err(err) => {
                            warn!(client_id, err = %err, "failed to serialize pane surface without assets");
                            broken_clients.push(client_id);
                            crate::render_prof::event("full_render.serialize_error");
                            continue;
                        }
                    }
                }
                Err(protocol::FramingError::Oversized { claimed, max }) => {
                    warn!(
                        client_id,
                        claimed, max, "skipping oversized frame for client"
                    );
                    crate::render_prof::event("full_render.serialize_oversized");
                    continue;
                }
                Err(err) => {
                    warn!(client_id, err = %err, "failed to serialize frame");
                    broken_clients.push(client_id);
                    crate::render_prof::event("full_render.serialize_error");
                    continue;
                }
            };
            let shell_graphics_pending = next_shell_graphics_delivery
                .as_ref()
                .is_some_and(crate::kitty_graphics::surface::DeliveryCache::has_pending);
            match writer.render.try_send(serialized) {
                Ok(()) => {
                    if let Some(delivery) = next_shell_graphics_delivery {
                        client.shell_graphics_delivery = delivery;
                    }
                    client.render_state.commit_sent_frame(prepared);
                    if shell_graphics_pending || shell_assets_deferred {
                        client.defer_full_render();
                        deferred_frame = true;
                    } else {
                        client.clear_deferred_render();
                    }
                    crate::render_prof::event("full_render.sent");
                }
                Err(std::sync::mpsc::TrySendError::Full(_)) => {
                    client.defer_full_render();
                    deferred_frame = true;
                }
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                    broken_clients.push(client_id);
                }
            }
        }

        if !broken_clients.is_empty() {
            for client_id in broken_clients {
                self.remove_client_and_resize_if_needed(client_id);
            }
        }

        let (cols, rows) = self.effective_size;
        if !deferred_frame {
            self.app.full_redraw_pending = false;
        }
        crate::render_prof::duration_since("full_render.total", full_started);
        debug!(cols, rows, foreground_client_id = ?self.foreground_client_id, "rendered virtual frame(s)");
    }
}
