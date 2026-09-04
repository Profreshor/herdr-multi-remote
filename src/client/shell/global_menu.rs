use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ClientGlobalMenuAction {
    Binding(crate::input::KeybindAction),
    WhatsNew,
    ActivateServer(String),
}

pub(super) fn global_menu_attention(snapshot: &ClientShellSnapshot) -> bool {
    snapshot.update_available.is_some() || snapshot.integration_updates_available
}

pub(super) fn global_menu_item_has_badge(
    snapshot: &ClientShellSnapshot,
    action: &ClientGlobalMenuAction,
) -> bool {
    (action == &ClientGlobalMenuAction::WhatsNew && snapshot.update_available.is_some())
        || (action == &ClientGlobalMenuAction::Binding(crate::input::KeybindAction::Settings)
            && snapshot.integration_updates_available)
}

pub(super) fn global_menu_items(
    snapshot: &ClientShellSnapshot,
    server_ids: &[String],
    server_lifecycle: &BTreeMap<String, ClientServerLifecycle>,
    active_server_id: &str,
) -> Vec<(String, ClientGlobalMenuAction)> {
    let mut items = vec![
        (
            "settings".into(),
            ClientGlobalMenuAction::Binding(crate::input::KeybindAction::Settings),
        ),
        (
            "keybinds".into(),
            ClientGlobalMenuAction::Binding(crate::input::KeybindAction::Help),
        ),
        (
            "reload config".into(),
            ClientGlobalMenuAction::Binding(crate::input::KeybindAction::ReloadConfig),
        ),
    ];
    if snapshot.update_available.is_some() || snapshot.latest_release_notes_available {
        items.push((
            if snapshot.update_available.is_some() {
                "update ready"
            } else {
                "what's new"
            }
            .into(),
            ClientGlobalMenuAction::WhatsNew,
        ));
    }
    if server_ids.len() > 1 {
        items.extend(server_ids.iter().map(|server_id| {
            let suffix = match server_lifecycle.get(server_id) {
                Some(ClientServerLifecycle::Connected) if server_id == active_server_id => {
                    " ✓".to_owned()
                }
                Some(ClientServerLifecycle::Connected) => String::new(),
                Some(ClientServerLifecycle::Connecting) => " (connecting)".to_owned(),
                Some(ClientServerLifecycle::Reconnecting(error)) => {
                    format!(" (reconnecting: {})", error.lines().next().unwrap_or(error))
                }
                Some(ClientServerLifecycle::Failed(error)) => {
                    format!(" (failed: {})", error.lines().next().unwrap_or(error))
                }
                Some(ClientServerLifecycle::VersionMismatch(error)) => format!(
                    " (version mismatch: {})",
                    error.lines().next().unwrap_or(error)
                ),
                None => " (unavailable)".to_owned(),
            };
            (
                format!("server: {}{suffix}", server_id.to_ascii_uppercase()),
                ClientGlobalMenuAction::ActivateServer(server_id.clone()),
            )
        }));
    }
    items.push((
        "detach".into(),
        ClientGlobalMenuAction::Binding(crate::input::KeybindAction::Detach),
    ));
    items
}

impl ClientShellState {
    pub(super) fn toggle_global_menu(&mut self) {
        if matches!(self.overlay, Some(ClientShellOverlay::GlobalMenu(_))) {
            self.overlay = None;
        } else {
            self.overlay = Some(ClientShellOverlay::GlobalMenu(ClientGlobalMenuOverlay {
                highlighted: 0,
            }));
        }
    }

    pub(super) fn move_global_menu_selection(&mut self, delta: isize) {
        let item_count = self
            .snapshot
            .as_deref()
            .map(|snapshot| {
                global_menu_items(
                    snapshot,
                    &self.server_ids,
                    &self.server_lifecycle,
                    &self.active_server_id,
                )
            })
            .map_or(0, |items| items.len());
        let Some(ClientShellOverlay::GlobalMenu(menu)) = self.overlay.as_mut() else {
            return;
        };
        menu.highlighted = (menu.highlighted as isize + delta)
            .clamp(0, item_count.saturating_sub(1) as isize) as usize;
    }

    pub(super) fn activate_global_menu_item(
        &mut self,
        index: usize,
        outcome: &mut ClientShellInput,
    ) {
        let Some(action) = self.snapshot.as_deref().and_then(|snapshot| {
            global_menu_items(
                snapshot,
                &self.server_ids,
                &self.server_lifecycle,
                &self.active_server_id,
            )
            .get(index)
            .map(|(_, action)| action.clone())
        }) else {
            return;
        };
        if action == ClientGlobalMenuAction::WhatsNew
            && self
                .snapshot
                .as_deref()
                .and_then(|snapshot| snapshot.release_notes.as_ref())
                .is_none()
        {
            return;
        }
        self.overlay = None;
        match action {
            ClientGlobalMenuAction::Binding(binding) => {
                self.record_binding(crate::input::KeybindMatch::Action(binding), outcome)
            }
            ClientGlobalMenuAction::WhatsNew => self.open_release_notes(),
            ClientGlobalMenuAction::ActivateServer(server_id) => {
                outcome
                    .actions
                    .push(ClientShellAction::ActivateServer(server_id));
            }
        }
        outcome.repaint = true;
    }
}
