use super::*;

pub(super) const MIN_TAB_WIDTH: u16 = 8;
pub(super) const NEW_TAB_WIDTH: u16 = 3;
pub(super) const WORKSPACE_HEADER_ROWS: u16 = 2;

pub(crate) struct ClientShellConfig {
    pub(super) sidebar_width: u16,
    pub(super) sidebar_min_width: u16,
    pub(super) sidebar_max_width: u16,
    pub(super) sidebar_start_collapsed: bool,
    pub(super) sidebar_collapsed_mode: SidebarCollapsedModeConfig,
    pub(super) mobile_width_threshold: u16,
    pub(super) tab_bar_position: TabBarPositionConfig,
    pub(super) hide_tab_bar_when_single_tab: bool,
    pub(super) spaces: SpacesSidebarConfig,
    pub(super) agents: crate::config::AgentsSidebarConfig,
    pub(super) agent_panel_sort: crate::config::AgentPanelSortConfig,
    pub(super) status_indicators: crate::config::StatusIndicatorStyle,
    pub(super) sound_enabled: bool,
    pub(super) toast_delivery: crate::config::ToastDelivery,
    pub(super) toast_delay_seconds: u64,
    pub(super) toast_position: crate::config::ToastHerdrPosition,
    pub(super) theme_name: String,
    pub(super) theme_runtime: crate::app::state::ThemeRuntimeConfig,
    pub(super) palette: Palette,
    pub(super) keybinds: LiveKeybindConfig,
    pub(super) prompt_new_tab_name: bool,
    pub(super) prompt_new_workspace_name: bool,
    pub(super) confirm_close: bool,
    pub(super) mouse_capture: bool,
    pub(super) mouse_scroll_lines: usize,
    pub(super) right_click_passthrough_modifiers: Option<crossterm::event::KeyModifiers>,
    pub(super) worktree_directory: std::path::PathBuf,
    pub(super) preferences_path: Option<std::path::PathBuf>,
    pub(super) preferences: preferences::ClientChromePreferences,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ClientShellLayout {
    pub sidebar: Rect,
    pub tab_bar: Rect,
    pub mobile_header: Rect,
    pub pane_surface: Rect,
}

#[derive(Default)]
pub(super) struct ShellHitMap {
    pub(super) workspaces: Vec<WorkspaceHit>,
    pub(super) workspace_body: Rect,
    pub(super) workspace_scrollbar: Rect,
    pub(super) workspace_scroll_metrics: Option<crate::pane::ScrollMetrics>,
    pub(super) workspace_max_scroll: usize,
    pub(super) tabs: Vec<(Rect, String)>,
    pub(super) panes: Vec<PaneHit>,
    pub(super) pane_splits: Vec<PaneSplitHit>,
    pub(super) agents: Vec<(Rect, String)>,
    pub(super) agent_body: Rect,
    pub(super) agent_scrollbar: Rect,
    pub(super) agent_scroll_metrics: Option<crate::pane::ScrollMetrics>,
    pub(super) agent_max_scroll: usize,
    pub(super) agent_sort_toggle: Rect,
    pub(super) sidebar_divider: Rect,
    pub(super) sidebar_section_divider: Rect,
    pub(super) sidebar_toggle: Rect,
    pub(super) new_workspace: Rect,
    pub(super) new_tab: Rect,
    pub(super) tab_scroll_left: Rect,
    pub(super) tab_scroll_right: Rect,
    pub(super) global_launcher: Rect,
    pub(super) notification_toast: Rect,
    pub(super) global_menu_rows: Vec<(Rect, usize)>,
    pub(super) context_menu_rows: Vec<(Rect, usize)>,
    pub(super) overlay_primary: Rect,
    pub(super) overlay_clear: Rect,
    pub(super) overlay_cancel: Rect,
    pub(super) navigator_popup: Rect,
    pub(super) navigator_search: Rect,
    pub(super) navigator_rows: Vec<(Rect, usize)>,
    pub(super) worktree_search: Rect,
    pub(super) worktree_rows: Vec<(Rect, usize)>,
    pub(super) help_popup: Rect,
    pub(super) help_scrollbar: Rect,
    pub(super) help_scroll_metrics: Option<crate::pane::ScrollMetrics>,
    pub(super) help_max_scroll: usize,
    pub(super) settings_popup: Rect,
    pub(super) settings_tabs: Vec<(Rect, ClientSettingsSection)>,
    pub(super) settings_choices: Vec<(Rect, usize)>,
}

#[derive(Clone)]
pub(super) struct PaneHit {
    pub(super) rect: Rect,
    pub(super) inner_rect: Rect,
    pub(super) pane_id: String,
    pub(super) mouse_reporting: bool,
    pub(super) sgr_pixel_mouse: bool,
    pub(super) pixel_width: u32,
    pub(super) pixel_height: u32,
}

#[derive(Clone)]
pub(super) struct PaneSplitHit {
    pub(super) direction: crate::protocol::PaneSurfaceSplitDirection,
    pub(super) pos: u16,
    pub(super) area: Rect,
    pub(super) hit_rect: Rect,
    pub(super) path: Vec<bool>,
    pub(super) topology_signature: u64,
}

pub(super) struct ClientPaneMouseGesture {
    pub(super) hit: PaneHit,
    pub(super) button: crossterm::event::MouseButton,
    pub(super) stripped_modifiers: crossterm::event::KeyModifiers,
}

pub(super) struct ClientWorkspacePress {
    pub(super) workspace_id: String,
    pub(super) start_column: u16,
    pub(super) start_row: u16,
}

pub(super) struct ClientTabPress {
    pub(super) tab_id: String,
    pub(super) workspace_id: String,
    pub(super) start_column: u16,
    pub(super) start_row: u16,
}

pub(super) enum ClientChromeDrag {
    SidebarWidth,
    SidebarSection,
    WorkspaceScrollbar {
        grab_row_offset: u16,
    },
    AgentScrollbar {
        grab_row_offset: u16,
    },
    HelpScrollbar {
        grab_row_offset: u16,
    },
    Tab {
        tab_id: String,
        workspace_id: String,
        insert_index: Option<usize>,
    },
    Workspace {
        source_workspace_id: String,
        target: Option<(Option<String>, u16)>,
    },
    PaneSplit {
        hit: PaneSplitHit,
        tab_id: String,
        grab_offset: i32,
        last_sent_ratio: Option<f32>,
        last_sent_at: Option<std::time::Instant>,
    },
}

pub(super) struct WorkspaceHit {
    pub(super) rect: Rect,
    pub(super) workspace_id: String,
    pub(super) indented: bool,
    pub(super) group_toggle: Option<(Rect, String)>,
}

#[derive(Debug)]
pub(crate) enum ClientShellAction {
    Endpoint {
        boot_id: String,
        request: Box<crate::api::schema::Request>,
    },
    Keybind(crate::input::KeybindAction),
    CustomCommand(Box<crate::config::CustomCommandKeybind>),
}

#[derive(Default)]
pub(crate) struct ClientShellInput {
    pub detach: bool,
    pub repaint: bool,
    pub resize: bool,
    pub query_host_appearance: bool,
    pub requests: Vec<ClientMessage>,
    pub actions: Vec<ClientShellAction>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ClientShellMode {
    Terminal,
    Prefix,
    Navigate,
    Resize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ClientShellOverlayKind {
    Rename,
    ConfirmClose,
    Help,
    Navigator,
    WorktreeCreate,
    WorktreeOpen,
    WorktreeRemove,
    ContextMenu,
    GlobalMenu,
    Settings,
}

#[derive(Debug)]
pub(super) enum ClientRenameTarget {
    NewWorkspace {
        cwd: Option<String>,
        suggested_name: String,
    },
    Workspace {
        workspace_id: String,
    },
    NewTab {
        workspace_id: String,
        default_name: String,
    },
    Tab {
        tab_id: String,
        auto_name: bool,
        original_name: String,
    },
    Pane {
        pane_id: String,
    },
}

#[derive(Debug)]
pub(super) struct ClientRenameOverlay {
    pub(super) title: &'static str,
    pub(super) input: String,
    pub(super) replace_on_type: bool,
    pub(super) target: ClientRenameTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClientNavigatorFilter {
    Blocked,
    Working,
    Idle,
    Done,
}

#[derive(Clone, Debug)]
pub(super) enum ClientNavigatorTarget {
    Workspace(String),
    Tab(String),
    Pane(String),
}

#[derive(Clone, Debug)]
pub(super) struct ClientNavigatorRow {
    pub(super) depth: u8,
    pub(super) label: String,
    pub(super) meta: String,
    pub(super) status: crate::api::schema::AgentStatus,
    pub(super) current: bool,
    pub(super) target: ClientNavigatorTarget,
}

#[derive(Debug)]
pub(super) struct ClientNavigatorOverlay {
    pub(super) query: String,
    pub(super) search_focused: bool,
    pub(super) selected: usize,
    pub(super) scroll: usize,
    pub(super) filter: Option<ClientNavigatorFilter>,
    pub(super) expanded_workspaces: HashSet<String>,
}

#[derive(Debug)]
pub(super) struct ClientHelpOverlay {
    pub(super) query: String,
    pub(super) search_focused: bool,
    pub(super) scroll: usize,
}

#[derive(Debug)]
pub(super) struct ClientGlobalMenuOverlay {
    pub(super) highlighted: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ClientSettingsSection {
    Theme,
    Indicators,
    Sound,
    Toast,
    Integrations,
}

impl ClientSettingsSection {
    pub(super) const ALL: &[Self] = &[
        Self::Theme,
        Self::Indicators,
        Self::Sound,
        Self::Toast,
        Self::Integrations,
    ];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Theme => "theme",
            Self::Indicators => "indicators",
            Self::Sound => "sound",
            Self::Toast => "toasts",
            Self::Integrations => "integrations",
        }
    }
}

#[derive(Debug)]
pub(super) struct ClientSettingsOverlay {
    pub(super) section: ClientSettingsSection,
    pub(super) selected: usize,
    pub(super) original_theme_name: String,
    pub(super) original_palette: Palette,
    pub(super) integrations: Vec<crate::api::schema::IntegrationInfo>,
    pub(super) integration_messages: Vec<String>,
    pub(super) loading_integrations: bool,
    pub(super) installing_integrations: bool,
}

#[derive(Debug)]
pub(super) struct ClientWorktreeCreateOverlay {
    pub(super) source_workspace_id: String,
    pub(super) repo_name: String,
    pub(super) branch: String,
    pub(super) checkout_path: String,
    pub(super) replace_on_type: bool,
    pub(super) error: Option<String>,
    pub(super) creating: bool,
}

#[derive(Debug, Clone)]
pub(super) struct ClientWorktreeOpenEntry {
    pub(super) path: String,
    pub(super) branch: Option<String>,
    pub(super) is_linked_worktree: bool,
    pub(super) is_detached: bool,
    pub(super) open_workspace_id: Option<String>,
    pub(super) label: String,
}

impl ClientWorktreeOpenEntry {
    pub(super) fn status_label(&self) -> &'static str {
        if self.open_workspace_id.is_some() {
            "open"
        } else if self.branch.is_some() {
            ""
        } else if self.is_detached && self.is_linked_worktree {
            "detached"
        } else {
            "root"
        }
    }

    pub(super) fn matches_query(&self, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        query.is_empty()
            || format!(
                "{} {} {} {}",
                self.label,
                self.branch.as_deref().unwrap_or_default(),
                self.path,
                self.status_label()
            )
            .to_lowercase()
            .contains(&query)
    }
}

#[derive(Debug)]
pub(super) struct ClientWorktreeOpenOverlay {
    pub(super) source_workspace_id: String,
    pub(super) entries: Vec<ClientWorktreeOpenEntry>,
    pub(super) selected: usize,
    pub(super) query: String,
    pub(super) search_focused: bool,
    pub(super) error: Option<String>,
    pub(super) opening: bool,
}

impl ClientWorktreeOpenOverlay {
    pub(super) fn filtered_indices(&self) -> Vec<usize> {
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| entry.matches_query(&self.query).then_some(index))
            .collect()
    }

