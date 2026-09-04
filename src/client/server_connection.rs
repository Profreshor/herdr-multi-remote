use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use crate::ipc::LocalStream;
use crate::protocol::ClientMessage;

use super::endpoint_commands::EndpointCommands;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct ServerId(Box<str>);

impl ServerId {
    pub(super) fn local() -> Self {
        Self("local".into())
    }

    pub(super) fn new(id: impl Into<Box<str>>) -> Self {
        Self(id.into())
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct ServerConnectionKey {
    pub(super) server_id: ServerId,
    pub(super) generation: u64,
}

pub(super) struct ServerConnection {
    pub(super) key: ServerConnectionKey,
    pub(super) write_stream: LocalStream,
    pub(super) endpoint_commands: EndpointCommands,
    pub(super) endpoint_methods: Option<Vec<String>>,
    pub(super) presentation_control: bool,
    pub(super) latest_snapshot: Option<Box<crate::protocol::ClientShellSnapshot>>,
    pub(super) window_title: Option<String>,
    presentation_active: Arc<AtomicBool>,
    _remote_bridge: Option<crate::remote::RemoteClientBridge>,
}

impl ServerConnection {
    pub(super) fn local(
        write_stream: LocalStream,
        endpoint_methods: Option<Vec<String>>,
        presentation_control: bool,
    ) -> Self {
        Self {
            key: ServerConnectionKey {
                server_id: ServerId::local(),
                generation: 0,
            },
            write_stream,
            endpoint_commands: EndpointCommands::default(),
            endpoint_methods,
            presentation_control,
            latest_snapshot: None,
            window_title: None,
            presentation_active: Arc::new(AtomicBool::new(true)),
            _remote_bridge: None,
        }
    }

    pub(super) fn remote(
        key: ServerConnectionKey,
        write_stream: LocalStream,
        endpoint_methods: Option<Vec<String>>,
        presentation_control: bool,
        remote_bridge: crate::remote::RemoteClientBridge,
    ) -> Self {
        Self {
            key,
            write_stream,
            endpoint_commands: EndpointCommands::default(),
            endpoint_methods,
            presentation_control,
            latest_snapshot: None,
            window_title: None,
            presentation_active: Arc::new(AtomicBool::new(false)),
            _remote_bridge: Some(remote_bridge),
        }
    }

    pub(super) fn presentation_active(&self) -> Arc<AtomicBool> {
        self.presentation_active.clone()
    }
}

pub(super) struct ServerConnections {
    entries: BTreeMap<ServerId, ServerConnection>,
    generations: BTreeMap<ServerId, u64>,
    active: ServerId,
    fleet_enabled: bool,
}

pub(super) struct RemoteConnectorThreads {
    cancelled: Arc<AtomicBool>,
    attempts: BTreeMap<ServerId, Arc<AtomicBool>>,
    handles: Vec<JoinHandle<()>>,
}

impl RemoteConnectorThreads {
    pub(super) fn new(cancelled: Arc<AtomicBool>) -> Self {
        Self {
            cancelled,
            attempts: BTreeMap::new(),
            handles: Vec::new(),
        }
    }

    pub(super) fn replace(&mut self, server_id: ServerId) -> Arc<AtomicBool> {
        let cancelled = Arc::new(AtomicBool::new(false));
        if let Some(previous) = self.attempts.insert(server_id, cancelled.clone()) {
            previous.store(true, Ordering::Release);
        }
        cancelled
    }

    pub(super) fn cancel(&mut self, server_id: &ServerId) {
        if let Some(cancelled) = self.attempts.remove(server_id) {
            cancelled.store(true, Ordering::Release);
        }
    }

    pub(super) fn push(&mut self, handle: JoinHandle<()>) {
        let mut index = 0;
        while index < self.handles.len() {
            if self.handles[index].is_finished() {
                let finished = self.handles.swap_remove(index);
                let _ = finished.join();
            } else {
                index += 1;
            }
        }
        self.handles.push(handle);
    }
}

impl Drop for RemoteConnectorThreads {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        for cancelled in self.attempts.values() {
            cancelled.store(true, Ordering::Release);
        }
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
    }
}

impl ServerConnections {
    pub(super) fn with_local(connection: ServerConnection) -> Self {
        debug_assert_eq!(connection.key.server_id, ServerId::local());
        let active = connection.key.server_id.clone();
        Self {
            entries: BTreeMap::from([(active.clone(), connection)]),
            generations: BTreeMap::from([(active.clone(), 0)]),
            active,
            fleet_enabled: false,
        }
    }

