use super::*;

impl ClientShellState {
    pub(crate) fn compose(&mut self, cols: u16, rows: u16) -> Option<FrameData> {
        let snapshot = self.snapshot.as_deref()?;
        let surface = self.pane_surface.as_ref()?;
        if snapshot.revision != surface.projection_revision {
            return None;
        }
        let layout = self.layout(cols, rows);
        let mut buffer = Buffer::empty(Rect::new(0, 0, cols, rows));
        self.hits = render::render_shell(
            &mut buffer,
            layout,
            snapshot,
            &self.config,
            &self.collapsed_groups,
            &mut self.workspace_scroll,
            &mut self.tab_scroll,
            self.sidebar_collapsed,
            (self.mode == ClientShellMode::Navigate)
                .then_some(self.navigate_workspace_id.as_deref())
                .flatten(),
        );
        self.hits.panes = surface
            .panes
            .iter()
            .map(|pane| {
                (
                    Rect::new(
                        layout.pane_surface.x.saturating_add(pane.rect.x),
                        layout.pane_surface.y.saturating_add(pane.rect.y),
                        pane.rect.width,
                        pane.rect.height,
                    ),
                    pane.pane_id.clone(),
                )
            })
            .collect();
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
        if let Some(overlay) = self.overlay.as_ref() {
            let graphics = std::mem::take(&mut frame.graphics);
            let mut composed = frame.to_ratatui_buffer()?;
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
            frame = FrameData::from_ratatui_buffer_with_hyperlinks(&composed, rendered.cursor, &[]);
            frame.graphics = graphics;
        }
        Some(frame)
    }
}