    pub(super) fn selected_entry_index(&self) -> Option<usize> {
        let filtered = self.filtered_indices();
        filtered
            .contains(&self.selected)
            .then_some(self.selected)
            .or_else(|| filtered.first().copied())
    }
}

#[derive(Debug)]
pub(super) struct ClientWorktreeRemoveOverlay {
    pub(super) workspace_id: String,
    pub(super) path: String,
    pub(super) error: Option<String>,
    pub(super) removing: bool,
    pub(super) force_confirmation: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ClientContextMenuAction {
    Rename,
    Close,
    NewWorktree,
    OpenWorktree,
    RemoveWorktree,
    ToggleGroup,
    NewTab,
    RenamePane,
    ClearPaneName,
    SwapWithFocusedPane,
    SplitRight,
    SplitDown,
    Zoom,
    ToggleRightClickPassthrough,
    ClosePane,
}

#[derive(Debug)]
pub(super) enum ClientContextMenuTarget {
    Workspace {
        workspace_id: String,
        is_git: bool,
        is_linked_worktree: bool,
        has_worktree_children: bool,
        collapsed: bool,
    },
    Tab {
        tab_id: String,
        workspace_id: String,
    },
    Pane {
        pane_id: String,
        workspace_id: String,
        source_pane_id: Option<String>,
        has_manual_label: bool,
        right_click_passthrough: bool,
    },
}

#[derive(Debug)]
pub(super) struct ClientContextMenuOverlay {
    pub(super) target: ClientContextMenuTarget,
    pub(super) x: u16,
    pub(super) y: u16,
    pub(super) highlighted: usize,
}

pub(super) struct ClientContextMenuItem {
    pub(super) label: &'static str,
    pub(super) action: ClientContextMenuAction,
}

#[derive(Debug)]
pub(super) struct ClientConfirmCloseOverlay {
    pub(super) workspace_id: String,
    pub(super) title: String,
    pub(super) detail: String,
}

#[derive(Debug)]
pub(super) enum ClientShellOverlay {
    Rename(ClientRenameOverlay),
    ConfirmClose(ClientConfirmCloseOverlay),
    Help(ClientHelpOverlay),
    Navigator(ClientNavigatorOverlay),
    WorktreeCreate(ClientWorktreeCreateOverlay),
    WorktreeOpen(ClientWorktreeOpenOverlay),
    WorktreeRemove(ClientWorktreeRemoveOverlay),
    ContextMenu(ClientContextMenuOverlay),
    GlobalMenu(ClientGlobalMenuOverlay),
    Settings(ClientSettingsOverlay),
}

impl ClientShellOverlay {
    pub(super) fn kind(&self) -> ClientShellOverlayKind {
        match self {
            Self::Rename(_) => ClientShellOverlayKind::Rename,
            Self::ConfirmClose(_) => ClientShellOverlayKind::ConfirmClose,
            Self::Help(_) => ClientShellOverlayKind::Help,
            Self::Navigator(_) => ClientShellOverlayKind::Navigator,
            Self::WorktreeCreate(_) => ClientShellOverlayKind::WorktreeCreate,
            Self::WorktreeOpen(_) => ClientShellOverlayKind::WorktreeOpen,
            Self::WorktreeRemove(_) => ClientShellOverlayKind::WorktreeRemove,
            Self::ContextMenu(_) => ClientShellOverlayKind::ContextMenu,
            Self::GlobalMenu(_) => ClientShellOverlayKind::GlobalMenu,
            Self::Settings(_) => ClientShellOverlayKind::Settings,
        }
    }
}

#[derive(Debug)]
pub(super) enum PendingEndpointKind {
    Generic,
    ReloadConfig,
    IntegrationList,
    IntegrationInstall,
    PrepareWorktreeCreate { workspace_id: String },
    PrepareWorktreeOpen { workspace_id: String },
    PrepareWorktreeRemove { workspace_id: String },
    WorktreeCreate,
    WorktreeOpen,
    WorktreeRemove { forced: bool },
}

pub(super) struct PendingEndpointRequest {
    pub(super) boot_id: String,
    pub(super) confirmation_workspace_id: Option<String>,
    pub(super) kind: PendingEndpointKind,
}

pub(crate) struct ClientShellEndpointError {
    pub code: Option<String>,
    pub message: String,
}

pub(crate) enum ClientShellNotificationEffect {
    Sound {
        sound: crate::sound::Sound,
        agent: Option<String>,
    },
    Terminal {
        title: String,
        body: Option<String>,
    },
    System {
        title: String,
        body: Option<String>,
    },
}

pub(super) struct ClientPendingNotification {
    pub(super) event: SemanticNotification,
    pub(super) deadline: std::time::Instant,
    pub(super) validate_state: bool,
}

pub(super) struct ClientVisibleNotification {
    pub(super) event: SemanticNotification,
    pub(super) deadline: std::time::Instant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ClientInputContext {
    pub(super) mode: ClientShellMode,
    pub(super) overlay: Option<ClientShellOverlayKind>,
}

type ClientInputLeases = crate::input::InputLeaseTable<u8, ClientInputContext, String>;

pub(crate) struct ClientShellState {
    pub(super) config: ClientShellConfig,
    pub(super) snapshot: Option<Box<ClientShellSnapshot>>,
    pub(super) pane_surface: Option<PaneSurfaceFrame>,
    pub(super) sidebar_collapsed: bool,
    pub(super) sidebar_collapsed_manual: bool,
    pub(super) sidebar_width: u16,
    pub(super) sidebar_width_manual: bool,
    pub(super) sidebar_section_split: f32,
    pub(super) sidebar_section_split_manual: bool,
    pub(super) agent_panel_sort_manual: bool,
    pub(super) last_sidebar_divider_click: Option<std::time::Instant>,
    pub(super) chrome_drag: Option<ClientChromeDrag>,
    pub(super) workspace_press: Option<ClientWorkspacePress>,
    pub(super) tab_press: Option<ClientTabPress>,
    pub(super) collapsed_groups: HashSet<String>,
    pub(super) workspace_scroll: usize,
    pub(super) agent_scroll: usize,
    pub(super) tab_scroll: usize,
    pub(super) reveal_focused_tab: bool,
    pub(super) last_tab_bar_width: Option<u16>,
    pub(super) hits: ShellHitMap,
    pub(super) mode: ClientShellMode,
    pub(super) navigate_workspace_id: Option<String>,
    pub(super) overlay: Option<ClientShellOverlay>,
    pub(super) previous_pane_id: Option<String>,
    pub(super) pane_mouse_gesture: Option<ClientPaneMouseGesture>,
    pub(super) host_mouse_pixels: Option<crate::input::mouse::HostPixels>,
    pub(super) input_leases: ClientInputLeases,
    pub(super) next_request_id: u64,
    pub(super) pending_requests: HashMap<String, PendingEndpointRequest>,
    pub(super) pending_integration_installs: usize,
    pub(super) pending_notifications: Vec<ClientPendingNotification>,
    pub(super) visible_notification: Option<ClientVisibleNotification>,
    pub(super) outer_focused: Option<bool>,
    pub(super) endpoint_error: Option<String>,
}

#[derive(Clone, Copy)]
pub(super) struct WorkspaceEntry {
    pub(super) index: usize,
    pub(super) indented: bool,
    pub(super) last_child: bool,
}

impl ClientShellState {
    pub(crate) fn new(mut config: ClientShellConfig) -> Self {
        let preferences = config.preferences;
        let sidebar_collapsed = preferences
            .sidebar_collapsed
            .unwrap_or(config.sidebar_start_collapsed);
        let (min_width, max_width) = crate::config::validated_sidebar_bounds(
            config.sidebar_min_width,
            config.sidebar_max_width,
        )
        .unwrap_or((18, 36));
        let sidebar_width = preferences
            .sidebar_width
            .unwrap_or(config.sidebar_width)
            .clamp(min_width, max_width);
        let sidebar_section_split = preferences
            .sidebar_section_split
            .filter(|split| split.is_finite())
            .map(|split| split.clamp(0.1, 0.9))
            .unwrap_or(0.5);
        if let Some(sort) = preferences.agent_panel_sort {
            config.agent_panel_sort = sort;
        }
        Self {
            config,
            snapshot: None,
            pane_surface: None,
            sidebar_collapsed,
            sidebar_collapsed_manual: preferences.sidebar_collapsed.is_some(),
            sidebar_width,
            sidebar_width_manual: preferences.sidebar_width.is_some(),
            sidebar_section_split,
            sidebar_section_split_manual: preferences.sidebar_section_split.is_some(),
            agent_panel_sort_manual: preferences.agent_panel_sort.is_some(),
            last_sidebar_divider_click: None,
            chrome_drag: None,
            workspace_press: None,
            tab_press: None,
            collapsed_groups: HashSet::new(),
            workspace_scroll: 0,
            agent_scroll: 0,
            tab_scroll: 0,
            reveal_focused_tab: true,
            last_tab_bar_width: None,
            hits: ShellHitMap::default(),
            mode: ClientShellMode::Terminal,
            navigate_workspace_id: None,
            overlay: None,
            previous_pane_id: None,
            pane_mouse_gesture: None,
            host_mouse_pixels: None,
            input_leases: ClientInputLeases::default(),
            next_request_id: 1,
            pending_requests: HashMap::new(),
            pending_integration_installs: 0,
            pending_notifications: Vec::new(),
            visible_notification: None,
            outer_focused: None,
            endpoint_error: None,
        }
    }

