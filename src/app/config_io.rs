use super::App;

impl App {
    pub(super) fn update_config_file<F>(&mut self, error_context: &str, update: F) -> bool
    where
        F: FnOnce(&str) -> String,
    {
        #[cfg(test)]
        if std::env::var_os(crate::config::CONFIG_PATH_ENV_VAR).is_none() {
            return false;
        }

        let path = crate::config::config_path();
        if let Some(parent) = path.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                crate::logging::config_write_failed(&path, error_context, &err.to_string());
                self.state.config_diagnostic =
                    Some(format!("failed to save {error_context}: {err}"));
                self.config_diagnostic_deadline =
                    Some(std::time::Instant::now() + std::time::Duration::from_secs(5));
                return false;
            }
        }

        let content = std::fs::read_to_string(&path).unwrap_or_default();
        let new_content = update(&content);
        if let Err(err) = std::fs::write(&path, new_content) {
            crate::logging::config_write_failed(&path, error_context, &err.to_string());
            self.state.config_diagnostic = Some(format!("failed to save {error_context}: {err}"));
            self.config_diagnostic_deadline =
                Some(std::time::Instant::now() + std::time::Duration::from_secs(5));
            return false;
        }

        true
    }

    pub(super) fn mark_onboarding_complete(&mut self) {
        self.update_config_file("onboarding setting", |content| {
            crate::config::upsert_top_level_bool(content, "onboarding", false)
        });
    }

    fn save_config_edit(&mut self, edit: crate::config::ConfigEdit<'_>) {
        if self.update_config_file(edit.description(), |content| edit.apply(content)) {
            self.apply_config_from_disk(false);
        }
    }

    pub(super) fn save_theme(&mut self, name: &str) {
        self.save_config_edit(crate::config::ConfigEdit::Theme(name));
    }

    pub(super) fn save_status_indicators(&mut self, style: crate::config::StatusIndicatorStyle) {
        self.save_config_edit(crate::config::ConfigEdit::StatusIndicators(style));
    }

    pub(super) fn save_sound(&mut self, enabled: bool) {
        self.save_config_edit(crate::config::ConfigEdit::Sound(enabled));
    }

    pub(super) fn save_toast_delivery(&mut self, delivery: crate::config::ToastDelivery) {
        self.save_config_edit(crate::config::ConfigEdit::ToastDelivery(delivery));
    }

    pub(super) fn save_agent_border_labels(&mut self, enabled: bool) {
        self.save_config_edit(crate::config::ConfigEdit::AgentBorderLabels(enabled));
    }

    pub(super) fn save_agent_panel_sort(&mut self, sort: crate::app::state::AgentPanelSort) {
        let sort = match sort {
            crate::app::state::AgentPanelSort::Spaces => {
                crate::config::AgentPanelSortConfig::Spaces
            }
            crate::app::state::AgentPanelSort::Priority => {
                crate::config::AgentPanelSortConfig::Priority
            }
        };
        self.save_config_edit(crate::config::ConfigEdit::AgentPanelSort(sort));
    }
}
