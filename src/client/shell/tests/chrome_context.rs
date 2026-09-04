use super::*;

#[test]
fn global_menu_only_adds_server_switching_for_multiple_connections() {
    let projected = snapshot();
    let local = super::super::global_menu::global_menu_items(
        &projected,
        &["local".into()],
        &BTreeMap::from([("local".into(), ClientServerLifecycle::Connected)]),
        "local",
    );
    assert!(local.iter().all(|(label, _)| !label.starts_with("server:")));

    let fleet = super::super::global_menu::global_menu_items(
        &projected,
        &["local".into(), "analytics".into()],
        &BTreeMap::from([
            ("local".into(), ClientServerLifecycle::Connected),
            ("analytics".into(), ClientServerLifecycle::Connected),
        ]),
        "local",
    );
    assert!(fleet.iter().any(|(label, action)| {
        label == "server: ANALYTICS"
            && *action
                == super::super::global_menu::ClientGlobalMenuAction::ActivateServer(
                    "analytics".into(),
                )
    }));
    assert!(fleet.iter().any(|(label, _)| label == "server: LOCAL ✓"));

    let failed = super::super::global_menu::global_menu_items(
        &projected,
        &["local".into(), "analytics".into()],
        &BTreeMap::from([
            ("local".into(), ClientServerLifecycle::Connected),
            (
                "analytics".into(),
                ClientServerLifecycle::Failed("permission denied".into()),
            ),
        ]),
        "local",
    );
    assert!(failed
        .iter()
        .any(|(label, _)| label == "server: ANALYTICS (failed: permission denied)"));
}

#[test]
fn fleet_sidebar_namespaces_duplicate_workspace_ids_and_routes_remote_clicks() {
    let mut local = snapshot();
    local.workspaces[0].label = "local-workspace".into();
    let mut remote = snapshot();
    remote.workspaces[0].label = "remote-workspace".into();
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.configure_servers(vec!["local".into(), "analytics".into()]);
    state.set_server_lifecycle("analytics", ClientServerLifecycle::Connected);
    state.set_snapshot(Box::new(local));
    state.set_server_snapshot("analytics".into(), Box::new(remote));
    state.set_pane_surface(surface());

    let frame = state.compose(100, 28).expect("fleet frame");
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
    assert!(rendered.contains("LOCAL"));
    assert!(rendered.contains("ANALYTICS"));
    assert!(state
        .hits
        .workspaces
        .iter()
        .any(|hit| { hit.server_id == "local" && hit.workspace_id == "ws_1" }));
    state.mode = ClientShellMode::Navigate;
    state.navigate_server_id = Some("local".into());
    state.navigate_workspace_id = Some("ws_1".into());
    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Down,
        KeyModifiers::empty(),
    ))]);
    assert_eq!(state.navigate_server_id.as_deref(), Some("analytics"));
    let keyboard = state.handle_raw_events(vec![RawInputEvent::Key(
        crate::input::TerminalKey::new(KeyCode::Enter, KeyModifiers::empty()),
    )]);
    assert!(matches!(
        keyboard.actions.as_slice(),
        [ClientShellAction::ActivateWorkspace { server_id, workspace_id }]
            if server_id == "analytics" && workspace_id == "ws_1"
    ));

    let remote_hit = state
        .hits
        .workspaces
        .iter()
        .find(|hit| hit.server_id == "analytics" && hit.workspace_id == "ws_1")
        .expect("remote workspace hit")
        .rect;
    let mouse = |kind| {
        RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind,
            column: remote_hit.x,
            row: remote_hit.y,
            modifiers: crossterm::event::KeyModifiers::empty(),
        })
    };
    state.handle_raw_events(vec![mouse(MouseEventKind::Down(MouseButton::Left))]);
    let outcome = state.handle_raw_events(vec![mouse(MouseEventKind::Up(MouseButton::Left))]);
    assert!(matches!(
        outcome.actions.as_slice(),
        [ClientShellAction::ActivateWorkspace { server_id, workspace_id }]
            if server_id == "analytics" && workspace_id == "ws_1"
    ));
}