    fn focused_tab_count(&self) -> usize {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return 0;
        };
        snapshot
            .tabs
            .iter()
            .filter(|tab| {
                Some(tab.workspace_id.as_str()) == snapshot.focused_workspace_id.as_deref()
            })
            .count()
    }

    pub(super) fn layout(&self, cols: u16, rows: u16) -> ClientShellLayout {
        self.config.layout(
            cols,
            rows,
            self.sidebar_collapsed,
            self.focused_tab_count(),
            self.sidebar_width,
        )
    }

    pub(crate) fn surface_size(&self, cols: u16, rows: u16) -> ClientSurfaceSize {
        let surface = self.layout(cols, rows).pane_surface;
        ClientSurfaceSize {
            cols: surface.width.max(1),
            rows: surface.height.max(1),
        }
    }

    pub(crate) fn set_snapshot(&mut self, snapshot: Box<ClientShellSnapshot>) {
        if self
            .pane_surface
            .as_ref()
            .is_none_or(|surface| surface.projection_revision != snapshot.revision)
        {
            self.hits = ShellHitMap::default();
        }
        if self
            .snapshot
            .as_ref()
            .is_some_and(|current| current.boot_id != snapshot.boot_id)
        {
            self.pane_surface = None;
            self.chrome_drag = None;
            self.workspace_press = None;
            self.tab_press = None;
            self.collapsed_groups.clear();
            self.workspace_scroll = 0;
            self.agent_scroll = 0;
            self.tab_scroll = 0;
            self.reveal_focused_tab = true;
            self.last_tab_bar_width = None;
            self.pending_requests.clear();
            self.pending_integration_installs = 0;
            self.pending_notifications.clear();
            self.visible_notification = None;
            self.endpoint_error = None;
            self.navigate_workspace_id = None;
            self.overlay = None;
            self.previous_pane_id = None;
            self.pane_mouse_gesture = None;
            self.host_mouse_pixels = None;
        } else if let Some(previous) = self
            .snapshot
            .as_deref()
            .and_then(|current| current.focused_pane_id.as_ref())
            .filter(|previous| Some(previous.as_str()) != snapshot.focused_pane_id.as_deref())
        {
            self.previous_pane_id = Some(previous.clone());
        }
        let tab_layout_changed = self.snapshot.as_deref().is_none_or(|current| {
            current.tabs.len() != snapshot.tabs.len()
                || current
                    .tabs
                    .iter()
                    .zip(&snapshot.tabs)
                    .any(|(left, right)| {
                        left.tab_id != right.tab_id
                            || left.workspace_id != right.workspace_id
                            || left.label != right.label
                            || left.zoomed != right.zoomed
                    })
                || render::tab_bar_status_width(current) != render::tab_bar_status_width(&snapshot)
        });
        if tab_layout_changed
            || self
                .snapshot
                .as_deref()
                .and_then(|current| current.focused_tab_id.as_deref())
                != snapshot.focused_tab_id.as_deref()
        {
            self.reveal_focused_tab = true;
        }
        if self.mode == ClientShellMode::Navigate
            && self.navigate_workspace_id.as_ref().is_none_or(|selected| {
                !snapshot
                    .workspaces
                    .iter()
                    .any(|workspace| &workspace.workspace_id == selected)
            })
        {
            self.navigate_workspace_id = snapshot.focused_workspace_id.clone();
        }
        self.snapshot = Some(snapshot);
    }

    pub(crate) fn set_pane_surface(&mut self, surface: PaneSurfaceFrame) {
        if self
            .snapshot
            .as_ref()
            .is_none_or(|snapshot| snapshot.revision != surface.projection_revision)
        {
            self.hits = ShellHitMap::default();
        }
        self.pane_surface = Some(surface);
    }

    pub(crate) fn invalidate_pane_surface(&mut self) {
        self.pane_surface = None;
        self.hits = ShellHitMap::default();
        self.host_mouse_pixels = None;
    }
}
