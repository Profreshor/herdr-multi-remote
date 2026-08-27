use super::*;

impl ClientShellState {
    pub(super) fn reload_client_config(&mut self) {
        match crate::config::load_live_config() {
            Ok(loaded) => {
                let diagnostics = self.config.apply_live_config(
                    &loaded.config,
                    &loaded.diagnostics,
                    &loaded.invalid_sections,
                );
                self.endpoint_error = crate::config::config_diagnostic_summary(&diagnostics);
            }
            Err(diagnostics) => {
                self.endpoint_error = crate::config::config_diagnostic_summary(&diagnostics);
            }
        }
    }
}

impl ClientShellConfig {
    pub(crate) fn from_config(config: &Config) -> Self {
        Self {
            sidebar_width: config.ui.sidebar_width,
            sidebar_min_width: config.ui.sidebar_min_width,
            sidebar_max_width: config.ui.sidebar_max_width,
            sidebar_start_collapsed: config.ui.sidebar_start_collapsed,
            sidebar_collapsed_mode: config.ui.sidebar_collapsed_mode,
            mobile_width_threshold: config.ui.mobile_width_threshold,
            tab_bar_position: config.ui.tab_bar_position,
            hide_tab_bar_when_single_tab: config.ui.hide_tab_bar_when_single_tab,
            spaces: config.ui.sidebar.spaces.clone(),
            palette: crate::app::client_palette_from_config(config),
            keybinds: config
                .live_keybinds_with_diagnostics()
                .map(|(keybinds, _diagnostics)| keybinds)
                .unwrap_or_else(|_diagnostics| LiveKeybindConfig {
                    prefix: config.prefix_key(),
                    keybinds: config.keybinds(),
                }),
            prompt_new_tab_name: config.ui.prompt_new_tab_name,
            prompt_new_workspace_name: config.ui.prompt_new_workspace_name,
            confirm_close: config.ui.confirm_close,
            worktree_directory: crate::worktree::expand_tilde_absolute_path(
                &config.worktrees.directory,
            ),
        }
    }

    pub(super) fn apply_live_config(
        &mut self,
        config: &Config,
        load_diagnostics: &[String],
        invalid_sections: &[String],
    ) -> Vec<String> {
        let mut diagnostics = load_diagnostics.to_vec();
        let invalid_section =
            |section: &str| invalid_sections.iter().any(|invalid| invalid == section);

        if !invalid_section("keys") {
            match config.live_keybinds_with_diagnostics() {
                Ok((keybinds, keybind_diagnostics)) => {
                    self.keybinds = keybinds;
                    diagnostics.extend(keybind_diagnostics);
                }
                Err(keybind_diagnostics) => diagnostics.extend(
                    keybind_diagnostics
                        .into_iter()
                        .map(|diagnostic| format!("{diagnostic}; kept current keybinds")),
                ),
            }
        }

        if !invalid_section("ui") {
            if let Some(diagnostic) = config.invalid_sidebar_bounds_diagnostic() {
                diagnostics.push(format!("{diagnostic}; keeping previous [ui] settings"));
            } else {
                let ui = &config.ui;
                self.sidebar_width = ui.sidebar_width;
                self.sidebar_min_width = ui.sidebar_min_width;
                self.sidebar_max_width = ui.sidebar_max_width;
                self.sidebar_collapsed_mode = ui.sidebar_collapsed_mode;
                self.mobile_width_threshold = ui.mobile_width_threshold;
                self.tab_bar_position = ui.tab_bar_position;
                self.hide_tab_bar_when_single_tab = ui.hide_tab_bar_when_single_tab;
                self.spaces = ui.sidebar.spaces.clone();
                self.prompt_new_tab_name = ui.prompt_new_tab_name;
                self.prompt_new_workspace_name = ui.prompt_new_workspace_name;
                self.confirm_close = ui.confirm_close;
            }
        }

        if !invalid_section("theme") {
            self.palette = crate::app::client_palette_from_config(config);
        }
        if !invalid_section("worktrees") {
            self.worktree_directory =
                crate::worktree::expand_tilde_absolute_path(&config.worktrees.directory);
        }

        diagnostics
    }