#[test]
fn fleet_workspace_context_menu_does_not_cross_server_ownership() {
    let mut local = snapshot();
    local.workspaces[0].branch = None;
    let mut remote = snapshot();
    remote.workspaces[0].branch = Some("remote-branch".into());
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.configure_servers(vec!["local".into(), "analytics".into()]);
    state.set_server_lifecycle("analytics", ClientServerLifecycle::Connected);
    state.set_snapshot(Box::new(local));
    state.set_server_snapshot("analytics".into(), Box::new(remote));
    state.set_pane_surface(surface());
    state.compose(100, 28).expect("fleet frame");

    let remote_hit = state
        .hits
        .workspaces
        .iter()
        .find(|hit| hit.server_id == "analytics" && hit.workspace_id == "ws_1")
        .expect("remote workspace hit")
        .rect;
    let outcome =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: remote_hit.x,
            row: remote_hit.y,
            modifiers: crossterm::event::KeyModifiers::empty(),
        })]);

    assert!(outcome.actions.is_empty());
    assert!(state.overlay.is_none());

    state.set_active_server("analytics".into());
    state.set_pane_surface(surface());
    state.compose(100, 28).expect("remote fleet frame");
    let remote_hit = state
        .hits
        .workspaces
        .iter()
        .find(|hit| hit.server_id == "analytics" && hit.workspace_id == "ws_1")
        .expect("active remote workspace hit")
        .rect;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column: remote_hit.x,
        row: remote_hit.y,
        modifiers: crossterm::event::KeyModifiers::empty(),
    })]);
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Workspace {
                ref workspace_id,
                is_git: true,
                ..
            },
            ..
        })) if workspace_id == "ws_1"
    ));
}

fn fleet_state_with_duplicate_remote_resource_ids() -> ClientShellState {
    let mut local = snapshot();
    local.boot_id = "local-boot".into();
    local.tabs[0].label = "local-tab".into();
    local.panes[0].label = Some("local-pane".into());

    let mut remote = snapshot();
    remote.boot_id = "analytics-boot".into();
    remote.tabs[0].label = "remote-tab".into();
    remote.panes[0].label = Some("remote-pane".into());

    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.configure_servers(vec!["local".into(), "analytics".into()]);
    state.set_server_lifecycle("analytics", ClientServerLifecycle::Connected);
    state.set_snapshot(Box::new(local));
    state.set_server_snapshot("analytics".into(), Box::new(remote.clone()));
    state.set_active_server("analytics".into());
    state.set_snapshot(Box::new(remote));
    state
}

fn sole_remote_endpoint_method(outcome: &ClientShellInput) -> &crate::api::schema::Method {
    let [ClientShellAction::Endpoint { boot_id, request }] = outcome.actions.as_slice() else {
        panic!("expected one endpoint request");
    };
    assert_eq!(boot_id, "analytics-boot");
    &request.method
}

#[test]
fn fleet_duplicate_tab_and_pane_ids_follow_the_active_server_for_focus() {
    let mut state = fleet_state_with_duplicate_remote_resource_ids();

    let local = state.server_snapshots["local"].as_ref();
    let remote = state.server_snapshots["analytics"].as_ref();
    assert_eq!(local.tabs[0].tab_id, remote.tabs[0].tab_id);
    assert_eq!(local.panes[0].pane_id, remote.panes[0].pane_id);
    assert_ne!(local.boot_id, remote.boot_id);
    assert_eq!(state.active_server_id, "analytics");

    let mut tab_focus = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::SwitchTab(0)),
        &mut tab_focus,
    );
    assert!(matches!(
        sole_remote_endpoint_method(&tab_focus),
        crate::api::schema::Method::TabFocus(target) if target.tab_id == "tab_1"
    ));

    let pane_focus = state.focus_pane("pane_1".into());
    assert!(matches!(
        sole_remote_endpoint_method(&pane_focus),
        crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_1"
    ));
}

