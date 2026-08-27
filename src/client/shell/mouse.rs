use super::*;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

impl ClientShellState {
    fn set_sidebar_width_from_column(&mut self, column: u16, outcome: &mut ClientShellInput) {
        let (min, max) = crate::config::validated_sidebar_bounds(
            self.config.sidebar_min_width,
            self.config.sidebar_max_width,
        )
        .unwrap_or((18, 36));
        let width = column.saturating_add(1).clamp(min, max);
        if self.sidebar_width != width {
            self.sidebar_width = width;
            self.sidebar_width_manual = true;
            self.invalidate_pane_surface();
            outcome.repaint = true;
            outcome.resize = true;
        }
    }

    fn set_sidebar_section_from_row(&mut self, row: u16, outcome: &mut ClientShellInput) {
        let divider = self.hits.sidebar_divider;
        if divider.height == 0 {
            return;
        }
        let ratio = row.saturating_sub(divider.y) as f32 / divider.height as f32;
        let ratio = ratio.clamp(0.1, 0.9);
        if (self.sidebar_section_split - ratio).abs() > f32::EPSILON {
            self.sidebar_section_split = ratio;
            self.sidebar_section_split_manual = true;
            outcome.repaint = true;
        }
    }

    fn pane_split_target_is_current(&self, hit: &PaneSplitHit, tab_id: &str) -> Option<bool> {
        let snapshot = self.snapshot.as_deref()?;
        let surface = self.pane_surface.as_ref()?;
        if snapshot.revision != surface.projection_revision {
            return None;
        }
        Some(
            snapshot.focused_tab_id.as_deref() == Some(tab_id)
                && pane_surface_topology_signature(surface) == hit.topology_signature,
        )
    }

    fn pane_split_ratio(hit: &PaneSplitHit, grab_offset: i32, point: (u16, u16)) -> f32 {
        let (pointer, origin, length) = match hit.direction {
            crate::protocol::PaneSurfaceSplitDirection::Horizontal => {
                (i32::from(point.0), i32::from(hit.area.x), hit.area.width)
            }
            crate::protocol::PaneSurfaceSplitDirection::Vertical => {
                (i32::from(point.1), i32::from(hit.area.y), hit.area.height)
            }
        };
        ((pointer + grab_offset - origin) as f32 / f32::from(length.max(1))).clamp(0.1, 0.9)
    }

    fn tab_drop_index_at(&self, point: (u16, u16)) -> Option<usize> {
        let snapshot = self.snapshot.as_deref()?;
        let workspace_id = snapshot.focused_workspace_id.as_deref()?;
        let tabs = snapshot
            .tabs
            .iter()
            .filter(|tab| tab.workspace_id == workspace_id)
            .collect::<Vec<_>>();
        let visible = self
            .hits
            .tabs
            .iter()
            .filter_map(|(rect, tab_id)| {
                tabs.iter()
                    .position(|tab| tab.tab_id == *tab_id)
                    .map(|index| (index, *rect))
            })
            .collect::<Vec<_>>();
        let (first_index, first_rect) = *visible.first()?;
        let (last_index, last_rect) = *visible.last()?;
        let on_tab_row = point.1 == first_rect.y;
        if !on_tab_row {
            return None;
        }
        if super::contains(self.hits.tab_scroll_left, point) {
            return Some(0);
        }
        if super::contains(self.hits.tab_scroll_right, point) {
            return Some(tabs.len());
        }
        let left_edge = if first_index == 0 {
            first_rect.x
        } else {
            self.hits.tab_scroll_left.right()
        };
        let right_edge = if last_index + 1 >= tabs.len() {
            last_rect.right()
        } else {
            self.hits.tab_scroll_right.x.saturating_sub(1)
        };
        if point.0 <= left_edge {
            return Some(first_index);
        }
        if point.0 >= right_edge {
            return Some(last_index + 1);
        }
        for (index, rect) in visible {
            let midpoint = rect.x + rect.width / 2;
            if point.0 < midpoint {
                return Some(index);
            }
            if point.0 < rect.right() {
                return Some(index + 1);
            }
        }
        Some(last_index + 1)
    }