    pub(super) fn key_for_id(&self, server_id: &str) -> Option<ServerConnectionKey> {
        self.entries
            .get(&ServerId::new(server_id))
            .map(|connection| connection.key.clone())
    }

    pub(super) fn reserve_generation(&mut self, server_id: &ServerId) -> u64 {
        let generation = self
            .generations
            .get(server_id)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        self.generations.insert(server_id.clone(), generation);
        generation
    }

    pub(super) fn is_current_generation(&self, key: &ServerConnectionKey) -> bool {
        self.generations.get(&key.server_id) == Some(&key.generation)
    }

    pub(super) fn active_mut(&mut self) -> &mut ServerConnection {
        self.current_mut()
            .expect("active server remains registered")
    }

    pub(super) fn current_mut(&mut self) -> Option<&mut ServerConnection> {
        self.entries.get_mut(&self.active)
    }

    pub(super) fn active_stream(&mut self) -> std::io::Result<&mut LocalStream> {
        self.current_mut()
            .map(|connection| &mut connection.write_stream)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "selected server is disconnected",
                )
            })
    }

    pub(super) fn active_key(&self) -> Option<ServerConnectionKey> {
        self.entries
            .get(&self.active)
            .map(|connection| connection.key.clone())
    }

    pub(super) fn active_is_remote(&self) -> bool {
        self.active != ServerId::local()
    }

    pub(super) fn selected_is(&self, server_id: &str) -> bool {
        self.active.as_str() == server_id
    }

    pub(super) fn fleet_mode(&self) -> bool {
        self.fleet_enabled
    }

    pub(super) fn set_fleet_mode(&mut self, enabled: bool) {
        self.fleet_enabled = enabled;
    }

    pub(super) fn send_active(&mut self, message: &ClientMessage) -> std::io::Result<()> {
        super::write_to_server(self.active_stream()?, message)
    }

    #[cfg(unix)]
    pub(super) fn send_to(
        &mut self,
        key: &ServerConnectionKey,
        message: &ClientMessage,
    ) -> std::io::Result<()> {
        let connection = self.get_mut(key).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "server is no longer connected",
            )
        })?;
        super::write_to_server(&mut connection.write_stream, message)
    }

    pub(super) fn is_live(&self, key: &ServerConnectionKey) -> bool {
        self.entries
            .get(&key.server_id)
            .is_some_and(|connection| connection.key == *key)
    }

    pub(super) fn is_active(&self, key: &ServerConnectionKey) -> bool {
        self.active == key.server_id && self.is_live(key)
    }

    pub(super) fn get_mut(&mut self, key: &ServerConnectionKey) -> Option<&mut ServerConnection> {
        self.is_live(key)
            .then(|| self.entries.get_mut(&key.server_id))
            .flatten()
    }

    pub(super) fn insert(&mut self, connection: ServerConnection) -> bool {
        if self
            .generations
            .get(&connection.key.server_id)
            .is_some_and(|generation| *generation > connection.key.generation)
            || self
                .entries
                .get(&connection.key.server_id)
                .is_some_and(|current| current.key.generation >= connection.key.generation)
        {
            return false;
        }
        self.generations
            .insert(connection.key.server_id.clone(), connection.key.generation);
        connection
            .presentation_active
            .store(self.active == connection.key.server_id, Ordering::Release);
        self.entries
            .insert(connection.key.server_id.clone(), connection);
        true
    }

    pub(super) fn activate(&mut self, key: &ServerConnectionKey) -> bool {
        if !self.is_live(key) {
            return false;
        }
        self.select(key.server_id.clone());
        true
    }

    pub(super) fn select(&mut self, server_id: ServerId) {
        if let Some(previous) = self.entries.get(&self.active) {
            previous.presentation_active.store(false, Ordering::Release);
        }
        self.active = server_id;
        if let Some(connection) = self.entries.get(&self.active) {
            connection
                .presentation_active
                .store(true, Ordering::Release);
        }
    }

    pub(super) fn fallback_key(&self) -> Option<ServerConnectionKey> {
        self.entries
            .get(&ServerId::local())
            .filter(|connection| connection.latest_snapshot.is_some())
            .or_else(|| {
                self.entries
                    .values()
                    .find(|connection| connection.latest_snapshot.is_some())
            })
            .map(|connection| connection.key.clone())
    }

    pub(super) fn remove(&mut self, key: &ServerConnectionKey) -> Option<ServerConnection> {
        self.is_live(key)
            .then(|| self.entries.remove(&key.server_id))
            .flatten()
    }

    pub(super) fn retire_protocol_source(
        &mut self,
        key: &ServerConnectionKey,
    ) -> Option<ServerConnection> {
        if !self.fleet_mode() {
            return None;
        }
        let removed = self.remove(key)?;
        self.reserve_generation(&key.server_id);
        Some(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream() -> LocalStream {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        use interprocess::local_socket::traits::Listener as _;

        let path = std::env::temp_dir().join(format!(
            "herdr-server-registry-test-{}-{}.sock",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);
        let listener = crate::ipc::bind_local_listener(&path).unwrap();
        let accept = std::thread::spawn(move || listener.accept().unwrap());
        let stream = crate::ipc::connect_local_stream(&path).unwrap();
        drop(accept.join().unwrap());
        let _ = std::fs::remove_file(path);
        stream
    }

    fn connection(server_id: &str, generation: u64, active: bool) -> ServerConnection {
        ServerConnection {
            key: ServerConnectionKey {
                server_id: ServerId::new(server_id),
                generation,
            },
            write_stream: stream(),
            endpoint_commands: EndpointCommands::default(),
            endpoint_methods: None,
            presentation_control: false,
            latest_snapshot: None,
            window_title: None,
            presentation_active: Arc::new(AtomicBool::new(active)),
            _remote_bridge: None,
        }
    }

    fn snapshot() -> Box<crate::protocol::ClientShellSnapshot> {
        Box::new(crate::protocol::ClientShellSnapshot {
            boot_id: "boot".into(),
            revision: 1,
            config_diagnostic: None,
            product_announcement: None,
            update_available: None,
            update_install_command: String::new(),
            server_keybindings_toml: None,
            latest_release_notes_available: false,
            integration_updates_available: false,
            worktree_directory: String::new(),
            release_notes: None,
            focused_workspace_id: None,
            focused_tab_id: None,
            focused_pane_id: None,
            tab_bar_right: Vec::new(),
            tab_bar_right_separator: String::new(),
            agent_view_label: None,
            agent_order: Vec::new(),
            workspaces: Vec::new(),
            tabs: Vec::new(),
            panes: Vec::new(),
            agents: Vec::new(),
            commands: Vec::new(),
        })
    }

    #[test]
    fn activation_rejects_stale_generations_and_moves_presentation_ownership() {
        let local = connection("local", 0, true);
        let local_active = local.presentation_active();
        let mut remote = connection("analytics", 2, false);
        let remote_active = remote.presentation_active();
        remote.latest_snapshot = Some(snapshot());
        let live = remote.key.clone();
        let stale = ServerConnectionKey {
            server_id: live.server_id.clone(),
            generation: 1,
        };
        let mut registry = ServerConnections::with_local(local);
        registry.insert(remote);

        assert!(!registry.activate(&stale));
        assert!(registry.activate(&live));
        assert!(!local_active.load(Ordering::Acquire));
        assert!(remote_active.load(Ordering::Acquire));
    }

    #[test]
    fn active_remote_removal_retains_selection_and_a_healthy_fallback() {
        let mut local = connection("local", 0, true);
        local.latest_snapshot = Some(snapshot());
        let local_key = local.key.clone();
        let mut remote = connection("analytics", 1, false);
        remote.latest_snapshot = Some(snapshot());
        let remote_key = remote.key.clone();
        let mut registry = ServerConnections::with_local(local);
        registry.insert(remote);
        assert!(registry.activate(&remote_key));

        registry.remove(&remote_key).expect("remove active remote");
        assert_eq!(registry.active_key(), None);
        assert!(!registry.is_live(&remote_key));
        let fallback = registry.fallback_key().expect("healthy local fallback");
        assert_eq!(fallback, local_key);
    }

    #[test]
    fn inactive_disconnect_does_not_change_the_active_server() {
        let local = connection("local", 0, true);
        let local_key = local.key.clone();
        let remote = connection("analytics", 1, false);
        let remote_key = remote.key.clone();
        let mut registry = ServerConnections::with_local(local);
        registry.insert(remote);

        registry
            .remove(&remote_key)
            .expect("remove inactive remote");

        assert!(registry.is_active(&local_key));
    }

    #[test]
    fn reconnect_restores_presentation_for_the_selected_server() {
        let local = connection("local", 0, true);
        let remote = connection("analytics", 1, false);
        let remote_key = remote.key.clone();
        let mut registry = ServerConnections::with_local(local);
        assert!(registry.insert(remote));
        assert!(registry.activate(&remote_key));
        registry.remove(&remote_key).unwrap();

        let replacement = connection("analytics", 2, false);
        let replacement_active = replacement.presentation_active();
        assert!(registry.insert(replacement));

        assert!(registry.active_key().is_some());
        assert!(replacement_active.load(Ordering::Acquire));
    }

    #[test]
    fn removed_generations_cannot_reconnect_after_a_newer_generation() {
        let local = connection("local", 0, true);
        let mut registry = ServerConnections::with_local(local);
        let generation_two = connection("analytics", 2, false);
        let generation_two_key = generation_two.key.clone();
        assert!(registry.insert(generation_two));
        registry.remove(&generation_two_key).unwrap();

        assert!(!registry.insert(connection("analytics", 1, false)));
        assert!(registry.insert(connection("analytics", 3, false)));
    }

    #[test]
    fn reserving_a_replacement_generation_rejects_the_cancelled_attempt() {
        let mut registry = ServerConnections::with_local(connection("local", 0, true));
        let server_id = ServerId::new("analytics");
        let first = registry.reserve_generation(&server_id);
        let replacement = registry.reserve_generation(&server_id);

        assert_eq!((first, replacement), (1, 2));
        assert!(!registry.insert(connection("analytics", first, false)));
        assert!(registry.insert(connection("analytics", replacement, false)));
    }

    #[test]
    fn fleet_mode_tracks_current_configuration_not_generation_history() {
        let mut registry = ServerConnections::with_local(connection("local", 0, true));
        registry.set_fleet_mode(true);
        registry.reserve_generation(&ServerId::new("analytics"));
        assert!(registry.fleet_mode());

        registry.set_fleet_mode(false);
        assert!(!registry.fleet_mode());
    }

    #[test]
    fn malformed_active_remote_is_retired_without_harming_a_healthy_sibling() {
        let local = connection("local", 0, true);
        let bad = connection("bad", 1, false);
        let bad_key = bad.key.clone();
        let healthy = connection("healthy", 1, false);
        let healthy_key = healthy.key.clone();
        let mut registry = ServerConnections::with_local(local);
        registry.set_fleet_mode(true);
        registry.insert(bad);
        registry.insert(healthy);
        assert!(registry.activate(&bad_key));

        registry
            .retire_protocol_source(&bad_key)
            .expect("bad source is isolated");

        assert!(!registry.is_live(&bad_key));
        assert!(registry.is_live(&healthy_key));
        assert_eq!(registry.active_key(), None);
        assert!(!registry.insert(connection("bad", 1, false)));
    }

    #[test]
    fn connector_threads_are_cancelled_and_joined_on_drop() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let exited = Arc::new(AtomicBool::new(false));
        let mut connectors = RemoteConnectorThreads::new(cancelled.clone());
        let worker_cancelled = cancelled.clone();
        let worker_exited = exited.clone();
        connectors.push(std::thread::spawn(move || {
            while !worker_cancelled.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            worker_exited.store(true, Ordering::Release);
        }));

        drop(connectors);

        assert!(cancelled.load(Ordering::Acquire));
        assert!(exited.load(Ordering::Acquire));
    }

    #[test]
    fn replacing_a_connector_cancels_the_previous_attempt() {
        let mut connectors = RemoteConnectorThreads::new(Arc::new(AtomicBool::new(false)));
        let server_id = ServerId::new("analytics");
        let first = connectors.replace(server_id.clone());
        let second = connectors.replace(server_id.clone());

        assert!(first.load(Ordering::Acquire));
        assert!(!second.load(Ordering::Acquire));
        connectors.cancel(&server_id);
        assert!(second.load(Ordering::Acquire));
    }

    #[test]
    fn pushing_a_connector_reaps_completed_attempts() {
        let mut connectors = RemoteConnectorThreads::new(Arc::new(AtomicBool::new(false)));
        for _ in 0..10 {
            connectors.push(std::thread::spawn(|| {}));
            while connectors
                .handles
                .last()
                .is_some_and(|handle| !handle.is_finished())
            {
                std::thread::yield_now();
            }
        }
        connectors.push(std::thread::spawn(|| {}));

        assert!(connectors.handles.len() <= 2);
    }
}
