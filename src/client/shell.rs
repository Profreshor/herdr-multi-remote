use std::collections::{HashMap, HashSet};

mod actions;
mod composition;
mod config;
mod input;
mod mouse;
mod overlay_input;
mod render;
mod state;
mod worktrees;

pub(super) use state::*;

use crossterm::event::KeyCode;
#[cfg(test)]
use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use unicode_width::UnicodeWidthStr;

use crate::app::state::Palette;
use crate::config::{
    Config, LiveKeybindConfig, SidebarCollapsedModeConfig, SpaceSidebarToken, SpacesSidebarConfig,
    TabBarPositionConfig,
};
use crate::protocol::{
    ClientMessage, ClientPaneInputEvent, ClientShellSnapshot, ClientShellWorkspace,
    ClientSurfaceSize, FrameData, PaneSurfaceFrame,
};
#[cfg(test)]
use crate::raw_input::RawInputEvent;

fn delete_overlay_word(rename: &mut ClientRenameOverlay) {
    if rename.replace_on_type {
        rename.input.clear();
        rename.replace_on_type = false;
        return;
    }
    while rename.input.chars().last().is_some_and(char::is_whitespace) {
        rename.input.pop();
    }
    let Some(word) = rename
        .input
        .chars()
        .last()
        .map(|character| character.is_alphanumeric() || character == '_')
    else {
        return;
    };
    while rename.input.chars().last().is_some_and(|character| {
        !character.is_whitespace() && (character.is_alphanumeric() || character == '_') == word
    }) {
        rename.input.pop();
    }
}

fn push_pane_event(pane_id: String, event: ClientPaneInputEvent, outcome: &mut ClientShellInput) {
    if let Some(ClientMessage::ClientShellPaneInput {
        pane_id: pending_pane,
        events,
    }) = outcome.requests.last_mut()
    {
        if *pending_pane == pane_id {
            events.push(event);
            return;
        }
    }
    outcome.requests.push(ClientMessage::ClientShellPaneInput {
        pane_id,
        events: vec![event],
    });
}

fn contains(rect: Rect, point: (u16, u16)) -> bool {
    rect.width > 0
        && rect.height > 0
        && point.0 >= rect.x
        && point.0 < rect.right()
        && point.1 >= rect.y
        && point.1 < rect.bottom()
}

fn status_dot(status: crate::api::schema::AgentStatus) -> &'static str {
    use crate::api::schema::AgentStatus;
    match status {
        AgentStatus::Working | AgentStatus::Blocked | AgentStatus::Done => "●",
        AgentStatus::Idle => "○",
        AgentStatus::Unknown => "·",
    }
}

fn status_text(status: crate::api::schema::AgentStatus) -> &'static str {
    use crate::api::schema::AgentStatus;
    match status {
        AgentStatus::Working => "working",
        AgentStatus::Blocked => "blocked",
        AgentStatus::Done => "done",
        AgentStatus::Idle => "idle",
        AgentStatus::Unknown => "unknown",
    }
}

fn status_color(
    status: crate::api::schema::AgentStatus,
    palette: &Palette,
) -> ratatui::style::Color {
    use crate::api::schema::AgentStatus;
    match status {
        AgentStatus::Working => palette.yellow,
        AgentStatus::Blocked => palette.red,
        AgentStatus::Done => palette.blue,
        AgentStatus::Idle => palette.green,
        AgentStatus::Unknown => palette.overlay0,
    }
}

fn panel_contrast_fg(palette: &Palette) -> ratatui::style::Color {
    match palette.panel_bg {
        ratatui::style::Color::Reset => palette.surface_dim,
        color => color,
    }
}

