use super::*;

pub(crate) fn render_collapsed_sidebar(
    buffer: &mut Buffer,
    area: Rect,
    snapshot: &ClientShellSnapshot,
    palette: &Palette,
    selected_workspace_id: Option<&str>,
    hits: &mut ShellHitMap,
) {
    render_sidebar_background(buffer, area, palette);
    let content_width = area.width.saturating_sub(1);
    for (index, workspace) in snapshot.workspaces.iter().enumerate() {
        let y = area.y.saturating_add(index as u16);
        if y >= area.bottom().saturating_sub(1) {
            break;
        }
        let rect = Rect::new(area.x, y, content_width, 1);
        let selected = selected_workspace_id == Some(workspace.workspace_id.as_str());
        let style = if selected {
            Style::default().fg(palette.text).bg(palette.selection_bg)
        } else if workspace.focused {
            Style::default().fg(palette.text).bg(palette.active_row_bg)
        } else {
            Style::default().fg(palette.overlay0)
        };
        if selected || workspace.focused {
            buffer.set_style(rect, style);
        }
        put_text(
            buffer,
            rect.x,
            rect.y,
            rect.width,
            &format!(
                "{:<2}{}",
                workspace.number,
                status_dot(workspace.agent_status)
            ),
            style,
        );
        hits.workspaces.push(WorkspaceHit {
            rect,
            workspace_id: workspace.workspace_id.clone(),
            group_toggle: None,
        });
    }
    hits.sidebar_toggle = Rect::new(
        area.x + content_width / 2,
        area.bottom().saturating_sub(1),
        u16::from(content_width > 0),
        u16::from(area.height > 0),
    );
    put_text(
        buffer,
        hits.sidebar_toggle.x,
        hits.sidebar_toggle.y,
        hits.sidebar_toggle.width,
        "»",
        Style::default().fg(palette.overlay0),
    );
}