    fn workspace_drop_target_at(&self, point: (u16, u16)) -> Option<(Option<String>, u16)> {
        if self.hits.workspace_body.height == 0
            || point.1 < self.hits.workspace_body.y.saturating_sub(1)
            || point.1 >= self.hits.new_workspace.y
        {
            return None;
        }
        let mut slots = self
            .hits
            .workspaces
            .iter()
            .filter(|hit| !hit.indented)
            .map(|hit| (Some(hit.workspace_id.clone()), hit.rect.y.saturating_sub(1)))
            .collect::<Vec<_>>();
        let snapshot = self.snapshot.as_deref()?;
        let entries = render::workspace_entries(snapshot, &self.collapsed_groups);
        let last_hit = self.hits.workspaces.last()?;
        let last_position = entries.iter().position(|entry| {
            snapshot
                .workspaces
                .get(entry.index)
                .is_some_and(|workspace| workspace.workspace_id == last_hit.workspace_id)
        })?;
        let next = entries.get(last_position + 1);
        if !next.is_some_and(|entry| entry.indented) {
            let before = next.and_then(|entry| {
                snapshot
                    .workspaces
                    .get(entry.index)
                    .map(|workspace| workspace.workspace_id.clone())
            });
            let row = last_hit.rect.bottom();
            if row < self.hits.new_workspace.y {
                slots.push((before, row));
            }
        }
        slots
            .into_iter()
            .enumerate()
            .min_by_key(|(index, (_, row))| (point.1.abs_diff(*row), *index))
            .map(|(_, target)| target)
    }

