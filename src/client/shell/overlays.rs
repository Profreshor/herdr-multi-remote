use super::*;

mod worktree_overlays;

pub(crate) struct OverlayRender {
    pub(crate) primary: Rect,
    pub(crate) clear: Rect,
    pub(crate) cancel: Rect,
    pub(crate) navigator_popup: Rect,
    pub(crate) navigator_search: Rect,
    pub(crate) navigator_rows: Vec<(Rect, usize)>,
    pub(crate) worktree_search: Rect,
    pub(crate) worktree_rows: Vec<(Rect, usize)>,
    pub(crate) cursor: Option<crate::protocol::CursorState>,
}

pub(crate) fn render_client_overlay(
    b: &mut Buffer,
    o: &ClientShellOverlay,
    s: &ClientShellSnapshot,
    k: &LiveKeybindConfig,
    p: &Palette,
) -> Option<OverlayRender> {
    if !matches!(
        o,
        ClientShellOverlay::Navigator(_) | ClientShellOverlay::ContextMenu(_)
    ) {
        for y in b.area.y..b.area.bottom() {
            for x in b.area.x..b.area.right() {
                let c = &mut b[(x, y)];
                c.set_style(c.style().add_modifier(Modifier::DIM));
            }
        }
    }
    match o {
        ClientShellOverlay::Rename(v) => render_rename_overlay(b, v, p),
        ClientShellOverlay::ConfirmClose(v) => render_confirm_close_overlay(b, v, p),
        ClientShellOverlay::Help(v) => render_help_overlay(b, v, k, p),
        ClientShellOverlay::Navigator(v) => render_navigator_overlay(b, v, s, p),
        ClientShellOverlay::WorktreeCreate(v) => {
            worktree_overlays::render_worktree_create_overlay(b, v, p)
        }
        ClientShellOverlay::WorktreeOpen(v) => {
            worktree_overlays::render_worktree_open_overlay(b, v, p)
        }
        ClientShellOverlay::WorktreeRemove(v) => {
            worktree_overlays::render_worktree_remove_overlay(b, v, p)
        }
        ClientShellOverlay::ContextMenu(_) => None,
    }
}

pub(crate) fn render_context_menu(
    buffer: &mut Buffer,
    menu: &ClientContextMenuOverlay,
    palette: &Palette,
) -> Option<Vec<(Rect, usize)>> {
    let items = menu.items();
    let screen = buffer.area;
    let max_item_width = items
        .iter()
        .map(|item| display_width(item.label))
        .max()
        .unwrap_or(0);
    let width = max_item_width
        .saturating_add(4)
        .max(14)
        .min(screen.width.max(1));
    let height = (items.len() as u16)
        .saturating_add(2)
        .min(screen.height.max(1));
    let x = menu
        .x
        .min(screen.x.saturating_add(screen.width.saturating_sub(width)));
    let y = menu.y.min(
        screen
            .y
            .saturating_add(screen.height.saturating_sub(height)),
    );
    let rect = Rect::new(x, y, width, height);
    let inner = panel(buffer, rect, palette.accent, palette.panel_bg)?;
    let mut rows = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let row_y = inner.y.saturating_add(index as u16);
        if row_y >= inner.bottom() {
            break;
        }
        let row = Rect::new(inner.x, row_y, inner.width, 1);
        let highlighted = index == menu.highlighted;
        let style = if highlighted {
            Style::default()
                .fg(panel_contrast_fg(palette))
                .bg(palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.text).bg(palette.panel_bg)
        };
        buffer.set_style(row, style);
        put_text(buffer, row.x, row.y, row.width, item.label, style);
        rows.push((row, index));
    }
    Some(rows)
}

