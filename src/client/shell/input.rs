use super::*;
use crate::protocol::ClientPaneInputEvent;
use crate::raw_input::RawInputEvent;
use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};

impl ClientShellState {
    #[cfg(any(unix, test))]
    pub(crate) fn handle_input_bytes(&mut self, data: &[u8]) -> ClientShellInput {
        self.handle_raw_events(crate::raw_input::parse_raw_input_bytes_sync(data))
    }

    #[cfg(any(unix, test))]
    pub(crate) fn handle_pixel_mouse(
        &mut self,
        data: &[u8],
        geometry: crate::input::mouse::HostGeometry,
    ) -> ClientShellInput {
        let Some((x, y)) = crate::input::mouse::parse_report(data) else {
            return ClientShellInput::default();
        };
        let Some((column, row)) = geometry.cell(x, y) else {
            return ClientShellInput::default();
        };
        let Some(cell_report) = crate::input::mouse::report_at_cell(data, column, row) else {
            return ClientShellInput::default();
        };
        let events = crate::raw_input::parse_raw_input_bytes_sync(&cell_report);
        if events.len() != 1 || !matches!(events[0], RawInputEvent::Mouse(_)) {
            return ClientShellInput::default();
        }
        self.host_mouse_pixels = Some(crate::input::mouse::HostPixels { x, y, geometry });
        let outcome = self.handle_raw_events(events);
        self.host_mouse_pixels = None;
        outcome
    }

    #[cfg(windows)]
    pub(crate) fn handle_client_events(
        &mut self,
        events: &[crate::protocol::ClientInputEvent],
    ) -> ClientShellInput {
        self.handle_raw_events(
            events
                .iter()
                .map(crate::protocol::ClientInputEvent::to_raw_input_event)
                .collect(),
        )
    }

    pub(super) fn handle_raw_events(&mut self, events: Vec<RawInputEvent>) -> ClientShellInput {
        let mut outcome = ClientShellInput::default();
        if !events.is_empty() && self.endpoint_error.take().is_some() {
            outcome.repaint = true;
        }
        for event in events {
            match event {
                RawInputEvent::Key(key) => self.handle_key(key, &mut outcome),
                RawInputEvent::Text(text) => {
                    let text = text.into_string();
                    if self.insert_overlay_text(&text) {
                        outcome.repaint = true;
                    } else if self.overlay.is_none() && self.mode == ClientShellMode::Terminal {
                        self.push_focused_pane_event(
                            ClientPaneInputEvent::TextCommit(text),
                            &mut outcome,
                        );
                    }
                }
                RawInputEvent::Paste(text) => {
                    if self.insert_overlay_text(&text) {
                        outcome.repaint = true;
                    } else if self.overlay.is_none() && self.mode == ClientShellMode::Terminal {
                        self.push_focused_pane_event(
                            ClientPaneInputEvent::Paste(text),
                            &mut outcome,
                        );
                    }
                }
                RawInputEvent::Mouse(mouse) => self.handle_mouse(mouse, &mut outcome),
                RawInputEvent::OuterFocusGained
                | RawInputEvent::OuterFocusLost
                | RawInputEvent::HostDefaultColor { .. }
                | RawInputEvent::HostPaletteColors { .. }
                | RawInputEvent::HostColorSchemeChanged(_)
                | RawInputEvent::HostCellSizeReport { .. }
                | RawInputEvent::Unsupported => {}
            }
        }
        outcome
    }

    fn handle_key(&mut self, key: crate::input::TerminalKey, outcome: &mut ClientShellInput) {
        const LOCAL_INPUT_SOURCE: u8 = 0;
        let lease_key = crate::input::InputLeaseKey::new(LOCAL_INPUT_SOURCE, &key);
        let key = self.input_leases.normalize_press(&lease_key, key);
        match key.kind {
            KeyEventKind::Press => {
                let initial_context = self.input_context();
                let target = self.route_key_press(&key, outcome);
                if let Some(target) = target.as_ref() {
                    self.push_pane_key(target.clone(), key.clone(), outcome);
                }
                let resulting_context = self.input_context();
                let plan = self.input_leases.complete_press(
                    lease_key,
                    &key,
                    Some(&initial_context),
                    Some(&resulting_context),
                    target,
                );
                self.execute_repeat_plan(lease_key, key, plan, outcome);
            }
            KeyEventKind::Repeat => {
                let context = self.input_context();
                let plan = self
                    .input_leases
                    .plan_repeat(lease_key, &key, Some(&context));
                self.execute_repeat_plan(lease_key, key, plan, outcome);
            }
            KeyEventKind::Release => {
                if let Some(lease) = self.input_leases.remove_forwarded(&lease_key) {
                    self.push_pane_key(lease.target, key, outcome);
                } else {
                    let _ = self.input_leases.remove(&lease_key);
                }
            }
        }
    }

