use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::App;

static NEXT_COMMAND_NAMESPACE: AtomicU64 = AtomicU64::new(1);

pub(super) fn new_command_namespace() -> String {
    let counter = NEXT_COMMAND_NAMESPACE.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{nanos}-{counter}", std::process::id())
}

#[derive(Debug)]
pub(super) struct EndpointCommandRegistry {
    entries: Vec<EndpointCommand>,
}

#[derive(Debug)]
struct EndpointCommand {
    id: String,
    binding: crate::config::CustomCommandKeybind,
    action: crate::protocol::ClientShellCommandAction,
}

impl EndpointCommandRegistry {
    pub(super) fn new(bindings: &[crate::config::CustomCommandKeybind]) -> Self {
        let namespace = new_command_namespace();
        let entries = bindings
            .iter()
            .filter_map(|binding| {
                let action =
                    crate::protocol::ClientShellCommandAction::try_from(binding.action).ok()?;
                Some((binding, action))
            })
            .enumerate()
            .map(|(index, (binding, action))| EndpointCommand {
                id: format!("cmd_{namespace}_{index}"),
                binding: binding.clone(),
                action,
            })
            .collect();
        Self { entries }
    }
}

impl App {
    pub(crate) fn client_shell_command_manifest(&self) -> Vec<crate::protocol::ClientShellCommand> {
        self.endpoint_commands
            .entries
            .iter()
            .map(|entry| crate::protocol::ClientShellCommand {
                command_id: entry.id.clone(),
                binding_label: entry.binding.label.clone(),
                action: entry.action,
            })
            .collect()
    }