    fn workspace_move_method(
        &self,
        source_workspace_id: &str,
        before_workspace_id: Option<&str>,
    ) -> Option<crate::api::schema::Method> {
        let snapshot = self.snapshot.as_deref()?;
        let source = snapshot
            .workspaces
            .iter()
            .find(|workspace| workspace.workspace_id == source_workspace_id)?;
        if source
            .worktree
            .as_ref()
            .is_some_and(|worktree| worktree.is_linked_worktree)
        {
            return None;
        }
        if before_workspace_id == Some(source_workspace_id) {
            return None;
        }
        let roots = snapshot
            .workspaces
            .iter()
            .filter(|workspace| {
                !workspace
                    .worktree
                    .as_ref()
                    .is_some_and(|worktree| worktree.is_linked_worktree)
            })
            .collect::<Vec<_>>();
        let source_position = roots
            .iter()
            .position(|workspace| workspace.workspace_id == source_workspace_id)?;
        let remaining = roots
            .iter()
            .copied()
            .filter(|workspace| workspace.workspace_id != source_workspace_id)
            .collect::<Vec<_>>();
        let insert_position = match before_workspace_id {
            Some(target) => remaining
                .iter()
                .position(|workspace| workspace.workspace_id == target)?,
            None => remaining.len(),
        };
        if insert_position == source_position {
            return None;
        }

        if let Some(worktree) = source.worktree.as_ref() {
            let workspace_ids = std::iter::once(source.workspace_id.clone())
                .chain(
                    snapshot
                        .workspaces
                        .iter()
                        .filter(|workspace| workspace.workspace_id != source.workspace_id)
                        .filter(|workspace| {
                            workspace
                                .worktree
                                .as_ref()
                                .is_some_and(|candidate| candidate.key == worktree.key)
                        })
                        .map(|workspace| workspace.workspace_id.clone()),
                )
                .collect();
            Some(crate::api::schema::Method::WorkspaceMoveBlock(
                crate::api::schema::WorkspaceMoveBlockParams {
                    workspace_ids,
                    before_workspace_id: before_workspace_id.map(str::to_owned),
                },
            ))
        } else {
            let insert_index = before_workspace_id
                .and_then(|target| {
                    snapshot
                        .workspaces
                        .iter()
                        .position(|workspace| workspace.workspace_id == target)
                })
                .unwrap_or(snapshot.workspaces.len());
            Some(crate::api::schema::Method::WorkspaceMove(
                crate::api::schema::WorkspaceMoveParams {
                    workspace_id: source.workspace_id.clone(),
                    insert_index,
                },
            ))
        }
    }

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
        if mouse.kind == MouseEventKind::Drag(MouseButton::Left) {
            match self.chrome_drag.as_ref() {
                Some(ClientChromeDrag::SidebarWidth) => {
                    self.set_sidebar_width_from_column(mouse.column, outcome);
                    return;
                }
                Some(ClientChromeDrag::SidebarSection) => {
                    self.set_sidebar_section_from_row(mouse.row, outcome);
                    return;
                }
                Some(ClientChromeDrag::WorkspaceScrollbar { grab_row_offset }) => {
                    if let Some(metrics) = self.hits.workspace_scroll_metrics {
                        let offset = crate::ui::scrollbar_offset_from_drag_row(
                            metrics,
                            self.hits.workspace_scrollbar,
                            mouse.row,
                            *grab_row_offset,
                        );
                        let next = metrics.max_offset_from_bottom.saturating_sub(offset);
                        if next != self.workspace_scroll {
                            self.workspace_scroll = next;
                            outcome.repaint = true;
                        }
                    }
                    return;
                }
                Some(ClientChromeDrag::AgentScrollbar { grab_row_offset }) => {
                    if let Some(metrics) = self.hits.agent_scroll_metrics {
                        let offset = crate::ui::scrollbar_offset_from_drag_row(
                            metrics,
                            self.hits.agent_scrollbar,
                            mouse.row,
                            *grab_row_offset,
                        );
                        let next = metrics.max_offset_from_bottom.saturating_sub(offset);
                        if next != self.agent_scroll {
                            self.agent_scroll = next;
                            outcome.repaint = true;
                        }
                    }
                    return;
                }
                Some(ClientChromeDrag::PaneSplit {
                    hit,
                    tab_id,
                    grab_offset,
                    last_sent_at,
                    ..
                }) => {
                    let hit = hit.clone();
                    let tab_id = tab_id.clone();
                    let grab_offset = *grab_offset;
                    match self.pane_split_target_is_current(&hit, &tab_id) {
                        Some(true) => {}
                        Some(false) => {
                            self.chrome_drag = None;
                            return;
                        }
                        None => return,
                    }
                    let ratio = Self::pane_split_ratio(&hit, grab_offset, point);
                    let now = std::time::Instant::now();
                    let should_send = last_sent_at.is_none_or(|last| {
                        now.duration_since(last) >= std::time::Duration::from_millis(33)
                    });
                    if let Some(ClientChromeDrag::PaneSplit {
                        last_sent_ratio,
                        last_sent_at,
                        ..
                    }) = self.chrome_drag.as_mut()
                    {
                        if should_send {
                            *last_sent_ratio = Some(ratio);
                            *last_sent_at = Some(now);
                        }
                    }
                    if should_send {
                        self.push_endpoint_method(
                            crate::api::schema::Method::LayoutSetSplitRatio(
                                crate::api::schema::LayoutSetSplitRatioParams {
                                    tab_id: Some(tab_id),
                                    pane_id: None,
                                    path: hit.path,
                                    ratio,
                                },
                            ),
                            outcome,
                        );
                    }
                    return;
                }
                Some(ClientChromeDrag::Tab { .. }) => {
                    let insert_index = self.tab_drop_index_at(point);
                    if let Some(ClientChromeDrag::Tab {
                        insert_index: current,
                        ..
                    }) = self.chrome_drag.as_mut()
                    {
                        *current = insert_index;
                    }
                    outcome.repaint = true;
                    return;
                }
                Some(ClientChromeDrag::Workspace { .. }) => {
                    let target = self.workspace_drop_target_at(point);
                    if let Some(ClientChromeDrag::Workspace {
                        target: current, ..
                    }) = self.chrome_drag.as_mut()
                    {
                        *current = target;
                    }
                    outcome.repaint = true;
                    return;
                }
                None => {}
            }
            if let Some(press) = self.workspace_press.as_ref() {
                let delta = mouse
                    .column
                    .abs_diff(press.start_column)
                    .max(mouse.row.abs_diff(press.start_row));
                if delta >= 1 {
                    let source_workspace_id = press.workspace_id.clone();
                    let draggable = self
                        .snapshot
                        .as_deref()
                        .and_then(|snapshot| {
                            snapshot
                                .workspaces
                                .iter()
                                .find(|workspace| workspace.workspace_id == source_workspace_id)
                        })
                        .is_some_and(|workspace| {
                            !workspace
                                .worktree
                                .as_ref()
                                .is_some_and(|worktree| worktree.is_linked_worktree)
                        });
                    if draggable {
                        if let Some(target) = self.workspace_drop_target_at(point) {
                            self.chrome_drag = Some(ClientChromeDrag::Workspace {
                                source_workspace_id,
                                target: Some(target),
                            });
                            outcome.repaint = true;
                        }
                    }
                }
                return;
            }
            if let Some(press) = self.tab_press.as_ref() {
                let delta = mouse
                    .column
                    .abs_diff(press.start_column)
                    .max(mouse.row.abs_diff(press.start_row));
                if delta >= 1 {
                    if let Some(insert_index) = self.tab_drop_index_at(point) {
                        self.chrome_drag = Some(ClientChromeDrag::Tab {
                            tab_id: press.tab_id.clone(),
                            workspace_id: press.workspace_id.clone(),
                            insert_index: Some(insert_index),
                        });
                        outcome.repaint = true;
                    }
                }
                return;
            }
        }
        if mouse.kind == MouseEventKind::Up(MouseButton::Left) {
            if let Some(drag) = self.chrome_drag.take() {
                self.workspace_press = None;
                self.tab_press = None;
                match drag {
                    ClientChromeDrag::Tab {
                        tab_id,
                        workspace_id,
                        ..
                    } => {
                        let insert_index = self.tab_drop_index_at(point);
                        let valid_drop = self.snapshot.as_deref().is_some_and(|snapshot| {
                            snapshot.focused_workspace_id.as_deref() == Some(workspace_id.as_str())
                                && snapshot.tabs.iter().any(|tab| {
                                    tab.tab_id == tab_id && tab.workspace_id == workspace_id
                                })
                                && insert_index.is_some_and(|index| {
                                    index
                                        <= snapshot
                                            .tabs
                                            .iter()
                                            .filter(|tab| tab.workspace_id == workspace_id)
                                            .count()
                                })
                        });
                        if valid_drop {
                            self.push_endpoint_method(
                                crate::api::schema::Method::TabMove(
                                    crate::api::schema::TabMoveParams {
                                        tab_id,
                                        insert_index: insert_index.unwrap_or_default(),
                                    },
                                ),
                                outcome,
                            );
                        }
                        outcome.repaint = true;
                    }
                    ClientChromeDrag::Workspace {
                        source_workspace_id,
                        target,
                    } => {
                        if let Some((before_workspace_id, _)) = target {
                            if let Some(method) = self.workspace_move_method(
                                &source_workspace_id,
                                before_workspace_id.as_deref(),
                            ) {
                                self.push_endpoint_method(method, outcome);
                            }
                        }
                        outcome.repaint = true;
                    }
                    ClientChromeDrag::PaneSplit {
                        hit,
                        tab_id,
                        grab_offset,
                        last_sent_ratio,
                        ..
                    } => {
                        let target_is_current =
                            self.pane_split_target_is_current(&hit, &tab_id) == Some(true);
                        let ratio = Self::pane_split_ratio(&hit, grab_offset, point);
                        if target_is_current
                            && last_sent_ratio
                                .is_none_or(|sent| (sent - ratio).abs() > f32::EPSILON)
                        {
                            self.push_endpoint_method(
                                crate::api::schema::Method::LayoutSetSplitRatio(
                                    crate::api::schema::LayoutSetSplitRatioParams {
                                        tab_id: Some(tab_id),
                                        pane_id: None,
                                        path: hit.path,
                                        ratio,
                                    },
                                ),
                                outcome,
                            );
                        }
                    }
                    ClientChromeDrag::SidebarWidth | ClientChromeDrag::SidebarSection => {
                        self.persist_chrome_preferences(outcome);
                    }
                    ClientChromeDrag::WorkspaceScrollbar { .. }
                    | ClientChromeDrag::AgentScrollbar { .. } => {}
                }
                return;
            }
            if let Some(press) = self.workspace_press.take() {
                self.push_endpoint_method(
                    crate::api::schema::Method::WorkspaceFocus(
                        crate::api::schema::WorkspaceTarget {
                            workspace_id: press.workspace_id,
                        },
                    ),
                    outcome,
                );
                return;
            }
            if let Some(press) = self.tab_press.take() {
                self.push_endpoint_method(
                    crate::api::schema::Method::TabFocus(crate::api::schema::TabTarget {
                        tab_id: press.tab_id,
                    }),
                    outcome,
                );
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
                if !self.config.mouse_capture {
                    return;
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
                if self
                    .hits
                    .tabs
                    .iter()
                    .any(|(rect, _)| super::contains(*rect, point))
                    || super::contains(self.hits.tab_scroll_left, point)
                    || super::contains(self.hits.tab_scroll_right, point)
                    || super::contains(self.hits.new_tab, point) =>
            {
                self.record_binding(
                    crate::input::KeybindMatch::Action(crate::input::KeybindAction::PreviousTab),
                    outcome,
                );
            }
            MouseEventKind::ScrollDown
                if self
                    .hits
                    .tabs
                    .iter()
                    .any(|(rect, _)| super::contains(*rect, point))
                    || super::contains(self.hits.tab_scroll_left, point)
                    || super::contains(self.hits.tab_scroll_right, point)
                    || super::contains(self.hits.new_tab, point) =>
            {
                self.record_binding(
                    crate::input::KeybindMatch::Action(crate::input::KeybindAction::NextTab),
                    outcome,
                );
            }
            MouseEventKind::ScrollUp if super::contains(self.hits.agent_body, point) => {
                let next = self.agent_scroll.saturating_sub(1);
                if next != self.agent_scroll {
                    self.agent_scroll = next;
                    outcome.repaint = true;
                }
            }
            MouseEventKind::ScrollDown if super::contains(self.hits.agent_body, point) => {
                let next = self
                    .agent_scroll
                    .saturating_add(1)
                    .min(self.hits.agent_max_scroll);
                if next != self.agent_scroll {
                    self.agent_scroll = next;
                    outcome.repaint = true;
                }
            }
            MouseEventKind::ScrollUp if super::contains(self.hits.workspace_body, point) => {
                let next = self.workspace_scroll.saturating_sub(1);
                if next != self.workspace_scroll {
                    self.workspace_scroll = next;
                    outcome.repaint = true;
                }
            }
            MouseEventKind::ScrollDown if super::contains(self.hits.workspace_body, point) => {
                let next = self
                    .workspace_scroll
                    .saturating_add(1)
                    .min(self.hits.workspace_max_scroll);
                if next != self.workspace_scroll {
                    self.workspace_scroll = next;
                    outcome.repaint = true;
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.workspace_press = None;
                self.tab_press = None;
                self.chrome_drag = None;
                if super::contains(self.hits.sidebar_divider, point)
                    && !super::contains(self.hits.sidebar_toggle, point)
                {
                    let now = std::time::Instant::now();
                    let double_click = self.last_sidebar_divider_click.is_some_and(|last| {
                        now.duration_since(last) <= std::time::Duration::from_millis(350)
                    });
                    self.last_sidebar_divider_click = Some(now);
                    if double_click {
                        self.sidebar_width = self.config.sidebar_width;
                        self.sidebar_width_manual = false;
                        self.invalidate_pane_surface();
                        outcome.repaint = true;
                        outcome.resize = true;
                        self.persist_chrome_preferences(outcome);
                    } else {
                        self.chrome_drag = Some(ClientChromeDrag::SidebarWidth);
                        self.set_sidebar_width_from_column(mouse.column, outcome);
                    }
                    return;
                }
                if super::contains(self.hits.sidebar_section_divider, point) {
                    self.chrome_drag = Some(ClientChromeDrag::SidebarSection);
                    self.set_sidebar_section_from_row(mouse.row, outcome);
                    return;
                }
                if super::contains(self.hits.workspace_scrollbar, point) {
                    if let Some(metrics) = self.hits.workspace_scroll_metrics {
                        if let Some(grab_row_offset) = crate::ui::scrollbar_thumb_grab_offset(
                            metrics,
                            self.hits.workspace_scrollbar,
                            mouse.row,
                        ) {
                            self.chrome_drag =
                                Some(ClientChromeDrag::WorkspaceScrollbar { grab_row_offset });
                        } else {
                            let offset = crate::ui::scrollbar_offset_from_row(
                                metrics,
                                self.hits.workspace_scrollbar,
                                mouse.row,
                            );
                            let next = metrics.max_offset_from_bottom.saturating_sub(offset);
                            if next != self.workspace_scroll {
                                self.workspace_scroll = next;
                                outcome.repaint = true;
                            }
                        }
                    }
                    return;
                }
                if super::contains(self.hits.agent_scrollbar, point) {
                    if let Some(metrics) = self.hits.agent_scroll_metrics {
                        if let Some(grab_row_offset) = crate::ui::scrollbar_thumb_grab_offset(
                            metrics,
                            self.hits.agent_scrollbar,
                            mouse.row,
                        ) {
                            self.chrome_drag =
                                Some(ClientChromeDrag::AgentScrollbar { grab_row_offset });
                        } else {
                            let offset = crate::ui::scrollbar_offset_from_row(
                                metrics,
                                self.hits.agent_scrollbar,
                                mouse.row,
                            );
                            let next = metrics.max_offset_from_bottom.saturating_sub(offset);
                            if next != self.agent_scroll {
                                self.agent_scroll = next;
                                outcome.repaint = true;
                            }
                        }
                    }
                    return;
                }
                if super::contains(self.hits.agent_sort_toggle, point) {
                    let sort = match self.config.agent_panel_sort {
                        crate::config::AgentPanelSortConfig::Spaces => {
                            crate::config::AgentPanelSortConfig::Priority
                        }
                        crate::config::AgentPanelSortConfig::Priority => {
                            crate::config::AgentPanelSortConfig::Spaces
                        }
                    };
                    self.config.agent_panel_sort = sort;
                    self.agent_panel_sort_manual = true;
                    self.agent_scroll = 0;
                    self.persist_chrome_preferences(outcome);
                    outcome.repaint = true;
                    return;
                }
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
                    self.sidebar_collapsed_manual = true;
                    self.invalidate_pane_surface();
                    outcome.repaint = true;
                    outcome.resize = true;
                    self.persist_chrome_preferences(outcome);
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
                let workspace_press = self
                    .hits
                    .workspaces
                    .iter()
                    .find(|hit| super::contains(hit.rect, point))
                    .map(|hit| ClientWorkspacePress {
                        workspace_id: hit.workspace_id.clone(),
                        start_column: mouse.column,
                        start_row: mouse.row,
                    });
                if let Some(workspace_press) = workspace_press {
                    self.workspace_press = Some(workspace_press);
                    return;
                }
                let tab_press = self
                    .config
                    .mouse_capture
                    .then(|| {
                        self.hits
                            .tabs
                            .iter()
                            .find(|(rect, _)| super::contains(*rect, point))
                            .and_then(|(_, tab_id)| {
                                let tab = self
                                    .snapshot
                                    .as_deref()?
                                    .tabs
                                    .iter()
                                    .find(|tab| tab.tab_id == *tab_id)?;
                                Some(ClientTabPress {
                                    tab_id: tab.tab_id.clone(),
                                    workspace_id: tab.workspace_id.clone(),
                                    start_column: mouse.column,
                                    start_row: mouse.row,
                                })
                            })
                    })
                    .flatten();
                if let Some(tab_press) = tab_press {
                    self.tab_press = Some(tab_press);
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
                let split_hit = self
                    .hits
                    .pane_splits
                    .iter()
                    .find(|hit| super::contains(hit.hit_rect, point))
                    .cloned();
                if let Some(hit) = split_hit {
                    let Some(tab_id) = self
                        .snapshot
                        .as_deref()
                        .and_then(|snapshot| snapshot.focused_tab_id.clone())
                    else {
                        return;
                    };
                    let pointer = match hit.direction {
                        crate::protocol::PaneSurfaceSplitDirection::Horizontal => mouse.column,
                        crate::protocol::PaneSurfaceSplitDirection::Vertical => mouse.row,
                    };
                    self.chrome_drag = Some(ClientChromeDrag::PaneSplit {
                        grab_offset: i32::from(hit.pos) - i32::from(pointer),
                        last_sent_ratio: None,
                        last_sent_at: None,
                        hit,
                        tab_id,
                    });
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
