use super::*;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

impl ClientShellState {
    pub(super) fn handle_mouse(&mut self, mouse: MouseEvent, outcome: &mut ClientShellInput) {
        let point = (mouse.column, mouse.row);
        if let Some(gesture) = self.pane_mouse_gesture.as_ref() {
            let gesture_event = matches!(
                mouse.kind,
                MouseEventKind::Drag(button) | MouseEventKind::Up(button)
                    if button == gesture.button
            );
            if gesture_event {
                let button = gesture.button;
                let modifiers = mouse.modifiers.difference(gesture.stripped_modifiers);
                let hit = self
                    .hits
                    .panes
                    .iter()
                    .find(|hit| hit.pane_id == gesture.hit.pane_id)
                    .cloned()
                    .unwrap_or_else(|| gesture.hit.clone());
                self.push_pane_mouse_event(&hit, mouse, modifiers, outcome);
                if mouse.kind == MouseEventKind::Up(button) {
                    self.pane_mouse_gesture = None;
                }
                return;
            }
            if matches!(
                mouse.kind,
                MouseEventKind::Down(_) | MouseEventKind::Drag(_) | MouseEventKind::Up(_)
            ) {
                return;
            }
        }
        if matches!(self.overlay, Some(ClientShellOverlay::ContextMenu(_))) {
            let row_hit = self
                .hits
                .context_menu_rows
                .iter()
                .find(|(rect, _)| super::contains(*rect, point))
                .copied();
            match mouse.kind {
                MouseEventKind::Moved => {
                    if let (Some((_, index)), Some(ClientShellOverlay::ContextMenu(menu))) =
                        (row_hit, self.overlay.as_mut())
                    {
                        menu.highlighted = index;
                        outcome.repaint = true;
                    }
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some((_, index)) = row_hit {
                        self.activate_context_menu_item(index, outcome);
                    } else {
                        self.overlay = None;
                        outcome.repaint = true;
                    }
                }
                _ => {}
            }
            return;
        }
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
            MouseEventKind::Down(MouseButton::Right) => {
                let pane_hit = self
                    .hits
                    .panes
                    .iter()
                    .find(|hit| super::contains(hit.inner_rect, point))
                    .cloned();
                if let Some(hit) = pane_hit {
                    let pane_owns_right_click = self
                        .snapshot
                        .as_deref()
                        .and_then(|snapshot| {
                            snapshot
                                .panes
                                .iter()
                                .find(|pane| pane.pane_id == hit.pane_id)
                        })
                        .is_some_and(|pane| pane.right_click_passthrough)
                        && mouse.modifiers.is_empty();
                    let configured_modifiers = self
                        .config
                        .right_click_passthrough_modifiers
                        .filter(|modifiers| *modifiers == mouse.modifiers);
                    if hit.mouse_reporting
                        && (pane_owns_right_click || configured_modifiers.is_some())
                    {
                        let stripped_modifiers =
                            configured_modifiers.unwrap_or(crossterm::event::KeyModifiers::empty());
                        self.push_pane_mouse_event(
                            &hit,
                            mouse,
                            mouse.modifiers.difference(stripped_modifiers),
                            outcome,
                        );
                        self.push_endpoint_method(
                            crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget {
                                pane_id: hit.pane_id.clone(),
                            }),
                            outcome,
                        );
                        self.pane_mouse_gesture = Some(ClientPaneMouseGesture {
                            hit,
                            button: MouseButton::Right,
                            stripped_modifiers,
                        });
                        return;
                    }
                }
                let workspace_id = (!self.sidebar_collapsed)
                    .then(|| {
                        self.hits
                            .workspaces
                            .iter()
                            .find(|hit| super::contains(hit.rect, point))
                            .map(|hit| hit.workspace_id.clone())
                    })
                    .flatten();
                if let Some(workspace_id) = workspace_id {
                    self.open_workspace_context_menu(workspace_id, mouse.column, mouse.row);
                    outcome.repaint = true;
                    return;
                }
                let tab_id = self
                    .hits
                    .tabs
                    .iter()
                    .find(|(rect, _)| super::contains(*rect, point))
                    .map(|(_, tab_id)| tab_id.clone());
                if let Some(tab_id) = tab_id {
                    self.open_tab_context_menu(tab_id, mouse.column, mouse.row);
                    outcome.repaint = true;
                    return;
                }
                let pane_id = self
                    .hits
                    .panes
                    .iter()
                    .find(|hit| super::contains(hit.rect, point))
                    .map(|hit| hit.pane_id.clone());
                if let Some(pane_id) = pane_id {
                    self.open_pane_context_menu(pane_id, mouse.column, mouse.row);
                    outcome.repaint = true;
                }
            }
            MouseEventKind::ScrollUp
                if (self
                    .hits
                    .tabs
                    .iter()
                    .any(|(rect, _)| super::contains(*rect, point))
                    || super::contains(self.hits.tab_scroll_left, point)
                    || super::contains(self.hits.tab_scroll_right, point)
                    || super::contains(self.hits.new_tab, point))
                    && (self.hits.tab_scroll_left.width > 0
                        || self.hits.tab_scroll_right.width > 0) =>
            {
                self.tab_scroll = self.tab_scroll.saturating_sub(1);
                outcome.repaint = true;
            }
            MouseEventKind::ScrollDown
                if (self
                    .hits
                    .tabs
                    .iter()
                    .any(|(rect, _)| super::contains(*rect, point))
                    || super::contains(self.hits.tab_scroll_left, point)
                    || super::contains(self.hits.tab_scroll_right, point)
                    || super::contains(self.hits.new_tab, point))
                    && (self.hits.tab_scroll_left.width > 0
                        || self.hits.tab_scroll_right.width > 0) =>
            {
                self.tab_scroll = self.tab_scroll.saturating_add(1);
                outcome.repaint = true;
            }
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
                if super::contains(self.hits.new_workspace, point) {
                    self.record_binding(
                        crate::input::KeybindMatch::Action(
                            crate::input::KeybindAction::NewWorkspace,
                        ),
                        outcome,
                    );
                    return;
                }
                if super::contains(self.hits.new_tab, point) {
                    self.record_binding(
                        crate::input::KeybindMatch::Action(crate::input::KeybindAction::NewTab),
                        outcome,
                    );
                    return;
                }
                if super::contains(self.hits.tab_scroll_left, point) {
                    self.tab_scroll = self.tab_scroll.saturating_sub(1);
                    outcome.repaint = true;
                    return;
                }
                if super::contains(self.hits.tab_scroll_right, point) {
                    let tab_count = self
                        .snapshot
                        .as_deref()
                        .and_then(|snapshot| {
                            snapshot.focused_workspace_id.as_deref().map(|id| {
                                snapshot
                                    .tabs
                                    .iter()
                                    .filter(|tab| tab.workspace_id == id)
                                    .count()
                            })
                        })
                        .unwrap_or(0);
                    self.tab_scroll = self
                        .tab_scroll
                        .saturating_add(1)
                        .min(tab_count.saturating_sub(1));
                    outcome.repaint = true;
                    return;
                }
                if super::contains(self.hits.sidebar_toggle, point) {
                    self.sidebar_collapsed = !self.sidebar_collapsed;
                    self.invalidate_pane_surface();
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
                let agent_pane_id = self
                    .hits
                    .agents
                    .iter()
                    .find(|(rect, _)| super::contains(*rect, point))
                    .map(|(_, pane_id)| pane_id.clone());
                if let Some(pane_id) = agent_pane_id {
                    self.push_endpoint_method(
                        crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget {
                            pane_id,
                        }),
                        outcome,
                    );
                    return;
                }
                let pane_hit = self
                    .hits
                    .panes
                    .iter()
                    .find(|hit| super::contains(hit.rect, point))
                    .cloned();
                if let Some(hit) = pane_hit {
                    if hit.mouse_reporting && super::contains(hit.inner_rect, point) {
                        self.push_pane_mouse_event(&hit, mouse, mouse.modifiers, outcome);
                        self.pane_mouse_gesture = Some(ClientPaneMouseGesture {
                            hit: hit.clone(),
                            button: MouseButton::Left,
                            stripped_modifiers: crossterm::event::KeyModifiers::empty(),
                        });
                    }
                    self.push_endpoint_method(
                        crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget {
                            pane_id: hit.pane_id,
                        }),
                        outcome,
                    );
                }
            }
            MouseEventKind::Down(MouseButton::Middle) => {
                if let Some(hit) = self
                    .hits
                    .panes
                    .iter()
                    .find(|hit| super::contains(hit.inner_rect, point) && hit.mouse_reporting)
                    .cloned()
                {
                    self.push_pane_mouse_event(&hit, mouse, mouse.modifiers, outcome);
                    self.pane_mouse_gesture = Some(ClientPaneMouseGesture {
                        hit,
                        button: MouseButton::Middle,
                        stripped_modifiers: crossterm::event::KeyModifiers::empty(),
                    });
                }
            }
            MouseEventKind::Up(MouseButton::Left | MouseButton::Middle)
            | MouseEventKind::Drag(MouseButton::Left | MouseButton::Middle) => {}
            MouseEventKind::Moved => {
                if let Some(hit) = self
                    .hits
                    .panes
                    .iter()
                    .find(|hit| super::contains(hit.inner_rect, point) && hit.mouse_reporting)
                    .cloned()
                {
                    self.push_pane_mouse_event(&hit, mouse, mouse.modifiers, outcome);
                }
            }
            MouseEventKind::ScrollUp
            | MouseEventKind::ScrollDown
            | MouseEventKind::ScrollLeft
            | MouseEventKind::ScrollRight => {
                if let Some(hit) = self
                    .hits
                    .panes
                    .iter()
                    .find(|hit| super::contains(hit.inner_rect, point))
                    .cloned()
                {
                    self.push_pane_mouse_event(&hit, mouse, mouse.modifiers, outcome);
                }
            }
            _ => {}
        }
    }

    fn push_pane_mouse_event(
        &self,
        hit: &PaneHit,
        mouse: MouseEvent,
        modifiers: crossterm::event::KeyModifiers,
        outcome: &mut ClientShellInput,
    ) {
        let Some(kind) = crate::protocol::ClientMouseKind::from_crossterm(mouse.kind) else {
            return;
        };
        let cell = ClientMousePosition::Cell {
            column: mouse.column.saturating_sub(hit.inner_rect.x),
            row: mouse.row.saturating_sub(hit.inner_rect.y),
        };
        let position = if hit.sgr_pixel_mouse && hit.pixel_width > 0 && hit.pixel_height > 0 {
            self.host_mouse_pixels
                .and_then(|pixels| {
                    pixels
                        .pane_position(hit.inner_rect, hit.pixel_width, hit.pixel_height)
                        .and_then(|position| match position {
                            crate::input::mouse::Position::Pixels { x, y } => {
                                Some(ClientMousePosition::Pixels {
                                    x,
                                    y,
                                    column: mouse.column.saturating_sub(hit.inner_rect.x),
                                    row: mouse.row.saturating_sub(hit.inner_rect.y),
                                })
                            }
                            crate::input::mouse::Position::Cell { .. } => None,
                        })
                })
                .unwrap_or(cell)
        } else {
            cell
        };
        push_pane_event(
            hit.pane_id.clone(),
            ClientPaneInputEvent::Mouse {
                kind,
                position,
                modifiers: modifiers.bits(),
                lines: self.config.mouse_scroll_lines.min(u16::MAX as usize) as u16,
            },
            outcome,
        );
    }
}