pub(crate) fn render_sidebar(
    buffer: &mut Buffer,
    area: Rect,
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
    collapsed_groups: &HashSet<String>,
    workspace_scroll: &mut usize,
    selected_workspace_id: Option<&str>,
    hits: &mut ShellHitMap,
) {
    let palette = &config.palette;
    render_sidebar_background(buffer, area, palette);
    let content_width = area.width.saturating_sub(1);
    let workspace_height = ((area.height as f32) * 0.5)
        .round()
        .clamp(3.0, area.height.saturating_sub(3) as f32) as u16;
    let workspace_area = Rect::new(area.x, area.y, content_width, workspace_height);
    put_text(
        buffer,
        workspace_area.x,
        workspace_area.y,
        workspace_area.width,
        " spaces",
        Style::default()
            .fg(palette.overlay0)
            .add_modifier(Modifier::BOLD),
    );

    let entries = workspace_entries(snapshot, collapsed_groups);
    let body = Rect::new(
        workspace_area.x,
        workspace_area.y.saturating_add(WORKSPACE_HEADER_ROWS),
        workspace_area.width,
        workspace_area
            .height
            .saturating_sub(WORKSPACE_HEADER_ROWS + 1),
    );
    *workspace_scroll = (*workspace_scroll).min(entries.len().saturating_sub(1));
    let mut y = body.y;
    for entry in entries.iter().skip(*workspace_scroll) {
        let Some(workspace) = snapshot.workspaces.get(entry.index) else {
            continue;
        };
        let rows = workspace_rows(workspace, entry.indented, &config.spaces);
        let row_height = rows.len().max(1).min(u16::MAX as usize) as u16;
        if y.saturating_add(row_height) > body.bottom() {
            break;
        }
        let rect = Rect::new(body.x, y, body.width, row_height);
        let selected = selected_workspace_id == Some(workspace.workspace_id.as_str());
        if selected {
            buffer.set_style(rect, Style::default().bg(palette.selection_bg));
        } else if workspace.focused {
            buffer.set_style(rect, Style::default().bg(palette.active_row_bg));
        }
        render_workspace_rows(buffer, rect, workspace, entry, rows, selected, palette);
        let group_toggle = parent_group_key(snapshot, entry.index).map(|key| {
            let rect = Rect::new(rect.right().saturating_sub(1), rect.y, 1, 1);
            put_text(
                buffer,
                rect.x,
                rect.y,
                rect.width,
                if collapsed_groups.contains(&key) {
                    "▸"
                } else {
                    "▾"
                },
                Style::default().fg(palette.accent),
            );
            (rect, key)
        });
        hits.workspaces.push(WorkspaceHit {
            rect,
            workspace_id: workspace.workspace_id.clone(),
            group_toggle,
        });
        y = y.saturating_add(row_height + config.spaces.row_gap);
    }

    let footer_y = workspace_area.bottom().saturating_sub(1);
    hits.new_workspace = Rect::new(
        workspace_area.x,
        footer_y,
        5.min(workspace_area.width),
        u16::from(workspace_area.height > 0),
    );
    put_text(
        buffer,
        workspace_area.x,
        footer_y,
        workspace_area.width,
        " new",
        Style::default().fg(palette.overlay0),
    );
    put_right_text(
        buffer,
        workspace_area,
        footer_y,
        "menu",
        Style::default().fg(palette.overlay0),
    );

    let detail_area = Rect::new(
        area.x,
        workspace_area.bottom(),
        content_width,
        area.bottom().saturating_sub(workspace_area.bottom()),
    );
    if detail_area.height > 0 {
        put_text(
            buffer,
            detail_area.x,
            detail_area.y,
            detail_area.width,
            &"─".repeat(detail_area.width as usize),
            Style::default().fg(palette.surface_dim),
        );
    }
    if detail_area.height > 1 {
        put_text(
            buffer,
            detail_area.x,
            detail_area.y + 1,
            detail_area.width,
            " agents",
            Style::default()
                .fg(palette.overlay0)
                .add_modifier(Modifier::BOLD),
        );
        put_right_text(
            buffer,
            detail_area,
            detail_area.y + 1,
            "priority",
            Style::default()
                .fg(palette.overlay0)
                .add_modifier(Modifier::BOLD),
        );
    }
    for (index, agent) in snapshot.agents.iter().enumerate() {
        let y = detail_area.y + 3 + index as u16;
        if y >= detail_area.bottom() {
            break;
        }
        let label = agent
            .name
            .as_deref()
            .or(agent.display_agent.as_deref())
            .or(agent.agent.as_deref())
            .or(agent.title.as_deref())
            .unwrap_or("terminal");
        let rect = Rect::new(detail_area.x, y, detail_area.width, 1);
        hits.agents.push((rect, agent.pane_id.clone()));
        put_text(
            buffer,
            detail_area.x,
            y,
            detail_area.width,
            &format!(" {}  {label}", status_dot(agent.agent_status)),
            Style::default().fg(if agent.focused {
                palette.text
            } else {
                palette.subtext0
            }),
        );
    }

    hits.sidebar_toggle = Rect::new(
        area.right().saturating_sub(2),
        area.bottom().saturating_sub(1),
        u16::from(area.width > 1),
        u16::from(area.height > 0),
    );
    put_text(
        buffer,
        hits.sidebar_toggle.x,
        hits.sidebar_toggle.y,
        hits.sidebar_toggle.width,
        "«",
        Style::default().fg(palette.overlay0),
    );
}

pub(crate) fn render_sidebar_background(buffer: &mut Buffer, area: Rect, palette: &Palette) {
    buffer.set_style(area, Style::default().bg(palette.sidebar_bg));
    let separator_x = area.right().saturating_sub(1);
    for y in area.y..area.bottom() {
        if let Some(cell) = buffer.cell_mut((separator_x, y)) {
            cell.set_symbol("│");
            cell.set_style(Style::default().fg(palette.surface_dim));
        }
    }
}