fn panel(
    b: &mut Buffer,
    a: Rect,
    c: ratatui::style::Color,
    bg: ratatui::style::Color,
) -> Option<Rect> {
    if a.width < 2 || a.height < 2 {
        return None;
    }
    let background = Style::default().bg(bg).remove_modifier(Modifier::DIM);
    let border = Style::default().fg(c).bg(bg).remove_modifier(Modifier::DIM);
    for y in a.y..a.bottom() {
        for x in a.x..a.right() {
            b[(x, y)].set_symbol(" ").set_style(background);
        }
    }
    for x in a.x..a.right() {
        b[(x, a.y)]
            .set_symbol(if x == a.x {
                "┌"
            } else if x + 1 == a.right() {
                "┐"
            } else {
                "─"
            })
            .set_style(border);
        let y = a.bottom() - 1;
        b[(x, y)]
            .set_symbol(if x == a.x {
                "└"
            } else if x + 1 == a.right() {
                "┘"
            } else {
                "─"
            })
            .set_style(border);
    }
    for y in a.y + 1..a.bottom() - 1 {
        b[(a.x, y)].set_symbol("│").set_style(border);
        b[(a.right() - 1, y)].set_symbol("│").set_style(border);
    }
    Some(Rect::new(a.x + 1, a.y + 1, a.width - 2, a.height - 2))
}
fn popup(a: Rect, w: u16, h: u16) -> Option<Rect> {
    let w = w.min(a.width.saturating_sub(4));
    let h = h.min(a.height.saturating_sub(2));
    if w < 4 || h < 4 {
        return None;
    }
    Some(Rect::new(
        a.x + (a.width - w) / 2,
        a.y + (a.height - h) / 2,
        w,
        h,
    ))
}
fn button(b: &mut Buffer, r: Rect, t: &str, s: Style) {
    b.set_style(r, s);
    let w = display_width(t).min(r.width);
    put_text(b, r.x + (r.width - w) / 2, r.y, w, t, s)
}
fn row(i: Rect, ws: &[u16], gap: u16, off: u16) -> Vec<Rect> {
    let total = ws.iter().sum::<u16>() + gap * (ws.len().saturating_sub(1) as u16);
    let mut x = i.x + i.width.saturating_sub(total) / 2;
    ws.iter()
        .map(|w| {
            let r = Rect::new(
                x,
                i.y + off.min(i.height.saturating_sub(1)),
                (*w).min(i.width.saturating_sub(x - i.x)),
                1,
            );
            x += *w + gap;
            r
        })
        .collect()
}
fn contrast(p: &Palette) -> ratatui::style::Color {
    match p.panel_bg {
        ratatui::style::Color::Reset => p.surface_dim,
        c => c,
    }
}
fn render_rename_overlay(
    b: &mut Buffer,
    v: &ClientRenameOverlay,
    p: &Palette,
) -> Option<OverlayRender> {
    let q = popup(b.area, 56, 7)?;
    let i = panel(b, q, p.accent, p.panel_bg)?;
    put_text(
        b,
        i.x,
        i.y,
        i.width,
        v.title,
        Style::default()
            .fg(p.text)
            .bg(p.panel_bg)
            .add_modifier(Modifier::BOLD),
    );
    let input = Rect::new(i.x, i.y + 2, i.width, 1);
    b.set_style(input, Style::default().fg(p.text).bg(p.surface0));
    put_text(
        b,
        input.x,
        input.y,
        input.width.saturating_sub(1),
        &format!(" {}", v.input),
        Style::default().fg(p.text).bg(p.surface0),
    );
    let rs = row(i, &[8, 10, 12], 2, 3);
    let [save, clear, cancel] = rs.as_slice() else {
        return None;
    };
    button(
        b,
        *save,
        " ↵ save ",
        Style::default()
            .fg(contrast(p))
            .bg(p.accent)
            .add_modifier(Modifier::BOLD),
    );
    let n = Style::default()
        .fg(p.text)
        .bg(p.surface0)
        .add_modifier(Modifier::BOLD);
    button(b, *clear, " ^c clear ", n);
    button(b, *cancel, " esc cancel ", n);
    Some(OverlayRender {
        primary: *save,
        clear: *clear,
        cancel: *cancel,
        navigator_popup: Rect::default(),
        navigator_search: Rect::default(),
        navigator_rows: Vec::new(),
        worktree_search: Rect::default(),
        worktree_rows: Vec::new(),
        cursor: Some(crate::protocol::CursorState {
            x: (input.x + 1 + display_width(&v.input)).min(input.right() - 1),
            y: input.y,
            visible: true,
            shape: 0,
        }),
    })
}

