use super::*;

fn rect_fits_frame(rect: protocol::SurfaceRect, frame: &FrameData) -> bool {
    rect.x.saturating_add(rect.width) <= frame.width
        && rect.y.saturating_add(rect.height) <= frame.height
}

fn patch_intersects_hyperlinks(
    frame: &FrameData,
    area: protocol::SurfaceRect,
    patch: &crate::pane::TerminalDirtyPatch,
) -> bool {
    if frame.hyperlinks.is_empty() || !rect_fits_frame(area, frame) {
        return false;
    }
    let width = usize::from(frame.width);
    patch.rows.iter().any(|(local_y, _)| {
        if *local_y >= area.height {
            return true;
        }
        let start = usize::from(area.y + *local_y) * width + usize::from(area.x);
        let end = start + usize::from(area.width);
        end > frame.cells.len()
            || frame.cells[start..end]
                .iter()
                .any(|cell| cell.hyperlink.is_some())
    })
}

fn apply_patch_row(frame: &mut FrameData, row: &protocol::PaneSurfacePatchRow) -> Option<bool> {
    if row.y >= frame.height
        || row.x.saturating_add(u16::try_from(row.cells.len()).ok()?) > frame.width
    {
        return None;
    }
    let start = usize::from(row.y) * usize::from(frame.width) + usize::from(row.x);
    let end = start + row.cells.len();
    if end > frame.cells.len() {
        return None;
    }
    let changed = frame.cells[start..end] != row.cells;
    frame.cells[start..end].clone_from_slice(&row.cells);
    Some(changed)
}

fn apply_rows(
    frame: &mut FrameData,
    area: protocol::SurfaceRect,
    patch: &crate::pane::TerminalDirtyPatch,
) -> Option<Vec<protocol::PaneSurfacePatchRow>> {
    if !rect_fits_frame(area, frame) {
        return None;
    }
    let mut rows = Vec::with_capacity(patch.rows.len());
    for (local_y, cells) in &patch.rows {
        if *local_y >= area.height || cells.len() != usize::from(area.width) {
            return None;
        }
        let y = area.y + *local_y;
        let row = protocol::PaneSurfacePatchRow {
            x: area.x,
            y,
            cells: cells.clone(),
        };
        apply_patch_row(frame, &row)?;
        rows.push(row);
    }
    Some(rows)
}

fn retained_scrollbar_patch(
    app: &app::App,
    frame: &mut FrameData,
    pane: &mut protocol::PaneSurfacePane,
    runtime: &crate::terminal::TerminalRuntime,
    metrics: Option<crate::pane::ScrollMetrics>,
) -> Option<Vec<protocol::PaneSurfacePatchRow>> {
    let next_rect = metrics
        .filter(|metrics| metrics.max_offset_from_bottom > 0)
        .filter(|_| app.state.pane_scrollbars && !runtime.alternate_screen_active())
        .and_then(|_| {
            let rect = protocol::SurfaceRect {
                x: pane.inner_rect.x.checked_add(pane.inner_rect.width)?,
                y: pane.inner_rect.y,
                width: 1,
                height: pane.inner_rect.height,
            };
            (rect_fits_frame(rect, frame)
                && rect.x >= pane.rect.x
                && rect.x < pane.rect.x.saturating_add(pane.rect.width))
            .then_some(rect)
        });
    let patch_rect = next_rect.or(pane.scrollbar_rect);
    pane.scrollbar_rect = next_rect;
    let Some(rect) = patch_rect else {
        return Some(Vec::new());
    };

    let track = Rect::new(0, 0, 1, rect.height);
    let mut buffer = ratatui::buffer::Buffer::empty(track);
    if let (Some(metrics), Some(_)) = (metrics, next_rect) {
        crate::ui::render_pane_scrollbar_buffer(
            &mut buffer,
            metrics,
            track,
            &app.state.palette,
            pane.focused,
        );
    }
    let cells = buffer
        .content
        .iter()
        .map(protocol::CellData::from_ratatui_cell)
        .collect::<Vec<_>>();
    let mut rows = Vec::new();
    for (offset, cell) in cells.into_iter().enumerate() {
        let row = protocol::PaneSurfacePatchRow {
            x: rect.x,
            y: rect.y.checked_add(u16::try_from(offset).ok()?)?,
            cells: vec![cell],
        };
        if apply_patch_row(frame, &row)? {
            rows.push(row);
        }
    }
    Some(rows)
}

