use super::*;

pub(crate) fn render_tab_bar(
    buffer: &mut Buffer,
    area: Rect,
    snapshot: &ClientShellSnapshot,
    palette: &Palette,
    tab_scroll: &mut usize,
    hits: &mut ShellHitMap,
) {
    buffer.set_style(area, Style::default().bg(palette.panel_bg));
    let tabs = snapshot
        .tabs
        .iter()
        .filter(|tab| Some(tab.workspace_id.as_str()) == snapshot.focused_workspace_id.as_deref())
        .collect::<Vec<_>>();
    *tab_scroll = (*tab_scroll).min(tabs.len().saturating_sub(1));
    let right = area.right();
    let mut x = area.x;
    for tab in tabs.iter().skip(*tab_scroll) {
        let name = if tab.zoomed {
            format!("{} Z", tab.label)
        } else {
            tab.label.clone()
        };
        let desired = display_width(&name).saturating_add(4).max(MIN_TAB_WIDTH);
        let remaining = right.saturating_sub(x);
        let reserved = NEW_TAB_WIDTH.min(remaining);
        let width = desired.min(remaining.saturating_sub(reserved));
        if width == 0 {
            break;
        }
        let rect = Rect::new(x, area.y, width, 1);
        let style = if tab.focused {
            let base = Style::default()
                .fg(panel_contrast_fg(palette))
                .bg(palette.accent);
            if tab.custom_label {
                base.add_modifier(Modifier::BOLD)
            } else {
                base
            }
        } else if tab.custom_label {
            Style::default().fg(palette.overlay1).bg(palette.surface0)
        } else {
            Style::default()
                .fg(palette.overlay0)
                .bg(palette.surface0)
                .add_modifier(Modifier::DIM)
        };
        let padding = width.saturating_sub(display_width(&name));
        let left = padding / 2;
        let text = format!(
            "{empty:left$}{name}{empty:right_padding$}",
            empty = "",
            left = left as usize,
            right_padding = padding.saturating_sub(left) as usize,
        );
        put_text(buffer, rect.x, rect.y, rect.width, &text, style);
        hits.tabs.push((rect, tab.tab_id.clone()));
        x = x.saturating_add(width + 1);
    }
    if x < right {
        put_text(
            buffer,
            x,
            area.y,
            right.saturating_sub(x).min(NEW_TAB_WIDTH),
            " + ",
            Style::default().fg(palette.overlay1).bg(palette.panel_bg),
        );
    }
}
