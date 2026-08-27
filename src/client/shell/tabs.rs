use super::*;

const TAB_SCROLL_BUTTON_WIDTH: u16 = 3;

pub(crate) fn render_tab_bar(
    buffer: &mut Buffer,
    area: Rect,
    snapshot: &ClientShellSnapshot,
    palette: &Palette,
    tab_scroll: &mut usize,
    reveal_focused_tab: &mut bool,
    hits: &mut ShellHitMap,
) {
    buffer.set_style(area, Style::default().bg(palette.panel_bg));
    let tabs = snapshot
        .tabs
        .iter()
        .filter(|tab| Some(tab.workspace_id.as_str()) == snapshot.focused_workspace_id.as_deref())
        .collect::<Vec<_>>();
    *tab_scroll = (*tab_scroll).min(tabs.len().saturating_sub(1));

    let desired_widths = tabs
        .iter()
        .map(|tab| {
            let label = tab_label(tab);
            display_width(&label).saturating_add(4).max(MIN_TAB_WIDTH)
        })
        .collect::<Vec<_>>();
    let desired_total = desired_widths
        .iter()
        .copied()
        .fold(0_u16, u16::saturating_add)
        .saturating_add(tabs.len().saturating_sub(1).min(u16::MAX as usize) as u16)
        .saturating_add(NEW_TAB_WIDTH);
    let overflow = desired_total > area.width
        && area.width
            >= MIN_TAB_WIDTH
                .saturating_add(NEW_TAB_WIDTH)
                .saturating_add(TAB_SCROLL_BUTTON_WIDTH.saturating_mul(2));
    if !overflow {
        *tab_scroll = 0;
    } else if *reveal_focused_tab {
        if let Some(focused) = tabs.iter().position(|tab| tab.focused) {
            let available = area
                .width
                .saturating_sub(NEW_TAB_WIDTH)
                .saturating_sub(TAB_SCROLL_BUTTON_WIDTH.saturating_mul(2));
            let last_visible = last_visible_tab(*tab_scroll, &desired_widths, available);
            if focused < *tab_scroll || last_visible.is_none_or(|last| focused > last) {
                *tab_scroll = focused;
            }
        }
    }
    *reveal_focused_tab = false;

    let mut x = area.x;
    let tab_right = if overflow {
        hits.tab_scroll_left =
            Rect::new(area.x, area.y, TAB_SCROLL_BUTTON_WIDTH.min(area.width), 1);
        put_text(
            buffer,
            hits.tab_scroll_left.x,
            area.y,
            hits.tab_scroll_left.width,
            " < ",
            Style::default()
                .fg(if *tab_scroll > 0 {
                    palette.overlay1
                } else {
                    palette.overlay0
                })
                .bg(palette.surface0),
        );
        x = hits.tab_scroll_left.right();
        area.right()
            .saturating_sub(NEW_TAB_WIDTH + TAB_SCROLL_BUTTON_WIDTH)
    } else {
        area.right().saturating_sub(NEW_TAB_WIDTH)
    };

    let mut last_visible = None;
    for (index, tab) in tabs.iter().enumerate().skip(*tab_scroll) {
        let name = tab_label(tab);
        let desired = desired_widths[index];
        let remaining = tab_right.saturating_sub(x);
        let width = desired.min(remaining);
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
        last_visible = Some(index);
        x = x.saturating_add(width + 1);
        if width < desired {
            break;
        }
    }

    if overflow {
        hits.tab_scroll_right = Rect::new(tab_right, area.y, TAB_SCROLL_BUTTON_WIDTH, 1);
        let can_scroll_right = last_visible.is_some_and(|index| index + 1 < tabs.len());
        put_text(
            buffer,
            hits.tab_scroll_right.x,
            area.y,
            hits.tab_scroll_right.width,
            " > ",
            Style::default()
                .fg(if can_scroll_right {
                    palette.overlay1
                } else {
                    palette.overlay0
                })
                .bg(palette.surface0),
        );
        hits.new_tab = Rect::new(
            hits.tab_scroll_right.right(),
            area.y,
            area.right()
                .saturating_sub(hits.tab_scroll_right.right())
                .min(NEW_TAB_WIDTH),
            1,
        );
    } else {
        hits.new_tab = Rect::new(
            x.min(area.right()),
            area.y,
            area.right().saturating_sub(x).min(NEW_TAB_WIDTH),
            1,
        );
    }
    put_text(
        buffer,
        hits.new_tab.x,
        area.y,
        hits.new_tab.width,
        " + ",
        Style::default().fg(palette.overlay1).bg(palette.panel_bg),
    );
}

fn last_visible_tab(start: usize, widths: &[u16], available: u16) -> Option<usize> {
    let mut remaining = available;
    let mut last = None;
    for (index, width) in widths.iter().copied().enumerate().skip(start) {
        if remaining == 0 {
            break;
        }
        last = Some(index);
        if width >= remaining {
            break;
        }
        remaining = remaining.saturating_sub(width.saturating_add(1));
    }
    last
}

fn tab_label(tab: &ClientShellTab) -> String {
    if tab.zoomed {
        format!("{} Z", tab.label)
    } else {
        tab.label.clone()
    }
}
