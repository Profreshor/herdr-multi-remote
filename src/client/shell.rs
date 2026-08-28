use std::collections::{HashMap, HashSet};

mod actions;
mod agent_sidebar;
mod composition;
mod config;
mod context_menu;
mod global_menu;
mod input;
mod mouse;
mod notifications;
mod overlay_input;
mod preferences;
mod render;
mod scroll;
mod settings;
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
    Config, LiveKeybindConfig, SidebarCollapsedModeConfig, SpacesSidebarConfig,
    TabBarPositionConfig,
};
use crate::protocol::{
    ClientMessage, ClientMousePosition, ClientPaneInputEvent, ClientShellSnapshot, ClientShellTab,
    ClientShellWorkspace, ClientSurfaceSize, FrameData, PaneSurfaceFrame, SemanticNotification,
    SemanticNotificationKind, SemanticNotificationSound,
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

fn push_target_event(
    target: ClientInputTarget,
    event: ClientPaneInputEvent,
    outcome: &mut ClientShellInput,
) {
    match target {
        ClientInputTarget::Pane(pane_id) => {
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
        ClientInputTarget::Popup(terminal_id) => {
            if let Some(ClientMessage::ClientShellPopupInput {
                terminal_id: pending_terminal,
                events,
            }) = outcome.requests.last_mut()
            {
                if *pending_terminal == terminal_id {
                    events.push(event);
                    return;
                }
            }
            outcome.requests.push(ClientMessage::ClientShellPopupInput {
                terminal_id,
                events: vec![event],
            });
        }
    }
}

fn contains(rect: Rect, point: (u16, u16)) -> bool {
    rect.width > 0
        && rect.height > 0
        && point.0 >= rect.x
        && point.0 < rect.right()
        && point.1 >= rect.y
        && point.1 < rect.bottom()
}

fn pane_surface_topology_signature(surface: &PaneSurfaceFrame) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    fn write(hash: &mut u64, bytes: &[u8]) {
        for byte in bytes {
            *hash ^= u64::from(*byte);
            *hash = hash.wrapping_mul(PRIME);
        }
        *hash ^= 0xff;
        *hash = hash.wrapping_mul(PRIME);
    }

    let mut pane_ids = surface
        .panes
        .iter()
        .map(|pane| pane.pane_id.as_bytes())
        .collect::<Vec<_>>();
    pane_ids.sort_unstable();
    let mut hash = OFFSET;
    for pane_id in pane_ids {
        write(&mut hash, pane_id);
    }
    let mut splits = surface.splits.iter().collect::<Vec<_>>();
    splits.sort_by(|left, right| left.path.cmp(&right.path));
    for split in splits {
        write(
            &mut hash,
            &[match split.direction {
                crate::protocol::PaneSurfaceSplitDirection::Horizontal => 0,
                crate::protocol::PaneSurfaceSplitDirection::Vertical => 1,
            }],
        );
        write(
            &mut hash,
            &split
                .path
                .iter()
                .map(|right| u8::from(*right))
                .collect::<Vec<_>>(),
        );
    }
    hash
}

fn status_icon(
    status: crate::api::schema::AgentStatus,
    style: crate::config::StatusIndicatorStyle,
) -> &'static str {
    use crate::api::schema::AgentStatus;
    use crate::config::StatusIndicatorStyle;
    match (style, status) {
        (
            StatusIndicatorStyle::Dots,
            AgentStatus::Working | AgentStatus::Blocked | AgentStatus::Done,
        ) => "●",
        (StatusIndicatorStyle::Dots, AgentStatus::Idle) => "○",
        (StatusIndicatorStyle::Dots, AgentStatus::Unknown) => "·",
        (StatusIndicatorStyle::Symbols, AgentStatus::Blocked) => "×",
        (StatusIndicatorStyle::Symbols, AgentStatus::Working) => "◐",
        (StatusIndicatorStyle::Symbols, AgentStatus::Done) => "✓",
        (StatusIndicatorStyle::Symbols, AgentStatus::Idle) => "○",
        (StatusIndicatorStyle::Symbols, AgentStatus::Unknown) => "·",
    }
}

fn status_dot(status: crate::api::schema::AgentStatus) -> &'static str {
    status_icon(status, crate::config::StatusIndicatorStyle::Dots)
}