pub(crate) fn client_navigator_rows(
    s: &ClientShellSnapshot,
    n: &ClientNavigatorOverlay,
) -> Vec<ClientNavigatorRow> {
    let q = n.query.trim().to_lowercase();
    let filter = |st| match n.filter {
        Some(ClientNavigatorFilter::Blocked) => st == crate::api::schema::AgentStatus::Blocked,
        Some(ClientNavigatorFilter::Working) => st == crate::api::schema::AgentStatus::Working,
        Some(ClientNavigatorFilter::Idle) => st == crate::api::schema::AgentStatus::Idle,
        Some(ClientNavigatorFilter::Done) => st == crate::api::schema::AgentStatus::Done,
        None => true,
    };
    let text = |v: &str| q.is_empty() || v.to_lowercase().contains(&q);
    let filtering = n.filter.is_some() || !q.is_empty();
    let mut out = Vec::new();
    for w in &s.workspaces {
        let mut children = Vec::new();
        for t in s.tabs.iter().filter(|t| t.workspace_id == w.workspace_id) {
            let mut panes = Vec::new();
            for (ix, p) in s.panes.iter().filter(|p| p.tab_id == t.tab_id).enumerate() {
                let a = s.agents.iter().find(|a| a.pane_id == p.pane_id);
                let st = a
                    .map(|a| a.agent_status)
                    .unwrap_or(crate::api::schema::AgentStatus::Unknown);
                let label = p
                    .label
                    .clone()
                    .or_else(|| a.and_then(|a| a.name.clone()))
                    .or_else(|| a.and_then(|a| a.display_agent.clone()))
                    .or_else(|| a.and_then(|a| a.title.clone()))
                    .unwrap_or_else(|| format!("pane {}", ix + 1));
                let meta = p
                    .foreground_cwd
                    .clone()
                    .or_else(|| p.cwd.clone())
                    .unwrap_or_default();
                if !filtering || filter(st) && (text(&label) || text(&meta)) {
                    panes.push(ClientNavigatorRow {
                        depth: 2,
                        label,
                        meta,
                        status: st,
                        current: s.focused_pane_id.as_deref() == Some(&p.pane_id),
                        target: ClientNavigatorTarget::Pane(p.pane_id.clone()),
                    })
                }
            }
            if !filtering || filter(t.agent_status) && text(&t.label) || !panes.is_empty() {
                children.push(ClientNavigatorRow {
                    depth: 1,
                    label: t.label.clone(),
                    meta: format!(
                        "{} panes",
                        s.panes.iter().filter(|p| p.tab_id == t.tab_id).count()
                    ),
                    status: t.agent_status,
                    current: s.focused_tab_id.as_deref() == Some(&t.tab_id),
                    target: ClientNavigatorTarget::Tab(t.tab_id.clone()),
                });
                children.extend(panes)
            }
        }
        let wm =
            filter(w.agent_status) && (text(&w.label) || w.branch.as_deref().is_some_and(&text));
        if !filtering || wm || !children.is_empty() {
            out.push(ClientNavigatorRow {
                depth: 0,
                label: w.label.clone(),
                meta: w.branch.clone().unwrap_or_default(),
                status: w.agent_status,
                current: s.focused_workspace_id.as_deref() == Some(&w.workspace_id),
                target: ClientNavigatorTarget::Workspace(w.workspace_id.clone()),
            });
            if n.expanded_workspaces.contains(&w.workspace_id) || filtering {
                out.extend(children)
            }
        }
    }
    out
}

