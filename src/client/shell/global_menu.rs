use super::*;

pub(super) const GLOBAL_MENU_ITEMS: &[(&str, crate::input::KeybindAction)] = &[
    ("settings", crate::input::KeybindAction::Settings),
    ("keybinds", crate::input::KeybindAction::Help),
    ("reload config", crate::input::KeybindAction::ReloadConfig),
    ("detach", crate::input::KeybindAction::Detach),
];

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
        let Some(ClientShellOverlay::GlobalMenu(menu)) = self.overlay.as_mut() else {
            return;
        };
        menu.highlighted = (menu.highlighted as isize + delta)
            .clamp(0, GLOBAL_MENU_ITEMS.len().saturating_sub(1) as isize)
            as usize;
    }

    pub(super) fn activate_global_menu_item(
        &mut self,
        index: usize,
        outcome: &mut ClientShellInput,
    ) {
        let Some((_, action)) = GLOBAL_MENU_ITEMS.get(index).copied() else {
            return;
        };
        self.overlay = None;
        self.record_binding(crate::input::KeybindMatch::Action(action), outcome);
        outcome.repaint = true;
    }
}
