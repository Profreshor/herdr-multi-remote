#[derive(Clone, Copy)]
pub(crate) enum ConfigEdit<'a> {
    Theme(&'a str),
    StatusIndicators(super::StatusIndicatorStyle),
    Sound(bool),
    ToastDelivery(super::ToastDelivery),
    AgentBorderLabels(bool),
    AgentPanelSort(super::AgentPanelSortConfig),
}

impl ConfigEdit<'_> {
    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::Theme(_) => "theme",
            Self::StatusIndicators(_) => "status indicators",
            Self::Sound(_) => "sound setting",
            Self::ToastDelivery(_) => "toast setting",
            Self::AgentBorderLabels(_) => "agent border labels",
            Self::AgentPanelSort(_) => "agent panel sort",
        }
    }

    pub(crate) fn apply(self, content: &str) -> String {
        match self {
            Self::Theme(name) => {
                let content =
                    super::upsert_section_value(content, "theme", "name", &format!("\"{name}\""));
                super::upsert_section_bool(&content, "theme", "auto_switch", false)
            }
            Self::StatusIndicators(style) => super::upsert_section_value(
                content,
                "ui",
                "status_indicators",
                &format!("\"{}\"", style.as_str()),
            ),
            Self::Sound(enabled) => {
                super::upsert_section_bool(content, "ui.sound", "enabled", enabled)
            }
            Self::ToastDelivery(delivery) => {
                let value = match delivery {
                    super::ToastDelivery::Off => "\"off\"",
                    super::ToastDelivery::Herdr => "\"herdr\"",
                    super::ToastDelivery::Terminal => "\"terminal\"",
                    super::ToastDelivery::System => "\"system\"",
                };
                let content = super::upsert_section_value(content, "ui.toast", "delivery", value);
                super::remove_section_key(&content, "ui.toast", "enabled")
            }
            Self::AgentBorderLabels(enabled) => super::upsert_section_bool(
                content,
                "ui",
                "show_agent_labels_on_pane_borders",
                enabled,
            ),
            Self::AgentPanelSort(sort) => super::upsert_section_value(
                content,
                "ui",
                "agent_panel_sort",
                &format!("\"{}\"", sort.as_str()),
            ),
        }
    }
}

pub(crate) fn update_file(
    description: &str,
    update: impl FnOnce(&str) -> String,
) -> Result<(), String> {
    let path = super::config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create config directory: {error}"))?;
    }
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(format!(
                "failed to read config before saving {description}: {error}"
            ));
        }
    };
    std::fs::write(&path, update(&content))
        .map_err(|error| format!("failed to save {description}: {error}"))
}

pub(crate) fn write_edit(edit: ConfigEdit<'_>) -> Result<(), String> {
    update_file(edit.description(), |content| edit.apply(content))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_preserve_unrelated_config() {
        let content = "[terminal]\ndefault_shell = \"fish\"\n";
        let edited =
            ConfigEdit::AgentPanelSort(super::super::AgentPanelSortConfig::Priority).apply(content);
        assert!(edited.contains("default_shell = \"fish\""));
        assert!(edited.contains("agent_panel_sort = \"priority\""));
    }
}
