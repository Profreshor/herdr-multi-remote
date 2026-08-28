use super::*;

impl ClientShellState {
    pub(crate) fn compose(&mut self, cols: u16, rows: u16) -> Option<FrameData> {
        let snapshot = self.snapshot.as_deref()?;
        let surface = self.pane_surface.as_ref()?;
        if snapshot.revision != surface.projection_revision {
            return None;
        }
        let layout = self.layout(cols, rows);
        if self.last_tab_bar_width != Some(layout.tab_bar.width) {
            self.last_tab_bar_width = Some(layout.tab_bar.width);
            self.reveal_focused_tab = true;
        }
        let tab_drag_insert_index = match &self.chrome_drag {
            Some(ClientChromeDrag::Tab { insert_index, .. }) => *insert_index,
            _ => None,
        };
        let (dragged_workspace_id, workspace_drop_indicator_row) = match &self.chrome_drag {
            Some(ClientChromeDrag::Workspace {
                source_workspace_id,
                target,
            }) => (
                Some(source_workspace_id.as_str()),
                target.as_ref().map(|(_, row)| *row),
            ),
            _ => (None, None),
        };
        let mut buffer = Buffer::empty(Rect::new(0, 0, cols, rows));
        self.hits = render::render_shell(
            &mut buffer,
            layout,
            snapshot,
            &self.config,
            render::ShellRenderState {
                collapsed_groups: &self.collapsed_groups,
                workspace_scroll: &mut self.workspace_scroll,
                agent_scroll: &mut self.agent_scroll,
                tab_scroll: &mut self.tab_scroll,
                reveal_focused_tab: &mut self.reveal_focused_tab,
                sidebar_collapsed: self.sidebar_collapsed,
                sidebar_section_split: self.sidebar_section_split,
                tab_drag_insert_index,
                selected_workspace_id: (self.mode == ClientShellMode::Navigate)
                    .then_some(self.navigate_workspace_id.as_deref())
                    .flatten(),
                dragged_workspace_id,
                workspace_drop_indicator_row,
            },
        );
        self.hits.panes = surface
            .panes
            .iter()
            .map(|pane| PaneHit {
                rect: Rect::new(
                    layout.pane_surface.x.saturating_add(pane.rect.x),
                    layout.pane_surface.y.saturating_add(pane.rect.y),
                    pane.rect.width,
                    pane.rect.height,
                ),
                inner_rect: Rect::new(
                    layout.pane_surface.x.saturating_add(pane.inner_rect.x),
                    layout.pane_surface.y.saturating_add(pane.inner_rect.y),
                    pane.inner_rect.width,
                    pane.inner_rect.height,
                ),
                pane_id: pane.pane_id.clone(),
                mouse_reporting: pane.mouse_reporting,
                sgr_pixel_mouse: pane.sgr_pixel_mouse,
                pixel_width: pane.pixel_width,
                pixel_height: pane.pixel_height,
            })
            .collect();
        let topology_signature = pane_surface_topology_signature(surface);
        self.hits.pane_splits = surface
            .splits
            .iter()
            .map(|split| PaneSplitHit {
                direction: split.direction,
                pos: match split.direction {
                    crate::protocol::PaneSurfaceSplitDirection::Horizontal => {
                        layout.pane_surface.x.saturating_add(split.pos)
                    }
                    crate::protocol::PaneSurfaceSplitDirection::Vertical => {
                        layout.pane_surface.y.saturating_add(split.pos)
                    }
                },
                area: Rect::new(
                    layout.pane_surface.x.saturating_add(split.area.x),
                    layout.pane_surface.y.saturating_add(split.area.y),
                    split.area.width,
                    split.area.height,
                ),
                hit_rect: Rect::new(
                    layout.pane_surface.x.saturating_add(split.hit_rect.x),
                    layout.pane_surface.y.saturating_add(split.hit_rect.y),
                    split.hit_rect.width,
                    split.hit_rect.height,
                ),
                path: split.path.clone(),
                topology_signature,
            })
            .collect();
        if !self.config.mouse_capture {
            self.hits.pane_splits.clear();
        }
        let mode_bar = render::render_mode_bar(
            &mut buffer,
            layout.pane_surface,
            self.mode,
            self.endpoint_error.as_deref(),
            &self.config.keybinds,
            &self.config.palette,
        );
        let mut frame = FrameData::from_ratatui_buffer_with_hyperlinks(&buffer, None, &[]);
        let mode_bar_cells = mode_bar.map(|bar| {
            let start = usize::from(bar.y) * usize::from(frame.width) + usize::from(bar.x);
            frame.cells[start..start + usize::from(bar.width)].to_vec()
        });
        blit_pane_surface(&mut frame, &surface.frame, layout.pane_surface);
        if let (Some(bar), Some(cells)) = (mode_bar, mode_bar_cells) {
            let start = usize::from(bar.y) * usize::from(frame.width) + usize::from(bar.x);
            frame.cells[start..start + usize::from(bar.width)].clone_from_slice(&cells);
            if frame
                .cursor
                .as_ref()
                .is_some_and(|cursor| cursor.y == bar.y)
            {
                frame.cursor = None;
            }
        }
        self.hits.notification_toast = Rect::default();
        if let Some(notification) = self.visible_notification.as_ref() {
            let graphics = std::mem::take(&mut frame.graphics);
            let cursor = frame.cursor.clone();
            let mut composed = frame.to_ratatui_buffer()?;
            self.hits.notification_toast = notifications::render_visible_notification(
                &mut composed,
                layout.pane_surface,
                notification,
                self.config.toast_position,
                &self.config.palette,
            );
            frame = FrameData::from_ratatui_buffer_with_hyperlinks(&composed, cursor, &[]);
            frame.graphics = graphics;
        }
        if let Some(overlay) = self.overlay.as_ref() {
            let graphics = std::mem::take(&mut frame.graphics);
            let mut composed = frame.to_ratatui_buffer()?;
            let cursor = if let ClientShellOverlay::ContextMenu(menu) = overlay {
                self.hits.context_menu_rows =
                    render::render_context_menu(&mut composed, menu, &self.config.palette)?;
                None
            } else if let ClientShellOverlay::GlobalMenu(menu) = overlay {
                self.hits.global_menu_rows = render::render_global_menu(
                    &mut composed,
                    self.hits.global_launcher,
                    menu,
                    &self.config.palette,
                )?;
                None
            } else {
                let rendered = render::render_client_overlay(
                    &mut composed,
                    overlay,
                    snapshot,
                    &self.config.keybinds,
                    &self.config.palette,
                )?;
                self.hits.overlay_primary = rendered.primary;
                self.hits.overlay_clear = rendered.clear;
                self.hits.overlay_cancel = rendered.cancel;
                self.hits.navigator_popup = rendered.navigator_popup;
                self.hits.navigator_search = rendered.navigator_search;
                self.hits.navigator_rows = rendered.navigator_rows;
                self.hits.worktree_search = rendered.worktree_search;
                self.hits.worktree_rows = rendered.worktree_rows;
                self.hits.help_popup = rendered.help_popup;
                self.hits.help_scrollbar = rendered.help_scrollbar;
                self.hits.help_scroll_metrics = rendered.help_scroll_metrics;
                self.hits.help_max_scroll = rendered.help_max_scroll;
                self.hits.settings_popup = rendered.settings_popup;
                self.hits.settings_tabs = rendered.settings_tabs;
                self.hits.settings_choices = rendered.settings_choices;
                rendered.cursor
            };
            frame = FrameData::from_ratatui_buffer_with_hyperlinks(&composed, cursor, &[]);
            frame.graphics = graphics;
        }
        if let Some(ClientShellOverlay::Help(help)) = self.overlay.as_mut() {
            help.scroll = help.scroll.min(self.hits.help_max_scroll);
        }
        Some(frame)
    }
}