pub(crate) fn workspace_entries(
    snapshot: &ClientShellSnapshot,
    collapsed_groups: &HashSet<String>,
) -> Vec<WorkspaceEntry> {
    let mut members = HashMap::<&str, Vec<usize>>::new();
    for (index, workspace) in snapshot.workspaces.iter().enumerate() {
        if let Some(worktree) = &workspace.worktree {
            members.entry(&worktree.key).or_default().push(index);
        }
    }
    let grouped = members
        .iter()
        .filter(|(_, indices)| {
            indices.len() >= 2
                && indices.iter().any(|index| {
                    snapshot.workspaces[*index]
                        .worktree
                        .as_ref()
                        .is_some_and(|worktree| !worktree.is_linked_worktree)
                })
        })
        .map(|(key, _)| *key)
        .collect::<HashSet<_>>();
    let mut emitted = HashSet::<&str>::new();
    let mut entries = Vec::new();
    for (index, workspace) in snapshot.workspaces.iter().enumerate() {
        let Some(worktree) = workspace
            .worktree
            .as_ref()
            .filter(|worktree| grouped.contains(worktree.key.as_str()))
        else {
            entries.push(WorkspaceEntry {
                index,
                indented: false,
                last_child: false,
            });
            continue;
        };
        if !emitted.insert(&worktree.key) {
            continue;
        }
        let Some(group_members) = members.get(worktree.key.as_str()) else {
            continue;
        };
        let parent = group_members
            .iter()
            .copied()
            .find(|member| {
                snapshot.workspaces[*member]
                    .worktree
                    .as_ref()
                    .is_some_and(|worktree| !worktree.is_linked_worktree)
            })
            .unwrap_or(index);
        entries.push(WorkspaceEntry {
            index: parent,
            indented: false,
            last_child: false,
        });
        if collapsed_groups.contains(&worktree.key) {
            if let Some(active) = group_members
                .iter()
                .copied()
                .find(|member| *member != parent && snapshot.workspaces[*member].focused)
            {
                entries.push(WorkspaceEntry {
                    index: active,
                    indented: true,
                    last_child: true,
                });
            }
            continue;
        }
        let children = group_members
            .iter()
            .copied()
            .filter(|member| *member != parent)
            .collect::<Vec<_>>();
        for (child_index, child) in children.iter().enumerate() {
            entries.push(WorkspaceEntry {
                index: *child,
                indented: true,
                last_child: child_index + 1 == children.len(),
            });
        }
    }
    entries
}

fn parent_group_key(snapshot: &ClientShellSnapshot, index: usize) -> Option<String> {
    let workspace = snapshot.workspaces.get(index)?;
    let worktree = workspace.worktree.as_ref()?;
    if worktree.is_linked_worktree {
        return None;
    }
    (snapshot
        .workspaces
        .iter()
        .filter(|candidate| {
            candidate
                .worktree
                .as_ref()
                .is_some_and(|candidate| candidate.key == worktree.key)
        })
        .count()
        >= 2)
        .then(|| worktree.key.clone())
}