    pub(crate) fn resolve_client_shell_command(
        &self,
        id: &str,
    ) -> Option<crate::config::CustomCommandKeybind> {
        self.endpoint_commands
            .entries
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| entry.binding.clone())
    }

    pub(crate) fn handle_command_invoke(
        &mut self,
        id: String,
        params: crate::api::schema::CommandInvokeParams,
    ) -> String {
        let Some(binding) = self.resolve_client_shell_command(&params.command_id) else {
            return crate::app::api::responses::encode_error(
                id,
                "command_not_found",
                "custom command manifest is stale; reload configuration",
            );
        };
        if let Err((code, message)) = self.focus_client_shell_command_target(&params) {
            return crate::app::api::responses::encode_error(id, code, message);
        }
        match self.execute_custom_command_binding(&binding) {
            Ok(()) => crate::app::api::responses::encode_success(
                id,
                crate::api::schema::ResponseResult::Ok {},
            ),
            Err(error) => {
                crate::app::api::responses::encode_error(id, "command_failed", error.to_string())
            }
        }
    }

    fn focus_client_shell_command_target(
        &mut self,
        params: &crate::api::schema::CommandInvokeParams,
    ) -> Result<(), (&'static str, String)> {
        if let Some(pane_id) = params.pane_id.as_deref() {
            let Some((workspace_index, pane)) = self.parse_pane_id(pane_id) else {
                return Err(("pane_not_found", format!("pane not found: {pane_id}")));
            };
            let Some(tab_index) =
                self.state.workspaces[workspace_index].find_tab_index_for_pane(pane)
            else {
                return Err(("pane_not_found", format!("pane not found: {pane_id}")));
            };
            self.validate_client_shell_command_parent_ids(params, workspace_index, tab_index)?;
            self.state.focus_pane_in_workspace(workspace_index, pane);
            return Ok(());
        }

        if let Some(tab_id) = params.tab_id.as_deref() {
            let Some((workspace_index, tab_index)) = self.parse_tab_id(tab_id) else {
                return Err(("tab_not_found", format!("tab not found: {tab_id}")));
            };
            self.validate_client_shell_command_workspace_id(params, workspace_index)?;
            self.state.switch_workspace_tab(workspace_index, tab_index);
            return Ok(());
        }

        if let Some(workspace_id) = params.workspace_id.as_deref() {
            let Some(workspace_index) = self.parse_workspace_id(workspace_id) else {
                return Err((
                    "workspace_not_found",
                    format!("workspace not found: {workspace_id}"),
                ));
            };
            self.state.switch_workspace(workspace_index);
        }
        Ok(())
    }

    fn validate_client_shell_command_parent_ids(
        &self,
        params: &crate::api::schema::CommandInvokeParams,
        workspace_index: usize,
        tab_index: usize,
    ) -> Result<(), (&'static str, String)> {
        self.validate_client_shell_command_workspace_id(params, workspace_index)?;
        if let Some(tab_id) = params.tab_id.as_deref() {
            let actual = self
                .public_tab_id(workspace_index, tab_index)
                .unwrap_or_default();
            if tab_id != actual {
                return Err((
                    "command_target_mismatch",
                    "command pane does not belong to the requested tab".to_owned(),
                ));
            }
        }
        Ok(())
    }

    fn validate_client_shell_command_workspace_id(
        &self,
        params: &crate::api::schema::CommandInvokeParams,
        workspace_index: usize,
    ) -> Result<(), (&'static str, String)> {
        if params
            .workspace_id
            .as_deref()
            .is_some_and(|workspace_id| workspace_id != self.public_workspace_id(workspace_index))
        {
            return Err((
                "command_target_mismatch",
                "command target does not belong to the requested workspace".to_owned(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    fn test_app() -> crate::app::App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        crate::app::App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        )
    }

    fn binding(action: crate::config::CustomCommandAction) -> crate::config::CustomCommandKeybind {
        crate::config::CustomCommandKeybind {
            bindings: crate::config::ActionKeybinds::prefix("z"),
            label: "prefix+z".into(),
            command: "secret-command --token hidden".into(),
            action,
            description: Some("safe description".into()),
            width: None,
            height: None,
        }
    }

    fn install(app: &mut crate::app::App, binding: crate::config::CustomCommandKeybind) {
        app.endpoint_commands = super::EndpointCommandRegistry::new(&[binding]);
    }

    #[test]
    fn manifest_exposes_opaque_ids_without_command_text() {
        let mut app = test_app();
        install(&mut app, binding(crate::config::CustomCommandAction::Shell));
        let manifest = app.client_shell_command_manifest();
        assert_eq!(manifest.len(), 1);
        assert_eq!(manifest[0].binding_label, "prefix+z");
        assert!(!format!("{:?}", manifest).contains("secret-command"));
        assert_eq!(
            app.resolve_client_shell_command(&manifest[0].command_id)
                .map(|binding| binding.command),
            Some("secret-command --token hidden".into())
        );
    }

    #[test]
    fn stale_command_id_is_rejected_after_definition_changes() {
        let mut app = test_app();
        install(&mut app, binding(crate::config::CustomCommandAction::Shell));
        let old_id = app.client_shell_command_manifest()[0].command_id.clone();
        let mut replacement = binding(crate::config::CustomCommandAction::Shell);
        replacement.command = "replacement-command".into();
        install(&mut app, replacement);

        let response = app.handle_command_invoke(
            "request-1".into(),
            crate::api::schema::CommandInvokeParams {
                command_id: old_id,
                workspace_id: None,
                tab_id: None,
                pane_id: None,
            },
        );
        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "command_not_found");
    }

    #[test]
    fn foreground_keybinding_projection_cannot_remap_command_ids() {
        let mut app = test_app();
        install(&mut app, binding(crate::config::CustomCommandAction::Shell));
        let command_id = app.client_shell_command_manifest()[0].command_id.clone();
        let mut foreground_binding = binding(crate::config::CustomCommandAction::Shell);
        foreground_binding.command = "different-foreground-command".into();
        app.state.keybinds.custom_commands = vec![foreground_binding];

        assert_eq!(
            app.resolve_client_shell_command(&command_id)
                .map(|binding| binding.command),
            Some("secret-command --token hidden".into())
        );
    }

    #[test]
    fn command_ids_do_not_alias_across_endpoint_restarts() {
        let mut old_app = test_app();
        install(
            &mut old_app,
            binding(crate::config::CustomCommandAction::Shell),
        );
        let old_id = old_app.client_shell_command_manifest()[0]
            .command_id
            .clone();

        let mut replacement_app = test_app();
        let mut replacement = binding(crate::config::CustomCommandAction::Shell);
        replacement.command = "replacement-command".into();
        install(&mut replacement_app, replacement);
        assert_ne!(
            replacement_app.client_shell_command_manifest()[0].command_id,
            old_id
        );
        let response = replacement_app.handle_command_invoke(
            "request-1".into(),
            crate::api::schema::CommandInvokeParams {
                command_id: old_id,
                workspace_id: None,
                tab_id: None,
                pane_id: None,
            },
        );
        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "command_not_found");
    }

    #[cfg(unix)]
    #[test]
    fn shell_command_invocation_executes_endpoint_owned_definition() {
        let mut app = test_app();
        let path = std::path::PathBuf::from(format!(
            "/var/tmp/herdr-command-invoke-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&path);
        let mut command = binding(crate::config::CustomCommandAction::Shell);
        command.command = format!("printf invoked > {}", path.display());
        install(&mut app, command);
        let command_id = app.client_shell_command_manifest()[0].command_id.clone();

        let response = app.handle_command_invoke(
            "request-1".into(),
            crate::api::schema::CommandInvokeParams {
                command_id,
                workspace_id: None,
                tab_id: None,
                pane_id: None,
            },
        );
        let success: crate::api::schema::SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.result, crate::api::schema::ResponseResult::Ok {});
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !path.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "invoked");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn popup_commands_are_absent_until_the_client_owns_their_surface() {
        let mut app = test_app();
        install(&mut app, binding(crate::config::CustomCommandAction::Popup));

        assert!(app.client_shell_command_manifest().is_empty());
        assert!(app.endpoint_commands.entries.is_empty());
    }
}