fn status_priority(status: crate::api::schema::AgentStatus) -> u8 {
    use crate::api::schema::AgentStatus;
    match status {
        AgentStatus::Blocked => 4,
        AgentStatus::Done => 3,
        AgentStatus::Working => 2,
        AgentStatus::Idle => 1,
        AgentStatus::Unknown => 0,
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
        AgentStatus::Done => palette.teal,
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
        PaneSurfaceSplit, PaneSurfaceSplitDirection, SurfaceRect,
    };

    fn snapshot() -> ClientShellSnapshot {
        ClientShellSnapshot {
            boot_id: "boot-1".into(),
            revision: 1,
            focused_workspace_id: Some("ws_1".into()),
            focused_tab_id: Some("tab_1".into()),
            focused_pane_id: Some("pane_1".into()),
            tab_bar_right: Vec::new(),
            tab_bar_right_separator: " ".into(),
            agent_view_label: None,
            agent_order: Vec::new(),
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
                right_click_passthrough: false,
            }],
            agents: Vec::new(),
            commands: Vec::new(),
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
                mouse_reporting: false,
                sgr_pixel_mouse: false,
                pixel_width: 0,
                pixel_height: 0,
            }],
            splits: Vec::new(),
            popup: None,
        }
    }

    fn surface_with_popup() -> PaneSurfaceFrame {
        let mut surface = surface();
        let popup_buffer = Buffer::with_lines(["popup-live", "", ""]);
        surface.popup = Some(Box::new(crate::protocol::ClientShellPopupSurface {
            terminal_id: "terminal-popup".into(),
            title: "popup title".into(),
            width: Some(crate::protocol::ClientShellPopupSize::Cells(12)),
            height: Some(crate::protocol::ClientShellPopupSize::Cells(5)),
            frame: FrameData::from_ratatui_buffer_with_hyperlinks(
                &popup_buffer,
                Some(crate::protocol::CursorState {
                    x: 2,
                    y: 1,
                    visible: true,
                    shape: 1,
                }),
                &[],
            ),
            mouse_reporting: true,
            sgr_pixel_mouse: false,
            pixel_width: 0,
            pixel_height: 0,
        }));
        surface
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
    fn client_composes_popup_terminal_content_inside_client_owned_chrome() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface_with_popup());

        let frame = state.compose(106, 20).expect("popup frame");
        let popup = state.hits.popup.as_ref().expect("popup hit geometry");
        assert_eq!(popup.rect.width, 12);
        assert_eq!(popup.rect.height, 5);
        assert_eq!(popup.inner_rect.width, 9);
        assert_eq!(popup.inner_rect.height, 3);
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
        assert!(text.contains("popup tit"));
        assert!(text.contains("popup-liv"));
        assert_eq!(
            frame.cursor.as_ref().map(|cursor| (cursor.x, cursor.y)),
            Some((popup.inner_rect.x + 2, popup.inner_rect.y + 1))
        );
    }

    #[test]
    fn popup_owns_keys_text_paste_and_mouse_before_shell_controls() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface_with_popup());
        state.compose(106, 20).expect("popup frame");

        for bytes in [b"x".as_slice(), b"\x02".as_slice(), b"\x1b".as_slice()] {
            let input = state.handle_input_bytes(bytes);
            assert!(matches!(
                &input.requests[..],
                [ClientMessage::ClientShellPopupInput { terminal_id, .. }]
                    if terminal_id == "terminal-popup"
            ));
            assert_eq!(state.mode, ClientShellMode::Terminal);
        }

        let text = state.handle_raw_events(vec![RawInputEvent::Text(
            crate::input::TextCommit::new("ime"),
        )]);
        assert!(matches!(
            &text.requests[..],
            [ClientMessage::ClientShellPopupInput { events, .. }]
                if matches!(&events[..], [ClientPaneInputEvent::TextCommit(value)] if value == "ime")
        ));
        let paste = state.handle_raw_events(vec![RawInputEvent::Paste("paste".into())]);
        assert!(matches!(
            &paste.requests[..],
            [ClientMessage::ClientShellPopupInput { events, .. }]
                if matches!(&events[..], [ClientPaneInputEvent::Paste(value)] if value == "paste")
        ));

        let popup = state.hits.popup.as_ref().expect("popup hit").clone();
        let mouse =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: popup.inner_rect.x + 3,
                row: popup.inner_rect.y + 1,
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(matches!(
            &mouse.requests[..],
            [ClientMessage::ClientShellPopupInput { events, .. }]
                if matches!(
                    &events[..],
                    [ClientPaneInputEvent::Mouse {
                        position: ClientMousePosition::Cell { column: 3, row: 1 },
                        ..
                    }]
                )
        ));
        assert!(state.pane_mouse_gesture.is_some());
        state.set_pane_surface(surface());
        assert!(state.pane_mouse_gesture.is_none());
    }

    #[test]
    fn popup_transition_dismisses_client_overlays_and_restores_pane_input_after_close() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());
        state.record_binding(
            crate::input::KeybindMatch::Action(crate::input::KeybindAction::Help),
            &mut ClientShellInput::default(),
        );
        assert!(state.overlay.is_some());

        state.set_pane_surface(surface_with_popup());
        assert!(state.overlay.is_none());
        assert_eq!(state.mode, ClientShellMode::Terminal);
        let popup_input = state.handle_input_bytes(b"p");
        assert!(matches!(
            &popup_input.requests[..],
            [ClientMessage::ClientShellPopupInput { .. }]
        ));

        state.set_pane_surface(surface());
        let pane_input = state.handle_input_bytes(b"p");
        assert!(matches!(
            &pane_input.requests[..],
            [ClientMessage::ClientShellPaneInput { pane_id, .. }] if pane_id == "pane_1"
        ));
    }

    #[test]
    fn popup_target_survives_surface_invalidation_during_resize() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface_with_popup());
        state.invalidate_pane_surface();

        assert!(matches!(
            &state.handle_input_bytes(b"x").requests[..],
            [ClientMessage::ClientShellPopupInput { terminal_id, .. }]
                if terminal_id == "terminal-popup"
        ));
        let mouse =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 0,
                row: 0,
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(mouse.requests.is_empty());
        assert!(mouse.actions.is_empty());
    }

    #[test]
    fn popup_close_reprocesses_held_key_repeats_into_the_focused_pane() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface_with_popup());

        let press = state.handle_raw_events(vec![RawInputEvent::Key(
            crate::input::TerminalKey::new(KeyCode::Char('x'), KeyModifiers::empty()),
        )]);
        assert!(matches!(
            &press.requests[..],
            [ClientMessage::ClientShellPopupInput { .. }]
        ));

        state.set_pane_surface(surface());
        let repeat = state.handle_raw_events(vec![RawInputEvent::Key(
            crate::input::TerminalKey::new(KeyCode::Char('x'), KeyModifiers::empty())
                .with_kind(crossterm::event::KeyEventKind::Repeat),
        )]);
        assert!(matches!(
            &repeat.requests[..],
            [ClientMessage::ClientShellPaneInput { pane_id, .. }] if pane_id == "pane_1"
        ));
    }

    #[test]
    fn pending_popup_suppresses_held_pane_repeats_but_preserves_release() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());
        let key = crate::input::TerminalKey::new(KeyCode::Char('x'), KeyModifiers::empty());
        assert!(matches!(
            &state
                .handle_raw_events(vec![RawInputEvent::Key(key.clone())])
                .requests[..],
            [ClientMessage::ClientShellPaneInput { .. }]
        ));

        state.popup_pending = true;
        let repeat = state.handle_raw_events(vec![RawInputEvent::Key(
            key.clone()
                .with_kind(crossterm::event::KeyEventKind::Repeat),
        )]);
        assert!(repeat.requests.is_empty());
        let release = state.handle_raw_events(vec![RawInputEvent::Key(
            key.with_kind(crossterm::event::KeyEventKind::Release),
        )]);
        assert!(matches!(
            &release.requests[..],
            [ClientMessage::ClientShellPaneInput { events, .. }]
                if matches!(
                    &events[..],
                    [ClientPaneInputEvent::Key {
                        kind: crate::protocol::ClientKeyKind::Release,
                        ..
                    }]
                )
        ));
    }

    #[test]
    fn pane_mouse_release_survives_popup_open_transition() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot()));
        let mut pane_surface = surface();
        pane_surface.panes[0].mouse_reporting = true;
        state.set_pane_surface(pane_surface);
        state.compose(106, 20).expect("pane frame");
        let pane = state.hits.panes[0].clone();

        let down =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: pane.inner_rect.x,
                row: pane.inner_rect.y,
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(matches!(
            &down.requests[..],
            [ClientMessage::ClientShellPaneInput { .. }]
        ));
        assert!(state.pane_mouse_gesture.is_some());

        state.set_pane_surface(surface_with_popup());
        let up =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: 0,
                row: 0,
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(matches!(
            &up.requests[..],
            [ClientMessage::ClientShellPaneInput { pane_id, events }]
                if pane_id == "pane_1"
                    && matches!(
                        &events[..],
                        [ClientPaneInputEvent::Mouse {
                            kind: crate::protocol::ClientMouseKind::Up(
                                crate::protocol::ClientMouseButton::Left
                            ),
                            ..
                        }]
                    )
        ));
        assert!(state.pane_mouse_gesture.is_none());
    }

    #[test]
    fn popup_command_blocks_underlying_input_until_surface_or_error() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        let binding = crate::config::CustomCommandKeybind {
            bindings: crate::config::ActionKeybinds::prefix("t"),
            label: "prefix+t".into(),
            command: "secret-popup-command".into(),
            action: crate::config::CustomCommandAction::Popup,
            description: None,
            width: None,
            height: None,
        };
        let mut projection = snapshot();
        projection
            .commands
            .push(crate::protocol::ClientShellCommand {
                command_id: "cmd_popup".into(),
                binding_label: binding.label.clone(),
                action: crate::protocol::ClientShellCommandAction::Popup,
            });
        state.set_snapshot(Box::new(projection));
        state.set_pane_surface(surface());

        let mut invoke = ClientShellInput::default();
        state.record_binding(crate::input::KeybindMatch::Command(binding), &mut invoke);
        assert!(state.popup_pending);
        assert!(state
            .handle_input_bytes(b"not-for-pane")
            .requests
            .is_empty());
        assert!(state
            .handle_raw_events(vec![RawInputEvent::Paste("secret".into())])
            .requests
            .is_empty());

        let request_id = match &invoke.actions[..] {
            [ClientShellAction::Endpoint { request, .. }] => request.id.clone(),
            other => panic!("expected popup command request, got {other:?}"),
        };
        let (repaint, _) = state.handle_endpoint_result(
            "boot-1",
            &request_id,
            Err(ClientShellEndpointError {
                code: Some("command_failed".into()),
                message: "popup failed".into(),
            }),
        );
        assert!(repaint);
        assert!(!state.popup_pending);
        assert!(matches!(
            &state.handle_input_bytes(b"p").requests[..],
            [ClientMessage::ClientShellPaneInput { .. }]
        ));

        let binding = crate::config::CustomCommandKeybind {
            bindings: crate::config::ActionKeybinds::prefix("t"),
            label: "prefix+t".into(),
            command: "secret-popup-command".into(),
            action: crate::config::CustomCommandAction::Popup,
            description: None,
            width: None,
            height: None,
        };
        let mut invoke = ClientShellInput::default();
        state.record_binding(crate::input::KeybindMatch::Command(binding), &mut invoke);
        let request_id = match &invoke.actions[..] {
            [ClientShellAction::Endpoint { request, .. }] => request.id.clone(),
            other => panic!("expected popup command request, got {other:?}"),
        };
        state.handle_endpoint_result(
            "boot-1",
            &request_id,
            Ok(crate::api::schema::ResponseResult::Ok {}),
        );
        assert!(state.popup_pending);
        assert!(state
            .handle_input_bytes(b"still-blocked")
            .requests
            .is_empty());
        let deadline = state.popup_pending_deadline.expect("pending timeout");
        state.tick_popup_pending(deadline);
        assert!(!state.popup_pending);
    }

    #[test]
    fn shell_refuses_mismatched_projection_and_clears_stale_hits_in_either_order() {
        let config = ClientShellConfig::from_config(&Config::default());
        let mut state = ClientShellState::new(config);
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());
        state.compose(106, 20).expect("initial frame");
        assert!(!state.hits.panes.is_empty());

        let mut replacement = snapshot();
        replacement.revision = 2;
        state.set_snapshot(Box::new(replacement));
        assert!(state.hits.panes.is_empty());
        assert!(state.compose(106, 20).is_none());

        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());
        state.compose(106, 20).expect("restored frame");
        let mut replacement_surface = surface();
        replacement_surface.projection_revision = 2;
        state.set_pane_surface(replacement_surface);
        assert!(state.hits.panes.is_empty());
        assert!(state.compose(106, 20).is_none());
    }

    #[test]
    fn mouse_hits_use_stable_workspace_tab_and_pane_ids() {
        let config = ClientShellConfig::from_config(&Config::default());
        let mut state = ClientShellState::new(config);
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());
        state.compose(106, 20).expect("composed frame");

        let workspace_down =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 2,
                row: 2,
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(workspace_down.actions.is_empty());
        let workspace =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
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
    fn resize_invalidation_drops_stale_hits_but_preserves_gesture_release() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot()));
        let mut pane_surface = surface();
        pane_surface.panes[0].mouse_reporting = true;
        state.set_pane_surface(pane_surface);
        state.compose(106, 20).expect("composed frame");
        let pane = state.hits.panes[0].clone();
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: pane.inner_rect.x + 1,
            row: pane.inner_rect.y + 1,
            modifiers: KeyModifiers::empty(),
        })]);
        assert!(state.pane_mouse_gesture.is_some());

        state.invalidate_pane_surface();
        assert!(state.pane_surface.is_none());
        assert!(state.hits.panes.is_empty());
        let stale_click =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Right),
                column: 27,
                row: 1,
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(stale_click.requests.is_empty());
        assert!(stale_click.actions.is_empty());

        let release =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: pane.inner_rect.x + 1,
                row: pane.inner_rect.y + 1,
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(matches!(
            &release.requests[..],
            [ClientMessage::ClientShellPaneInput { pane_id, events }]
                if pane_id == "pane_1"
                    && matches!(
                        &events[..],
                        [ClientPaneInputEvent::Mouse {
                            kind: crate::protocol::ClientMouseKind::Up(
                                crate::protocol::ClientMouseButton::Left
                            ),
                            ..
                        }]
                    )
        ));
        assert!(state.pane_mouse_gesture.is_none());
    }

    #[test]
    fn pane_split_drag_uses_projected_handle_and_stable_tab_path() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot()));
        let mut pane_surface = surface();
        pane_surface.splits.push(PaneSurfaceSplit {
            direction: PaneSurfaceSplitDirection::Horizontal,
            pos: 40,
            area: SurfaceRect {
                x: 0,
                y: 0,
                width: 80,
                height: 19,
            },
            hit_rect: SurfaceRect {
                x: 40,
                y: 0,
                width: 1,
                height: 19,
            },
            path: vec![false, true],
        });
        state.set_pane_surface(pane_surface);
        state.compose(106, 20).expect("split pane surface");
        let split = state.hits.pane_splits[0].clone();

        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: split.hit_rect.x,
            row: split.hit_rect.y + 2,
            modifiers: KeyModifiers::empty(),
        })]);
        assert!(matches!(
            state.chrome_drag,
            Some(ClientChromeDrag::PaneSplit { .. })
        ));
        let mut replacement = snapshot();
        replacement.revision = 2;
        replacement
            .tab_bar_right
            .push(crate::protocol::ClientShellTabStatusSegment {
                text: "updated".into(),
                accent: false,
            });
        let mut replacement_surface = surface();
        replacement_surface.projection_revision = 2;
        replacement_surface.splits.push(PaneSurfaceSplit {
            direction: PaneSurfaceSplitDirection::Horizontal,
            pos: 40,
            area: SurfaceRect {
                x: 0,
                y: 0,
                width: 80,
                height: 19,
            },
            hit_rect: SurfaceRect {
                x: 40,
                y: 0,
                width: 1,
                height: 19,
            },
            path: vec![false, true],
        });
        state.set_snapshot(Box::new(replacement));
        state.set_pane_surface(replacement_surface);
        let drag =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: split.area.x + 48,
                row: split.hit_rect.y + 2,
                modifiers: KeyModifiers::empty(),
            })]);
        let [ClientShellAction::Endpoint { request, .. }] = &drag.actions[..] else {
            panic!("pane split drag should use endpoint API");
        };
        assert!(matches!(
            &request.method,
            crate::api::schema::Method::LayoutSetSplitRatio(params)
                if params.tab_id.as_deref() == Some("tab_1")
                    && params.path == vec![false, true]
                    && (params.ratio - 0.6).abs() < f32::EPSILON
        ));
        let release =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: split.area.x + 48,
                row: split.hit_rect.y + 2,
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(release.actions.is_empty());
        assert!(state.chrome_drag.is_none());
    }

    #[test]
    fn disabled_mouse_chrome_keeps_tab_wheel_but_removes_split_drag_hits() {
        let mut config = Config::default();
        config.ui.mouse_capture = false;
        let mut projected = snapshot();
        let mut second_tab = projected.tabs[0].clone();
        second_tab.tab_id = "tab_2".into();
        second_tab.number = 2;
        second_tab.label = "2".into();
        second_tab.focused = false;
        projected.tabs.push(second_tab);
        let mut pane_surface = surface();
        pane_surface.splits.push(PaneSurfaceSplit {
            direction: PaneSurfaceSplitDirection::Horizontal,
            pos: 40,
            area: SurfaceRect {
                x: 0,
                y: 0,
                width: 80,
                height: 19,
            },
            hit_rect: SurfaceRect {
                x: 40,
                y: 0,
                width: 1,
                height: 19,
            },
            path: Vec::new(),
        });
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
        state.set_snapshot(Box::new(projected));
        state.set_pane_surface(pane_surface);
        state.compose(106, 20).expect("mouse-disabled shell");
        assert!(state.hits.pane_splits.is_empty());
        let first_tab = state.hits.tabs[0].0;
        let wheel =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: first_tab.x,
                row: first_tab.y,
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(matches!(
            &wheel.actions[..],
            [ClientShellAction::Endpoint { request, .. }]
                if matches!(
                    &request.method,
                    crate::api::schema::Method::TabFocus(target) if target.tab_id == "tab_2"
                )
        ));
    }

    #[test]
    fn pane_mouse_input_keeps_stable_target_and_endpoint_encoding() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot()));
        let mut pane_surface = surface();
        pane_surface.panes[0].mouse_reporting = true;
        state.set_pane_surface(pane_surface);
        state.compose(106, 20).expect("composed frame");
        let pane = state.hits.panes[0].clone();

        let click =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: pane.inner_rect.x + 2,
                row: pane.inner_rect.y + 1,
                modifiers: KeyModifiers::ALT,
            })]);
        let [ClientMessage::ClientShellPaneInput { pane_id, events }] = &click.requests[..] else {
            panic!("pane application click should use targeted canonical input");
        };
        assert_eq!(pane_id, "pane_1");
        assert!(matches!(
            &events[..],
            [ClientPaneInputEvent::Mouse {
                kind: crate::protocol::ClientMouseKind::Down(
                    crate::protocol::ClientMouseButton::Left
                ),
                position: ClientMousePosition::Cell { column: 2, row: 1 },
                modifiers,
                ..
            }] if *modifiers == KeyModifiers::ALT.bits()
        ));
        let moved =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Moved,
                column: 0,
                row: 0,
                modifiers: KeyModifiers::ALT,
            })]);
        assert!(moved.requests.is_empty());
        assert!(state.pane_mouse_gesture.is_some());
        state.hits.panes.clear();
        let release =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: 0,
                row: 0,
                modifiers: KeyModifiers::ALT,
            })]);
        assert!(matches!(
            &release.requests[..],
            [ClientMessage::ClientShellPaneInput { pane_id, events }]
                if pane_id == "pane_1"
                    && matches!(
                        &events[..],
                        [ClientPaneInputEvent::Mouse {
                            kind: crate::protocol::ClientMouseKind::Up(
                                crate::protocol::ClientMouseButton::Left
                            ),
                            ..
                        }]
                    )
        ));
        assert!(state.pane_mouse_gesture.is_none());
    }

    #[test]
    fn pane_pixel_mouse_preserves_pane_relative_pixel_coordinates() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot()));
        let mut pane_surface = surface();
        pane_surface.panes[0].mouse_reporting = true;
        pane_surface.panes[0].sgr_pixel_mouse = true;
        pane_surface.panes[0].pixel_width = 39;
        pane_surface.panes[0].pixel_height = 38;
        state.set_pane_surface(pane_surface);
        state.compose(106, 20).expect("composed frame");
        let pane = state.hits.panes[0].clone();
        let geometry =
            crate::input::mouse::HostGeometry::new(106, 20, 1060, 400).expect("host geometry");
        let x = u32::from(pane.inner_rect.x) * 10 + 21;
        let y = u32::from(pane.inner_rect.y) * 20 + 21;
        let report = format!("\x1b[<0;{x};{y}M");

        let outcome = state.handle_pixel_mouse(report.as_bytes(), geometry);
        assert!(matches!(
            &outcome.requests[..],
            [ClientMessage::ClientShellPaneInput { pane_id, events }]
                if pane_id == "pane_1"
                    && matches!(
                        &events[..],
                        [ClientPaneInputEvent::Mouse {
                            kind: crate::protocol::ClientMouseKind::Down(
                                crate::protocol::ClientMouseButton::Left
                            ),
                            position: ClientMousePosition::Pixels { x: 20, y: 20, .. },
                            ..
                        }]
                    )
        ));
    }

    #[test]
    fn pixel_host_reports_use_cells_without_target_pixel_mode_and_release_outside() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot()));
        let mut pane_surface = surface();
        pane_surface.panes[0].mouse_reporting = true;
        state.set_pane_surface(pane_surface);
        state.compose(106, 20).expect("composed frame");
        let pane = state.hits.panes[0].clone();
        let geometry =
            crate::input::mouse::HostGeometry::new(106, 20, 1060, 400).expect("host geometry");
        let x = u32::from(pane.inner_rect.x) * 10 + 21;
        let y = u32::from(pane.inner_rect.y) * 20 + 21;

        let down = state.handle_pixel_mouse(format!("\x1b[<0;{x};{y}M").as_bytes(), geometry);
        assert!(matches!(
            &down.requests[..],
            [ClientMessage::ClientShellPaneInput { events, .. }]
                if matches!(
                    &events[..],
                    [ClientPaneInputEvent::Mouse {
                        position: ClientMousePosition::Cell { column: 2, row: 1 },
                        ..
                    }]
                )
        ));

        state.hits.panes.clear();
        let release = state.handle_pixel_mouse(b"\x1b[<0;1;1m", geometry);
        assert!(matches!(
            &release.requests[..],
            [ClientMessage::ClientShellPaneInput { pane_id, events }]
                if pane_id == "pane_1"
                    && matches!(
                        &events[..],
                        [ClientPaneInputEvent::Mouse {
                            kind: crate::protocol::ClientMouseKind::Up(
                                crate::protocol::ClientMouseButton::Left
                            ),
                            position: ClientMousePosition::Cell { .. },
                            ..
                        }]
                    )
        ));
        assert!(state.pane_mouse_gesture.is_none());
    }

    #[test]
    fn pane_owned_right_click_forwards_the_complete_gesture() {
        let mut snapshot = snapshot();
        snapshot.panes[0].right_click_passthrough = true;
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot));
        let mut pane_surface = surface();
        pane_surface.panes[0].mouse_reporting = true;
        state.set_pane_surface(pane_surface);
        state.compose(106, 20).expect("composed frame");
        let pane = state.hits.panes[0].clone();

        let down =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Right),
                column: pane.inner_rect.x + 1,
                row: pane.inner_rect.y,
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(matches!(
            &down.requests[..],
            [ClientMessage::ClientShellPaneInput { pane_id, .. }] if pane_id == "pane_1"
        ));
        assert!(state.overlay.is_none());
        assert!(state.pane_mouse_gesture.is_some());

        let up =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Right),
                column: 0,
                row: 0,
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(matches!(
            &up.requests[..],
            [ClientMessage::ClientShellPaneInput { pane_id, events }]
                if pane_id == "pane_1"
                    && matches!(
                        &events[..],
                        [ClientPaneInputEvent::Mouse {
                            kind: crate::protocol::ClientMouseKind::Up(
                                crate::protocol::ClientMouseButton::Right
                            ),
                            ..
                        }]
                    )
        ));
        assert!(state.pane_mouse_gesture.is_none());
    }

    #[test]
    fn shell_new_controls_use_the_same_client_action_routes_as_keybinds() {
        let mut config = Config::default();
        config.ui.prompt_new_workspace_name = false;
        config.ui.prompt_new_tab_name = true;
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());
        state.compose(106, 20).expect("composed frame");

        let new_workspace = state.hits.new_workspace;
        let create_workspace =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: new_workspace.x + 1,
                row: new_workspace.y,
                modifiers: KeyModifiers::empty(),
            })]);
        let [ClientShellAction::Endpoint { request, .. }] = &create_workspace.actions[..] else {
            panic!("new workspace click should use the endpoint API");
        };
        assert!(matches!(
            request.method,
            crate::api::schema::Method::WorkspaceCreate(_)
        ));

        let new_tab = state.hits.new_tab;
        let open_new_tab =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: new_tab.x + 1,
                row: new_tab.y,
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(open_new_tab.actions.is_empty());
        assert!(matches!(
            state.overlay,
            Some(ClientShellOverlay::Rename(ClientRenameOverlay {
                target: ClientRenameTarget::NewTab { .. },
                ..
            }))
        ));
    }

    #[test]
    fn tab_overflow_controls_scroll_the_client_owned_tab_bar() {
        let mut snapshot = snapshot();
        snapshot.tabs.extend((2..=8).map(|number| ClientShellTab {
            tab_id: format!("tab_{number}"),
            workspace_id: "ws_1".into(),
            number,
            label: number.to_string(),
            custom_label: false,
            zoomed: false,
            focused: false,
            agent_status: AgentStatus::Idle,
        }));
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot));
        state.set_pane_surface(surface());
        state.compose(80, 20).expect("overflow tab bar");

        assert!(state.hits.tab_scroll_right.width > 0);
        let scroll_right = state.hits.tab_scroll_right;
        let outcome =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: scroll_right.x + 1,
                row: scroll_right.y,
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(outcome.repaint);
        assert_eq!(state.tab_scroll, 1);

        let mut update = state.snapshot.as_deref().expect("snapshot").clone();
        update.focused_tab_id = Some("tab_8".into());
        for tab in &mut update.tabs {
            tab.focused = tab.tab_id == "tab_8";
        }
        state.set_snapshot(Box::new(update));
        state.compose(80, 20).expect("focused overflow tab");
        assert!(state.hits.tabs.iter().any(|(_, tab_id)| tab_id == "tab_8"));

        state.compose(300, 20).expect("tabs without overflow");
        assert_eq!(state.tab_scroll, 0);
        assert_eq!(state.hits.tabs.len(), 8);
        state.compose(80, 20).expect("focused tab after narrowing");
        assert!(state.hits.tabs.iter().any(|(_, tab_id)| tab_id == "tab_8"));
    }

    #[test]
    fn client_owned_sidebar_dividers_resize_live() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());
        state.compose(106, 30).expect("expanded sidebar");
        let workspace_body = state.hits.workspace_body;
        let needless_scroll =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: workspace_body.x,
                row: workspace_body.y,
                modifiers: KeyModifiers::empty(),
            })]);
        assert_eq!(state.hits.workspace_max_scroll, 0);
        assert_eq!(state.workspace_scroll, 0);
        assert!(!needless_scroll.repaint);
        let width_divider = state.hits.sidebar_divider;
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: width_divider.x,
            row: width_divider.y + 2,
            modifiers: KeyModifiers::empty(),
        })]);
        let resize =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: 31,
                row: width_divider.y + 2,
                modifiers: KeyModifiers::empty(),
            })]);
        assert_eq!(state.sidebar_width, 32);
        assert!(state.sidebar_width_manual);
        assert!(resize.repaint);
        assert!(resize.resize);
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 31,
            row: width_divider.y + 2,
            modifiers: KeyModifiers::empty(),
        })]);

        state.set_pane_surface(surface());
        state.compose(106, 30).expect("resized sidebar");
        let section_divider = state.hits.sidebar_section_divider;
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: section_divider.x + 2,
            row: section_divider.y,
            modifiers: KeyModifiers::empty(),
        })]);
        let split =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: section_divider.x + 2,
                row: 20,
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(state.sidebar_section_split > 0.6);
        assert!(split.repaint);
        assert!(!split.resize);
    }

    #[test]
    fn manual_client_chrome_preferences_round_trip_per_endpoint() {
        let path = std::env::temp_dir().join(format!(
            "herdr-client-shell-prefs-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let config =
            ClientShellConfig::from_config(&Config::default()).with_preferences_path(path.clone());
        let mut state = ClientShellState::new(config);
        state.sidebar_width = 31;
        state.sidebar_width_manual = true;
        state.sidebar_section_split = 0.7;
        state.sidebar_section_split_manual = true;
        state.sidebar_collapsed = true;
        state.sidebar_collapsed_manual = true;
        state.persist_chrome_preferences(&mut ClientShellInput::default());

        let reloaded_config =
            ClientShellConfig::from_config(&Config::default()).with_preferences_path(path.clone());
        let reloaded = ClientShellState::new(reloaded_config);
        assert_eq!(reloaded.sidebar_width, 31);
        assert!(reloaded.sidebar_width_manual);
        assert_eq!(reloaded.sidebar_section_split, 0.7);
        assert!(reloaded.sidebar_section_split_manual);
        assert!(reloaded.sidebar_collapsed);
        assert!(reloaded.sidebar_collapsed_manual);
        std::fs::remove_file(path).expect("remove client chrome preferences");
    }

    #[test]
    fn tab_click_waits_for_release_and_drag_reorders_by_stable_id() {
        let mut projected = snapshot();
        for index in 2..=3 {
            let mut tab = projected.tabs[0].clone();
            tab.tab_id = format!("tab_{index}");
            tab.number = index;
            tab.label = index.to_string();
            tab.focused = false;
            projected.tabs.push(tab);
        }
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(projected));
        state.set_pane_surface(surface());
        state.compose(106, 20).expect("three tabs");
        let first = state.hits.tabs[0].0;
        let third = state.hits.tabs[2].0;

        let down =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: first.x + 1,
                row: first.y,
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(down.actions.is_empty());
        let drag =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: third.right().saturating_sub(1),
                row: third.y,
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(drag.repaint);
        assert!(matches!(
            state.chrome_drag,
            Some(ClientChromeDrag::Tab {
                ref tab_id,
                insert_index: Some(3),
                ..
            }) if tab_id == "tab_1"
        ));
        let frame = state.compose(106, 20).expect("tab drop indicator");
        assert!(frame
            .cells
            .iter()
            .take(frame.width as usize)
            .any(|cell| cell.symbol == "│"));

        let release =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: third.right().saturating_sub(1),
                row: third.y,
                modifiers: KeyModifiers::empty(),
            })]);
        let [ClientShellAction::Endpoint { request, .. }] = &release.actions[..] else {
            panic!("tab drag should use endpoint API");
        };
        assert!(matches!(
            &request.method,
            crate::api::schema::Method::TabMove(params)
                if params.tab_id == "tab_1" && params.insert_index == 3
        ));

        state.compose(106, 20).expect("tabs after drag");
        let second = state.hits.tabs[1].0;
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: second.x + 1,
            row: second.y,
            modifiers: KeyModifiers::empty(),
        })]);
        let click =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: second.x + 1,
                row: second.y,
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(matches!(
            &click.actions[0],
            ClientShellAction::Endpoint { request, .. }
                if matches!(&request.method, crate::api::schema::Method::TabFocus(target) if target.tab_id == "tab_2")
        ));
    }

    #[test]
    fn tab_drag_clears_its_drop_target_after_leaving_the_tab_row() {
        let mut projected = snapshot();
        for index in 2..=3 {
            let mut tab = projected.tabs[0].clone();
            tab.tab_id = format!("tab_{index}");
            tab.number = index;
            tab.label = index.to_string();
            tab.focused = false;
            projected.tabs.push(tab);
        }
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(projected));
        state.set_pane_surface(surface());
        state.compose(106, 20).expect("three tabs");
        let first = state.hits.tabs[0].0;
        let third = state.hits.tabs[2].0;
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: first.x + 1,
            row: first.y,
            modifiers: KeyModifiers::empty(),
        })]);
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: third.x,
            row: third.y,
            modifiers: KeyModifiers::empty(),
        })]);
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: third.x,
            row: third.y + 1,
            modifiers: KeyModifiers::empty(),
        })]);
        assert!(matches!(
            state.chrome_drag,
            Some(ClientChromeDrag::Tab {
                insert_index: None,
                ..
            })
        ));
        let release =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: third.x,
                row: third.y + 1,
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(release.actions.is_empty());
    }

    #[test]
    fn tab_wheel_switches_tabs_without_changing_overflow_scroll() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());
        state.compose(106, 20).expect("tab bar");
        let tab = state.hits.tabs[0].0;

        let outcome =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: tab.x,
                row: tab.y,
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(matches!(
            &outcome.actions[..],
            [ClientShellAction::Endpoint { request, .. }]
                if matches!(
                    &request.method,
                    crate::api::schema::Method::TabFocus(target) if target.tab_id == "tab_1"
                )
        ));
        assert_eq!(state.tab_scroll, 0);
        state.compose(106, 20).expect("tab bar after wheel");
        assert!(state.hits.tabs.iter().any(|(_, tab_id)| tab_id == "tab_1"));
    }

    #[test]
    fn collapsed_workspace_jitter_remains_a_click() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.sidebar_collapsed = true;
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());
        state.compose(106, 20).expect("collapsed sidebar");
        let workspace = state.hits.workspaces[0].rect;
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: workspace.x,
            row: workspace.y,
            modifiers: KeyModifiers::empty(),
        })]);
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: workspace.x + 1,
            row: workspace.y,
            modifiers: KeyModifiers::empty(),
        })]);
        assert!(state.chrome_drag.is_none());
        assert!(state.workspace_press.is_some());
        let release =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: workspace.x + 1,
                row: workspace.y,
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(matches!(
            &release.actions[..],
            [ClientShellAction::Endpoint { request, .. }]
                if matches!(
                    &request.method,
                    crate::api::schema::Method::WorkspaceFocus(target)
                        if target.workspace_id == "ws_1"
                )
        ));
    }

    #[test]
    fn sidebar_scrollbars_use_proportional_shared_geometry_and_drag() {
        let mut projected = snapshot();
        for index in 2..=10 {
            let mut workspace = projected.workspaces[0].clone();
            workspace.workspace_id = format!("ws_{index}");
            workspace.number = index;
            workspace.label = format!("workspace-{index}");
            workspace.focused = false;
            projected.workspaces.push(workspace);
        }
        for index in 1..=10 {
            projected.agents.push(crate::protocol::ClientShellAgent {
                pane_id: format!("agent-pane-{index}"),
                workspace_id: "ws_1".into(),
                tab_id: "tab_1".into(),
                name: Some(format!("agent-{index}")),
                display_agent: None,
                agent: Some("codex".into()),
                title: None,
                terminal_title: None,
                terminal_title_stripped: None,
                agent_status: AgentStatus::Idle,
                state_change_seq: index,
                state_labels: Vec::new(),
                tokens: Vec::new(),
                focused: false,
            });
        }
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(projected));
        state.set_pane_surface(surface());
        state.compose(106, 20).expect("overflowing sidebars");

        for agent in [false, true] {
            let (track, metrics) = if agent {
                (
                    state.hits.agent_scrollbar,
                    state.hits.agent_scroll_metrics.expect("agent metrics"),
                )
            } else {
                (
                    state.hits.workspace_scrollbar,
                    state
                        .hits
                        .workspace_scroll_metrics
                        .expect("workspace metrics"),
                )
            };
            assert!(track.width > 0);
            let thumb = crate::ui::scrollbar_thumb(metrics, track).expect("scrollbar thumb");
            assert!(thumb.len > 1);
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: track.x,
                row: thumb.top,
                modifiers: KeyModifiers::empty(),
            })]);
            let dragged =
                state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                    kind: MouseEventKind::Drag(MouseButton::Left),
                    column: track.x,
                    row: track.bottom().saturating_sub(1),
                    modifiers: KeyModifiers::empty(),
                })]);
            assert!(dragged.repaint);
            if agent {
                assert_eq!(state.agent_scroll, metrics.max_offset_from_bottom);
            } else {
                assert_eq!(state.workspace_scroll, metrics.max_offset_from_bottom);
            }
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: track.x,
                row: track.bottom().saturating_sub(1),
                modifiers: KeyModifiers::empty(),
            })]);
        }
    }

    #[test]
    fn tab_bar_renders_endpoint_status_ellipses_and_clamps_to_useful_scroll() {
        let mut projected = snapshot();
        projected.tab_bar_right = vec![
            crate::protocol::ClientShellTabStatusSegment {
                text: "ZOOM".into(),
                accent: true,
            },
            crate::protocol::ClientShellTabStatusSegment {
                text: "host".into(),
                accent: false,
            },
        ];
        projected.tab_bar_right_separator = " · ".into();
        for number in 2..=8 {
            projected.tabs.push(ClientShellTab {
                tab_id: format!("tab_{number}"),
                workspace_id: "ws_1".into(),
                number,
                label: number.to_string(),
                custom_label: false,
                zoomed: false,
                focused: false,
                agent_status: AgentStatus::Idle,
            });
        }
        let mut config = ClientShellConfig::from_config(&Config::default());
        config.mobile_width_threshold = 0;
        let mut state = ClientShellState::new(config);
        state.set_snapshot(Box::new(projected));
        state.set_pane_surface(surface());
        let frame = state.compose(106, 20).expect("status and overflow tabs");
        let top = frame.cells[..frame.width as usize]
            .iter()
            .map(|cell| cell.symbol.as_str())
            .collect::<String>();
        assert!(top.contains("ZOOM · host"));
        assert!(top.contains('…'));

        state.tab_scroll = usize::MAX;
        state.reveal_focused_tab = false;
        state.compose(106, 20).expect("clamped tab scroll");
        assert!(state.tab_scroll < 7);
        let manual_scroll = state.tab_scroll;
        let mut replacement = (**state.snapshot.as_ref().expect("snapshot")).clone();
        replacement.revision = 2;
        replacement.tab_bar_right[1].text = "tick".into();
        let mut replacement_surface = surface();
        replacement_surface.projection_revision = 2;
        state.set_snapshot(Box::new(replacement));
        state.set_pane_surface(replacement_surface);
        assert!(!state.reveal_focused_tab);
        state.compose(106, 20).expect("same-width status update");
        assert_eq!(state.tab_scroll, manual_scroll);

        state.compose(45, 20).expect("narrow tabs win over status");
        let narrow = state.compose(45, 20).expect("narrow tab frame");
        let top = narrow.cells[..narrow.width as usize]
            .iter()
            .map(|cell| cell.symbol.as_str())
            .collect::<String>();
        assert!(!top.contains("ZOOM · host"));
    }

    #[test]
    fn context_menus_capture_stable_targets_and_route_actions() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());
        state.compose(106, 20).expect("composed frame");

        let workspace = state.hits.workspaces[0].rect;
        let open_workspace_menu =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Right),
                column: workspace.x + 2,
                row: workspace.y,
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(open_workspace_menu.actions.is_empty());
        assert!(matches!(
            state.overlay,
            Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
                target: ClientContextMenuTarget::Workspace { ref workspace_id, .. },
                ..
            })) if workspace_id == "ws_1"
        ));
        let workspace_items = match state.overlay.as_ref() {
            Some(ClientShellOverlay::ContextMenu(menu)) => menu.items(),
            _ => panic!("workspace context menu"),
        };
        assert!(workspace_items
            .iter()
            .any(|item| item.action == ClientContextMenuAction::NewWorktree));
        state.compose(106, 20).expect("workspace context menu");
        let rename = state.hits.context_menu_rows[0].0;
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rename.x + 1,
            row: rename.y,
            modifiers: KeyModifiers::empty(),
        })]);
        assert!(matches!(
            state.overlay,
            Some(ClientShellOverlay::Rename(ClientRenameOverlay {
                target: ClientRenameTarget::Workspace { ref workspace_id },
                ..
            })) if workspace_id == "ws_1"
        ));

        state.overlay = None;
        state.compose(106, 20).expect("composed frame");
        let pane = state.hits.panes[0].rect;
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: pane.x + 1,
            row: pane.y,
            modifiers: KeyModifiers::empty(),
        })]);
        state.compose(106, 20).expect("pane context menu");
        let split_index = match state.overlay.as_ref() {
            Some(ClientShellOverlay::ContextMenu(menu)) => menu
                .items()
                .iter()
                .position(|item| item.action == ClientContextMenuAction::SplitRight)
                .expect("split right item"),
            _ => panic!("pane context menu"),
        };
        let split = state.hits.context_menu_rows[split_index].0;
        let outcome =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: split.x + 1,
                row: split.y,
                modifiers: KeyModifiers::empty(),
            })]);
        let [ClientShellAction::Endpoint { request, .. }] = &outcome.actions[..] else {
            panic!("pane split context action should use endpoint API");
        };
        assert!(matches!(
            &request.method,
            crate::api::schema::Method::PaneSplit(params)
                if params.target_pane_id.as_deref() == Some("pane_1")
                    && params.direction == crate::api::schema::SplitDirection::Right
        ));
    }

    #[test]
    fn context_menu_keyboard_and_outside_click_are_client_owned() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());
        state.compose(106, 20).expect("composed frame");
        let tab = state.hits.tabs[0].0;
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: tab.x + 1,
            row: tab.y,
            modifiers: KeyModifiers::empty(),
        })]);
        state.compose(106, 20).expect("tab context menu");
        let moved = state.handle_input_bytes(b"\x1b[B");
        assert!(moved.repaint);
        assert!(matches!(
            state.overlay,
            Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
                highlighted: 1,
                ..
            }))
        ));
        let text = state.handle_raw_events(vec![RawInputEvent::Text(
            crate::input::TextCommit::new("not pane input"),
        )]);
        assert!(text.requests.is_empty());
        let paste = state.handle_raw_events(vec![RawInputEvent::Paste("not pane input".into())]);
        assert!(paste.requests.is_empty());
        let outside =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 105,
                row: 19,
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(outside.repaint);
        assert!(state.overlay.is_none());
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

        let mut replacement = (**state.snapshot.as_ref().expect("snapshot")).clone();
        replacement.revision = 2;
        replacement.workspaces[1].agent_status = AgentStatus::Blocked;
        let mut replacement_surface = surface();
        replacement_surface.projection_revision = 2;
        state.collapsed_groups.insert("repo".into());
        state.set_snapshot(Box::new(replacement));
        state.set_pane_surface(replacement_surface);
        let collapsed = state.compose(106, 20).expect("collapsed worktree group");
        let parent = state.hits.workspaces[0].rect;
        let status_cell = usize::from(parent.y) * usize::from(collapsed.width)
            + usize::from(parent.x.saturating_add(1));
        assert_eq!(
            collapsed.cells[status_cell].fg,
            crate::protocol::color_to_u32(state.config.palette.red)
        );
    }

    #[test]
    fn workspace_click_waits_for_release_and_drag_reorders_by_stable_id() {
        let mut projected = snapshot();
        for index in 2..=3 {
            let mut workspace = projected.workspaces[0].clone();
            workspace.workspace_id = format!("ws_{index}");
            workspace.number = index;
            workspace.label = format!("workspace-{index}");
            workspace.focused = false;
            projected.workspaces.push(workspace);
        }
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(projected));
        state.set_pane_surface(surface());
        state.compose(106, 24).expect("three workspaces");
        let first = state.hits.workspaces[0].rect;
        let third = state.hits.workspaces[2].rect;

        let down =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: first.x + 2,
                row: first.y,
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(down.actions.is_empty());
        let drag =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: third.x + 2,
                row: third.bottom(),
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(drag.repaint);
        assert!(matches!(
            state.chrome_drag,
            Some(ClientChromeDrag::Workspace {
                ref source_workspace_id,
                target: Some((None, _)),
            }) if source_workspace_id == "ws_1"
        ));
        let frame = state.compose(106, 24).expect("workspace drop indicator");
        assert!(frame
            .cells
            .chunks(frame.width as usize)
            .any(|row| row.iter().take(20).any(|cell| cell.symbol == "─")));

        let release =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: third.x + 2,
                row: third.bottom(),
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(matches!(
            &release.actions[..],
            [ClientShellAction::Endpoint { request, .. }]
                if matches!(
                    &request.method,
                    crate::api::schema::Method::WorkspaceMove(params)
                        if params.workspace_id == "ws_1" && params.insert_index == 3
                )
        ));

        state.compose(106, 24).expect("workspaces after drag");
        let second = state.hits.workspaces[1].rect;
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: second.x + 2,
            row: second.y,
            modifiers: KeyModifiers::empty(),
        })]);
        let click =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: second.x + 2,
                row: second.y,
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(matches!(
            &click.actions[..],
            [ClientShellAction::Endpoint { request, .. }]
                if matches!(
                    &request.method,
                    crate::api::schema::Method::WorkspaceFocus(target)
                        if target.workspace_id == "ws_2"
                )
        ));
    }

    #[test]
    fn workspace_drag_moves_parent_worktree_as_one_block_and_rejects_child() {
        let mut projected = snapshot();
        projected.workspaces[0].worktree = Some(ClientShellWorktree {
            key: "repo".into(),
            label: "repo".into(),
            is_linked_worktree: false,
        });
        let mut child = projected.workspaces[0].clone();
        child.workspace_id = "ws_child".into();
        child.number = 2;
        child.label = "feature".into();
        child.focused = false;
        child.worktree = Some(ClientShellWorktree {
            key: "repo".into(),
            label: "repo".into(),
            is_linked_worktree: true,
        });
        let mut other = projected.workspaces[0].clone();
        other.workspace_id = "ws_other".into();
        other.number = 3;
        other.label = "other".into();
        other.focused = false;
        other.worktree = None;
        projected.workspaces.extend([child, other]);

        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(projected));
        state.set_pane_surface(surface());
        state.compose(106, 24).expect("worktree workspaces");
        assert!(state.hits.workspaces[1].indented);
        let parent = state.hits.workspaces[0].rect;
        let child = state.hits.workspaces[1].rect;
        let other = state.hits.workspaces[2].rect;

        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: parent.x + 2,
            row: parent.y,
            modifiers: KeyModifiers::empty(),
        })]);
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: other.x + 2,
            row: other.bottom(),
            modifiers: KeyModifiers::empty(),
        })]);
        let moved =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: other.x + 2,
                row: other.bottom(),
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(matches!(
            &moved.actions[..],
            [ClientShellAction::Endpoint { request, .. }]
                if matches!(
                    &request.method,
                    crate::api::schema::Method::WorkspaceMoveBlock(params)
                        if params.workspace_ids == ["ws_1", "ws_child"]
                            && params.before_workspace_id.is_none()
                )
        ));

        state.compose(106, 24).expect("worktree child");
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: child.x + 2,
            row: child.y,
            modifiers: KeyModifiers::empty(),
        })]);
        let dragging_child =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: other.x + 2,
                row: other.bottom(),
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(dragging_child.actions.is_empty());
        assert!(state.chrome_drag.is_none());
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
    fn global_menu_opens_from_sidebar_and_routes_client_actions() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());
        state.compose(106, 30).expect("shell frame");
        let launcher = state.hits.global_launcher;
        assert_ne!(launcher, Rect::default());

        let open =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: launcher.x,
                row: launcher.y,
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(open.repaint);
        let menu = state.compose(106, 30).expect("global menu");
        let text = menu
            .cells
            .chunks(menu.width as usize)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("settings"));
        assert!(text.contains("keybinds"));
        assert!(text.contains("reload config"));
        assert!(text.contains("detach"));

        let keybinds = state.hits.global_menu_rows[1].0;
        let help =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: keybinds.x,
                row: keybinds.y,
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(help.actions.is_empty());
        assert!(matches!(state.overlay, Some(ClientShellOverlay::Help(_))));

        state.overlay = Some(ClientShellOverlay::GlobalMenu(ClientGlobalMenuOverlay {
            highlighted: 3,
        }));
        let detach = state.handle_input_bytes(b"\r");
        assert!(detach.detach);
        assert!(state.overlay.is_none());
    }

    #[test]
    fn client_settings_preview_restore_and_endpoint_integrations_are_owned_by_overlay() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());
        state.overlay = Some(ClientShellOverlay::GlobalMenu(ClientGlobalMenuOverlay {
            highlighted: 0,
        }));
        let open = state.handle_input_bytes(b"\r");
        assert!(open.actions.is_empty());
        assert!(matches!(
            state.overlay,
            Some(ClientShellOverlay::Settings(ClientSettingsOverlay {
                section: ClientSettingsSection::Theme,
                ..
            }))
        ));
        let original_theme = state.config.theme_name.clone();
        let original_palette = state.config.palette.clone();
        state.handle_input_bytes(b"j");
        assert_ne!(state.config.theme_name, original_theme);
        assert_ne!(state.config.palette.accent, original_palette.accent);
        state.handle_input_bytes(b"\x1b");
        assert!(state.overlay.is_none());
        assert_eq!(state.config.theme_name, original_theme);
        assert_eq!(state.config.palette.accent, original_palette.accent);

        state.open_settings_overlay();
        state.handle_input_bytes(b"j");
        state.handle_input_bytes(b"\t");
        state
            .compose(106, 30)
            .expect("settings outside-click geometry");
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::empty(),
        })]);
        assert!(state.overlay.is_none());
        assert_eq!(state.config.theme_name, original_theme);
        assert_eq!(state.config.palette.accent, original_palette.accent);

        state.open_settings_overlay();
        state.compose(106, 30).expect("settings overlay");
        for _ in 0..3 {
            let next = state.handle_input_bytes(b"\t");
            assert!(next.actions.is_empty());
        }
        let integrations = state.handle_input_bytes(b"\t");
        let [ClientShellAction::Endpoint { request, .. }] = &integrations.actions[..] else {
            panic!("integration section should request endpoint status");
        };
        assert!(matches!(
            request.method,
            crate::api::schema::Method::IntegrationList(_)
        ));
        let request_id = request.id.clone();
        assert!(
            state
                .handle_endpoint_result(
                    "boot-1",
                    &request_id,
                    Ok(crate::api::schema::ResponseResult::IntegrationList {
                        integrations: vec![
                            crate::api::schema::IntegrationInfo {
                                target: crate::api::schema::IntegrationTarget::Codex,
                                label: "codex".into(),
                                command: "codex".into(),
                                available: true,
                                state: crate::api::schema::IntegrationState::Outdated,
                            },
                            crate::api::schema::IntegrationInfo {
                                target: crate::api::schema::IntegrationTarget::Claude,
                                label: "claude".into(),
                                command: "claude".into(),
                                available: false,
                                state: crate::api::schema::IntegrationState::NotInstalled,
                            },
                        ],
                    }),
                )
                .0
        );
        let frame = state.compose(106, 30).expect("loaded integrations");
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
        assert!(text.contains("update available"));
        assert!(text.contains("not found"));
        assert!(!text.contains("pane labels"));

        let popup = state.hits.settings_popup;
        let blank_click =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: popup.right().saturating_sub(2),
                row: popup.y + 3,
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(!blank_click.repaint);
        assert!(matches!(
            state.overlay,
            Some(ClientShellOverlay::Settings(_))
        ));

        let install = state.handle_input_bytes(b"\r");
        assert_eq!(install.actions.len(), 1);
        assert!(matches!(
            &install.actions[0],
            ClientShellAction::Endpoint { request, .. }
                if matches!(
                    request.method,
                    crate::api::schema::Method::IntegrationInstall(
                        crate::api::schema::IntegrationInstallParams {
                            target: crate::api::schema::IntegrationTarget::Codex
                        }
                    )
                )
        ));
        let escape = state.handle_input_bytes(b"\x1b");
        assert!(!escape.repaint);
        assert!(matches!(
            state.overlay,
            Some(ClientShellOverlay::Settings(_))
        ));
        let install_request_id = match &install.actions[0] {
            ClientShellAction::Endpoint { request, .. } => request.id.clone(),
            _ => unreachable!("integration install action"),
        };
        let (repaint, refresh_actions) = state.handle_endpoint_result(
            "boot-1",
            &install_request_id,
            Ok(crate::api::schema::ResponseResult::IntegrationInstall {
                target: crate::api::schema::IntegrationTarget::Codex,
                details: crate::api::schema::IntegrationInstallResult {
                    messages: vec!["installed codex".into()],
                },
            }),
        );
        assert!(repaint);
        assert!(matches!(
            refresh_actions.as_slice(),
            [ClientShellAction::Endpoint { request, .. }]
                if matches!(request.method, crate::api::schema::Method::IntegrationList(_))
        ));
        assert!(matches!(
            state.overlay,
            Some(ClientShellOverlay::Settings(ClientSettingsOverlay {
                loading_integrations: true,
                installing_integrations: false,
                ref integration_messages,
                ..
            })) if integration_messages == &["installed codex"]
        ));
    }

    #[test]
    fn custom_binding_invokes_only_the_endpoint_manifest_id() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        let binding = crate::config::CustomCommandKeybind {
            bindings: crate::config::ActionKeybinds::prefix("z"),
            label: "prefix+z".into(),
            command: "secret-command --token hidden".into(),
            action: crate::config::CustomCommandAction::Shell,
            description: None,
            width: None,
            height: None,
        };
        let mut projection = snapshot();
        projection
            .commands
            .push(crate::protocol::ClientShellCommand {
                command_id: "cmd_0123456789abcdef0123456789abcdef".into(),
                binding_label: binding.label.clone(),
                action: crate::protocol::ClientShellCommandAction::Shell,
            });
        state.set_snapshot(Box::new(projection));

        let mut outcome = ClientShellInput::default();
        state.record_binding(crate::input::KeybindMatch::Command(binding), &mut outcome);

        let [ClientShellAction::Endpoint { request, .. }] = &outcome.actions[..] else {
            panic!("expected endpoint command invocation");
        };
        let crate::api::schema::Method::CommandInvoke(params) = &request.method else {
            panic!("expected command.invoke");
        };
        assert_eq!(params.command_id, "cmd_0123456789abcdef0123456789abcdef");
        assert_eq!(params.workspace_id.as_deref(), Some("ws_1"));
        assert_eq!(params.tab_id.as_deref(), Some("tab_1"));
        assert_eq!(params.pane_id.as_deref(), Some("pane_1"));
        assert!(!serde_json::to_string(request)
            .unwrap()
            .contains("secret-command"));
    }

    #[test]
    fn custom_binding_missing_from_endpoint_manifest_is_not_forwarded() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot()));
        let binding = crate::config::CustomCommandKeybind {
            bindings: crate::config::ActionKeybinds::prefix("z"),
            label: "prefix+z".into(),
            command: "secret-command".into(),
            action: crate::config::CustomCommandAction::Shell,
            description: None,
            width: None,
            height: None,
        };

        let mut outcome = ClientShellInput::default();
        state.record_binding(crate::input::KeybindMatch::Command(binding), &mut outcome);

        assert!(outcome.actions.is_empty());
        assert!(outcome.repaint);
        assert!(state
            .endpoint_error
            .as_deref()
            .is_some_and(|error| error.contains("not available")));
    }

    #[test]
    fn help_overlay_restores_released_search_scroll_and_custom_binding_behavior() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state
            .config
            .keybinds
            .keybinds
            .custom_commands
            .push(crate::config::CustomCommandKeybind {
                bindings: crate::config::ActionKeybinds::prefix("z"),
                label: "prefix+z".into(),
                command: "plugin.action".into(),
                action: crate::config::CustomCommandAction::PluginAction,
                description: Some("run plugin action".into()),
                width: None,
                height: None,
            });
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
        assert!(text.contains("global"));
        assert!(state.hits.help_max_scroll > 0);
        assert_ne!(state.hits.help_scrollbar, Rect::default());

        state.handle_input_bytes(b"/");
        state.handle_input_bytes(b"plugin");
        let custom = state.compose(106, 30).expect("custom help search");
        let text = custom
            .cells
            .chunks(custom.width as usize)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("custom"));
        assert!(text.contains("run plugin action"));
        state.handle_input_bytes(b"\x1b");

        state.handle_input_bytes(b"/");
        state.handle_input_bytes(b"does-not-exist");
        let empty = state.compose(106, 30).expect("empty help search");
        let text = empty
            .cells
            .chunks(empty.width as usize)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("no matching keybinds"));

        state.handle_input_bytes(b"\x1b");
        assert!(matches!(
            state.overlay,
            Some(ClientShellOverlay::Help(ClientHelpOverlay {
                search_focused: false,
                ref query,
                scroll: 0,
            })) if query.is_empty()
        ));
        state.compose(106, 30).expect("restored help");
        state.handle_input_bytes(b"\x1b[F");
        assert!(matches!(
            state.overlay,
            Some(ClientShellOverlay::Help(ClientHelpOverlay { scroll, .. }))
                if scroll == state.hits.help_max_scroll
        ));
        state.handle_input_bytes(b"\x1b[H");
        assert!(matches!(
            state.overlay,
            Some(ClientShellOverlay::Help(ClientHelpOverlay {
                scroll: 0,
                ..
            }))
        ));
        state.handle_input_bytes(b"?");
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
                terminal_title: None,
                terminal_title_stripped: None,
                agent_status: AgentStatus::Idle,
                state_change_seq: 1,
                state_labels: Vec::new(),
                tokens: Vec::new(),
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
                terminal_title: None,
                terminal_title_stripped: None,
                agent_status: AgentStatus::Idle,
                state_change_seq: 2,
                state_labels: Vec::new(),
                tokens: Vec::new(),
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
    fn agent_sidebar_honors_priority_symbols_tokens_and_stable_hits() {
        let mut projected = snapshot();
        let mut second_pane = projected.panes[0].clone();
        second_pane.pane_id = "pane_2".into();
        second_pane.focused = false;
        projected.panes.push(second_pane);
        projected.agents = vec![
            ClientShellAgent {
                pane_id: "pane_1".into(),
                workspace_id: "ws_1".into(),
                tab_id: "tab_1".into(),
                name: Some("pi one".into()),
                display_agent: None,
                agent: Some("pi".into()),
                title: None,
                terminal_title: Some("first title".into()),
                terminal_title_stripped: Some("first".into()),
                agent_status: AgentStatus::Done,
                state_change_seq: 10,
                state_labels: Vec::new(),
                tokens: vec![("summary".into(), "review complete".into())],
                focused: true,
            },
            ClientShellAgent {
                pane_id: "pane_2".into(),
                workspace_id: "ws_1".into(),
                tab_id: "tab_1".into(),
                name: Some("pi two".into()),
                display_agent: None,
                agent: Some("pi".into()),
                title: None,
                terminal_title: Some("second title".into()),
                terminal_title_stripped: Some("second".into()),
                agent_status: AgentStatus::Blocked,
                state_change_seq: 20,
                state_labels: vec![("blocked".into(), "needs input".into())],
                tokens: vec![("summary".into(), "waiting for Can".into())],
                focused: false,
            },
        ];
        let mut config = Config::default();
        config.ui.agent_panel_sort = crate::config::AgentPanelSortConfig::Priority;
        config.ui.status_indicators = crate::config::StatusIndicatorStyle::Symbols;
        config.ui.sidebar.agents.rows = vec![vec![crate::config::AgentSidebarToken::Agent]];
        config.ui.sidebar.agents.rows_by_agent.insert(
            "pi".into(),
            vec![
                vec![
                    crate::config::AgentSidebarToken::StateIcon,
                    crate::config::AgentSidebarToken::StateText,
                ],
                vec![
                    crate::config::AgentSidebarToken::Agent,
                    crate::config::AgentSidebarToken::Custom("summary".into()),
                ],
            ],
        );
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
        state.set_snapshot(Box::new(projected));
        state.set_pane_surface(surface());

        let frame = state.compose(106, 30).expect("agent sidebar frame");
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
        assert!(text.contains("× needs input"), "frame: {text}");
        assert!(text.contains("pi two"), "frame: {text}");
        assert!(text.contains("waiting for"), "frame: {text}");
        assert_eq!(
            state
                .hits
                .agents
                .first()
                .map(|(_, pane_id)| pane_id.as_str()),
            Some("pane_2")
        );

        let first = state.hits.agents[0].0;
        let click =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: first.x,
                row: first.y,
                modifiers: KeyModifiers::empty(),
            })]);
        let [ClientShellAction::Endpoint { request, .. }] = &click.actions[..] else {
            panic!("agent row should focus through endpoint API");
        };
        assert!(matches!(
            &request.method,
            crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_2"
        ));

        state.compose(106, 10).expect("short agent sidebar frame");
        assert_eq!(
            state
                .hits
                .agents
                .first()
                .map(|(_, pane_id)| pane_id.as_str()),
            Some("pane_2")
        );
        let body = state.hits.agent_body;
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: body.x,
            row: body.y,
            modifiers: KeyModifiers::empty(),
        })]);
        state
            .compose(106, 10)
            .expect("scrolled agent sidebar frame");
        assert_eq!(
            state
                .hits
                .agents
                .first()
                .map(|(_, pane_id)| pane_id.as_str()),
            Some("pane_1")
        );

        state.sidebar_collapsed = true;
        let compact = state.compose(106, 30).expect("compact agent sidebar frame");
        let blocked = state
            .hits
            .agents
            .iter()
            .find(|(_, pane_id)| pane_id == "pane_2")
            .expect("blocked compact agent")
            .0;
        let row_start = blocked.y as usize * compact.width as usize + blocked.x as usize;
        assert_ne!(compact.cells[row_start].fg, compact.cells[row_start + 2].fg);
        assert_eq!(compact.cells[row_start].bg, compact.cells[row_start + 2].bg);
    }

    #[test]
    fn active_agent_view_controls_sidebar_order_and_focus_indices() {
        let mut projected = snapshot();
        let mut second_pane = projected.panes[0].clone();
        second_pane.pane_id = "pane_2".into();
        second_pane.focused = false;
        projected.panes.push(second_pane);
        projected.agents = vec![
            ClientShellAgent {
                pane_id: "pane_1".into(),
                workspace_id: "ws_1".into(),
                tab_id: "tab_1".into(),
                name: Some("first".into()),
                display_agent: None,
                agent: Some("pi".into()),
                title: None,
                terminal_title: None,
                terminal_title_stripped: None,
                agent_status: AgentStatus::Idle,
                state_change_seq: 1,
                state_labels: Vec::new(),
                tokens: Vec::new(),
                focused: true,
            },
            ClientShellAgent {
                pane_id: "pane_2".into(),
                workspace_id: "ws_1".into(),
                tab_id: "tab_1".into(),
                name: Some("second".into()),
                display_agent: None,
                agent: Some("pi".into()),
                title: None,
                terminal_title: None,
                terminal_title_stripped: None,
                agent_status: AgentStatus::Blocked,
                state_change_seq: 2,
                state_labels: Vec::new(),
                tokens: Vec::new(),
                focused: false,
            },
        ];
        projected.agent_view_label = Some("review".into());
        projected.agent_order = vec!["pane_2".into()];
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(projected));
        state.set_pane_surface(surface());
        state.compose(106, 30).expect("filtered agent sidebar");
        assert_eq!(
            state
                .hits
                .agents
                .iter()
                .map(|(_, pane_id)| pane_id.as_str())
                .collect::<Vec<_>>(),
            vec!["pane_2"]
        );
        assert_eq!(state.hits.agent_sort_toggle, Rect::default());

        let mut focus = ClientShellInput::default();
        state.record_binding(
            crate::input::KeybindMatch::Action(crate::input::KeybindAction::FocusAgent(0)),
            &mut focus,
        );
        assert!(matches!(
            &focus.actions[..],
            [ClientShellAction::Endpoint { request, .. }]
                if matches!(
                    &request.method,
                    crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_2"
                )
        ));
    }

    #[test]
    fn agent_sort_toggle_is_client_local_and_persists_per_endpoint() {
        let path = std::env::temp_dir().join(format!(
            "herdr-shell-agent-sort-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        let mut projected = snapshot();
        projected.agents.push(ClientShellAgent {
            pane_id: "pane_1".into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            name: Some("pi".into()),
            display_agent: None,
            agent: Some("pi".into()),
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: AgentStatus::Working,
            state_change_seq: 1,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: true,
        });
        let config =
            ClientShellConfig::from_config(&Config::default()).with_preferences_path(path.clone());
        let mut state = ClientShellState::new(config);
        state.set_snapshot(Box::new(projected));
        state.set_pane_surface(surface());
        state.compose(106, 30).expect("agent sidebar frame");
        let toggle = state.hits.agent_sort_toggle;

        let click =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: toggle.x,
                row: toggle.y,
                modifiers: KeyModifiers::empty(),
            })]);

        assert_eq!(
            state.config.agent_panel_sort,
            crate::config::AgentPanelSortConfig::Priority
        );
        assert!(click.actions.is_empty());
        let reloaded_config =
            ClientShellConfig::from_config(&Config::default()).with_preferences_path(path.clone());
        let reloaded = ClientShellState::new(reloaded_config);
        assert_eq!(
            reloaded.config.agent_panel_sort,
            crate::config::AgentPanelSortConfig::Priority
        );
        assert!(reloaded.agent_panel_sort_manual);
        std::fs::remove_file(path).expect("remove agent sort preferences");
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
        let restored = frame.to_ratatui_buffer().expect("overlay frame");
        assert!(!restored
            .cell((26, 7))
            .expect("overlay title cell")
            .modifier
            .contains(Modifier::DIM));
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
        assert!(
            state
                .handle_endpoint_result(
                    "boot-1",
                    &request_id,
                    Err(ClientShellEndpointError {
                        code: Some("confirmation_required".into()),
                        message: "confirmation required".into(),
                    }),
                )
                .0
        );
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

        let invalid = state.handle_input_bytes(b"9");
        assert!(invalid.actions.is_empty());
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

        let modified = state.handle_input_bytes(b"\x1b[1;2D");
        assert!(matches!(
            &modified.actions[..],
            [ClientShellAction::Endpoint { request, .. }]
                if matches!(
                    &request.method,
                    crate::api::schema::Method::PaneResize(params)
                        if params.direction == crate::api::schema::PaneDirection::Left
                )
        ));
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
        assert!(
            state
                .handle_endpoint_result("boot-1", &request_id, Ok(worktree_list_result(None)))
                .0
        );
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
    fn semantic_notifications_use_client_policy_and_stable_navigation_targets() {
        let mut config = ClientShellConfig::from_config(&Config::default());
        config.toast_delivery = crate::config::ToastDelivery::Herdr;
        config.toast_delay_seconds = 0;
        let mut state = ClientShellState::new(config);
        let mut projected = snapshot();
        projected.agents.push(ClientShellAgent {
            pane_id: "pane_2".into(),
            workspace_id: "ws_2".into(),
            tab_id: "tab_2".into(),
            name: None,
            display_agent: Some("codex".into()),
            agent: Some("codex".into()),
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: AgentStatus::Blocked,
            state_change_seq: 1,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: false,
        });
        state.set_snapshot(Box::new(projected));
        state.set_pane_surface(surface());
        let now = std::time::Instant::now();
        let (effects, repaint) = state.receive_notification(
            SemanticNotification {
                kind: SemanticNotificationKind::NeedsAttention,
                title: "codex needs attention".into(),
                body: Some("other · 2".into()),
                sound: Some(SemanticNotificationSound::Request),
                agent: Some("codex".into()),
                workspace_id: Some("ws_2".into()),
                tab_id: Some("tab_2".into()),
                pane_id: Some("pane_2".into()),
                position: None,
            },
            now,
        );
        assert!(repaint);
        assert!(matches!(
            effects.as_slice(),
            [ClientShellNotificationEffect::Sound {
                sound: crate::sound::Sound::Request,
                ..
            }]
        ));
        let frame = state.compose(100, 28).expect("notification frame");
        let rendered = frame
            .cells
            .chunks(frame.width as usize)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("codex needs attention"));
        let hit = state.hits.notification_toast;
        let click = RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: hit.x,
            row: hit.y,
            modifiers: KeyModifiers::empty(),
        });
        let outcome = state.handle_raw_events(vec![click]);
        assert!(outcome.actions.iter().any(|action| matches!(
            action,
            ClientShellAction::Endpoint { request, .. }
                if matches!(
                    &request.method,
                    crate::api::schema::Method::PaneFocus(params)
                        if params.pane_id == "pane_2"
                )
        )));
        assert!(state.visible_notification.is_none());

        state.receive_notification(
            SemanticNotification {
                kind: SemanticNotificationKind::NeedsAttention,
                title: "codex needs attention".into(),
                body: None,
                sound: None,
                agent: Some("codex".into()),
                workspace_id: Some("ws_2".into()),
                tab_id: Some("tab_2".into()),
                pane_id: Some("pane_2".into()),
                position: None,
            },
            now,
        );
        let mut keybind = ClientShellInput::default();
        state.record_binding(
            crate::input::KeybindMatch::Action(crate::input::KeybindAction::OpenNotificationTarget),
            &mut keybind,
        );
        assert!(keybind.actions.iter().any(|action| matches!(
            action,
            ClientShellAction::Endpoint { request, .. }
                if matches!(
                    &request.method,
                    crate::api::schema::Method::PaneFocus(params)
                        if params.pane_id == "pane_2"
                )
        )));
        assert!(state.visible_notification.is_none());

        state.receive_notification(
            SemanticNotification {
                kind: SemanticNotificationKind::NeedsAttention,
                title: "first".into(),
                body: None,
                sound: None,
                agent: Some("codex".into()),
                workspace_id: Some("ws_2".into()),
                tab_id: Some("tab_2".into()),
                pane_id: Some("pane_2".into()),
                position: None,
            },
            now,
        );
        assert!(state.visible_notification.is_some());
        state.config.toast_delay_seconds = 1;
        let (_, repaint) = state.receive_notification(
            SemanticNotification {
                kind: SemanticNotificationKind::NeedsAttention,
                title: "replacement".into(),
                body: None,
                sound: None,
                agent: Some("codex".into()),
                workspace_id: Some("ws_2".into()),
                tab_id: Some("tab_2".into()),
                pane_id: Some("pane_2".into()),
                position: None,
            },
            now,
        );
        assert!(repaint);
        assert!(state.visible_notification.is_none());
        assert_eq!(state.pending_notifications.len(), 1);
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