#[derive(Clone)]
struct WorkspaceToken {
    text: String,
    kind: WorkspaceTokenKind,
    style: crate::config::SidebarTokenStyle,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkspaceTokenKind {
    StateIcon,
    StateText,
    Workspace,
    Branch,
    GitStatus,
    Custom,
}

fn workspace_rows(
    workspace: &ClientShellWorkspace,
    indented: bool,
    config: &SpacesSidebarConfig,
) -> Vec<Vec<WorkspaceToken>> {
    let label = if indented && !workspace.custom_label {
        workspace
            .branch
            .as_deref()
            .and_then(|branch| branch.strip_prefix("worktree/").or(Some(branch)))
            .unwrap_or(&workspace.label)
    } else {
        &workspace.label
    };
    let token_values = workspace.tokens.iter().cloned().collect::<HashMap<_, _>>();
    config
        .rows
        .iter()
        .filter_map(|row| {
            let row = row
                .iter()
                .filter_map(|configured| {
                    let (token, style) = configured.parts();
                    let (text, kind) = match token {
                        SpaceSidebarToken::StateIcon => (
                            status_dot(workspace.agent_status).to_owned(),
                            WorkspaceTokenKind::StateIcon,
                        ),
                        SpaceSidebarToken::StateText => (
                            status_text(workspace.agent_status).to_owned(),
                            WorkspaceTokenKind::StateText,
                        ),
                        SpaceSidebarToken::Workspace => {
                            (label.to_owned(), WorkspaceTokenKind::Workspace)
                        }
                        SpaceSidebarToken::Branch if !indented => {
                            (workspace.branch.clone()?, WorkspaceTokenKind::Branch)
                        }
                        SpaceSidebarToken::Branch => return None,
                        SpaceSidebarToken::GitStatus if !indented => {
                            let (ahead, behind) = workspace.git_ahead_behind?;
                            if ahead == 0 && behind == 0 {
                                return None;
                            }
                            (format!("↑{ahead} ↓{behind}"), WorkspaceTokenKind::GitStatus)
                        }
                        SpaceSidebarToken::GitStatus => return None,
                        SpaceSidebarToken::Custom(name) => {
                            (token_values.get(name)?.clone(), WorkspaceTokenKind::Custom)
                        }
                        SpaceSidebarToken::Styled { .. } => return None,
                    };
                    Some(WorkspaceToken { text, kind, style })
                })
                .collect::<Vec<_>>();
            (!row.is_empty()).then_some(row)
        })
        .collect()
}

fn render_workspace_rows(
    buffer: &mut Buffer,
    area: Rect,
    workspace: &ClientShellWorkspace,
    entry: &WorkspaceEntry,
    rows: Vec<Vec<WorkspaceToken>>,
    selected: bool,
    palette: &Palette,
) {
    for (row_index, row) in rows.iter().enumerate() {
        let y = area.y + row_index as u16;
        if y >= area.bottom() {
            break;
        }
        let mut x = area.x;
        if entry.indented {
            let prefix = if row_index == 0 {
                if entry.last_child {
                    "   └─ "
                } else {
                    "   ├─ "
                }
            } else if entry.last_child {
                "        "
            } else {
                "   │    "
            };
            x = put_segment(
                buffer,
                x,
                y,
                area.right(),
                prefix,
                Style::default().fg(palette.overlay0),
            );
        } else if row_index == 0 {
            x = x.saturating_add(1);
        } else {
            x = x.saturating_add(3);
        }
        for (index, token) in row.iter().enumerate() {
            if index > 0 {
                let separator = if row[index - 1].kind == WorkspaceTokenKind::StateIcon
                    || token.kind == WorkspaceTokenKind::GitStatus
                {
                    " "
                } else {
                    " · "
                };
                x = put_segment(
                    buffer,
                    x,
                    y,
                    area.right().saturating_sub(2),
                    separator,
                    Style::default().fg(palette.overlay0),
                );
            }
            let mut style = match token.kind {
                WorkspaceTokenKind::StateIcon => {
                    Style::default().fg(status_color(workspace.agent_status, palette))
                }
                WorkspaceTokenKind::StateText => Style::default()
                    .fg(status_color(workspace.agent_status, palette))
                    .add_modifier(Modifier::DIM),
                WorkspaceTokenKind::Workspace => {
                    let base = Style::default().fg(if workspace.focused {
                        palette.text
                    } else {
                        palette.subtext0
                    });
                    if workspace.focused {
                        base.add_modifier(Modifier::BOLD)
                    } else {
                        base
                    }
                }
                WorkspaceTokenKind::Branch | WorkspaceTokenKind::GitStatus => {
                    Style::default().fg(if workspace.focused {
                        palette.mauve
                    } else {
                        palette.overlay0
                    })
                }
                WorkspaceTokenKind::Custom => Style::default().fg(palette.overlay1),
            };
            if let Some(fg) = token.style.fg {
                style = style.fg(fg.ratatui());
            }
            if token.style.bold == Some(true) {
                style = style.add_modifier(Modifier::BOLD);
            } else if token.style.bold == Some(false) {
                style = style.remove_modifier(Modifier::BOLD);
            }
            if token.style.dim == Some(true) {
                style = style.add_modifier(Modifier::DIM);
            } else if token.style.dim == Some(false) {
                style = style.remove_modifier(Modifier::DIM);
            }
            x = put_segment(
                buffer,
                x,
                y,
                area.right().saturating_sub(2),
                &token.text,
                style,
            );
        }
    }

    let background = if selected {
        Some(palette.selection_bg)
    } else if workspace.focused {
        Some(palette.active_row_bg)
    } else {
        None
    };
    if let Some(background) = background {
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                buffer[(x, y)].set_bg(background);
            }
        }
    }
}