fn blit_pane_surface(target: &mut FrameData, source: &FrameData, area: Rect) {
    let copy_width = source.width.min(area.width);
    let copy_height = source.height.min(area.height);
    let hyperlink_base = target.hyperlinks.len() as u32;
    target.hyperlinks.extend(source.hyperlinks.iter().cloned());

    for row in 0..copy_height {
        for col in 0..copy_width {
            let source_index = row as usize * source.width as usize + col as usize;
            let target_x = area.x + col;
            let target_y = area.y + row;
            let target_index = target_y as usize * target.width as usize + target_x as usize;
            let (Some(source_cell), Some(target_cell)) = (
                source.cells.get(source_index),
                target.cells.get_mut(target_index),
            ) else {
                continue;
            };
            *target_cell = source_cell.clone();
            target_cell.hyperlink = source_cell.hyperlink.and_then(|index| {
                ((index as usize) < source.hyperlinks.len()).then_some(hyperlink_base + index)
            });
        }
    }

    target.cursor = source.cursor.as_ref().and_then(|cursor| {
        (cursor.x < copy_width && cursor.y < copy_height).then(|| crate::protocol::CursorState {
            x: area.x + cursor.x,
            y: area.y + cursor.y,
            visible: cursor.visible,
            shape: cursor.shape,
        })
    });
    target.graphics.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::AgentStatus;
    use crate::protocol::{
        ClientShellAgent, ClientShellPane, ClientShellTab, ClientShellWorktree, PaneSurfacePane,
        SurfaceRect,
    };

    fn snapshot() -> ClientShellSnapshot {
        ClientShellSnapshot {
            boot_id: "boot-1".into(),
            revision: 1,
            focused_workspace_id: Some("ws_1".into()),
            focused_tab_id: Some("tab_1".into()),
            focused_pane_id: Some("pane_1".into()),
            workspaces: vec![ClientShellWorkspace {
                workspace_id: "ws_1".into(),
                number: 1,
                label: "client-shell".into(),
                custom_label: false,
                branch: Some("main".into()),
                git_ahead_behind: None,
                tokens: Vec::new(),
                worktree: None,
                focused: true,
                agent_status: AgentStatus::Idle,
            }],
            tabs: vec![ClientShellTab {
                tab_id: "tab_1".into(),
                workspace_id: "ws_1".into(),
                number: 1,
                label: "1".into(),
                custom_label: false,
                zoomed: false,
                focused: true,
                agent_status: AgentStatus::Idle,
            }],
            panes: vec![ClientShellPane {
                pane_id: "pane_1".into(),
                workspace_id: "ws_1".into(),
                tab_id: "tab_1".into(),
                label: None,
                cwd: Some("/repo".into()),
                foreground_cwd: Some("/repo".into()),
                focused: true,
            }],
            agents: Vec::new(),
        }
    }

    fn worktree_list_result(open_workspace_id: Option<&str>) -> crate::api::schema::ResponseResult {
        crate::api::schema::ResponseResult::WorktreeList {
            source: crate::api::schema::WorktreeSourceInfo {
                repo_key: "repo-key".into(),
                repo_name: "repo".into(),
                repo_root: "/repo".into(),
                source_checkout_path: "/repo".into(),
                source_workspace_id: Some("ws_1".into()),
            },
            worktrees: vec![crate::api::schema::WorktreeInfo {
                path: "/repo-feature".into(),
                branch: Some("feature".into()),
                is_bare: false,
                is_detached: false,
                is_prunable: false,
                is_linked_worktree: true,
                open_workspace_id: open_workspace_id.map(str::to_owned),
                label: "repo".into(),
            }],
        }
    }

    fn surface() -> PaneSurfaceFrame {
        let surface_buffer = Buffer::with_lines(["LIVE", "PANE"]);
        PaneSurfaceFrame {
            projection_revision: 1,
            frame: FrameData::from_ratatui_buffer_with_hyperlinks(
                &surface_buffer,
                Some(crate::protocol::CursorState {
                    x: 1,
                    y: 1,
                    visible: true,
                    shape: 2,
                }),
                &[],
            ),
            panes: vec![PaneSurfacePane {
                pane_id: "pane_1".into(),
                rect: SurfaceRect {
                    x: 0,
                    y: 0,
                    width: 4,
                    height: 2,
                },
                inner_rect: SurfaceRect {
                    x: 0,
                    y: 0,
                    width: 4,
                    height: 2,
                },
                scrollbar_rect: None,
                focused: true,
            }],
        }
    }

    #[test]
    fn desktop_composition_keeps_shell_outside_origin_relative_surface() {
        let config = ClientShellConfig::from_config(&Config::default());
        let mut state = ClientShellState::new(config);
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());

        let frame = state.compose(106, 20).expect("composed frame");
        let text = frame
            .cells
            .chunks(frame.width as usize)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("spaces"));
        assert!(text.contains("client-shell"));
        assert!(text.contains("main"));
        assert!(text.contains("LIVE"));
        assert!(!text.contains("1 1"));
        assert_eq!(
            frame.cursor.as_ref().map(|cursor| (cursor.x, cursor.y)),
            Some((27, 2))
        );
    }

    #[test]
    fn shell_refuses_to_compose_mismatched_projection_and_surface() {
        let config = ClientShellConfig::from_config(&Config::default());
        let mut state = ClientShellState::new(config);
        let mut snapshot = snapshot();
        snapshot.revision = 2;
        state.set_snapshot(Box::new(snapshot));
        state.set_pane_surface(surface());
        assert!(state.compose(106, 20).is_none());
    }

    #[test]
    fn mouse_hits_use_stable_workspace_tab_and_pane_ids() {
        let config = ClientShellConfig::from_config(&Config::default());
        let mut state = ClientShellState::new(config);
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());
        state.compose(106, 20).expect("composed frame");

        let workspace =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 2,
                row: 2,
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(workspace.requests.is_empty());
        let [ClientShellAction::Endpoint { request, .. }] = &workspace.actions[..] else {
            panic!("workspace click should use the endpoint API");
        };
        assert!(matches!(
            &request.method,
            crate::api::schema::Method::WorkspaceFocus(target)
                if target.workspace_id == "ws_1"
        ));

        let pane =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 27,
                row: 1,
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(pane.requests.is_empty());
        let [ClientShellAction::Endpoint { request, .. }] = &pane.actions[..] else {
            panic!("pane click should use the endpoint API");
        };
        assert!(matches!(
            &request.method,
            crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_1"
        ));
    }

    #[test]
    fn grouped_worktrees_render_parent_branch_and_indented_child() {
        let config = ClientShellConfig::from_config(&Config::default());
        let mut state = ClientShellState::new(config);
        let mut snapshot = snapshot();
        snapshot.workspaces[0].worktree = Some(ClientShellWorktree {
            key: "repo".into(),
            label: "repo".into(),
            is_linked_worktree: false,
        });
        snapshot.workspaces.push(ClientShellWorkspace {
            workspace_id: "ws_2".into(),
            number: 2,
            label: "repo-feature".into(),
            custom_label: false,
            branch: Some("worktree/feature".into()),
            git_ahead_behind: None,
            tokens: Vec::new(),
            worktree: Some(ClientShellWorktree {
                key: "repo".into(),
                label: "repo".into(),
                is_linked_worktree: true,
            }),
            focused: false,
            agent_status: AgentStatus::Idle,
        });
        state.set_snapshot(Box::new(snapshot));
        state.set_pane_surface(surface());
        let frame = state.compose(106, 20).expect("composed frame");
        let text = frame
            .cells
            .chunks(frame.width as usize)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("main"));
        assert!(text.contains("└─"));
        assert!(text.contains("feature"));
    }

    #[test]
    fn shell_targets_unconsumed_input_and_keeps_prefix_local() {
        let config = ClientShellConfig::from_config(&Config::default());
        let mut state = ClientShellState::new(config);
        state.set_snapshot(Box::new(snapshot()));

        let text = state.handle_input_bytes(b"hello");
        assert_eq!(text.requests.len(), 1);
        let ClientMessage::ClientShellPaneInput { pane_id, events } = &text.requests[0] else {
            panic!("expected targeted pane input");
        };
        assert_eq!(pane_id, "pane_1");
        assert_eq!(events.len(), 5);
        assert!(matches!(
            &events[0],
            ClientPaneInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Char('h'),
                generated_text: Some(text),
                ..
            } if text == "h"
        ));

        let interrupt = state.handle_input_bytes(b"\x1b[99;5u");
        assert_eq!(interrupt.requests.len(), 1);
        let ClientMessage::ClientShellPaneInput { events, .. } = &interrupt.requests[0] else {
            panic!("expected semantic interrupt");
        };
        assert!(matches!(
            &events[..],
            [ClientPaneInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Char('c'),
                modifiers,
                kind: crate::protocol::ClientKeyKind::Press,
                ..
            }] if *modifiers == KeyModifiers::CONTROL.bits()
        ));

        let alt = state.handle_input_bytes(b"\x1b[120;3u");
        let ClientMessage::ClientShellPaneInput { events, .. } = &alt.requests[0] else {
            panic!("expected semantic alt key");
        };
        assert!(matches!(
            &events[..],
            [ClientPaneInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Char('x'),
                modifiers,
                ..
            }] if *modifiers == KeyModifiers::ALT.bits()
        ));
        assert!(!state.handle_input_bytes(&[0x02]).detach);
        let detach = state.handle_input_bytes(b"q");
        assert!(detach.detach);
        assert!(detach.requests.is_empty());
    }

    #[test]
    fn configured_prefix_is_client_owned_and_renders_its_bar() {
        let config = toml::from_str::<Config>(
            r#"
[keys]
prefix = "ctrl+a"
detach = "prefix+x"
"#,
        )
        .expect("configured keybinds");
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());

        let old_default = state.handle_input_bytes(&[0x02]);
        assert_eq!(
            old_default.requests.len(),
            1,
            "ctrl-b should reach the pane"
        );

        let prefix = state.handle_input_bytes(&[0x01]);
        assert!(prefix.requests.is_empty());
        assert!(prefix.repaint);
        let frame = state.compose(106, 20).expect("prefix frame");
        let text = frame
            .cells
            .chunks(frame.width as usize)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("PREFIX"), "frame: {text:?}");
        assert!(text.contains("ctrl+a"), "frame: {text:?}");

        let detach = state.handle_input_bytes(b"x");
        assert!(detach.detach);
        assert!(detach.requests.is_empty());
    }

    #[test]
    fn prefix_endpoint_action_uses_public_api_with_stable_ids() {
        let mut config = Config::default();
        config.ui.prompt_new_tab_name = false;
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
        state.set_snapshot(Box::new(snapshot()));

        assert!(state.handle_input_bytes(&[0x02]).actions.is_empty());
        let create = state.handle_input_bytes(b"c");
        let [ClientShellAction::Endpoint { boot_id, request }] = &create.actions[..] else {
            panic!("expected one endpoint action: {:?}", create.actions);
        };
        assert_eq!(boot_id, "boot-1");
        match &request.method {
            crate::api::schema::Method::TabCreate(params) => {
                assert_eq!(params.workspace_id.as_deref(), Some("ws_1"));
                assert!(params.focus);
            }
            other => panic!("expected tab.create, got {other:?}"),
        }
        assert!(state.pending_requests.contains_key(&request.id));
    }

    #[test]
    fn pane_key_release_keeps_the_press_target() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot()));

        let press = state.handle_input_bytes(b"\x1b[99;5u");
        let release = state.handle_input_bytes(b"\x1b[99;5:3u");
        let ClientMessage::ClientShellPaneInput {
            pane_id: press_target,
            ..
        } = &press.requests[0]
        else {
            panic!("expected targeted press");
        };
        let ClientMessage::ClientShellPaneInput {
            pane_id: release_target,
            events,
        } = &release.requests[0]
        else {
            panic!("expected targeted release");
        };
        assert_eq!(release_target, press_target);
        assert!(matches!(
            &events[..],
            [ClientPaneInputEvent::Key {
                kind: crate::protocol::ClientKeyKind::Release,
                ..
            }]
        ));
    }

    #[test]
    fn help_overlay_uses_live_keymap_and_owns_filter_state() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());
        let mut open = ClientShellInput::default();
        state.record_binding(
            crate::input::KeybindMatch::Action(crate::input::KeybindAction::Help),
            &mut open,
        );
        let initial = state.compose(106, 30).expect("help overlay");
        let text = initial
            .cells
            .chunks(initial.width as usize)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("keybinds"));
        assert!(text.contains("prefix mode"));

        assert!(state.handle_input_bytes(b"/").actions.is_empty());
        assert!(state.handle_input_bytes(b"workspace").actions.is_empty());
        let filtered = state.compose(106, 30).expect("filtered help");
        let text = filtered
            .cells
            .chunks(filtered.width as usize)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("workspace navigation"));
        assert!(!text.contains("prefix mode"));
        assert!(filtered
            .cursor
            .as_ref()
            .is_some_and(|cursor| cursor.visible));

        assert!(state.handle_input_bytes(b"\x1b").repaint);
        assert!(matches!(state.overlay, Some(ClientShellOverlay::Help(_))));
        assert!(state.handle_input_bytes(b"\x1b").repaint);
        assert!(state.overlay.is_none());
    }

    #[test]
    fn pane_cycle_last_and_agent_actions_resolve_to_stable_pane_ids() {
        let mut initial = snapshot();
        let mut second = initial.panes[0].clone();
        second.pane_id = "pane_2".into();
        second.focused = false;
        initial.panes.push(second);
        initial.agents = vec![
            ClientShellAgent {
                pane_id: "pane_1".into(),
                workspace_id: "ws_1".into(),
                tab_id: "tab_1".into(),
                name: Some("first".into()),
                display_agent: None,
                agent: None,
                title: None,
                agent_status: AgentStatus::Idle,
                focused: true,
            },
            ClientShellAgent {
                pane_id: "pane_2".into(),
                workspace_id: "ws_1".into(),
                tab_id: "tab_1".into(),
                name: Some("second".into()),
                display_agent: None,
                agent: None,
                title: None,
                agent_status: AgentStatus::Idle,
                focused: false,
            },
        ];
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(initial.clone()));

        let mut cycle = ClientShellInput::default();
        state.record_binding(
            crate::input::KeybindMatch::Action(crate::input::KeybindAction::CyclePaneNext),
            &mut cycle,
        );
        let [ClientShellAction::Endpoint { request, .. }] = &cycle.actions[..] else {
            panic!("pane cycle should use endpoint API");
        };
        assert!(matches!(
            &request.method,
            crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_2"
        ));

        let mut replacement = initial;
        replacement.revision = 2;
        replacement.focused_pane_id = Some("pane_2".into());
        replacement.panes[0].focused = false;
        replacement.panes[1].focused = true;
        state.set_snapshot(Box::new(replacement));
        let mut last = ClientShellInput::default();
        state.record_binding(
            crate::input::KeybindMatch::Action(crate::input::KeybindAction::LastPane),
            &mut last,
        );
        let [ClientShellAction::Endpoint { request, .. }] = &last.actions[..] else {
            panic!("last pane should use endpoint API");
        };
        assert!(matches!(
            &request.method,
            crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_1"
        ));

        let mut agent = ClientShellInput::default();
        state.record_binding(
            crate::input::KeybindMatch::Action(crate::input::KeybindAction::FocusAgent(1)),
            &mut agent,
        );
        let [ClientShellAction::Endpoint { request, .. }] = &agent.actions[..] else {
            panic!("agent focus should use endpoint API");
        };
        assert!(matches!(
            &request.method,
            crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_2"
        ));
    }

    #[test]
    fn workspace_actions_preserve_selected_target_and_client_confirmation() {
        let mut snapshot = snapshot();
        let mut second = snapshot.workspaces[0].clone();
        second.workspace_id = "ws_2".into();
        second.number = 2;
        second.label = "second".into();
        second.focused = false;
        snapshot.workspaces.push(second);
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot));
        state.navigate_workspace_id = Some("ws_2".into());

        let mut rename = ClientShellInput::default();
        state.record_binding(
            crate::input::KeybindMatch::Action(crate::input::KeybindAction::RenameWorkspace),
            &mut rename,
        );
        assert!(matches!(
            state.overlay.as_ref(),
            Some(ClientShellOverlay::Rename(ClientRenameOverlay {
                target: ClientRenameTarget::Workspace { workspace_id },
                ..
            })) if workspace_id == "ws_2"
        ));
        assert!(state.handle_input_bytes(&[0x15]).actions.is_empty());
        assert!(state.handle_input_bytes(b"renamed").actions.is_empty());
        let save = state.handle_input_bytes(b"\r");
        let [ClientShellAction::Endpoint { request, .. }] = &save.actions[..] else {
            panic!("workspace rename should use endpoint API");
        };
        assert!(matches!(
            &request.method,
            crate::api::schema::Method::WorkspaceRename(params)
                if params.workspace_id == "ws_2" && params.label == "renamed"
        ));

        state.navigate_workspace_id = Some("ws_2".into());
        let mut close = ClientShellInput::default();
        state.record_binding(
            crate::input::KeybindMatch::Action(crate::input::KeybindAction::CloseWorkspace),
            &mut close,
        );
        assert!(close.actions.is_empty());
        assert!(matches!(
            state.overlay.as_ref(),
            Some(ClientShellOverlay::ConfirmClose(ClientConfirmCloseOverlay {
                workspace_id,
                ..
            })) if workspace_id == "ws_2"
        ));
        let confirm = state.handle_input_bytes(b"\r");
        let [ClientShellAction::Endpoint { request, .. }] = &confirm.actions[..] else {
            panic!("workspace confirmation should use endpoint API");
        };
        assert!(matches!(
            &request.method,
            crate::api::schema::Method::WorkspaceClose(params)
                if params.workspace_id == "ws_2" && params.close_group
        ));
    }

    #[test]
    fn named_workspace_overlay_uses_projected_pane_cwd() {
        let mut config = Config::default();
        config.ui.prompt_new_workspace_name = true;
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
        state.set_snapshot(Box::new(snapshot()));
        let mut open = ClientShellInput::default();
        state.record_binding(
            crate::input::KeybindMatch::Action(crate::input::KeybindAction::NewWorkspace),
            &mut open,
        );
        assert!(matches!(
            state.overlay.as_ref(),
            Some(ClientShellOverlay::Rename(ClientRenameOverlay {
                input: value,
                target: ClientRenameTarget::NewWorkspace { cwd, .. },
                ..
            })) if value == "repo" && cwd.as_deref() == Some("/repo")
        ));
        let create = state.handle_input_bytes(b"\r");
        let [ClientShellAction::Endpoint { request, .. }] = &create.actions[..] else {
            panic!("named workspace should use endpoint API");
        };
        assert!(matches!(
            &request.method,
            crate::api::schema::Method::WorkspaceCreate(params)
                if params.cwd.as_deref() == Some("/repo") && params.label.is_none()
        ));
    }

    #[test]
    fn new_tab_overlay_owns_text_cursor_and_submits_public_api_request() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());
        let mut open = ClientShellInput::default();
        state.record_binding(
            crate::input::KeybindMatch::Action(crate::input::KeybindAction::NewTab),
            &mut open,
        );
        assert!(open.actions.is_empty());
        let frame = state.compose(106, 20).expect("new tab overlay");
        let text = frame
            .cells
            .chunks(frame.width as usize)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("new tab"));
        assert!(text.contains("save"));
        assert!(frame.cursor.as_ref().is_some_and(|cursor| cursor.visible));

        assert!(state.handle_input_bytes(b"logs").actions.is_empty());
        let create = state.handle_input_bytes(b"\r");
        let [ClientShellAction::Endpoint { request, .. }] = &create.actions[..] else {
            panic!("new tab save should use endpoint API");
        };
        assert!(matches!(
            &request.method,
            crate::api::schema::Method::TabCreate(params)
                if params.workspace_id.as_deref() == Some("ws_1")
                    && params.label.as_deref() == Some("logs")
        ));
        assert!(state.overlay.is_none());
    }

    #[test]
    fn rename_pane_empty_value_is_preserved_as_a_clear_request() {
        let mut snapshot = snapshot();
        snapshot.panes[0].label = Some("build".into());
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot));
        let mut open = ClientShellInput::default();
        state.record_binding(
            crate::input::KeybindMatch::Action(crate::input::KeybindAction::RenamePane),
            &mut open,
        );
        assert!(state.handle_input_bytes(&[0x15]).actions.is_empty());
        let save = state.handle_input_bytes(b"\r");
        let [ClientShellAction::Endpoint { request, .. }] = &save.actions[..] else {
            panic!("pane rename should use endpoint API");
        };
        assert!(matches!(
            &request.method,
            crate::api::schema::Method::PaneRename(params)
                if params.pane_id == "pane_1" && params.label.as_deref() == Some("")
        ));
    }

    #[test]
    fn close_confirmation_error_becomes_client_owned_overlay_and_stable_group_close() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());
        let mut close = ClientShellInput::default();
        state.record_binding(
            crate::input::KeybindMatch::Action(crate::input::KeybindAction::ClosePane),
            &mut close,
        );
        let [ClientShellAction::Endpoint { request, .. }] = &close.actions[..] else {
            panic!("pane close should use endpoint API");
        };
        let request_id = request.id.clone();
        assert!(state.handle_endpoint_result(
            "boot-1",
            &request_id,
            Err(ClientShellEndpointError {
                code: Some("confirmation_required".into()),
                message: "confirmation required".into(),
            }),
        ));
        let frame = state.compose(106, 20).expect("confirmation overlay");
        let text = frame
            .cells
            .chunks(frame.width as usize)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Close workspace?"));
        assert!(text.contains("1 pane"));

        let confirm = state.handle_input_bytes(b"\r");
        let [ClientShellAction::Endpoint { request, .. }] = &confirm.actions[..] else {
            panic!("confirmation should use endpoint API");
        };
        assert!(matches!(
            &request.method,
            crate::api::schema::Method::WorkspaceClose(params)
                if params.workspace_id == "ws_1" && params.close_group
        ));
    }

    #[test]
    fn navigate_mode_selects_workspace_locally_then_focuses_by_stable_id() {
        let mut snapshot = snapshot();
        let mut second = snapshot.workspaces[0].clone();
        second.workspace_id = "ws_2".into();
        second.number = 2;
        second.label = "second".into();
        second.focused = false;
        snapshot.workspaces.push(second);
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot));
        state.set_pane_surface(surface());

        assert!(state.handle_input_bytes(&[0x02]).actions.is_empty());
        let enter_navigate = state.handle_input_bytes(b"w");
        assert!(enter_navigate.repaint);
        assert_eq!(state.mode, ClientShellMode::Navigate);
        assert_eq!(state.navigate_workspace_id.as_deref(), Some("ws_1"));

        let move_selection = state.handle_input_bytes(b"\x1b[B");
        assert!(move_selection.actions.is_empty());
        assert_eq!(state.navigate_workspace_id.as_deref(), Some("ws_2"));
        let frame = state.compose(106, 20).expect("navigate frame");
        let text = frame
            .cells
            .chunks(frame.width as usize)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("second"));
        assert!(text.contains("NAVIGATE"));

        let focus = state.handle_input_bytes(b"\r");
        let [ClientShellAction::Endpoint { request, .. }] = &focus.actions[..] else {
            panic!("selected workspace should use endpoint API");
        };
        assert!(matches!(
            &request.method,
            crate::api::schema::Method::WorkspaceFocus(target)
                if target.workspace_id == "ws_2"
        ));
        assert_eq!(state.mode, ClientShellMode::Terminal);
    }

    #[test]
    fn resize_mode_reuses_endpoint_resize_and_stays_active_until_done() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());

        assert!(state.handle_input_bytes(&[0x02]).actions.is_empty());
        assert!(state.handle_input_bytes(b"r").actions.is_empty());
        assert_eq!(state.mode, ClientShellMode::Resize);

        let resize = state.handle_input_bytes(b"h");
        let [ClientShellAction::Endpoint { request, .. }] = &resize.actions[..] else {
            panic!("resize should use endpoint API");
        };
        assert!(matches!(
            &request.method,
            crate::api::schema::Method::PaneResize(params)
                if params.pane_id.as_deref() == Some("pane_1")
                    && params.direction == crate::api::schema::PaneDirection::Left
        ));
        assert_eq!(state.mode, ClientShellMode::Resize);

        assert!(state.handle_input_bytes(b"\r").actions.is_empty());
        assert_eq!(state.mode, ClientShellMode::Terminal);
    }

    #[test]
    fn navigator_owns_search_mouse_selection_and_stable_target_focus() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());
        let mut open = ClientShellInput::default();
        state.record_binding(
            crate::input::KeybindMatch::Action(crate::input::KeybindAction::OpenNavigator),
            &mut open,
        );
        let navigator = state.compose(106, 30).expect("navigator overlay");
        let navigator_text = navigator
            .cells
            .chunks(navigator.width as usize)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(navigator_text.contains("client-shell"));
        assert!(navigator_text.contains("pane 1"));

        let search = state.hits.navigator_search;
        let focus_search =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: search.x,
                row: search.y,
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(focus_search.repaint);
        assert!(matches!(
            state.overlay,
            Some(ClientShellOverlay::Navigator(ClientNavigatorOverlay {
                search_focused: true,
                ..
            }))
        ));
        assert!(state.handle_input_bytes(b"client").actions.is_empty());
        let filtered = state.compose(106, 30).expect("filtered navigator");
        assert!(filtered
            .cursor
            .as_ref()
            .is_some_and(|cursor| cursor.visible));

        state.handle_input_bytes(b"\x1b");
        state.handle_input_bytes(b"a");
        state.compose(106, 30).expect("navigator rows");
        let pane_index = {
            let snapshot = state.snapshot.as_deref().expect("snapshot");
            let ClientShellOverlay::Navigator(navigator) =
                state.overlay.as_ref().expect("navigator")
            else {
                panic!("expected navigator");
            };
            render::client_navigator_rows(snapshot, navigator)
                .iter()
                .position(|row| matches!(row.target, ClientNavigatorTarget::Pane(_)))
                .expect("pane row")
        };
        let pane_rect = state
            .hits
            .navigator_rows
            .iter()
            .find(|(_, index)| *index == pane_index)
            .map(|(rect, _)| *rect)
            .expect("visible pane row");
        let select =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Moved,
                column: pane_rect.x + 6,
                row: pane_rect.y,
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(select.repaint);
        let accept =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: pane_rect.x + 6,
                row: pane_rect.y,
                modifiers: KeyModifiers::empty(),
            })]);
        let [ClientShellAction::Endpoint { request, .. }] = &accept.actions[..] else {
            panic!("navigator pane click should use endpoint API");
        };
        assert!(matches!(
            &request.method,
            crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_1"
        ));
        assert!(state.overlay.is_none());
    }

    #[test]
    fn worktree_create_prepares_from_public_list_and_submits_derived_path() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());
        let mut prepare = ClientShellInput::default();
        state.record_binding(
            crate::input::KeybindMatch::Action(crate::input::KeybindAction::NewWorktree),
            &mut prepare,
        );
        let [ClientShellAction::Endpoint { request, .. }] = &prepare.actions[..] else {
            panic!("new worktree should prepare through worktree.list");
        };
        assert!(matches!(
            &request.method,
            crate::api::schema::Method::WorktreeList(params)
                if params.workspace_id.as_deref() == Some("ws_1")
        ));
        let request_id = request.id.clone();
        assert!(state.handle_endpoint_result(
            "boot-1",
            &request_id,
            Ok(worktree_list_result(None)),
        ));
        let frame = state.compose(106, 30).expect("new worktree modal");
        let text = frame
            .cells
            .chunks(frame.width as usize)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("new worktree"));
        assert!(text.contains("create and open"));
        assert!(frame.cursor.as_ref().is_some_and(|cursor| cursor.visible));

        assert!(state
            .handle_input_bytes(b"feature/client-shell")
            .actions
            .is_empty());
        let submit = state.handle_input_bytes(b"\r");
        let [ClientShellAction::Endpoint { request, .. }] = &submit.actions[..] else {
            panic!("worktree create should use endpoint API");
        };
        assert!(matches!(
            &request.method,
            crate::api::schema::Method::WorktreeCreate(params)
                if params.workspace_id.as_deref() == Some("ws_1")
                    && params.branch.as_deref() == Some("feature/client-shell")
                    && params.path.as_deref().is_some_and(|path| path.ends_with("repo/feature-client-shell"))
                    && params.focus
        ));
    }

    #[test]
    fn worktree_open_filters_and_clicks_a_stable_public_entry() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());
        let mut prepare = ClientShellInput::default();
        state.record_binding(
            crate::input::KeybindMatch::Action(crate::input::KeybindAction::OpenWorktree),
            &mut prepare,
        );
        let [ClientShellAction::Endpoint { request, .. }] = &prepare.actions[..] else {
            panic!("open worktree should prepare through worktree.list");
        };
        let request_id = request.id.clone();
        state.handle_endpoint_result("boot-1", &request_id, Ok(worktree_list_result(None)));
        let frame = state.compose(106, 30).expect("open worktree modal");
        let text = frame
            .cells
            .chunks(frame.width as usize)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("feature"));
        let row = state.hits.worktree_rows[0].0;
        let open =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: row.x + 2,
                row: row.y,
                modifiers: KeyModifiers::empty(),
            })]);
        let [ClientShellAction::Endpoint { request, .. }] = &open.actions[..] else {
            panic!("worktree row should open through endpoint API");
        };
        assert!(matches!(
            &request.method,
            crate::api::schema::Method::WorktreeOpen(params)
                if params.workspace_id.as_deref() == Some("ws_1")
                    && params.path.as_deref() == Some("/repo-feature")
                    && params.focus
        ));
    }

    #[test]
    fn worktree_remove_escalates_dirty_failure_to_force_confirmation() {
        let mut snapshot = snapshot();
        snapshot.workspaces[0].worktree = Some(ClientShellWorktree {
            key: "repo-key".into(),
            label: "repo".into(),
            is_linked_worktree: true,
        });
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot));
        state.set_pane_surface(surface());
        let mut prepare = ClientShellInput::default();
        state.record_binding(
            crate::input::KeybindMatch::Action(crate::input::KeybindAction::RemoveWorktree),
            &mut prepare,
        );
        let [ClientShellAction::Endpoint { request, .. }] = &prepare.actions[..] else {
            panic!("remove worktree should prepare through worktree.list");
        };
        let request_id = request.id.clone();
        state.handle_endpoint_result(
            "boot-1",
            &request_id,
            Ok(worktree_list_result(Some("ws_1"))),
        );
        let remove = state.handle_input_bytes(b"\r");
        let [ClientShellAction::Endpoint { request, .. }] = &remove.actions[..] else {
            panic!("worktree remove should use endpoint API");
        };
        assert!(matches!(
            &request.method,
            crate::api::schema::Method::WorktreeRemove(params)
                if params.workspace_id == "ws_1" && !params.force
        ));
        let request_id = request.id.clone();
        state.handle_endpoint_result(
            "boot-1",
            &request_id,
            Err(ClientShellEndpointError {
                code: Some("dirty_worktree_requires_force".into()),
                message: "dirty worktree".into(),
            }),
        );
        let frame = state.compose(106, 30).expect("force remove modal");
        let text = frame
            .cells
            .chunks(frame.width as usize)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("delete anyway"));
        assert!(text.contains("permanently deleted"));
        let force = state.handle_input_bytes(b"\r");
        let [ClientShellAction::Endpoint { request, .. }] = &force.actions[..] else {
            panic!("forced worktree remove should use endpoint API");
        };
        assert!(matches!(
            &request.method,
            crate::api::schema::Method::WorktreeRemove(params)
                if params.workspace_id == "ws_1" && params.force
        ));
    }

    #[test]
    fn mobile_layout_reserves_only_client_header() {
        let config = ClientShellConfig::from_config(&Config::default());
        let state = ClientShellState::new(config);
        let layout = state.layout(44, 20);
        assert_eq!(layout.mobile_header, Rect::new(0, 0, 44, 2));
        assert_eq!(layout.pane_surface, Rect::new(0, 2, 44, 18));
        assert_eq!(
            state.surface_size(44, 20),
            ClientSurfaceSize { cols: 44, rows: 18 }
        );
    }
}