    fn execute_repeat_plan(
        &mut self,
        lease_key: crate::input::InputLeaseKey<u8>,
        key: crate::input::TerminalKey,
        plan: crate::input::RepeatPlan<ClientInputContext, String>,
        outcome: &mut ClientShellInput,
    ) {
        match plan {
            crate::input::RepeatPlan::Forwarded(target) => {
                self.push_pane_key(target, key, outcome);
            }
            crate::input::RepeatPlan::Reprocess {
                context,
                repetitions,
                tracked,
            } => {
                for _ in 0..repetitions {
                    let current = self.input_context();
                    if !self.input_leases.reprocess_allowed(
                        lease_key,
                        &context,
                        Some(&current),
                        tracked,
                    ) {
                        break;
                    }
                    let repeated = key
                        .clone()
                        .with_repeat_count(1)
                        .with_kind(KeyEventKind::Repeat);
                    if let Some(target) = self.route_key_press(&repeated, outcome) {
                        self.push_pane_key(target, repeated, outcome);
                    }
                }
            }
            crate::input::RepeatPlan::Ignore => {}
        }
    }

    fn route_key_press(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) -> Option<String> {
        if self.overlay.is_some() {
            self.route_overlay_key(key, outcome);
            return None;
        }
        if matches!(key.code, KeyCode::Modifier(_)) {
            return None;
        }

        match self.mode {
            ClientShellMode::Terminal => {
                if let Some(binding) =
                    crate::input::resolve_direct_binding(&self.config.keybinds.keybinds, key)
                {
                    self.record_binding(binding, outcome);
                    return None;
                }
                if crate::config::terminal_key_matches_combo(key, self.config.keybinds.prefix) {
                    self.mode = ClientShellMode::Prefix;
                    outcome.repaint = true;
                    return None;
                }
                self.focused_pane_id()
            }
            ClientShellMode::Prefix => {
                if crate::config::terminal_key_matches_combo(key, self.config.keybinds.prefix) {
                    self.mode = ClientShellMode::Terminal;
                    outcome.repaint = true;
                    return self.focused_pane_id();
                }
                if key.code == KeyCode::Esc {
                    self.mode = ClientShellMode::Terminal;
                    outcome.repaint = true;
                    return None;
                }
                if let Some(binding) =
                    crate::input::resolve_prefix_binding(&self.config.keybinds.keybinds, key)
                {
                    self.mode = ClientShellMode::Terminal;
                    outcome.repaint = true;
                    self.record_binding(binding, outcome);
                    return None;
                }
                self.mode = ClientShellMode::Terminal;
                outcome.repaint = true;
                None
            }
            ClientShellMode::Navigate => {
                self.route_navigate_key(key, outcome);
                None
            }
            ClientShellMode::Resize => {
                self.route_resize_key(key, outcome);
                None
            }
        }
    }