fn retained_cursor(
    app: &app::App,
    panes: &[protocol::PaneSurfacePane],
) -> Option<protocol::CursorState> {
    let pane = panes.iter().find(|pane| pane.focused)?;
    let (workspace_index, pane_id) = app.parse_pane_id(&pane.pane_id)?;
    if !app.state.pane_exposes_host_cursor(workspace_index, pane_id) {
        return None;
    }
    let runtime = app.state.runtime_for_pane_in_workspace(
        &app.terminal_runtimes,
        workspace_index,
        pane_id,
    )?;
    if runtime.synchronized_output_active() {
        return None;
    }
    let area = Rect::new(
        pane.inner_rect.x,
        pane.inner_rect.y,
        pane.inner_rect.width,
        pane.inner_rect.height,
    );
    runtime
        .cursor_state(area, true)
        .map(|cursor| protocol::CursorState {
            x: cursor.x,
            y: cursor.y,
            visible: cursor.visible && !crate::ui::pane_is_scrolled_back(runtime),
            shape: cursor.shape,
        })
}

impl HeadlessServer {
    /// Applies terminal dirty rows to the committed origin-relative pane surface.
    /// Any presentation or geometry uncertainty falls back to the complete renderer.
    pub(super) fn render_retained_pane_surface_and_stream(
        &mut self,
        pty_sources: &HashSet<crate::layout::PaneId>,
    ) -> bool {
        crate::render_prof::event("retained_surface.attempt");
        let started = crate::render_prof::timer();
        macro_rules! fallback {
            ($reason:literal) => {{
                crate::render_prof::event(concat!("retained_surface.fallback.", $reason));
                crate::render_prof::duration_since("retained_surface.total", started);
                return false;
            }};
        }
        macro_rules! success {
            ($reason:literal) => {{
                crate::render_prof::event("retained_surface.success");
                crate::render_prof::event(concat!("retained_surface.success.", $reason));
                crate::render_prof::duration_since("retained_surface.total", started);
                return true;
            }};
        }

        if pty_sources.is_empty()
            || self.app.full_redraw_pending
            || self.app.state.popup_pane.is_some()
            || self.app.state.reveal_hidden_cursor_for_cjk_ime
        {
            fallback!("unsafe_state");
        }
        let targets = render_targets(&self.clients, self.foreground_client_id);
        let [(client_id, (cols, rows), _, _, ClientConnectionMode::ClientShell)] =
            targets.as_slice()
        else {
            fallback!("not_single_shell");
        };
        let Some(client) = self.clients.get(client_id) else {
            fallback!("client_missing");
        };
        if client.deferred_render() != DeferredRender::None {
            fallback!("render_deferred");
        }
        let Some(mut next_surface) = client.render_state.last_pane_surface().cloned() else {
            fallback!("no_baseline");
        };
        if next_surface.boot_id != self.client_shell_boot_id
            || next_surface.projection_revision != client.shell_projection_revision
            || next_surface.frame.width != *cols
            || next_surface.frame.height != *rows
            || next_surface.popup.is_some()
            || !next_surface.graphics.assets.is_empty()
            || !next_surface.graphics.placements.is_empty()
            || !next_surface.graphics.retained_assets.is_empty()
            || !next_surface.frame.graphics.is_empty()
        {
            fallback!("baseline_mismatch");
        }

        let mut changed_panes = Vec::new();
        let mut patch_rows = Vec::new();
        let mut metadata_changed = false;
        let mut matched_sources = HashSet::new();
        for pane in &mut next_surface.panes {
            let Some((workspace_index, pane_id)) = self.app.parse_pane_id(&pane.pane_id) else {
                fallback!("pane_missing");
            };
            if !pty_sources.contains(&pane_id) {
                continue;
            }
            matched_sources.insert(pane_id);
            let Some(runtime) = self.app.state.runtime_for_pane_in_workspace(
                &self.app.terminal_runtimes,
                workspace_index,
                pane_id,
            ) else {
                fallback!("runtime_missing");
            };
            let revision_before = runtime.content_seq();
            if !revision_before.is_multiple_of(2) {
                fallback!("unstable_content");
            }
            let patch =
                match runtime.collect_dirty_patch(pane.inner_rect.width, pane.inner_rect.height) {
                    crate::pane::TerminalDirtyPatchOutcome::Clean => {
                        crate::render_prof::event("retained_surface.pane_clean");
                        crate::pane::TerminalDirtyPatch { rows: Vec::new() }
                    }
                    crate::pane::TerminalDirtyPatchOutcome::Patch(patch) => patch,
                    crate::pane::TerminalDirtyPatchOutcome::Fallback => {
                        fallback!("terminal_patch");
                    }
                };
            let revision = runtime.content_seq();
            if revision != revision_before || !revision.is_multiple_of(2) {
                fallback!("content_changed");
            }
            if patch_intersects_hyperlinks(&next_surface.frame, pane.inner_rect, &patch) {
                fallback!("hyperlink");
            }
            let previous_pane = pane.clone();
            let Some(rows) = apply_rows(&mut next_surface.frame, pane.inner_rect, &patch) else {
                fallback!("invalid_patch");
            };
            patch_rows.extend(rows);
            let scroll_metrics = runtime.scroll_metrics();
            let Some(scrollbar_rows) = retained_scrollbar_patch(
                &self.app,
                &mut next_surface.frame,
                pane,
                runtime,
                scroll_metrics,
            ) else {
                fallback!("scrollbar_patch");
            };
            patch_rows.extend(scrollbar_rows);
            pane.content_revision = revision;
            pane.mouse_reporting = runtime.mouse_reporting_enabled();
            pane.sgr_pixel_mouse = runtime.sgr_pixel_mouse_enabled();
            pane.scroll = scroll_metrics.map(|metrics| protocol::PaneSurfaceScrollMetrics {
                offset_from_bottom: metrics.offset_from_bottom as u64,
                max_offset_from_bottom: metrics.max_offset_from_bottom as u64,
                viewport_rows: metrics.viewport_rows as u64,
            });
            metadata_changed |= *pane != previous_pane;
            changed_panes.push(pane.clone());
        }
        if !pty_sources.is_subset(&matched_sources) {
            fallback!("source_not_visible");
        }

        let cursor = retained_cursor(&self.app, &next_surface.panes);
        let cursor_changed = cursor != next_surface.frame.cursor;
        next_surface.frame.cursor = cursor.clone();
        if patch_rows.is_empty() && !cursor_changed && !metadata_changed {
            success!("unchanged");
        }

        let patch = protocol::PaneSurfacePatch {
            boot_id: self.client_shell_boot_id.clone(),
            projection_revision: client.shell_projection_revision,
            base_surface_revision: next_surface.surface_revision,
            surface_revision: 0,
            rows: patch_rows,
            panes: changed_panes,
            cursor,
        };
        let Some(client) = self.clients.get_mut(client_id) else {
            fallback!("client_disappeared");
        };
        let Some(writer) = client.writer.as_ref().cloned() else {
            fallback!("writer_missing");
        };
        let Some(prepared) = client
            .render_state
            .prepare_pane_surface_patch(patch, next_surface)
        else {
            fallback!("prepare");
        };
        let serialized = match Self::frame_server_message(prepared.message()) {
            Ok(serialized) => serialized,
            Err(error) => {
                warn!(client_id, %error, "failed to serialize retained pane surface patch");
                fallback!("serialize");
            }
        };
        crate::render_prof::counter("retained_surface.bytes", serialized.len() as u64);
        match writer.render.try_send(serialized) {
            Ok(()) => {
                client.clear_deferred_render();
                client.render_state.commit_sent_frame(prepared);
                success!("sent");
            }
            Err(std::sync::mpsc::TrySendError::Full(_)) => {
                client.defer_full_render();
                success!("queue_full");
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                self.remove_client_and_resize_if_needed(*client_id);
                success!("disconnected");
            }
        }
    }
}