fn render_navigator_overlay(
    b: &mut Buffer,
    n: &ClientNavigatorOverlay,
    s: &ClientShellSnapshot,
    p: &Palette,
) -> Option<OverlayRender> {
    let a = b.area;
    let mx = (a.width / 16).max(2);
    let my = (a.height / 10).max(1);
    let q = Rect::new(
        a.x + mx,
        a.y + my,
        a.width.saturating_sub(mx * 2).max(4),
        a.height.saturating_sub(my * 2).max(4),
    );
    let i = panel(b, q, p.accent, p.panel_bg)?;
    let rows = client_navigator_rows(s, n);
    let search = if n.search_focused {
        format!(" / {}", n.query)
    } else if let Some(f) = n.filter {
        format!(
            " / {}",
            match f {
                ClientNavigatorFilter::Blocked => "blocked",
                ClientNavigatorFilter::Working => "working",
                ClientNavigatorFilter::Idle => "idle",
                ClientNavigatorFilter::Done => "done",
            }
        )
    } else if n.query.is_empty() {
        " / search panes".to_owned()
    } else {
        format!(" / {}", n.query)
    };
    put_text(
        b,
        i.x,
        i.y,
        i.width,
        &search,
        Style::default()
            .fg(if n.search_focused { p.text } else { p.overlay0 })
            .bg(p.panel_bg),
    );
    put_right_text(
        b,
        i,
        i.y,
        &format!("{} panes", s.panes.len()),
        Style::default().fg(p.overlay0).bg(p.panel_bg),
    );
    put_text(
        b,
        i.x,
        i.y + 1,
        i.width,
        &"─".repeat(i.width as usize),
        Style::default().fg(p.surface1).bg(p.panel_bg),
    );
    let body = Rect::new(i.x, i.y + 2, i.width, i.height.saturating_sub(4));
    let max = rows.len().saturating_sub(body.height as usize);
    let scroll = n
        .scroll
        .max(
            n.selected
                .saturating_sub(body.height.saturating_sub(1) as usize),
        )
        .min(n.selected)
        .min(max);
    let mut row_hits = Vec::new();
    for (vis, (ix, r)) in rows
        .iter()
        .enumerate()
        .skip(scroll)
        .take(body.height as usize)
        .enumerate()
    {
        let rect = Rect::new(body.x, body.y + vis as u16, body.width, 1);
        row_hits.push((rect, ix));
        let st = if ix == n.selected {
            Style::default()
                .fg(contrast(p))
                .bg(p.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(if r.current { p.text } else { p.subtext0 })
                .bg(p.panel_bg)
        };
        b.set_style(rect, st);
        let tree = match r.depth {
            0 => match &r.target {
                ClientNavigatorTarget::Workspace(id) if n.expanded_workspaces.contains(id) => "▾",
                _ => "▸",
            },
            1 => "└──",
            _ => "   └──",
        };
        let label = format!(
            " {} {} {} {}",
            if r.current { "◆" } else { " " },
            tree,
            status_dot(r.status),
            r.label
        );
        put_text(b, rect.x, rect.y, rect.width, &label, st);
        if !r.meta.is_empty() {
            let label_width = display_width(&label).min(rect.width);
            let meta = Rect::new(
                rect.x.saturating_add(label_width).saturating_add(1),
                rect.y,
                rect.width.saturating_sub(label_width.saturating_add(1)),
                1,
            );
            put_right_text(b, meta, rect.y, &r.meta, st)
        }
    }
    let dy = i.bottom() - 2;
    if let Some(r) = rows.get(n.selected) {
        put_text(
            b,
            i.x,
            dy,
            i.width,
            &format!(" {} · {}", r.label, r.meta),
            Style::default().fg(p.overlay0).bg(p.panel_bg),
        )
    }
    put_text(
        b,
        i.x,
        i.bottom() - 1,
        i.width,
        if n.search_focused {
            " search type · move ↑↓/ctrl+n/p · open enter · back esc"
        } else {
            " move j/k · expand space · filter a/b/w/i/d · search / · open enter · close esc"
        },
        Style::default().fg(p.overlay0).bg(p.panel_bg),
    );
    Some(OverlayRender {
        primary: Rect::default(),
        clear: Rect::default(),
        cancel: Rect::default(),
        navigator_popup: q,
        navigator_search: Rect::new(i.x, i.y, i.width, 1),
        navigator_rows: row_hits,
        worktree_search: Rect::default(),
        worktree_rows: Vec::new(),
        cursor: n.search_focused.then(|| crate::protocol::CursorState {
            x: i.x + 3 + display_width(&n.query),
            y: i.y,
            visible: true,
            shape: 0,
        }),
    })
}

fn render_help_overlay(
    b: &mut Buffer,
    h: &ClientHelpOverlay,
    k: &LiveKeybindConfig,
    p: &Palette,
) -> Option<OverlayRender> {
    let q = popup(b.area, 76, 22)?;
    let i = panel(b, q, p.accent, p.panel_bg)?;
    if i.width < 20 || i.height < 6 {
        return None;
    }
    put_text(
        b,
        i.x,
        i.y,
        i.width,
        "keybinds",
        Style::default()
            .fg(p.text)
            .bg(p.panel_bg)
            .add_modifier(Modifier::BOLD),
    );
    let close = Rect::new(i.right() - 13, i.y, 13, 1);
    button(
        b,
        close,
        if h.search_focused {
            " esc back "
        } else {
            " esc close "
        },
        Style::default()
            .fg(contrast(p))
            .bg(p.accent)
            .add_modifier(Modifier::BOLD),
    );
    let sy = i.y + 1;
    put_text(
        b,
        i.x,
        sy,
        i.width,
        &if h.search_focused {
            format!(" / {}", h.query)
        } else {
            " / press / to filter by command or shortcut".to_owned()
        },
        Style::default()
            .fg(if h.search_focused { p.text } else { p.overlay0 })
            .bg(p.panel_bg),
    );
    let groups = crate::input::filter_keybind_help_groups(
        crate::input::keybind_help_groups(&k.keybinds, k.prefix),
        &h.query,
    );
    let mut lines = Vec::new();
    for (g, es) in groups {
        lines.push((
            format!(" {g}"),
            Style::default()
                .fg(p.accent)
                .bg(p.panel_bg)
                .add_modifier(Modifier::BOLD),
        ));
        for (key, label) in es {
            lines.push((
                format!(" {key}  {label}"),
                Style::default().fg(p.text).bg(p.panel_bg),
            ));
        }
        lines.push((String::new(), Style::default().bg(p.panel_bg)));
    }
    let body = Rect::new(i.x, i.y + 3, i.width, i.height.saturating_sub(5));
    for (r, (t, st)) in lines
        .into_iter()
        .skip(h.scroll)
        .take(body.height as usize)
        .enumerate()
    {
        put_text(b, body.x, body.y + r as u16, body.width, &t, st)
    }
    put_text(
        b,
        i.x,
        i.bottom() - 1,
        i.width,
        if h.search_focused {
            " filter type/backspace · clear ctrl+u · scroll ↑↓/pgup/pgdn · back esc"
        } else {
            " search / · scroll j/k/↑↓/pgup/pgdn · close esc/enter"
        },
        Style::default().fg(p.overlay0).bg(p.panel_bg),
    );
    Some(OverlayRender {
        primary: Rect::default(),
        clear: Rect::default(),
        cancel: close,
        navigator_popup: Rect::default(),
        navigator_search: Rect::default(),
        navigator_rows: Vec::new(),
        worktree_search: Rect::default(),
        worktree_rows: Vec::new(),
        cursor: h.search_focused.then(|| crate::protocol::CursorState {
            x: (i.x + 3 + display_width(&h.query)).min(i.right() - 1),
            y: sy,
            visible: true,
            shape: 0,
        }),
    })
}
fn render_confirm_close_overlay(
    b: &mut Buffer,
    c: &ClientConfirmCloseOverlay,
    p: &Palette,
) -> Option<OverlayRender> {
    let q = popup(b.area, 64, 6)?;
    let i = panel(b, q, p.red, p.panel_bg)?;
    put_text(
        b,
        i.x,
        i.y,
        i.width,
        &format!(" {}", c.title),
        Style::default()
            .fg(p.red)
            .bg(p.panel_bg)
            .add_modifier(Modifier::BOLD),
    );
    put_text(
        b,
        i.x,
        i.y + 1,
        i.width,
        &format!(" {}", c.detail),
        Style::default().fg(p.text).bg(p.panel_bg),
    );
    let rs = row(i, &[13, 12], 2, 3);
    let [ok, cancel] = rs.as_slice() else {
        return None;
    };
    button(
        b,
        *ok,
        " ↵ confirm ",
        Style::default()
            .fg(contrast(p))
            .bg(p.red)
            .add_modifier(Modifier::BOLD),
    );
    button(
        b,
        *cancel,
        " esc cancel ",
        Style::default()
            .fg(p.text)
            .bg(p.surface0)
            .add_modifier(Modifier::BOLD),
    );
    Some(OverlayRender {
        primary: *ok,
        clear: Rect::default(),
        cancel: *cancel,
        navigator_popup: Rect::default(),
        navigator_search: Rect::default(),
        navigator_rows: Vec::new(),
        worktree_search: Rect::default(),
        worktree_rows: Vec::new(),
        cursor: None,
    })
}