    fn route_navigate_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) {
        use crate::input::{KeybindAction, KeybindDispatch, KeybindMatch};

        if key.code == KeyCode::Esc
            || crate::config::terminal_key_matches_combo(key, self.config.keybinds.prefix)
        {
            self.mode = ClientShellMode::Terminal;
            self.navigate_workspace_id = None;
            outcome.repaint = true;
            return;
        }

        if self
            .config
            .keybinds
            .keybinds
            .navigate
            .workspace_up
            .matches_direct_key(key)
        {
            self.move_navigate_workspace(-1);
            outcome.repaint = true;
            return;
        }
        if self
            .config
            .keybinds
            .keybinds
            .navigate
            .workspace_down
            .matches_direct_key(key)
        {
            self.move_navigate_workspace(1);
            outcome.repaint = true;
            return;
        }

        if let Some(index) = ('1'..='9').position(|digit| {
            crate::config::terminal_key_matches_combo(
                key,
                (KeyCode::Char(digit), KeyModifiers::empty()),
            )
        }) {
            self.mode = ClientShellMode::Terminal;
            self.navigate_workspace_id = None;
            self.record_binding(
                KeybindMatch::Action(KeybindAction::SwitchWorkspace(index)),
                outcome,
            );
            outcome.repaint = true;
            return;
        }

        let (code, modifiers) = crate::config::normalize_key_combo((key.code, key.modifiers));
        if modifiers.is_empty() {
            match code {
                KeyCode::Enter => {
                    let selected = self.navigate_workspace_id.clone();
                    self.mode = ClientShellMode::Terminal;
                    self.navigate_workspace_id = None;
                    if let Some(workspace_id) = selected {
                        self.push_endpoint_method(
                            crate::api::schema::Method::WorkspaceFocus(
                                crate::api::schema::WorkspaceTarget { workspace_id },
                            ),
                            outcome,
                        );
                    }
                    outcome.repaint = true;
                    return;
                }
                KeyCode::Tab => {
                    self.record_navigate_binding(
                        KeybindMatch::Action(KeybindAction::CyclePaneNext),
                        false,
                        outcome,
                    );
                    return;
                }
                KeyCode::BackTab => {
                    self.record_navigate_binding(
                        KeybindMatch::Action(KeybindAction::CyclePanePrevious),
                        false,
                        outcome,
                    );
                    return;
                }
                KeyCode::Left => {
                    self.record_navigate_binding(
                        KeybindMatch::Action(KeybindAction::FocusPaneLeft),
                        true,
                        outcome,
                    );
                    return;
                }
                KeyCode::Right => {
                    self.record_navigate_binding(
                        KeybindMatch::Action(KeybindAction::FocusPaneRight),
                        true,
                        outcome,
                    );
                    return;
                }
                _ => {}
            }
        }

        let pane_action = [
            (
                &self.config.keybinds.keybinds.navigate.pane_left,
                KeybindAction::FocusPaneLeft,
            ),
            (
                &self.config.keybinds.keybinds.navigate.pane_down,
                KeybindAction::FocusPaneDown,
            ),
            (
                &self.config.keybinds.keybinds.navigate.pane_up,
                KeybindAction::FocusPaneUp,
            ),
            (
                &self.config.keybinds.keybinds.navigate.pane_right,
                KeybindAction::FocusPaneRight,
            ),
        ]
        .into_iter()
        .find_map(|(bindings, action)| bindings.matches_direct_key(key).then_some(action));
        if let Some(action) = pane_action {
            self.record_navigate_binding(KeybindMatch::Action(action), true, outcome);
            return;
        }

        let binding = crate::input::resolve_non_indexed_action(
            &self.config.keybinds.keybinds,
            key,
            KeybindDispatch::Prefix,
        )
        .filter(|action| {
            !matches!(
                action,
                KeybindAction::FocusPaneLeft
                    | KeybindAction::FocusPaneDown
                    | KeybindAction::FocusPaneUp
                    | KeybindAction::FocusPaneRight
            )
        })
        .map(KeybindMatch::Action)
        .or_else(|| {
            crate::input::resolve_custom_command(
                &self.config.keybinds.keybinds,
                key,
                KeybindDispatch::Prefix,
            )
            .map(KeybindMatch::Command)
        })
        .or_else(|| {
            crate::input::resolve_indexed_action(
                &self.config.keybinds.keybinds,
                key,
                KeybindDispatch::Prefix,
            )
            .map(KeybindMatch::Action)
        });
        if let Some(binding) = binding {
            self.record_navigate_binding(binding, false, outcome);
        }
    }

    fn record_navigate_binding(
        &mut self,
        binding: crate::input::KeybindMatch,
        preserve_navigate: bool,
        outcome: &mut ClientShellInput,
    ) {
        use crate::input::{KeybindAction, KeybindMatch};

        if let KeybindMatch::Action(KeybindAction::CyclePaneNext) = binding {
            self.cycle_pane(false, outcome);
        } else if let KeybindMatch::Action(KeybindAction::CyclePanePrevious) = binding {
            self.cycle_pane(true, outcome);
        } else {
            if !preserve_navigate {
                self.mode = ClientShellMode::Terminal;
                self.navigate_workspace_id = None;
            }
            self.record_binding(binding, outcome);
        }
        if !preserve_navigate {
            if self.mode == ClientShellMode::Navigate {
                self.mode = ClientShellMode::Terminal;
            }
            self.navigate_workspace_id = None;
        }
        outcome.repaint = true;
    }

    fn move_navigate_workspace(&mut self, delta: isize) {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        let entries = render::workspace_entries(snapshot, &self.collapsed_groups);
        if entries.is_empty() {
            return;
        }
        let current = self
            .navigate_workspace_id
            .as_deref()
            .and_then(|selected| {
                entries
                    .iter()
                    .position(|entry| snapshot.workspaces[entry.index].workspace_id == selected)
            })
            .unwrap_or(0);
        let next = (current as isize + delta).rem_euclid(entries.len() as isize) as usize;
        self.navigate_workspace_id = Some(
            snapshot.workspaces[entries[next].index]
                .workspace_id
                .clone(),
        );
    }

    fn cycle_pane(&mut self, reverse: bool, outcome: &mut ClientShellInput) {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        let Some(surface) = self.pane_surface.as_ref() else {
            return;
        };
        if surface.panes.is_empty() {
            return;
        }
        let current = snapshot
            .focused_pane_id
            .as_deref()
            .and_then(|focused| {
                surface
                    .panes
                    .iter()
                    .position(|pane| pane.pane_id == focused)
            })
            .unwrap_or(0);
        let next = if reverse {
            (current + surface.panes.len() - 1) % surface.panes.len()
        } else {
            (current + 1) % surface.panes.len()
        };
        let pane_id = surface.panes[next].pane_id.clone();
        self.push_endpoint_method(
            crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget { pane_id }),
            outcome,
        );
    }

    fn route_resize_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) {
        let resize_bindings = &self.config.keybinds.keybinds.resize_mode;
        if key.code == KeyCode::Esc
            || key.code == KeyCode::Enter
            || resize_bindings.matches_prefix_key(key)
            || resize_bindings.matches_direct_key(key)
        {
            self.mode = ClientShellMode::Terminal;
            outcome.repaint = true;
            return;
        }

        let (code, modifiers) = crate::config::normalize_key_combo((key.code, key.modifiers));
        if !modifiers.is_empty() {
            return;
        }
        let action = match code {
            KeyCode::Char('h') | KeyCode::Left => Some(crate::input::KeybindAction::ResizePaneLeft),
            KeyCode::Char('j') | KeyCode::Down => Some(crate::input::KeybindAction::ResizePaneDown),
            KeyCode::Char('k') | KeyCode::Up => Some(crate::input::KeybindAction::ResizePaneUp),
            KeyCode::Char('l') | KeyCode::Right => {
                Some(crate::input::KeybindAction::ResizePaneRight)
            }
            _ => None,
        };
        if let Some(action) = action {
            self.record_binding(crate::input::KeybindMatch::Action(action), outcome);
        }
    }

    fn input_context(&self) -> ClientInputContext {
        ClientInputContext {
            mode: self.mode,
            overlay: self.overlay.as_ref().map(ClientShellOverlay::kind),
        }
    }

    pub(super) fn focused_pane_id(&self) -> Option<String> {
        self.snapshot
            .as_deref()
            .and_then(|snapshot| snapshot.focused_pane_id.clone())
    }

    fn push_pane_key(
        &self,
        pane_id: String,
        key: crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) {
        if let Some(event) = ClientPaneInputEvent::from_terminal_key(key) {
            super::push_pane_event(pane_id, event, outcome);
        }
    }

    fn push_focused_pane_event(&self, event: ClientPaneInputEvent, outcome: &mut ClientShellInput) {
        if let Some(pane_id) = self.focused_pane_id() {
            super::push_pane_event(pane_id, event, outcome);
        }
    }
}
