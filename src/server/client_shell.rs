use ratatui::layout::Rect;

use crate::app;
use crate::protocol::{self, FrameData};

pub(super) fn snapshot(
    app: &app::App,
    boot_id: &str,
    revision: u64,
) -> protocol::ClientShellSnapshot {
    let snapshot = app.session_snapshot();
    let workspaces = snapshot
        .workspaces
        .into_iter()
        .zip(&app.state.workspaces)
        .map(|(workspace, state)| {
            let mut tokens = workspace.tokens.into_iter().collect::<Vec<_>>();
            tokens.sort_by(|left, right| left.0.cmp(&right.0));
            protocol::ClientShellWorkspace {
                workspace_id: workspace.workspace_id,
                number: workspace.number,
                label: workspace.label,
                custom_label: state.custom_name.is_some(),
                branch: state.branch(),
                git_ahead_behind: state.git_ahead_behind(),
                tokens,
                worktree: workspace
                    .worktree
                    .map(|worktree| protocol::ClientShellWorktree {
                        key: worktree.repo_key,
                        label: worktree.repo_name,
                        is_linked_worktree: worktree.is_linked_worktree,
                    }),
                focused: workspace.focused,
                agent_status: workspace.agent_status,
            }
        })
        .collect();
    let tabs = snapshot
        .tabs
        .into_iter()
        .zip(
            app.state
                .workspaces
                .iter()
                .flat_map(|workspace| workspace.tabs.iter()),
        )
        .map(|(tab, state)| protocol::ClientShellTab {
            tab_id: tab.tab_id,
            workspace_id: tab.workspace_id,
            number: tab.number,
            label: tab.label,
            custom_label: !state.is_auto_named(),
            zoomed: state.zoomed,
            focused: tab.focused,
            agent_status: tab.agent_status,
        })
        .collect();
    let panes = snapshot
        .panes
        .into_iter()
        .map(|pane| {
            let right_click_passthrough = app
                .parse_pane_id(&pane.pane_id)
                .and_then(|(workspace_index, pane_id)| {
                    app.state
                        .workspaces
                        .get(workspace_index)?
                        .pane_state(pane_id)
                })
                .is_some_and(|pane| pane.right_click_passthrough);
            protocol::ClientShellPane {
                pane_id: pane.pane_id,
                workspace_id: pane.workspace_id,
                tab_id: pane.tab_id,
                label: pane.label,
                cwd: pane.cwd,
                foreground_cwd: pane.foreground_cwd,
                focused: pane.focused,
                right_click_passthrough,
            }
        })
        .collect();
    let agents = snapshot
        .agents
        .into_iter()
        .map(|agent| protocol::ClientShellAgent {
            pane_id: agent.pane_id,
            workspace_id: agent.workspace_id,
            tab_id: agent.tab_id,
            name: agent.name,
            display_agent: agent.display_agent,
            agent: agent.agent,
            title: agent.title,
            agent_status: agent.agent_status,
            focused: agent.focused,
        })
        .collect();

    protocol::ClientShellSnapshot {
        boot_id: boot_id.to_owned(),
        revision,
        focused_workspace_id: snapshot.focused_workspace_id,
        focused_tab_id: snapshot.focused_tab_id,
        focused_pane_id: snapshot.focused_pane_id,
        workspaces,
        tabs,
        panes,
        agents,
    }
}

pub(super) fn render_pane_surface(
    app: &mut app::App,
    area: Rect,
    is_foreground: bool,
    cell_size: crate::kitty_graphics::HostCellSize,
) -> (FrameData, Vec<protocol::PaneSurfacePane>) {
    let (buffer, cursor, hyperlinks, layout) =
        crate::server::render_stream::render_tab_surface_virtual(
            &app.state,
            &app.terminal_runtimes,
            area,
            is_foreground,
            cell_size,
        );
    let panes = app
        .state
        .active
        .map(|workspace_index| {
            layout
                .pane_infos
                .iter()
                .filter_map(|pane| {
                    app.public_pane_id(workspace_index, pane.id).map(|pane_id| {
                        let runtime = app.state.runtime_for_pane_in_workspace(
                            &app.terminal_runtimes,
                            workspace_index,
                            pane.id,
                        );
                        let mouse_reporting =
                            runtime.is_some_and(|runtime| runtime.mouse_reporting_enabled());
                        let sgr_pixel_mouse =
                            runtime.is_some_and(|runtime| runtime.sgr_pixel_mouse_enabled());
                        let (pixel_width, pixel_height) = if cell_size.is_known() {
                            (
                                u32::from(pane.inner_rect.width) * cell_size.width_px,
                                u32::from(pane.inner_rect.height) * cell_size.height_px,
                            )
                        } else {
                            (0, 0)
                        };
                        protocol::PaneSurfacePane {
                            pane_id,
                            rect: pane.rect.into(),
                            inner_rect: pane.inner_rect.into(),
                            scrollbar_rect: pane.scrollbar_rect.map(Into::into),
                            focused: pane.is_focused,
                            mouse_reporting,
                            sgr_pixel_mouse,
                            pixel_width,
                            pixel_height,
                        }
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    (
        FrameData::from_ratatui_buffer_with_hyperlinks(&buffer, cursor, &hyperlinks),
        panes,
    )
}
