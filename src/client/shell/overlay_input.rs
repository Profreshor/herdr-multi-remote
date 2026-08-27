use super::*;

impl ClientShellState {
    pub(super) fn open_navigator_overlay(&mut self) {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        let expanded_workspaces = snapshot
            .workspaces
            .iter()
            .map(|workspace| workspace.workspace_id.clone())
            .collect();
        let mut navigator = ClientNavigatorOverlay {
            query: String::new(),
            search_focused: false,
            selected: 0,
            scroll: 0,
            filter: None,
            expanded_workspaces,
        };
        let rows = render::client_navigator_rows(snapshot, &navigator);
        navigator.selected = rows.iter().position(|row| row.current).unwrap_or(0);
        self.overlay = Some(ClientShellOverlay::Navigator(navigator));
    }

    pub(super) fn move_navigator_selection(&mut self, delta: isize) {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        let Some(ClientShellOverlay::Navigator(navigator)) = self.overlay.as_mut() else {
            return;
        };
        let rows = render::client_navigator_rows(snapshot, navigator);
        if rows.is_empty() {
            navigator.selected = 0;
            return;
        }
        navigator.selected = (navigator.selected as isize + delta)
            .clamp(0, rows.len().saturating_sub(1) as isize) as usize;
    }

    pub(super) fn accept_navigator_selection(&mut self, outcome: &mut ClientShellInput) {
        let target = {
            let Some(snapshot) = self.snapshot.as_deref() else {
                return;
            };
            let Some(ClientShellOverlay::Navigator(navigator)) = self.overlay.as_ref() else {
                return;
            };
            render::client_navigator_rows(snapshot, navigator)
                .get(navigator.selected)
                .map(|row| row.target.clone())
        };
        let Some(target) = target else {
            return;
        };
        self.overlay = None;
        let method = match target {
            ClientNavigatorTarget::Workspace(workspace_id) => {
                crate::api::schema::Method::WorkspaceFocus(crate::api::schema::WorkspaceTarget {
                    workspace_id,
                })
            }
            ClientNavigatorTarget::Tab(tab_id) => {
                crate::api::schema::Method::TabFocus(crate::api::schema::TabTarget { tab_id })
            }
            ClientNavigatorTarget::Pane(pane_id) => {
                crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget { pane_id })
            }
        };
        self.push_endpoint_method(method, outcome);
        outcome.repaint = true;
    }

    pub(super) fn toggle_selected_navigator_workspace(&mut self) {
        let workspace_id = {
            let Some(snapshot) = self.snapshot.as_deref() else {
                return;
            };
            let Some(ClientShellOverlay::Navigator(navigator)) = self.overlay.as_ref() else {
                return;
            };
            render::client_navigator_rows(snapshot, navigator)
                .get(navigator.selected)
                .and_then(|row| match &row.target {
                    ClientNavigatorTarget::Workspace(workspace_id) => Some(workspace_id.clone()),
                    _ => None,
                })
        };
        if let (Some(workspace_id), Some(ClientShellOverlay::Navigator(navigator))) =
            (workspace_id, self.overlay.as_mut())
        {
            if !navigator.expanded_workspaces.remove(&workspace_id) {
                navigator.expanded_workspaces.insert(workspace_id);
            }
            navigator.selected = 0;
            navigator.scroll = 0;
        }
    }

    pub(super) fn workspace_action_id(&self) -> Option<String> {
        self.navigate_workspace_id.clone().or_else(|| {
            self.snapshot
                .as_deref()
                .and_then(|snapshot| snapshot.focused_workspace_id.clone())
        })
    }

    pub(super) fn open_new_workspace_overlay(&mut self) {
        let cwd = self.snapshot.as_deref().and_then(|snapshot| {
            let pane_id = snapshot.focused_pane_id.as_deref()?;
            let pane = snapshot.panes.iter().find(|pane| pane.pane_id == pane_id)?;
            pane.foreground_cwd.clone().or_else(|| pane.cwd.clone())
        });
        let suggested_name = cwd
            .as_deref()
            .map(std::path::Path::new)
            .map(crate::workspace::derive_label_from_cwd)
            .unwrap_or_else(|| "workspace".to_owned());
        self.overlay = Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            title: "new workspace",
            input: suggested_name.clone(),
            replace_on_type: true,
            target: ClientRenameTarget::NewWorkspace {
                cwd,
                suggested_name,
            },
        }));
    }

    pub(super) fn open_rename_workspace_overlay(&mut self) {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        let Some(workspace_id) = self.workspace_action_id() else {
            return;
        };
        let Some(workspace) = snapshot
            .workspaces
            .iter()
            .find(|workspace| workspace.workspace_id == workspace_id)
        else {
            return;
        };
        self.overlay = Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            title: "rename workspace",
            input: workspace.label.clone(),
            replace_on_type: false,
            target: ClientRenameTarget::Workspace { workspace_id },
        }));
    }

    pub(super) fn open_new_tab_overlay(&mut self) {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        let Some(workspace_id) = snapshot.focused_workspace_id.clone() else {
            return;
        };
        let default_name = (snapshot
            .tabs
            .iter()
            .filter(|tab| tab.workspace_id == workspace_id)
            .count()
            + 1)
        .to_string();
        self.overlay = Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            title: "new tab",
            input: default_name.clone(),
            replace_on_type: true,
            target: ClientRenameTarget::NewTab {
                workspace_id,
                default_name,
            },
        }));
    }

    pub(super) fn open_rename_tab_overlay(&mut self) {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        let Some(tab_id) = snapshot.focused_tab_id.as_deref() else {
            return;
        };
        let Some(tab) = snapshot.tabs.iter().find(|tab| tab.tab_id == tab_id) else {
            return;
        };
        self.overlay = Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            title: "rename tab",
            input: tab.label.clone(),
            replace_on_type: false,
            target: ClientRenameTarget::Tab {
                tab_id: tab.tab_id.clone(),
                auto_name: !tab.custom_label,
                original_name: tab.label.clone(),
            },
        }));
    }

    pub(super) fn open_rename_pane_overlay(&mut self) {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        let Some(pane_id) = snapshot.focused_pane_id.as_deref() else {
            return;
        };
        let Some(pane) = snapshot.panes.iter().find(|pane| pane.pane_id == pane_id) else {
            return;
        };
        self.overlay = Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            title: "rename pane",
            input: pane.label.clone().unwrap_or_default(),
            replace_on_type: pane.label.is_none(),
            target: ClientRenameTarget::Pane {
                pane_id: pane.pane_id.clone(),
            },
        }));
    }

    pub(super) fn insert_overlay_text(&mut self, text: &str) -> bool {
        if self.insert_worktree_overlay_text(text) {
            return true;
        }
        match self.overlay.as_mut() {
            Some(ClientShellOverlay::Rename(rename)) => {
                if rename.replace_on_type {
                    rename.input.clear();
                    rename.replace_on_type = false;
                }
                rename.input.push_str(text);
                true
            }
            Some(ClientShellOverlay::Help(help)) if help.search_focused => {
                help.query.push_str(text);
                help.scroll = 0;
                true
            }
            Some(ClientShellOverlay::Navigator(navigator)) if navigator.search_focused => {
                navigator.query.push_str(text);
                navigator.filter = None;
                navigator.selected = 0;
                true
            }
            _ => false,
        }
    }

    pub(super) fn route_overlay_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) {
        use crossterm::event::KeyModifiers;

        if matches!(self.overlay, Some(ClientShellOverlay::ContextMenu(_))) {
            match key.code {
                KeyCode::Esc => {
                    self.overlay = None;
                    outcome.repaint = true;
                }
                KeyCode::Up => {
                    self.move_context_menu_selection(-1);
                    outcome.repaint = true;
                }
                KeyCode::Down => {
                    self.move_context_menu_selection(1);
                    outcome.repaint = true;
                }
                KeyCode::Enter => {
                    let highlighted = match self.overlay.as_ref() {
                        Some(ClientShellOverlay::ContextMenu(menu)) => menu.highlighted,
                        _ => return,
                    };
                    self.activate_context_menu_item(highlighted, outcome);
                }
                _ => {}
            }
            return;
        }

        if self.route_worktree_overlay_key(key, outcome) {
            return;
        }
        if matches!(self.overlay, Some(ClientShellOverlay::Navigator(_))) {
            let (code, modifiers) = crate::config::normalize_key_combo((key.code, key.modifiers));
            let search_focused = matches!(
                self.overlay,
                Some(ClientShellOverlay::Navigator(ClientNavigatorOverlay {
                    search_focused: true,
                    ..
                }))
            );
            if code == KeyCode::Esc {
                if search_focused {
                    if let Some(ClientShellOverlay::Navigator(navigator)) = self.overlay.as_mut() {
                        navigator.search_focused = false;
                    }
                } else {
                    self.overlay = None;
                }
                outcome.repaint = true;
                return;
            }
            if code == KeyCode::Enter {
                self.accept_navigator_selection(outcome);
                return;
            }
            if search_focused {
                if code == KeyCode::Up
                    || code == KeyCode::Char('p') && modifiers.contains(KeyModifiers::CONTROL)
                {
                    self.move_navigator_selection(-1);
                    outcome.repaint = true;
                    return;
                }
                if code == KeyCode::Down
                    || code == KeyCode::Char('n') && modifiers.contains(KeyModifiers::CONTROL)
                {
                    self.move_navigator_selection(1);
                    outcome.repaint = true;
                    return;
                }
                if let Some(ClientShellOverlay::Navigator(navigator)) = self.overlay.as_mut() {
                    if code == KeyCode::Char('u') && modifiers.contains(KeyModifiers::CONTROL) {
                        navigator.query.clear();
                        navigator.filter = None;
                        navigator.selected = 0;
                    } else if code == KeyCode::Backspace {
                        navigator.query.pop();
                        navigator.filter = None;
                        navigator.selected = 0;
                    } else if let KeyCode::Char(character) = code {
                        if modifiers.difference(KeyModifiers::SHIFT).is_empty() {
                            navigator.filter = None;
                            if let Some(text) = key.generated_text.as_deref() {
                                navigator.query.push_str(text);
                            } else {
                                navigator.query.push(character);
                            }
                            navigator.selected = 0;
                        }
                    }
                    outcome.repaint = true;
                }
                return;
            }
            if code == KeyCode::Backspace && modifiers.is_empty() {
                if let Some(ClientShellOverlay::Navigator(navigator)) = self.overlay.as_mut() {
                    if navigator.filter.take().is_some() {
                        navigator.selected = 0;
                    }
                }
                outcome.repaint = true;
                return;
            }
            if code == KeyCode::Home && modifiers.is_empty() {
                if let Some(ClientShellOverlay::Navigator(navigator)) = self.overlay.as_mut() {
                    navigator.selected = 0;
                    navigator.scroll = 0;
                }
                outcome.repaint = true;
                return;
            }
            if matches!(code, KeyCode::End | KeyCode::Char('G')) && modifiers.is_empty() {
                let last = self
                    .snapshot
                    .as_deref()
                    .zip(self.overlay.as_ref())
                    .and_then(|(snapshot, overlay)| match overlay {
                        ClientShellOverlay::Navigator(navigator) => {
                            Some(render::client_navigator_rows(snapshot, navigator).len())
                        }
                        _ => None,
                    })
                    .unwrap_or(0)
                    .saturating_sub(1);
                if let Some(ClientShellOverlay::Navigator(navigator)) = self.overlay.as_mut() {
                    navigator.selected = last;
                }
                outcome.repaint = true;
                return;
            }
            if code == KeyCode::Char('/') && modifiers.is_empty() {
                if let Some(ClientShellOverlay::Navigator(navigator)) = self.overlay.as_mut() {
                    navigator.search_focused = true;
                    navigator.filter = None;
                }
                outcome.repaint = true;
                return;
            }
            if matches!(code, KeyCode::Down | KeyCode::Char('j')) && modifiers.is_empty() {
                self.move_navigator_selection(1);
                outcome.repaint = true;
                return;
            }
            if matches!(code, KeyCode::Up | KeyCode::Char('k')) && modifiers.is_empty() {
                self.move_navigator_selection(-1);
                outcome.repaint = true;
                return;
            }
            if code == KeyCode::Char('d') && modifiers.contains(KeyModifiers::CONTROL) {
                self.move_navigator_selection(8);
                outcome.repaint = true;
                return;
            }
            if code == KeyCode::Char('u') && modifiers.contains(KeyModifiers::CONTROL) {
                self.move_navigator_selection(-8);
                outcome.repaint = true;
                return;
            }
            if let Some(filter) = match code {
                KeyCode::Char('b') if modifiers.is_empty() => Some(ClientNavigatorFilter::Blocked),
                KeyCode::Char('w') if modifiers.is_empty() => Some(ClientNavigatorFilter::Working),
                KeyCode::Char('i') if modifiers.is_empty() => Some(ClientNavigatorFilter::Idle),
                KeyCode::Char('d') if modifiers.is_empty() => Some(ClientNavigatorFilter::Done),
                _ => None,
            } {
                if let Some(ClientShellOverlay::Navigator(navigator)) = self.overlay.as_mut() {
                    navigator.query.clear();
                    navigator.filter = Some(filter);
                    navigator.selected = 0;
                }
                outcome.repaint = true;
                return;
            }
            if code == KeyCode::Char('a') && modifiers.is_empty() {
                if let Some(ClientShellOverlay::Navigator(navigator)) = self.overlay.as_mut() {
                    navigator.query.clear();
                    navigator.filter = None;
                    navigator.selected = 0;
                }
                outcome.repaint = true;
                return;
            }
            if code == KeyCode::Char(' ') && modifiers.is_empty() {
                self.toggle_selected_navigator_workspace();
                outcome.repaint = true;
                return;
            }
            return;
        }

        if let Some(ClientShellOverlay::Help(help)) = self.overlay.as_mut() {
            let (code, modifiers) = crate::config::normalize_key_combo((key.code, key.modifiers));
            if code == KeyCode::Esc {
                if help.search_focused {
                    help.search_focused = false;
                } else {
                    self.overlay = None;
                }
                outcome.repaint = true;
                return;
            }
            if !help.search_focused && code == KeyCode::Enter {
                self.overlay = None;
                outcome.repaint = true;
                return;
            }
            if !help.search_focused && modifiers.is_empty() && code == KeyCode::Char('/') {
                help.search_focused = true;
                outcome.repaint = true;
                return;
            }
            if help.search_focused {
                if code == KeyCode::Char('u') && modifiers.contains(KeyModifiers::CONTROL) {
                    help.query.clear();
                    help.scroll = 0;
                    outcome.repaint = true;
                    return;
                }
                if code == KeyCode::Backspace {
                    help.query.pop();
                    help.scroll = 0;
                    outcome.repaint = true;
                    return;
                }
                if let KeyCode::Char(character) = code {
                    if modifiers.difference(KeyModifiers::SHIFT).is_empty() {
                        if let Some(text) = key.generated_text.as_deref() {
                            help.query.push_str(text);
                        } else {
                            help.query.push(character);
                        }
                        help.scroll = 0;
                        outcome.repaint = true;
                        return;
                    }
                }
            }
            let delta = match code {
                KeyCode::Up | KeyCode::Char('k') => Some(-1isize),
                KeyCode::Down | KeyCode::Char('j') => Some(1),
                KeyCode::PageUp => Some(-10),
                KeyCode::PageDown => Some(10),
                _ => None,
            };
            if let Some(delta) = delta {
                help.scroll = help.scroll.saturating_add_signed(delta);
                outcome.repaint = true;
            }
            return;
        }

        if matches!(self.overlay, Some(ClientShellOverlay::ConfirmClose(_))) {
            if key.code == KeyCode::Enter {
                let Some(ClientShellOverlay::ConfirmClose(confirm)) = self.overlay.take() else {
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
            } else if key.code == KeyCode::Esc {
                self.overlay = None;
                self.mode = ClientShellMode::Navigate;
                self.navigate_workspace_id = self
                    .snapshot
                    .as_deref()
                    .and_then(|snapshot| snapshot.focused_workspace_id.clone());
                outcome.repaint = true;
            }
            return;
        }

        let Some(ClientShellOverlay::Rename(rename)) = self.overlay.as_mut() else {
            return;
        };
        if key.code == KeyCode::Enter {
            self.save_rename_overlay(outcome);
            return;
        }
        if key.code == KeyCode::Esc {
            self.overlay = None;
            outcome.repaint = true;
            return;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            rename.input.clear();
            rename.replace_on_type = false;
            outcome.repaint = true;
            return;
        }
        if (key.code == KeyCode::Char('u') && key.modifiers.contains(KeyModifiers::CONTROL))
            || (key.code == KeyCode::Backspace && key.modifiers.contains(KeyModifiers::SUPER))
        {
            rename.input.clear();
            rename.replace_on_type = false;
            outcome.repaint = true;
            return;
        }
        if key.code == KeyCode::Backspace
            && (key.modifiers.contains(KeyModifiers::CONTROL)
                || key.modifiers.contains(KeyModifiers::ALT))
            || matches!(key.code, KeyCode::Char('h' | 'w'))
                && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            delete_overlay_word(rename);
            outcome.repaint = true;
            return;
        }
        if key.code == KeyCode::Backspace {
            if rename.replace_on_type {
                rename.input.clear();
                rename.replace_on_type = false;
            } else {
                rename.input.pop();
            }
            outcome.repaint = true;
            return;
        }
        if let KeyCode::Char(character) = key.code {
            if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
                if rename.replace_on_type {
                    rename.input.clear();
                    rename.replace_on_type = false;
                }
                if let Some(text) = key.generated_text.as_deref() {
                    rename.input.push_str(text);
                } else {
                    rename.input.push(character);
                }
                outcome.repaint = true;
            }
        }
    }

    pub(super) fn save_rename_overlay(&mut self, outcome: &mut ClientShellInput) {
        let Some(ClientShellOverlay::Rename(rename)) = self.overlay.take() else {
            return;
        };
        let trimmed = rename.input.trim();
        let method = match rename.target {
            ClientRenameTarget::NewWorkspace {
                cwd,
                suggested_name,
            } => Some(crate::api::schema::Method::WorkspaceCreate(
                crate::api::schema::WorkspaceCreateParams {
                    cwd,
                    focus: true,
                    label: (!trimmed.is_empty() && trimmed != suggested_name)
                        .then(|| trimmed.to_owned()),
                    env: Default::default(),
                },
            )),
            ClientRenameTarget::Workspace { workspace_id } => (!trimmed.is_empty()).then(|| {
                crate::api::schema::Method::WorkspaceRename(
                    crate::api::schema::WorkspaceRenameParams {
                        workspace_id,
                        label: trimmed.to_owned(),
                    },
                )
            }),
            ClientRenameTarget::NewTab {
                workspace_id,
                default_name,
            } => Some(crate::api::schema::Method::TabCreate(
                crate::api::schema::TabCreateParams {
                    workspace_id: Some(workspace_id),
                    cwd: None,
                    focus: true,
                    label: (!trimmed.is_empty() && trimmed != default_name)
                        .then(|| trimmed.to_owned()),
                    env: Default::default(),
                },
            )),
            ClientRenameTarget::Tab {
                tab_id,
                auto_name,
                original_name,
            } => (!(trimmed.is_empty() || auto_name && trimmed == original_name)).then(|| {
                crate::api::schema::Method::TabRename(crate::api::schema::TabRenameParams {
                    tab_id,
                    label: trimmed.to_owned(),
                })
            }),
            ClientRenameTarget::Pane { pane_id } => Some(crate::api::schema::Method::PaneRename(
                crate::api::schema::PaneRenameParams {
                    pane_id,
                    label: Some(trimmed.to_owned()),
                },
            )),
        };
        if let Some(method) = method {
            self.push_endpoint_method(method, outcome);
        }
        outcome.repaint = true;
    }

    pub(super) fn open_confirm_close_overlay(&mut self, workspace_id: String) {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        let Some(workspace) = snapshot
            .workspaces
            .iter()
            .find(|workspace| workspace.workspace_id == workspace_id)
        else {
            return;
        };
        let group_key = workspace
            .worktree
            .as_ref()
            .filter(|worktree| !worktree.is_linked_worktree)
            .map(|worktree| worktree.key.as_str());
        let group = group_key
            .map(|key| {
                snapshot
                    .workspaces
                    .iter()
                    .filter(|member| {
                        member
                            .worktree
                            .as_ref()
                            .is_some_and(|worktree| worktree.key == key)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| vec![workspace]);
        let closes_group = group.len() > 1;
        let pane_count = group
            .iter()
            .map(|member| {
                snapshot
                    .panes
                    .iter()
                    .filter(|pane| pane.workspace_id == member.workspace_id)
                    .count()
            })
            .sum::<usize>();
        let panes = if pane_count == 1 {
            "1 pane".to_owned()
        } else {
            format!("{pane_count} panes")
        };
        let scope = if closes_group {
            format!("{} workspaces, {panes}", group.len())
        } else {
            panes
        };
        self.overlay = Some(ClientShellOverlay::ConfirmClose(
            ClientConfirmCloseOverlay {
                workspace_id,
                title: if closes_group {
                    "Close worktree group?".to_owned()
                } else {
                    "Close workspace?".to_owned()
                },
                detail: format!("{} — {scope}", workspace.label),
            },
        ));
    }
}