    pub(super) fn layout(
        &self,
        cols: u16,
        rows: u16,
        sidebar_collapsed: bool,
        tab_count: usize,
    ) -> ClientShellLayout {
        if cols <= self.mobile_width_threshold {
            let header_height = rows.min(2);
            return ClientShellLayout {
                sidebar: Rect::default(),
                tab_bar: Rect::default(),
                mobile_header: Rect::new(0, 0, cols, header_height),
                pane_surface: Rect::new(0, header_height, cols, rows.saturating_sub(header_height)),
            };
        }

        let sidebar_width = if sidebar_collapsed {
            match self.sidebar_collapsed_mode {
                SidebarCollapsedModeConfig::Compact => 4,
                SidebarCollapsedModeConfig::Hidden => 0,
            }
        } else {
            let (min, max) = crate::config::validated_sidebar_bounds(
                self.sidebar_min_width,
                self.sidebar_max_width,
            )
            .unwrap_or((18, 36));
            self.sidebar_width.clamp(min, max)
        }
        .min(cols.saturating_sub(1));
        let main = Rect::new(sidebar_width, 0, cols.saturating_sub(sidebar_width), rows);
        let show_tab_bar = rows > 1 && !(self.hide_tab_bar_when_single_tab && tab_count == 1);
        let tab_height = u16::from(show_tab_bar);
        let (tab_bar, pane_surface) = match self.tab_bar_position {
            TabBarPositionConfig::Top => (
                Rect::new(main.x, 0, main.width, tab_height),
                Rect::new(
                    main.x,
                    tab_height,
                    main.width,
                    rows.saturating_sub(tab_height),
                ),
            ),
            TabBarPositionConfig::Bottom => (
                Rect::new(
                    main.x,
                    rows.saturating_sub(tab_height),
                    main.width,
                    tab_height,
                ),
                Rect::new(main.x, 0, main.width, rows.saturating_sub(tab_height)),
            ),
        };

        ClientShellLayout {
            sidebar: Rect::new(0, 0, sidebar_width, rows),
            tab_bar,
            mobile_header: Rect::default(),
            pane_surface,
        }
    }

    pub(crate) fn initial_surface_size(&self, cols: u16, rows: u16) -> ClientSurfaceSize {
        let surface = self
            .layout(cols, rows, self.sidebar_start_collapsed, 0)
            .pane_surface;
        ClientSurfaceSize {
            cols: surface.width.max(1),
            rows: surface.height.max(1),
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyModifiers};

    use super::*;

    #[test]
    fn live_reload_applies_client_owned_sections() {
        let mut shell = ClientShellConfig::from_config(&Config::default());
        let mut next = Config::default();
        next.ui.sidebar_width = 31;
        next.ui.tab_bar_position = TabBarPositionConfig::Bottom;
        next.keys.prefix = "ctrl+a".to_owned();
        next.worktrees.directory = "/var/tmp/herdr-reloaded-worktrees".to_owned();

        let diagnostics = shell.apply_live_config(&next, &[], &[]);

        assert!(diagnostics.is_empty());
        assert_eq!(shell.sidebar_width, 31);
        assert_eq!(shell.tab_bar_position, TabBarPositionConfig::Bottom);
        assert_eq!(
            shell.keybinds.prefix,
            (KeyCode::Char('a'), KeyModifiers::CONTROL)
        );
        assert_eq!(
            shell.worktree_directory,
            std::path::PathBuf::from("/var/tmp/herdr-reloaded-worktrees")
        );
    }

    #[test]
    fn live_reload_preserves_invalid_client_owned_sections() {
        let mut initial = Config::default();
        initial.ui.sidebar_width = 29;
        initial.keys.prefix = "ctrl+x".to_owned();
        initial.worktrees.directory = "/var/tmp/herdr-current-worktrees".to_owned();
        let mut shell = ClientShellConfig::from_config(&initial);

        let mut invalid = Config::default();
        invalid.ui.sidebar_width = 35;
        invalid.keys.prefix = "ctrl+a".to_owned();
        invalid.worktrees.directory = "/var/tmp/herdr-invalid-worktrees".to_owned();
        let invalid_sections = vec!["ui".to_owned(), "keys".to_owned(), "worktrees".to_owned()];
        shell.apply_live_config(&invalid, &[], &invalid_sections);

        assert_eq!(shell.sidebar_width, 29);
        assert_eq!(
            shell.keybinds.prefix,
            (KeyCode::Char('x'), KeyModifiers::CONTROL)
        );
        assert_eq!(
            shell.worktree_directory,
            std::path::PathBuf::from("/var/tmp/herdr-current-worktrees")
        );
    }
}
