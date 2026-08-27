use super::*;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

impl ClientShellState {
    pub(super) fn handle_mouse(&mut self, mouse: MouseEvent, outcome: &mut ClientShellInput) {
        let point = (mouse.column, mouse.row);
        if matches!(
            self.overlay,
            Some(
                ClientShellOverlay::WorktreeCreate(_)
                    | ClientShellOverlay::WorktreeOpen(_)
                    | ClientShellOverlay::WorktreeRemove(_)
            )
        ) {
            match mouse.kind {
                MouseEventKind::ScrollUp
                    if matches!(self.overlay, Some(ClientShellOverlay::WorktreeOpen(_))) =>
                {
                    self.move_worktree_open_selection(-1);
                    outcome.repaint = true;
                }
                MouseEventKind::ScrollDown
                    if matches!(self.overlay, Some(ClientShellOverlay::WorktreeOpen(_))) =>
                {
                    self.move_worktree_open_selection(1);
                    outcome.repaint = true;
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if super::contains(self.hits.overlay_cancel, point) {
                        let busy =
                            matches!(
                                self.overlay,
                                Some(
                                    ClientShellOverlay::WorktreeCreate(
                                        ClientWorktreeCreateOverlay { creating: true, .. }
                                    ) | ClientShellOverlay::WorktreeOpen(
                                        ClientWorktreeOpenOverlay { opening: true, .. }
                                    ) | ClientShellOverlay::WorktreeRemove(
                                        ClientWorktreeRemoveOverlay { removing: true, .. }
                                    )
                                )
                            );
                        if !busy {
                            self.overlay = None;
                            outcome.repaint = true;
                        }
                    } else if super::contains(self.hits.worktree_search, point) {
                        if let Some(ClientShellOverlay::WorktreeOpen(open)) = self.overlay.as_mut()
                        {
                            open.search_focused = true;
                            outcome.repaint = true;
                        }
                    } else if let Some((_, index)) = self
                        .hits
                        .worktree_rows
                        .iter()
                        .find(|(rect, _)| super::contains(*rect, point))
                        .copied()
                    {
                        if let Some(ClientShellOverlay::WorktreeOpen(open)) = self.overlay.as_mut()
                        {
                            open.selected = index;
                        }
                        self.submit_worktree_open(outcome);
                    } else if super::contains(self.hits.overlay_primary, point) {
                        match self.overlay.as_ref() {
                            Some(ClientShellOverlay::WorktreeCreate(_)) => {
                                self.submit_worktree_create(outcome)
                            }
                            Some(ClientShellOverlay::WorktreeOpen(_)) => {
                                self.submit_worktree_open(outcome)
                            }
                            Some(ClientShellOverlay::WorktreeRemove(_)) => {
                                self.submit_worktree_remove(outcome)
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            return;
        }
        if let Some(ClientShellOverlay::Help(help)) = self.overlay.as_mut() {
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    help.scroll = help.scroll.saturating_sub(1);
                    outcome.repaint = true;
                }
                MouseEventKind::ScrollDown => {
                    help.scroll = help.scroll.saturating_add(1);
                    outcome.repaint = true;
                }
                MouseEventKind::Down(MouseButton::Left)
                    if super::contains(self.hits.overlay_primary, point)
                        || super::contains(self.hits.overlay_cancel, point) =>
                {
                    self.overlay = None;
                    outcome.repaint = true;
                }
                _ => {}
            }
            return;
        }
        if matches!(self.overlay, Some(ClientShellOverlay::Navigator(_))) {
            let row_hit = self
                .hits
                .navigator_rows
                .iter()
                .find(|(rect, _)| super::contains(*rect, point))
                .copied();
            match mouse.kind {
                MouseEventKind::Moved => {
                    if let Some((_, index)) = row_hit {
                        if let Some(ClientShellOverlay::Navigator(navigator)) =
                            self.overlay.as_mut()
                        {
                            navigator.selected = index;
                        }
                        outcome.repaint = true;
                    }
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if super::contains(self.hits.navigator_search, point) {
                        if let Some(ClientShellOverlay::Navigator(navigator)) =
                            self.overlay.as_mut()
                        {
                            navigator.search_focused = true;
                            navigator.filter = None;
                        }
                        outcome.repaint = true;
                    } else if let Some((rect, index)) = row_hit {
                        if let Some(ClientShellOverlay::Navigator(navigator)) =
                            self.overlay.as_mut()
                        {
                            navigator.selected = index;
                        }
                        let workspace = self
                            .snapshot
                            .as_deref()
                            .zip(self.overlay.as_ref())
                            .and_then(|(snapshot, overlay)| match overlay {
                                ClientShellOverlay::Navigator(navigator) => {
                                    render::client_navigator_rows(snapshot, navigator)
                                        .get(index)
                                        .map(|row| {
                                            matches!(
                                                row.target,
                                                ClientNavigatorTarget::Workspace(_)
                                            )
                                        })
                                }
                                _ => None,
                            })
                            .unwrap_or(false);
                        if workspace && mouse.column <= rect.x.saturating_add(3) {
                            self.toggle_selected_navigator_workspace();
                            outcome.repaint = true;
                        } else {
                            self.accept_navigator_selection(outcome);
                        }
                    } else if !super::contains(self.hits.navigator_popup, point) {
                        self.overlay = None;
                        outcome.repaint = true;
                    }
                }
                MouseEventKind::ScrollUp => {
                    self.move_navigator_selection(-3);
                    outcome.repaint = true;
                }
                MouseEventKind::ScrollDown => {
                    self.move_navigator_selection(3);
                    outcome.repaint = true;
                }
                _ => {}
            }
            return;
        }
        if self.overlay.is_some() {
            if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
                return;
            }
            if super::contains(self.hits.overlay_primary, point) {
                match self.overlay.as_ref() {
                    Some(ClientShellOverlay::Rename(_)) => self.save_rename_overlay(outcome),
                    Some(ClientShellOverlay::ConfirmClose(_)) => {
                        let Some(ClientShellOverlay::ConfirmClose(confirm)) = self.overlay.take()
                        else {
                            return;
                        };
                        self.push_endpoint_method(
                            crate::api::schema::Method::WorkspaceClose(
                                crate::api::schema::WorkspaceCloseParams {
                                    workspace_id: confirm.workspace_id,
                                    close_group: true,
                                },
                            ),
                            outcome,
                        );
                        outcome.repaint = true;
                    }
                    _ => {}
                }
            } else if super::contains(self.hits.overlay_clear, point) {
                if let Some(ClientShellOverlay::Rename(rename)) = self.overlay.as_mut() {
                    rename.input.clear();
                    rename.replace_on_type = false;
                    outcome.repaint = true;
                }
            } else {
                self.overlay = None;
                outcome.repaint = true;
            }
            return;
        }

        match mouse.kind {
            MouseEventKind::ScrollUp
                if self
                    .hits
                    .workspaces
                    .iter()
                    .any(|hit| super::contains(hit.rect, point)) =>
            {
                self.workspace_scroll = self.workspace_scroll.saturating_sub(1);
                outcome.repaint = true;
            }
            MouseEventKind::ScrollDown
                if self
                    .hits
                    .workspaces
                    .iter()
                    .any(|hit| super::contains(hit.rect, point)) =>
            {
                self.workspace_scroll = self.workspace_scroll.saturating_add(1);
                outcome.repaint = true;
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if super::contains(self.hits.sidebar_toggle, point) {
                    self.sidebar_collapsed = !self.sidebar_collapsed;
                    self.pane_surface = None;
                    outcome.repaint = true;
                    outcome.resize = true;
                    return;
                }
                for hit in &self.hits.workspaces {
                    if let Some((rect, key)) = &hit.group_toggle {
                        if super::contains(*rect, point) {
                            if !self.collapsed_groups.remove(key) {
                                self.collapsed_groups.insert(key.clone());
                            }
                            outcome.repaint = true;
                            return;
                        }
                    }
                }
                let workspace_id = self
                    .hits
                    .workspaces
                    .iter()
                    .find(|hit| super::contains(hit.rect, point))
                    .map(|hit| hit.workspace_id.clone());
                if let Some(workspace_id) = workspace_id {
                    self.push_endpoint_method(
                        crate::api::schema::Method::WorkspaceFocus(
                            crate::api::schema::WorkspaceTarget { workspace_id },
                        ),
                        outcome,
                    );
                    return;
                }
                let tab_id = self
                    .hits
                    .tabs
                    .iter()
                    .find(|(rect, _)| super::contains(*rect, point))
                    .map(|(_, tab_id)| tab_id.clone());
                if let Some(tab_id) = tab_id {
                    self.push_endpoint_method(
                        crate::api::schema::Method::TabFocus(crate::api::schema::TabTarget {
                            tab_id,
                        }),
                        outcome,
                    );
                    return;
                }
                let pane_id = self
                    .hits
                    .panes
                    .iter()
                    .find(|(rect, _)| super::contains(*rect, point))
                    .map(|(_, pane_id)| pane_id.clone());
                if let Some(pane_id) = pane_id {
                    self.push_endpoint_method(
                        crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget {
                            pane_id,
                        }),
                        outcome,
                    );
                }
            }
            _ => {}
        }
    }
}