#[test]
fn fleet_active_server_routes_create_split_and_kill_requests() {
    let mut state = fleet_state_with_duplicate_remote_resource_ids();

    let mut create = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::NewWorkspace),
        &mut create,
    );
    assert!(matches!(
        sole_remote_endpoint_method(&create),
        crate::api::schema::Method::WorkspaceCreate(params)
            if params.source_workspace_id.as_deref() == Some("ws_1")
    ));

    let mut split = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::SplitVertical),
        &mut split,
    );
    assert!(matches!(
        sole_remote_endpoint_method(&split),
        crate::api::schema::Method::PaneSplit(params)
            if params.workspace_id.as_deref() == Some("ws_1")
                && params.target_pane_id.as_deref() == Some("pane_1")
    ));

    let mut kill = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::ClosePane),
        &mut kill,
    );
    assert!(matches!(
        sole_remote_endpoint_method(&kill),
        crate::api::schema::Method::PaneClose(target) if target.pane_id == "pane_1"
    ));
}

#[test]
fn workspace_picker_starts_from_the_active_server() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.configure_servers(vec!["local".into(), "analytics".into()]);
    state.set_snapshot(Box::new(snapshot()));
    state.set_active_server("analytics".into());
    let mut outcome = ClientShellInput::default();

    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::WorkspacePicker),
        &mut outcome,
    );

    assert_eq!(state.navigate_server_id.as_deref(), Some("analytics"));
    assert_eq!(state.navigate_workspace_id.as_deref(), Some("ws_1"));
}

#[test]
fn switching_servers_restores_each_servers_last_cell_surface() {
    let mut local = snapshot();
    local.boot_id = "local-boot".into();
    let mut remote = snapshot();
    remote.boot_id = "analytics-boot".into();
    let mut local_surface = surface();
    local_surface.boot_id = local.boot_id.clone();
    local_surface.frame.cells[0].symbol = "L".into();
    let mut remote_surface = surface();
    remote_surface.boot_id = remote.boot_id.clone();
    remote_surface.frame.cells[0].symbol = "R".into();

    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.configure_servers(vec!["local".into(), "analytics".into()]);
    state.set_snapshot(Box::new(local));
    state.set_pane_surface(local_surface);
    state.set_server_snapshot("analytics".into(), Box::new(remote));

    state.set_active_server("analytics".into());
    assert!(state.pane_surface.is_none());
    state.set_pane_surface(remote_surface);
    state.set_active_server("local".into());
    assert_eq!(
        state.pane_surface.as_ref().unwrap().frame.cells[0].symbol,
        "L"
    );
    state.set_active_server("analytics".into());
    assert_eq!(
        state.pane_surface.as_ref().unwrap().frame.cells[0].symbol,
        "R"
    );
    assert!(state
        .pane_surface
        .as_ref()
        .is_some_and(|surface| surface.graphics == Default::default()));
}

#[test]
fn switching_servers_drops_a_cached_surface_from_an_old_boot() {
    let mut remote = snapshot();
    remote.boot_id = "analytics-boot".into();
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.configure_servers(vec!["local".into(), "analytics".into()]);
    state.set_snapshot(Box::new(snapshot()));
    state.set_server_snapshot("analytics".into(), Box::new(remote));
    let mut stale_surface = surface();
    stale_surface.boot_id = "retired-analytics-boot".into();
    state
        .server_surfaces
        .insert("analytics".into(), stale_surface);

    state.set_active_server("analytics".into());

    assert!(state.pane_surface.is_none());
    assert!(!state.server_surfaces.contains_key("analytics"));
}

#[test]
fn changed_remote_identity_forgets_cached_state_before_reconfiguration() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.configure_servers(vec!["local".into(), "analytics".into()]);
    state.set_server_lifecycle("analytics", ClientServerLifecycle::Connected);
    state.set_server_snapshot("analytics".into(), Box::new(snapshot()));
    state.server_surfaces.insert("analytics".into(), surface());

    state.forget_server("analytics");
    state.configure_servers(vec!["local".into(), "analytics".into()]);

    assert!(!state.has_server_snapshot("analytics"));
    assert!(!state.server_surfaces.contains_key("analytics"));
    assert_eq!(
        state.server_lifecycle.get("analytics"),
        Some(&ClientServerLifecycle::Connecting)
    );
}

#[test]
fn fleet_sidebar_preserves_active_worktree_grouping() {
    let mut local = snapshot();
    local.workspaces[0].worktree = Some(ClientShellWorktree {
        key: "repo".into(),
        label: "repo".into(),
        is_linked_worktree: false,
    });
    let mut child = local.workspaces[0].clone();
    child.workspace_id = "ws_child".into();
    child.label = "repo-feature".into();
    child.branch = Some("worktree/feature".into());
    child.focused = false;
    child.worktree = Some(ClientShellWorktree {
        key: "repo".into(),
        label: "repo".into(),
        is_linked_worktree: true,
    });
    local.workspaces.push(child);

    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.configure_servers(vec!["local".into(), "analytics".into()]);
    state.set_server_lifecycle("analytics", ClientServerLifecycle::Connected);
    state.set_snapshot(Box::new(local));
    state.set_server_snapshot("analytics".into(), Box::new(snapshot()));
    state.set_pane_surface(surface());
    let frame = state.compose(106, 24).expect("fleet worktree frame");
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

    let local_hits = state
        .hits
        .workspaces
        .iter()
        .filter(|hit| hit.server_id == "local")
        .collect::<Vec<_>>();
    assert_eq!(local_hits.len(), 2);
    assert!(!local_hits[0].indented);
    assert!(local_hits[0].group_toggle.is_some());
    assert!(local_hits[1].indented);
    assert!(rendered.contains("└─"));
    assert!(rendered.contains("feature"));
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
fn focused_workspace_change_reveals_new_workspace_in_full_sidebar() {
    let mut initial = snapshot();
    let template = initial.workspaces[0].clone();
    initial.workspaces = (1..=12)
        .map(|number| ClientShellWorkspace {
            workspace_id: format!("ws_{number}"),
            number,
            label: format!("space-{number}"),
            branch: None,
            focused: number == 1,
            ..template.clone()
        })
        .collect();

    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(initial));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("full sidebar");
    assert!(state.hits.workspace_max_scroll > 0);
    assert!(state
        .hits
        .workspaces
        .iter()
        .all(|hit| hit.workspace_id != "ws_12"));

    let mut update = state.snapshot.as_deref().expect("snapshot").clone();
    update.revision = 2;
    update.focused_workspace_id = Some("ws_12".into());
    for workspace in &mut update.workspaces {
        workspace.focused = workspace.workspace_id == "ws_12";
    }
    let mut updated_surface = surface();
    updated_surface.projection_revision = 2;
    state.set_snapshot(Box::new(update));
    state.set_pane_surface(updated_surface);
    state.compose(106, 2).expect("zero-height workspace body");
    assert!(state.reveal_focused_workspace);
    state.compose(106, 20).expect("updated full sidebar");

    assert!(state
        .hits
        .workspaces
        .iter()
        .any(|hit| hit.workspace_id == "ws_12"));
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
    let split = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
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
fn global_menu_opens_from_sidebar_and_routes_client_actions() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 30).expect("shell frame");
    let launcher = state.hits.global_launcher;
    assert_ne!(launcher, Rect::default());

    let open = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
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
    let help = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
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
fn close_confirmation_error_becomes_client_owned_overlay_and_stable_group_close() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.configure_servers(vec!["local".into(), "analytics-prod".into()]);
    state.set_server_lifecycle("analytics-prod", ClientServerLifecycle::Connected);
    state.set_active_server("analytics-prod".into());
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
    assert!(text.contains("ANALYTICS-PROD"));

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
