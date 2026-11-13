use crate::backend::{BackendCommand, BackendEvent, Direction, RemoteFileInfo, SftpWorker};
use crate::model::{
    AuthMethod, ConnectionProfile, FileEntry, Settings, ThemePreference, new_id, sort_entries,
};
use crate::util::{
    format_mode, format_mode_symbolic, format_progress, format_size, format_speed, format_time,
    join_remote, path_file_name, remote_parent,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::*;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::*;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::theme::{Theme, ThemeMode};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, InteractiveElementExt as _, Sizable, StyledExt, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};
use std::time::{Instant, UNIX_EPOCH};

static APP_ICON_IMAGE: LazyLock<Arc<Image>> = LazyLock::new(|| {
    Arc::new(Image::from_bytes(
        ImageFormat::Png,
        include_bytes!("../assets/app-icon.png").to_vec(),
    ))
});

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    Local,
    Remote,
}

#[derive(Clone)]
struct DragItem {
    source: Side,
    path: String,
    name: String,
    is_dir: bool,
}

#[derive(Clone, Debug)]
enum SidebarDragItem {
    Group { id: String, name: String },
    Connection { id: String, name: String },
}

impl SidebarDragItem {
    fn name(&self) -> &str {
        match self {
            Self::Group { name, .. } | Self::Connection { name, .. } => name,
        }
    }

    fn is_group(&self) -> bool {
        matches!(self, Self::Group { .. })
    }
}

/// Where a sidebar drag would be inserted if the pointer is released now.
#[derive(Clone, Debug, PartialEq, Eq)]
enum SidebarDropTarget {
    /// Insert a group before or after another group header.
    Group { id: String, before: bool },
    /// Insert a connection before or after another connection row.
    Connection { id: String, before: bool },
    /// Append a connection to the end of a group.
    GroupEnd { group_id: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StatusKind {
    Info,
    Success,
    Error,
}

#[derive(Clone)]
struct ConnectedInfo {
    profile_id: String,
    name: String,
    endpoint: String,
    username: String,
}

struct LocalPane {
    path: PathBuf,
    all_entries: Arc<Vec<FileEntry>>,
    entries: Arc<Vec<FileEntry>>,
    selected: Option<usize>,
    error: Option<String>,
}

struct RemotePane {
    path: String,
    all_entries: Arc<Vec<FileEntry>>,
    entries: Arc<Vec<FileEntry>>,
    selected: Option<usize>,
    loading: bool,
    error: Option<String>,
}

struct SessionState {
    info: ConnectedInfo,
    pane: RemotePane,
    scroll: UniformListScrollHandle,
    home: String,
}

struct TransferView {
    id: u64,
    name: String,
    direction: Direction,
    total: u64,
    transferred: u64,
    speed: f64,
    last_sample: Instant,
    last_bytes: u64,
    done: bool,
    ok: bool,
    cancelled: bool,
    cancelling: bool,
    message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToastKind {
    Success,
    Cancel,
    Error,
}

struct ToastItem {
    id: u64,
    message: SharedString,
    kind: ToastKind,
}

enum ContextTarget {
    Group {
        id: String,
        name: String,
    },
    Connection {
        id: String,
        name: String,
    },
    File {
        side: Side,
        session_id: Option<String>,
        path: String,
        name: String,
        is_dir: bool,
        permissions: Option<u32>,
    },
}

struct ContextMenuState {
    position: gpui_kit::Point<Pixels>,
    target: ContextTarget,
}

struct ConnectionForm {
    editing_id: Option<String>,
    copying: bool,
    group_id: Option<String>,
    name: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    username: Entity<InputState>,
    password: Entity<InputState>,
    key_path: Entity<InputState>,
    passphrase: Entity<InputState>,
    remote_dir: Entity<InputState>,
    use_key: bool,
    error: Option<SharedString>,
}

struct GroupForm {
    editing_id: Option<String>,
    name: Entity<InputState>,
    error: Option<SharedString>,
}

struct RenameForm {
    side: Side,
    session_id: Option<String>,
    path: String,
    name: Entity<InputState>,
    error: Option<SharedString>,
}

struct ChmodForm {
    session_id: String,
    path: String,
    name: String,
    /// Permission bits for owner / group / other, masked to `0o777`.
    mode: u32,
}

struct FileInfoForm {
    name: String,
    path: String,
    loading: bool,
    info: Option<RemoteFileInfo>,
    error: Option<String>,
}

#[derive(Clone)]
struct QueuedUpload {
    session_id: String,
    local: PathBuf,
    remote: String,
    name: String,
}

#[derive(Clone)]
enum ConfirmAction {
    DeleteConnection(String),
    DeleteGroup(String),
    DeleteLocalFile {
        path: String,
        name: String,
        is_dir: bool,
    },
    DeleteRemoteFile {
        session_id: String,
        path: String,
        name: String,
    },
    UploadOverwrite(QueuedUpload),
}

struct ConfirmDialog {
    title: SharedString,
    message: SharedString,
    action: ConfirmAction,
    /// Label of the confirm button, e.g. `删除` or `覆盖`.
    confirm_label: SharedString,
    /// Whether the confirm button uses the danger style.
    danger: bool,
}

enum DialogState {
    None,
    Connection(ConnectionForm),
    Group(GroupForm),
    Rename(RenameForm),
    Chmod(ChmodForm),
    FileInfo(FileInfoForm),
    Confirm(ConfirmDialog),
}

impl Default for DialogState {
    fn default() -> Self {
        Self::None
    }
}

pub struct RgFtpApp {
    settings: Settings,
    worker: SftpWorker,
    _event_task: Task<()>,
    local: LocalPane,
    local_scroll: UniformListScrollHandle,
    sessions: HashMap<String, SessionState>,
    active_session: Option<String>,
    connecting: HashSet<String>,
    dialog: DialogState,
    transfer: Option<TransferView>,
    pending_transfers: usize,
    status: SharedString,
    status_kind: StatusKind,
    collapsed_groups: HashSet<String>,
    context_menu: Option<ContextMenuState>,
    show_hidden_files: bool,
    local_search: Entity<InputState>,
    remote_search: Entity<InputState>,
    local_query: String,
    remote_query: String,
    _search_subscriptions: Vec<Subscription>,
    toasts: Vec<ToastItem>,
    next_toast_id: u64,
    pending_overwrite: std::collections::VecDeque<QueuedUpload>,
    sidebar_drop_target: Option<SidebarDropTarget>,
}

impl RgFtpApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = Settings::load();

        crate::taskbar::init(window);

        match settings.theme {
            Some(ThemePreference::Light) => {
                Theme::change(ThemeMode::Light, Some(window), cx);
            }
            Some(ThemePreference::Dark) => {
                Theme::change(ThemeMode::Dark, Some(window), cx);
            }
            None => Theme::sync_system_appearance(Some(window), cx),
        }

        let local_path = settings
            .local_dir
            .clone()
            .filter(|path| path.is_dir())
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."));

        let collapsed_groups: HashSet<String> = settings
            .groups
            .iter()
            .map(|group| group.id.clone())
            .collect();

        let (event_tx, event_rx) = async_channel::unbounded();
        let worker = SftpWorker::spawn(event_tx);

        let local_search = make_input(window, cx, "模糊搜索本地文件", "", false);
        let remote_search = make_input(window, cx, "模糊搜索远程文件", "", false);

        let mut app = Self {
            settings,
            worker,
            _event_task: Task::ready(()),
            local: LocalPane {
                path: local_path.clone(),
                all_entries: Arc::new(Vec::new()),
                entries: Arc::new(Vec::new()),
                selected: None,
                error: None,
            },
            local_scroll: UniformListScrollHandle::new(),
            sessions: HashMap::new(),
            active_session: None,
            connecting: HashSet::new(),
            dialog: DialogState::None,
            transfer: None,
            pending_transfers: 0,
            status: "从左侧选择一个连接开始".into(),
            status_kind: StatusKind::Info,
            collapsed_groups,
            context_menu: None,
            show_hidden_files: false,
            local_search: local_search.clone(),
            remote_search: remote_search.clone(),
            local_query: String::new(),
            remote_query: String::new(),
            _search_subscriptions: Vec::new(),
            toasts: Vec::new(),
            next_toast_id: 1,
            pending_overwrite: std::collections::VecDeque::new(),
            sidebar_drop_target: None,
        };

        app._search_subscriptions = vec![
            cx.subscribe_in(
                &local_search,
                window,
                |this: &mut Self, state, event: &gpui_kit::component::input::InputEvent, _, cx| {
                    if matches!(event, gpui_kit::component::input::InputEvent::Change) {
                        let query = state.read(cx).value().to_string();
                        this.set_local_query(query, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &remote_search,
                window,
                |this: &mut Self, state, event: &gpui_kit::component::input::InputEvent, _, cx| {
                    if matches!(event, gpui_kit::component::input::InputEvent::Change) {
                        let query = state.read(cx).value().to_string();
                        this.set_remote_query(query, cx);
                    }
                },
            ),
        ];

        app.load_local_dir(local_path, cx);

        app._event_task = cx.spawn(async move |this, cx| {
            while let Ok(event) = event_rx.recv().await {
                if this
                    .update(cx, |app, cx| app.handle_backend_event(event, cx))
                    .is_err()
                {
                    break;
                }
            }
        });

        app
    }

    fn handle_backend_event(&mut self, event: BackendEvent, cx: &mut Context<Self>) {
        match event {
            BackendEvent::Connected { profile_id, home } => {
                let Some(profile) = self.settings.connection(&profile_id).cloned() else {
                    return;
                };
                self.connecting.remove(&profile_id);
                self.sessions.insert(
                    profile_id.clone(),
                    SessionState {
                        info: ConnectedInfo {
                            profile_id: profile_id.clone(),
                            name: profile.name.clone(),
                            endpoint: profile.endpoint(),
                            username: profile.username.clone(),
                        },
                        pane: RemotePane {
                            path: home.clone(),
                            all_entries: Arc::new(Vec::new()),
                            entries: Arc::new(Vec::new()),
                            selected: None,
                            loading: true,
                            error: None,
                        },
                        scroll: UniformListScrollHandle::new(),
                        home,
                    },
                );
                self.active_session = Some(profile_id);
                self.status = format!("已连接 {}", profile.endpoint()).into();
                self.status_kind = StatusKind::Success;
            }
            BackendEvent::ConnectFailed { profile_id, error } => {
                let profile_name = self
                    .settings
                    .connection(&profile_id)
                    .map(|profile| profile.name.clone())
                    .unwrap_or_else(|| "服务器".to_string());
                self.connecting.remove(&profile_id);
                let was_active = self.active_session.as_deref() == Some(profile_id.as_str());
                self.sessions.remove(&profile_id);
                if was_active {
                    self.select_fallback_session();
                }
                self.status = format!("连接 {profile_name} 失败：{error}").into();
                self.status_kind = StatusKind::Error;
                self.push_toast(
                    ToastKind::Error,
                    format!("连接 {profile_name} 失败：{error}"),
                    cx,
                );
            }
            BackendEvent::Listed {
                session_id,
                path,
                entries,
            } => {
                let all_entries = Arc::new(entries);
                let visible_entries = Arc::new(filter_entries(
                    &all_entries,
                    &self.remote_query,
                    self.show_hidden_files,
                ));
                if let Some(session) = self.sessions.get_mut(&session_id) {
                    session.pane.path = path;
                    session.pane.all_entries = all_entries;
                    session.pane.entries = visible_entries;
                    session.pane.selected = None;
                    session.pane.loading = false;
                    session.pane.error = None;
                    session.scroll.scroll_to_item(0, ScrollStrategy::Top);
                }
                Self::refresh_scrollbar_layout(cx);
            }
            BackendEvent::ListFailed {
                session_id,
                path,
                error,
            } => {
                if let Some(session) = self.sessions.get_mut(&session_id) {
                    session.pane.path = path;
                    session.pane.loading = false;
                    session.pane.error = Some(error.clone());
                }
                if self.active_session.as_deref() == Some(session_id.as_str()) {
                    self.status = error.into();
                    self.status_kind = StatusKind::Error;
                }
            }
            BackendEvent::Disconnected {
                session_id,
                message,
            } => {
                self.connecting.remove(&session_id);
                let was_active = self.active_session.as_deref() == Some(session_id.as_str());
                let removed = self.sessions.remove(&session_id);
                if was_active {
                    self.select_fallback_session();
                }

                if let Some(session) = removed {
                    let name = session.info.name;
                    if let Some(active_id) = self.active_session.clone() {
                        let active_name = self
                            .sessions
                            .get(&active_id)
                            .map(|session| session.info.name.clone())
                            .unwrap_or_default();
                        self.status = format!("{message}：{name}，当前连接 {active_name}").into();
                    } else {
                        self.status = format!("{message}：{name}").into();
                    }
                    self.status_kind = StatusKind::Info;
                } else if was_active {
                    self.status = message.into();
                    self.status_kind = StatusKind::Info;
                }
            }
            BackendEvent::TransferStarted {
                id,
                name,
                direction,
                total,
            } => {
                self.transfer = Some(TransferView {
                    id,
                    name: name.clone(),
                    direction,
                    total,
                    transferred: 0,
                    speed: 0.0,
                    last_sample: Instant::now(),
                    last_bytes: 0,
                    done: false,
                    ok: true,
                    cancelled: false,
                    cancelling: false,
                    message: String::new(),
                });
                self.status = format!("{} {}", direction.label(), name).into();
                self.status_kind = StatusKind::Info;
            }
            BackendEvent::TransferTotal { id, total } => {
                if let Some(transfer) = &mut self.transfer
                    && transfer.id == id
                {
                    transfer.total = total;
                }
            }
            BackendEvent::TransferProgress { id, transferred } => {
                if let Some(transfer) = &mut self.transfer
                    && transfer.id == id
                {
                    let now = Instant::now();
                    let elapsed = now.duration_since(transfer.last_sample).as_secs_f64();
                    let delta = transferred.saturating_sub(transfer.last_bytes);
                    if elapsed >= 0.2 {
                        let sample = delta as f64 / elapsed;
                        transfer.speed = if transfer.speed <= 0.0 {
                            sample
                        } else {
                            transfer.speed * 0.7 + sample * 0.3
                        };
                        transfer.last_sample = now;
                        transfer.last_bytes = transferred;
                    }
                    transfer.transferred = transferred;
                }
            }
            BackendEvent::TransferDone {
                session_id,
                id,
                name,
                direction,
                ok,
                cancelled,
                message,
            } => {
                self.pending_transfers = self.pending_transfers.saturating_sub(1);
                if let Some(transfer) = &mut self.transfer
                    && transfer.id == id
                {
                    transfer.done = true;
                    transfer.ok = ok;
                    transfer.cancelled = cancelled;
                    transfer.cancelling = false;
                    transfer.message = message.clone();
                    if transfer.total > 0 && !cancelled {
                        transfer.transferred = transfer.total;
                    }
                }

                if self.pending_transfers == 0 {
                    match direction {
                        Direction::Upload => self.refresh_session_remote(&session_id, cx),
                        Direction::Download => {
                            let path = self.local.path.clone();
                            self.load_local_dir(path, cx);
                        }
                    }
                }

                if cancelled {
                    self.push_toast(
                        ToastKind::Cancel,
                        format!("{}已取消：{}", direction.label(), name),
                        cx,
                    );
                } else if !ok {
                    self.push_toast(
                        ToastKind::Error,
                        format!("{}失败：{}", direction.label(), message),
                        cx,
                    );
                } else {
                    self.push_toast(
                        ToastKind::Success,
                        format!("{}成功：{}", direction.label(), name),
                        cx,
                    );
                }

                // Every outcome is reported by a toast, so the progress row
                // disappears as soon as the transfer finishes.
                self.transfer = None;
            }
            BackendEvent::FileOpFinished {
                session_id,
                ok,
                message,
            } => {
                self.status = message.into();
                self.status_kind = if ok {
                    StatusKind::Success
                } else {
                    StatusKind::Error
                };
                if ok {
                    self.refresh_session_remote(&session_id, cx);
                }
            }
            BackendEvent::FileStat { path, result } => {
                if let DialogState::FileInfo(form) = &mut self.dialog
                    && form.path == path
                {
                    form.loading = false;
                    match result {
                        Ok(info) => {
                            form.info = Some(info);
                            form.error = None;
                        }
                        Err(error) => {
                            form.error = Some(error);
                            form.info = None;
                        }
                    }
                }
            }
            BackendEvent::Error { message } => {
                self.status = message.into();
                self.status_kind = StatusKind::Error;
            }
        }

        self.sync_taskbar_progress();
        cx.notify();
    }

    /// Shows a self-dismissing notification in the top-right corner.
    fn push_toast(
        &mut self,
        kind: ToastKind,
        message: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        let id = self.next_toast_id;
        self.next_toast_id += 1;
        self.toasts.push(ToastItem {
            id,
            message: message.into(),
            kind,
        });
        if self.toasts.len() > 4 {
            self.toasts.remove(0);
        }

        // Errors usually carry a reason, so leave them on screen long enough
        // to be read.
        let duration = match kind {
            ToastKind::Error => std::time::Duration::from_secs(6),
            _ => std::time::Duration::from_secs(3),
        };

        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(duration).await;
            let _ = this.update(cx, |app, cx| {
                app.toasts.retain(|toast| toast.id != id);
                cx.notify();
            });
        })
        .detach();

        cx.notify();
    }

    fn render_toasts(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors;

        v_flex()
            .id("toasts")
            .absolute()
            .top(px(52.0))
            .right(px(16.0))
            .gap_2()
            .children(self.toasts.iter().map(|toast| {
                // The success / danger colors are saturated mid-tone fills, so
                // a dark ink reads much better than the theme's light
                // `*_foreground`. The cancel toast keeps the muted pairing.
                let dark_ink = hsla(0.0, 0.0, 0.06, 0.95);
                let (bg, fg, icon) = match toast.kind {
                    ToastKind::Success => (colors.success, dark_ink, IconName::CircleCheck),
                    ToastKind::Cancel => (colors.muted, colors.foreground, IconName::CircleX),
                    ToastKind::Error => (colors.danger, dark_ink, IconName::CircleAlert),
                };

                h_flex()
                    .w(px(320.0))
                    .px_3()
                    .py_2()
                    .gap_2()
                    .rounded_md()
                    .bg(bg)
                    .text_color(fg)
                    .shadow_lg()
                    .child(Icon::new(icon).small().text_color(fg))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .child(toast.message.clone()),
                    )
                    .into_any_element()
            }))
            .into_any_element()
    }

    /// Whether any upload / download is still running.
    fn is_transferring(&self) -> bool {
        self.pending_transfers > 0
            || self
                .transfer
                .as_ref()
                .is_some_and(|transfer| !transfer.done)
    }

    /// Mirrors the running transfer onto the Windows taskbar button, and
    /// removes the overlay once nothing is in flight.
    fn sync_taskbar_progress(&self) {
        match &self.transfer {
            Some(transfer) if !transfer.done => {
                crate::taskbar::set_progress(transfer.transferred, transfer.total);
            }
            _ if self.pending_transfers > 0 => {
                crate::taskbar::set_progress(0, 0);
            }
            _ => crate::taskbar::clear(),
        }
    }

    fn select_fallback_session(&mut self) {
        self.active_session = self
            .settings
            .connections
            .iter()
            .find(|connection| self.sessions.contains_key(&connection.id))
            .map(|connection| connection.id.clone())
            .or_else(|| self.sessions.keys().next().cloned());
    }

    fn active_session_state(&self) -> Option<&SessionState> {
        self.active_session
            .as_ref()
            .and_then(|id| self.sessions.get(id))
    }

    fn active_session_state_mut(&mut self) -> Option<&mut SessionState> {
        let id = self.active_session.clone()?;
        self.sessions.get_mut(&id)
    }

    /// `uniform_list` only reports its scroll metrics after the first layout
    /// pass, so request a couple of follow-up frames to let the scrollbar
    /// size its thumb from real viewport and content sizes.
    fn refresh_scrollbar_layout(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            for _ in 0..2 {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(80))
                    .await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
        })
        .detach();
    }

    fn load_local_dir(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let read_dir = std::fs::read_dir(&path);
        match read_dir {
            Ok(read_dir) => {
                let mut entries = Vec::new();
                for entry in read_dir.flatten() {
                    let entry_path = entry.path();
                    let name = entry.file_name().to_string_lossy().to_string();
                    let Ok(metadata) = entry.metadata() else {
                        continue;
                    };
                    let is_dir = metadata.is_dir();
                    let hidden = crate::util::is_hidden(&name, &metadata);
                    let modified = metadata
                        .modified()
                        .ok()
                        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                        .map(|duration| duration.as_secs() as i64);
                    entries.push(FileEntry {
                        path: entry_path.to_string_lossy().to_string(),
                        name,
                        is_dir,
                        hidden,
                        size: if is_dir { 0 } else { metadata.len() },
                        modified,
                        permissions: None,
                    });
                }
                sort_entries(&mut entries);
                let all_entries = Arc::new(entries);
                self.local.entries =
                    Arc::new(filter_entries(&all_entries, &self.local_query, false));
                self.local.all_entries = all_entries;
                self.local.path = path.clone();
                self.local.selected = None;
                self.local.error = None;
                self.local_scroll.scroll_to_item(0, ScrollStrategy::Top);
                self.settings.local_dir = Some(path);
                self.settings.save();
            }
            Err(error) => {
                self.local.error = Some(format!("无法打开目录 {}：{error}", path.display()));
                self.local.path = path;
                self.local.entries = Arc::new(Vec::new());
                self.local.all_entries = Arc::new(Vec::new());
                self.local.selected = None;
            }
        }
        Self::refresh_scrollbar_layout(cx);
        cx.notify();
    }

    fn refresh_local(&mut self, cx: &mut Context<Self>) {
        let path = self.local.path.clone();
        self.load_local_dir(path, cx);
    }

    fn local_up(&mut self, cx: &mut Context<Self>) {
        if let Some(parent) = self.local.path.parent() {
            let parent = parent.to_path_buf();
            self.load_local_dir(parent, cx);
        }
    }

    fn local_home(&mut self, cx: &mut Context<Self>) {
        if let Some(home) = dirs::home_dir() {
            self.load_local_dir(home, cx);
        }
    }

    fn refresh_session_remote(&mut self, session_id: &str, cx: &mut Context<Self>) {
        let Some(session) = self.sessions.get_mut(session_id) else {
            return;
        };
        session.pane.loading = true;
        session.pane.error = None;
        let path = session.pane.path.clone();
        self.worker.send(BackendCommand::List {
            session_id: session_id.to_string(),
            path,
        });
        cx.notify();
    }

    fn refresh_remote(&mut self, cx: &mut Context<Self>) {
        let Some(session_id) = self.active_session.clone() else {
            self.status = "尚未连接服务器".into();
            self.status_kind = StatusKind::Info;
            cx.notify();
            return;
        };
        self.refresh_session_remote(&session_id, cx);
    }

    fn remote_up(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.active_session_state() else {
            return;
        };
        let parent = remote_parent(&session.pane.path);
        self.open_remote_dir(parent, cx);
    }

    fn remote_home(&mut self, cx: &mut Context<Self>) {
        if let Some(home) = self
            .active_session_state()
            .map(|session| session.home.clone())
        {
            self.open_remote_dir(home, cx);
        }
    }

    fn open_remote_dir(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(session_id) = self.active_session.clone() else {
            return;
        };
        let Some(session) = self.sessions.get_mut(&session_id) else {
            return;
        };
        session.pane.path = path.clone();
        session.pane.loading = true;
        session.pane.error = None;
        session.pane.selected = None;
        self.worker.send(BackendCommand::List { session_id, path });
        cx.notify();
    }

    fn toggle_hidden_files(&mut self, cx: &mut Context<Self>) {
        self.show_hidden_files = !self.show_hidden_files;
        let show_hidden = self.show_hidden_files;
        let query = self.remote_query.clone();
        for session in self.sessions.values_mut() {
            session.pane.entries = Arc::new(filter_entries(
                &session.pane.all_entries,
                &query,
                show_hidden,
            ));
            session.pane.selected = None;
            session.scroll.scroll_to_item(0, ScrollStrategy::Top);
        }
        cx.notify();
    }

    /// Applies the local search box to the current directory listing.
    fn set_local_query(&mut self, query: String, cx: &mut Context<Self>) {
        self.local_query = query;
        self.local.entries = Arc::new(filter_entries(
            &self.local.all_entries,
            &self.local_query,
            false,
        ));
        self.local.selected = None;
        self.local_scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    /// Applies the remote search box to every open session.
    fn set_remote_query(&mut self, query: String, cx: &mut Context<Self>) {
        self.remote_query = query;
        let query = self.remote_query.clone();
        let show_hidden = self.show_hidden_files;
        for session in self.sessions.values_mut() {
            session.pane.entries = Arc::new(filter_entries(
                &session.pane.all_entries,
                &query,
                show_hidden,
            ));
            session.pane.selected = None;
            session.scroll.scroll_to_item(0, ScrollStrategy::Top);
        }
        cx.notify();
    }

    fn on_entry_click(
        &mut self,
        side: Side,
        index: usize,
        click_count: usize,
        cx: &mut Context<Self>,
    ) {
        match side {
            Side::Local => {
                self.local.selected = Some(index);
                if click_count >= 2
                    && let Some(entry) = self.local.entries.get(index).cloned()
                    && entry.is_dir
                {
                    self.load_local_dir(PathBuf::from(entry.path), cx);
                    return;
                }
            }
            Side::Remote => {
                let Some(session) = self.active_session_state_mut() else {
                    return;
                };
                session.pane.selected = Some(index);
                let entry = session.pane.entries.get(index).cloned();
                if click_count >= 2
                    && let Some(entry) = entry
                    && entry.is_dir
                {
                    self.open_remote_dir(entry.path, cx);
                    return;
                }
            }
        }
        cx.notify();
    }

    fn connect_profile(&mut self, profile_id: &str, cx: &mut Context<Self>) {
        let Some(profile) = self.settings.connection(profile_id).cloned() else {
            return;
        };

        let was_active = self.active_session.as_deref() == Some(profile_id);
        self.connecting.insert(profile_id.to_string());
        self.sessions.remove(profile_id);
        if was_active {
            self.select_fallback_session();
        }
        self.status = format!("正在连接 {} ...", profile.endpoint()).into();
        self.status_kind = StatusKind::Info;
        self.worker.send(BackendCommand::Connect { profile });
        cx.notify();
    }

    fn disconnect_session(&mut self, session_id: &str, cx: &mut Context<Self>) {
        if !self.sessions.contains_key(session_id) {
            return;
        }
        self.connecting.remove(session_id);
        let name = self
            .sessions
            .get(session_id)
            .map(|session| session.info.name.clone())
            .unwrap_or_default();
        self.worker.send(BackendCommand::Disconnect {
            session_id: session_id.to_string(),
        });
        self.status = format!("正在断开 {name}...").into();
        self.status_kind = StatusKind::Info;
        cx.notify();
    }

    fn disconnect(&mut self, cx: &mut Context<Self>) {
        if self.is_transferring() {
            self.status = "传输进行中，无法断开连接".into();
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        }
        if let Some(session_id) = self.active_session.clone() {
            self.disconnect_session(&session_id, cx);
        }
    }

    fn cancel_current_transfer(&mut self, cx: &mut Context<Self>) {
        let Some(transfer) = &mut self.transfer else {
            return;
        };
        if transfer.done || transfer.cancelling {
            return;
        }

        transfer.cancelling = true;
        let name = transfer.name.clone();
        let direction = transfer.direction;
        self.worker.cancel_current_transfer();
        self.status = format!("正在取消{}：{name}", direction.label()).into();
        self.status_kind = StatusKind::Info;
        cx.notify();
    }

    /// True when the current remote listing already contains `name`.
    fn remote_has_entry(&self, session_id: &str, name: &str) -> bool {
        self.sessions.get(session_id).is_some_and(|session| {
            session
                .pane
                .all_entries
                .iter()
                .any(|entry| entry.name == name)
        })
    }

    fn start_upload(&mut self, upload: QueuedUpload, cx: &mut Context<Self>) {
        self.pending_transfers += 1;
        self.worker.send(BackendCommand::Upload {
            session_id: upload.session_id,
            local: upload.local,
            remote: upload.remote,
            display_name: upload.name,
        });
        cx.notify();
    }

    /// Starts uploads that do not collide, then asks about each collision.
    fn enqueue_uploads(
        &mut self,
        session_id: String,
        remote_path: String,
        paths: Vec<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        let mut ready = Vec::new();
        let mut conflicts = Vec::new();

        for path in paths {
            let name = path_file_name(&path);
            let remote = join_remote(&remote_path, &name);
            let upload = QueuedUpload {
                session_id: session_id.clone(),
                local: path,
                remote,
                name,
            };
            if self.remote_has_entry(&session_id, &upload.name) {
                conflicts.push(upload);
            } else {
                ready.push(upload);
            }
        }

        let started = ready.len();
        for upload in ready {
            self.start_upload(upload, cx);
        }
        self.pending_overwrite.extend(conflicts);

        if started > 0 {
            self.status = format!("已加入 {started} 个上传任务").into();
            self.status_kind = StatusKind::Info;
        }
        self.prompt_next_overwrite(cx);
        cx.notify();
    }

    fn upload_external_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let Some((session_id, remote_path)) = self
            .active_session_state()
            .map(|session| (session.info.profile_id.clone(), session.pane.path.clone()))
        else {
            self.status = "请先连接服务器再上传文件".into();
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        };

        self.enqueue_uploads(session_id, remote_path, paths, cx);
    }

    fn handle_drop_on_remote(&mut self, item: DragItem, cx: &mut Context<Self>) {
        if item.source != Side::Local {
            return;
        }
        let Some((session_id, remote_path)) = self
            .active_session_state()
            .map(|session| (session.info.profile_id.clone(), session.pane.path.clone()))
        else {
            self.status = "请先连接服务器再上传文件".into();
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        };

        let local = PathBuf::from(item.path);
        self.enqueue_uploads(session_id, remote_path, vec![local], cx);
        self.status = "开始上传".into();
        self.status_kind = StatusKind::Info;
        cx.notify();
    }

    fn handle_drop_on_local(&mut self, item: DragItem, cx: &mut Context<Self>) {
        if item.source != Side::Remote {
            return;
        }
        let Some(session_id) = self
            .active_session_state()
            .map(|session| session.info.profile_id.clone())
        else {
            return;
        };

        let local = self.local.path.join(&item.name);
        self.pending_transfers += 1;
        self.worker.send(BackendCommand::Download {
            session_id,
            remote: item.path,
            local,
            display_name: item.name,
        });
        self.status = "开始下载".into();
        self.status_kind = StatusKind::Info;
        cx.notify();
    }

    fn toggle_group(&mut self, id: String, cx: &mut Context<Self>) {
        if !self.collapsed_groups.remove(&id) {
            self.collapsed_groups.insert(id);
        }
        cx.notify();
    }

    fn set_sidebar_drop_target(
        &mut self,
        target: Option<SidebarDropTarget>,
        cx: &mut Context<Self>,
    ) {
        if self.sidebar_drop_target != target {
            self.sidebar_drop_target = target;
            cx.notify();
        }
    }

    /// Applies the pending sidebar drop, if the pointer was released over a
    /// valid insertion point.
    fn drop_on_sidebar(&mut self, item: &SidebarDragItem, cx: &mut Context<Self>) {
        let target = self.sidebar_drop_target.take();
        let moved = match (item, target.as_ref()) {
            (
                SidebarDragItem::Group { id, .. },
                Some(SidebarDropTarget::Group {
                    id: target_id,
                    before,
                }),
            ) => self.settings.move_group(id, target_id, *before),
            (
                SidebarDragItem::Connection { id, .. },
                Some(SidebarDropTarget::Connection {
                    id: target_id,
                    before,
                }),
            ) => self.settings.move_connection(id, target_id, *before),
            (
                SidebarDragItem::Connection { id, .. },
                Some(SidebarDropTarget::GroupEnd { group_id }),
            ) => self.settings.move_connection_to_group(id, group_id),
            _ => false,
        };

        if moved {
            self.settings.save();
            self.status = match item {
                SidebarDragItem::Group { .. } => "分组顺序已更新",
                SidebarDragItem::Connection { .. } => "连接顺序已更新",
            }
            .into();
            self.status_kind = StatusKind::Info;
        }
        cx.notify();
    }

    fn close_dialog(&mut self, cx: &mut Context<Self>) {
        // Dismissing an overwrite prompt skips that file and moves on to the
        // next conflicting upload, if any.
        if matches!(
            &self.dialog,
            DialogState::Confirm(confirm)
                if matches!(confirm.action, ConfirmAction::UploadOverwrite(_))
        ) {
            self.pending_overwrite.pop_front();
            self.dialog = DialogState::None;
            self.prompt_next_overwrite(cx);
            cx.notify();
            return;
        }

        self.dialog = DialogState::None;
        cx.notify();
    }

    fn open_new_group(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.context_menu = None;
        let name = make_input(window, cx, "例如：生产环境", "", false);
        self.dialog = DialogState::Group(GroupForm {
            editing_id: None,
            name,
            error: None,
        });
        if let DialogState::Group(form) = &self.dialog {
            form.name.update(cx, |state, cx| state.focus(window, cx));
        }
        cx.notify();
    }

    fn open_edit_group(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.context_menu = None;
        let Some(group) = self.settings.group(id).cloned() else {
            return;
        };
        let name = make_input(window, cx, "分组名称", &group.name, false);
        self.dialog = DialogState::Group(GroupForm {
            editing_id: Some(group.id),
            name,
            error: None,
        });
        if let DialogState::Group(form) = &self.dialog {
            form.name.update(cx, |state, cx| state.focus(window, cx));
        }
        cx.notify();
    }

    fn open_new_connection(
        &mut self,
        group_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.context_menu = None;
        let group_id = group_id
            .filter(|id| self.settings.groups.iter().any(|group| &group.id == id))
            .or_else(|| self.settings.groups.first().map(|group| group.id.clone()));
        let Some(group_id) = group_id else {
            let message: SharedString = "请先创建一个分组，再新建连接".into();
            self.status = message.clone();
            self.status_kind = StatusKind::Error;
            self.push_toast(ToastKind::Error, message, cx);
            self.open_new_group(window, cx);
            return;
        };
        let name = make_input(window, cx, "连接名称", "", false);
        let host = make_input(window, cx, "example.com", "", false);
        let port = make_input(window, cx, "22", "22", false);
        let username = make_input(window, cx, "root", "", false);
        let password = make_input(window, cx, "密码", "", true);
        let key_path = make_input(window, cx, "~/.ssh/id_ed25519", "~/.ssh/id_ed25519", false);
        let passphrase = make_input(window, cx, "私钥口令（可选）", "", true);
        let remote_dir = make_input(window, cx, "/home/user（可选）", "", false);

        self.dialog = DialogState::Connection(ConnectionForm {
            editing_id: None,
            copying: false,
            group_id: Some(group_id),
            name,
            host,
            port,
            username,
            password,
            key_path,
            passphrase,
            remote_dir,
            use_key: false,
            error: None,
        });
        if let DialogState::Connection(form) = &self.dialog {
            form.name.update(cx, |state, cx| state.focus(window, cx));
        }
        cx.notify();
    }

    fn open_edit_connection(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.context_menu = None;
        let Some(profile) = self.settings.connection(id).cloned() else {
            return;
        };
        let (use_key, password_value, key_path, passphrase) = match &profile.auth {
            AuthMethod::Password { password } => {
                (false, password.clone(), String::new(), String::new())
            }
            AuthMethod::PrivateKey { path, passphrase } => {
                (true, String::new(), path.clone(), passphrase.clone())
            }
        };

        let name = make_input(window, cx, "连接名称", &profile.name, false);
        let host = make_input(window, cx, "example.com", &profile.host, false);
        let port = make_input(window, cx, "22", &profile.port.to_string(), false);
        let username = make_input(window, cx, "root", &profile.username, false);
        let password = make_input(window, cx, "密码", &password_value, true);
        let key_path = make_input(window, cx, "~/.ssh/id_ed25519", &key_path, false);
        let passphrase = make_input(window, cx, "私钥口令（可选）", &passphrase, true);
        let remote_dir = make_input(window, cx, "/home/user（可选）", &profile.remote_dir, false);

        self.dialog = DialogState::Connection(ConnectionForm {
            editing_id: Some(profile.id),
            copying: false,
            group_id: profile.group_id,
            name,
            host,
            port,
            username,
            password,
            key_path,
            passphrase,
            remote_dir,
            use_key,
            error: None,
        });
        if let DialogState::Connection(form) = &self.dialog {
            form.name.update(cx, |state, cx| state.focus(window, cx));
        }
        cx.notify();
    }

    fn open_copy_connection(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.context_menu = None;
        let Some(profile) = self.settings.connection(id).cloned() else {
            return;
        };
        let (use_key, password_value, key_path, passphrase) = match &profile.auth {
            AuthMethod::Password { password } => {
                (false, password.clone(), String::new(), String::new())
            }
            AuthMethod::PrivateKey { path, passphrase } => {
                (true, String::new(), path.clone(), passphrase.clone())
            }
        };

        let name = make_input(
            window,
            cx,
            "连接名称",
            &format!("{} 副本", profile.name),
            false,
        );
        let host = make_input(window, cx, "example.com", &profile.host, false);
        let port = make_input(window, cx, "22", &profile.port.to_string(), false);
        let username = make_input(window, cx, "root", &profile.username, false);
        let password = make_input(window, cx, "密码", &password_value, true);
        let key_path = make_input(window, cx, "~/.ssh/id_ed25519", &key_path, false);
        let passphrase = make_input(window, cx, "私钥口令（可选）", &passphrase, true);
        let remote_dir = make_input(window, cx, "/home/user（可选）", &profile.remote_dir, false);

        self.dialog = DialogState::Connection(ConnectionForm {
            editing_id: None,
            copying: true,
            group_id: profile.group_id,
            name,
            host,
            port,
            username,
            password,
            key_path,
            passphrase,
            remote_dir,
            use_key,
            error: None,
        });
        if let DialogState::Connection(form) = &self.dialog {
            form.name.update(cx, |state, cx| state.focus(window, cx));
        }
        cx.notify();
    }

    fn open_confirm(
        &mut self,
        title: &str,
        message: String,
        action: ConfirmAction,
        cx: &mut Context<Self>,
    ) {
        self.context_menu = None;
        self.dialog = DialogState::Confirm(ConfirmDialog {
            title: title.to_string().into(),
            message: message.into(),
            action,
            confirm_label: "删除".into(),
            danger: true,
        });
        cx.notify();
    }

    /// Asks before overwriting a remote file that was just dropped onto the
    /// server pane.
    fn prompt_next_overwrite(&mut self, cx: &mut Context<Self>) {
        let Some(upload) = self.pending_overwrite.front().cloned() else {
            return;
        };
        self.dialog = DialogState::Confirm(ConfirmDialog {
            title: "文件已存在".into(),
            message: format!("服务器当前目录已存在“{}”，是否覆盖？", upload.name).into(),
            action: ConfirmAction::UploadOverwrite(upload),
            confirm_label: "覆盖".into(),
            danger: false,
        });
        cx.notify();
    }

    fn open_context_menu(
        &mut self,
        target: ContextTarget,
        position: gpui_kit::Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.context_menu = Some(ContextMenuState { position, target });
        cx.notify();
    }

    fn close_context_menu(&mut self, cx: &mut Context<Self>) {
        self.context_menu = None;
        cx.notify();
    }

    fn open_file_context_menu(
        &mut self,
        side: Side,
        path: String,
        position: gpui_kit::Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let (session_id, entry) = match side {
            Side::Local => {
                let Some(index) = self.local.entries.iter().position(|e| e.path == path) else {
                    return;
                };
                self.local.selected = Some(index);
                (None, self.local.entries[index].clone())
            }
            Side::Remote => {
                let Some(session_id) = self.active_session.clone() else {
                    return;
                };
                let Some(session) = self.sessions.get_mut(&session_id) else {
                    return;
                };
                let Some(index) = session.pane.entries.iter().position(|e| e.path == path) else {
                    return;
                };
                session.pane.selected = Some(index);
                (Some(session_id), session.pane.entries[index].clone())
            }
        };

        self.open_context_menu(
            ContextTarget::File {
                side,
                session_id,
                path: entry.path,
                name: entry.name,
                is_dir: entry.is_dir,
                permissions: entry.permissions,
            },
            position,
            cx,
        );
    }

    fn open_rename_file(
        &mut self,
        side: Side,
        session_id: Option<String>,
        path: String,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.context_menu = None;
        let input = make_input(window, cx, "新的名称", name, false);
        self.dialog = DialogState::Rename(RenameForm {
            side,
            session_id,
            path,
            name: input,
            error: None,
        });
        cx.notify();
    }

    fn open_chmod_file(
        &mut self,
        session_id: String,
        path: String,
        name: String,
        mode: Option<u32>,
        cx: &mut Context<Self>,
    ) {
        self.context_menu = None;
        self.dialog = DialogState::Chmod(ChmodForm {
            session_id,
            path,
            name,
            mode: mode.map(|mode| mode & 0o777).unwrap_or(0o644),
        });
        cx.notify();
    }

    fn open_file_info(
        &mut self,
        session_id: String,
        path: String,
        name: String,
        cx: &mut Context<Self>,
    ) {
        self.context_menu = None;
        self.worker.send(BackendCommand::StatRemote {
            session_id,
            path: path.clone(),
        });
        self.dialog = DialogState::FileInfo(FileInfoForm {
            name,
            path,
            loading: true,
            info: None,
            error: None,
        });
        cx.notify();
    }

    fn save_rename(&mut self, cx: &mut Context<Self>) {
        let DialogState::Rename(form) = &self.dialog else {
            return;
        };
        let name = form.name.read(cx).value().trim().to_string();
        let side = form.side;
        let session_id = form.session_id.clone();
        let path = form.path.clone();

        if name.is_empty() {
            self.set_rename_error("请填写新的名称", cx);
            return;
        }
        if name.contains('/') || name.contains('\\') {
            self.set_rename_error("名称不能包含路径分隔符", cx);
            return;
        }

        match side {
            Side::Local => {
                let old = PathBuf::from(&path);
                if old.file_name().and_then(|n| n.to_str()) == Some(name.as_str()) {
                    self.dialog = DialogState::None;
                    cx.notify();
                    return;
                }
                let Some(parent) = old.parent() else {
                    self.set_rename_error("无法确定所在目录", cx);
                    return;
                };
                let target = parent.join(&name);
                match std::fs::rename(&old, &target) {
                    Ok(()) => {
                        self.dialog = DialogState::None;
                        self.status = format!("已重命名为 {name}").into();
                        self.status_kind = StatusKind::Success;
                        let dir = self.local.path.clone();
                        self.load_local_dir(dir, cx);
                    }
                    Err(error) => self.set_rename_error(format!("重命名失败：{error}"), cx),
                }
            }
            Side::Remote => {
                let Some(session_id) = session_id else {
                    self.set_rename_error("尚未连接服务器", cx);
                    return;
                };
                let target = join_remote(&remote_parent(&path), &name);
                self.worker.send(BackendCommand::RenameRemote {
                    session_id,
                    from: path,
                    to: target,
                });
                self.dialog = DialogState::None;
                self.status = format!("正在重命名为 {name} ...").into();
                self.status_kind = StatusKind::Info;
                cx.notify();
            }
        }
    }

    fn save_chmod(&mut self, cx: &mut Context<Self>) {
        let DialogState::Chmod(form) = &self.dialog else {
            return;
        };
        let session_id = form.session_id.clone();
        let path = form.path.clone();
        let mode = form.mode;

        self.worker.send(BackendCommand::ChmodRemote {
            session_id,
            path,
            mode,
        });
        self.dialog = DialogState::None;
        self.status = format!("正在修改权限为 {} ...", format_mode(mode)).into();
        self.status_kind = StatusKind::Info;
        cx.notify();
    }

    fn set_rename_error(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        if let DialogState::Rename(form) = &mut self.dialog {
            form.error = Some(message.into());
        }
        cx.notify();
    }

    fn save_group(&mut self, cx: &mut Context<Self>) {
        let DialogState::Group(form) = &self.dialog else {
            return;
        };
        let name = form.name.read(cx).value().trim().to_string();
        let editing_id = form.editing_id.clone();

        if name.is_empty() {
            self.set_group_error("请填写分组名称", cx);
            return;
        }

        if let Some(id) = editing_id {
            if let Some(group) = self.settings.groups.iter_mut().find(|g| g.id == id) {
                group.name = name.clone();
            }
        } else {
            self.settings.groups.push(crate::model::ConnectionGroup {
                id: new_id("group"),
                name: name.clone(),
            });
        }

        self.settings.save();
        self.dialog = DialogState::None;
        self.status = format!("分组“{name}”已保存").into();
        self.status_kind = StatusKind::Success;
        cx.notify();
    }

    fn save_connection(&mut self, connect_after: bool, cx: &mut Context<Self>) {
        let DialogState::Connection(form) = &self.dialog else {
            return;
        };

        let name = form.name.read(cx).value().trim().to_string();
        let host = form.host.read(cx).value().trim().to_string();
        let port_text = form.port.read(cx).value().trim().to_string();
        let username = form.username.read(cx).value().trim().to_string();
        let password = form.password.read(cx).value().to_string();
        let key_path = form.key_path.read(cx).value().trim().to_string();
        let passphrase = form.passphrase.read(cx).value().to_string();
        let remote_dir = form.remote_dir.read(cx).value().trim().to_string();
        let use_key = form.use_key;
        let editing_id = form.editing_id.clone();
        let group_id = form.group_id.clone();

        if name.is_empty() {
            self.set_connection_error("请填写连接名称", cx);
            return;
        }
        if host.is_empty() {
            self.set_connection_error("请填写主机地址", cx);
            return;
        }
        if username.is_empty() {
            self.set_connection_error("请填写用户名", cx);
            return;
        }
        if use_key && key_path.is_empty() {
            self.set_connection_error("请选择或填写私钥文件路径", cx);
            return;
        }

        let port = if port_text.is_empty() {
            22
        } else {
            match port_text.parse::<u16>() {
                Ok(port) if port > 0 => port,
                _ => {
                    self.set_connection_error("端口必须是 1-65535 之间的数字", cx);
                    return;
                }
            }
        };

        let Some(group_id) =
            group_id.filter(|id| self.settings.groups.iter().any(|group| &group.id == id))
        else {
            self.set_connection_error("请选择一个分组", cx);
            return;
        };

        let profile = ConnectionProfile {
            id: editing_id.clone().unwrap_or_else(|| new_id("connection")),
            name: name.clone(),
            group_id: Some(group_id),
            host,
            port,
            username,
            auth: if use_key {
                AuthMethod::PrivateKey {
                    path: key_path,
                    passphrase,
                }
            } else {
                AuthMethod::Password { password }
            },
            remote_dir,
        };

        if let Some(id) = editing_id {
            if let Some(existing) = self
                .settings
                .connections
                .iter_mut()
                .find(|connection| connection.id == id)
            {
                *existing = profile.clone();
            }
        } else {
            self.settings.connections.push(profile.clone());
        }

        self.settings.save();
        self.dialog = DialogState::None;
        self.status = format!("连接“{name}”已保存").into();
        self.status_kind = StatusKind::Success;

        if connect_after {
            self.connect_profile(&profile.id, cx);
        } else {
            cx.notify();
        }
    }

    fn set_group_error(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        if let DialogState::Group(form) = &mut self.dialog {
            form.error = Some(message.into());
        }
        cx.notify();
    }

    fn set_connection_error(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        if let DialogState::Connection(form) = &mut self.dialog {
            form.error = Some(message.into());
        }
        cx.notify();
    }

    fn apply_confirm(&mut self, action: ConfirmAction, cx: &mut Context<Self>) {
        match action {
            ConfirmAction::UploadOverwrite(upload) => {
                self.pending_overwrite.pop_front();
                self.dialog = DialogState::None;
                self.start_upload(upload, cx);
                self.prompt_next_overwrite(cx);
                cx.notify();
                return;
            }
            ConfirmAction::DeleteLocalFile { path, name, is_dir } => {
                let target = PathBuf::from(&path);
                let result = if is_dir {
                    std::fs::remove_dir_all(&target)
                } else {
                    std::fs::remove_file(&target)
                };
                self.dialog = DialogState::None;
                match result {
                    Ok(()) => {
                        self.status = format!("已删除 {name}").into();
                        self.status_kind = StatusKind::Success;
                        let dir = self.local.path.clone();
                        self.load_local_dir(dir, cx);
                    }
                    Err(error) => {
                        self.status = format!("删除 {name} 失败：{error}").into();
                        self.status_kind = StatusKind::Error;
                        cx.notify();
                    }
                }
                return;
            }
            ConfirmAction::DeleteRemoteFile {
                session_id,
                path,
                name,
            } => {
                self.worker
                    .send(BackendCommand::DeleteRemote { session_id, path });
                self.dialog = DialogState::None;
                self.status = format!("正在删除 {name} ...").into();
                self.status_kind = StatusKind::Info;
                cx.notify();
                return;
            }
            ConfirmAction::DeleteConnection(id) => {
                let was_active = self.active_session.as_deref() == Some(id.as_str());
                let had_session = self.sessions.contains_key(&id);
                let was_connecting = self.connecting.contains(&id);
                if had_session || was_connecting {
                    self.worker.send(BackendCommand::Disconnect {
                        session_id: id.clone(),
                    });
                }
                self.sessions.remove(&id);
                self.connecting.remove(&id);
                if was_active {
                    self.select_fallback_session();
                }
                self.settings
                    .connections
                    .retain(|connection| connection.id != id);
                self.status = "连接已删除".into();
            }
            ConfirmAction::DeleteGroup(id) => {
                let group_name = self
                    .settings
                    .group(&id)
                    .map(|group| group.name.clone())
                    .unwrap_or_default();
                let target = self
                    .settings
                    .groups
                    .iter()
                    .find(|group| group.id != id)
                    .map(|group| (group.id.clone(), group.name.clone()));
                let has_connections = self
                    .settings
                    .connections
                    .iter()
                    .any(|connection| connection.group_id.as_deref() == Some(id.as_str()));

                let Some((target_id, target_name)) = target else {
                    // Keep the last group if it still owns connections:
                    // otherwise those connections would disappear from the UI.
                    if has_connections {
                        let message: SharedString =
                            "这是最后一个分组，请先创建其他分组或删除其中的连接".into();
                        self.status = message.clone();
                        self.status_kind = StatusKind::Error;
                        self.push_toast(ToastKind::Error, message, cx);
                        self.dialog = DialogState::None;
                        cx.notify();
                        return;
                    }
                    self.settings.groups.retain(|group| group.id != id);
                    self.status = format!("分组“{group_name}”已删除").into();
                    self.settings.save();
                    self.status_kind = StatusKind::Info;
                    self.dialog = DialogState::None;
                    cx.notify();
                    return;
                };

                self.settings.groups.retain(|group| group.id != id);
                for connection in &mut self.settings.connections {
                    if connection.group_id.as_deref() == Some(id.as_str()) {
                        connection.group_id = Some(target_id.clone());
                    }
                }
                self.status = if has_connections {
                    format!("分组“{group_name}”已删除，原有连接已移至“{target_name}”").into()
                } else {
                    format!("分组“{group_name}”已删除").into()
                };
            }
        }

        self.settings.save();
        self.status_kind = StatusKind::Info;
        self.dialog = DialogState::None;
        cx.notify();
    }

    fn pick_local_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("选择本地目录".into()),
        });

        cx.spawn_in(window, async move |this, cx| {
            let path = match receiver.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Ok(None)) => None,
                Ok(Err(error)) => {
                    let _ = this.update_in(cx, |app, _, cx| {
                        app.status = format!("打开目录选择器失败：{error}").into();
                        app.status_kind = StatusKind::Error;
                        cx.notify();
                    });
                    None
                }
                Err(_) => None,
            };

            if let Some(path) = path {
                let _ = this.update_in(cx, |app, _, cx| app.load_local_dir(path, cx));
            }
        })
        .detach();
    }

    fn pick_key_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("选择 SSH 私钥".into()),
        });

        cx.spawn_in(window, async move |this, cx| {
            let path = match receiver.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Ok(None)) => None,
                Ok(Err(error)) => {
                    let _ = this.update_in(cx, |app, _, cx| {
                        app.status = format!("打开文件选择器失败：{error}").into();
                        app.status_kind = StatusKind::Error;
                        cx.notify();
                    });
                    None
                }
                Err(_) => None,
            };

            if let Some(path) = path {
                let text = path.to_string_lossy().to_string();
                let _ = this.update_in(cx, move |app, window, cx| {
                    if let DialogState::Connection(form) = &mut app.dialog {
                        form.key_path
                            .update(cx, |state, cx| state.set_value(text, window, cx));
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }

    fn render_header(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors;
        let dark = cx.theme().mode.is_dark();
        let is_macos = cfg!(target_os = "macos");
        // `muted_foreground` is fairly dim on the dark title bar, which makes the
        // thin window-control glyphs hard to read. Brighten them in dark mode.
        let control_fg = if dark {
            colors.foreground.opacity(0.85)
        } else {
            colors.muted_foreground
        };

        let control_button =
            |id: &'static str, icon: IconName, area: WindowControlArea, close: bool| {
                let hover_bg = if close {
                    colors.danger
                } else {
                    colors.secondary_hover
                };
                let hover_fg = if close {
                    // `danger_foreground` is a dark red in the dark theme, which
                    // leaves the close glyph barely visible on the red hover
                    // background. Plain white reads clearly in both themes.
                    white()
                } else {
                    colors.foreground
                };

                div()
                    .id(id)
                    .h_full()
                    .w(px(46.0))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(control_fg)
                    .window_control_area(area)
                    .hover(move |this| this.bg(hover_bg).text_color(hover_fg))
                    .when(!cfg!(target_os = "windows"), |this| {
                        this.on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            match area {
                                WindowControlArea::Min => window.minimize_window(),
                                WindowControlArea::Max => window.zoom_window(),
                                WindowControlArea::Close => window.remove_window(),
                                WindowControlArea::Drag => {}
                            }
                        })
                    })
                    .child(Icon::new(icon).small())
                    .into_any_element()
            };

        let drag_area = h_flex()
            .id("titlebar-drag")
            .h_full()
            .flex_1()
            .min_w_0()
            .gap_3()
            .window_control_area(WindowControlArea::Drag)
            .when(!cfg!(target_os = "windows"), |this| {
                this.on_double_click(|_, window, _| window.zoom_window())
            })
            .child(
                img(APP_ICON_IMAGE.clone())
                    .size(px(30.0))
                    .object_fit(ObjectFit::Contain)
                    .flex_shrink_0(),
            )
            .child(
                v_flex()
                    .gap_0()
                    .child(div().text_sm().font_semibold().child("RGFTP"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(colors.muted_foreground)
                            .child("SFTP 文件传输"),
                    ),
            )
            .child(div().flex_1());

        let mut controls = h_flex().h_full().flex_shrink_0();
        if !is_macos {
            controls = controls
                .child(control_button(
                    "window-minimize",
                    IconName::WindowMinimize,
                    WindowControlArea::Min,
                    false,
                ))
                .child(control_button(
                    "window-maximize",
                    if window.is_maximized() {
                        IconName::WindowRestore
                    } else {
                        IconName::WindowMaximize
                    },
                    WindowControlArea::Max,
                    false,
                ))
                .child(control_button(
                    "window-close",
                    IconName::WindowClose,
                    WindowControlArea::Close,
                    true,
                ));
        }

        h_flex()
            .h(px(48.0))
            .w_full()
            .flex_shrink_0()
            .pl(if is_macos { px(80.0) } else { px(12.0) })
            .bg(colors.background)
            .border_b_1()
            .border_color(colors.border)
            .child(drag_area)
            .child(
                Button::new("theme-toggle")
                    .ghost()
                    .small()
                    .icon(if dark { IconName::Sun } else { IconName::Moon })
                    .tooltip(if dark {
                        "切换到浅色主题"
                    } else {
                        "切换到深色主题"
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        let preference = if cx.theme().mode.is_dark() {
                            ThemePreference::Light
                        } else {
                            ThemePreference::Dark
                        };
                        let mode = match preference {
                            ThemePreference::Light => ThemeMode::Light,
                            ThemePreference::Dark => ThemeMode::Dark,
                        };
                        Theme::change(mode, Some(window), cx);
                        this.settings.theme = Some(preference);
                        this.settings.save();
                        cx.notify();
                    })),
            )
            .child(controls)
            .into_any_element()
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors;

        let groups: Vec<(String, String, Vec<ConnectionProfile>)> = self
            .settings
            .groups
            .iter()
            .map(|group| {
                (
                    group.id.clone(),
                    group.name.clone(),
                    self.settings
                        .connections
                        .iter()
                        .filter(|connection| {
                            connection.group_id.as_deref() == Some(group.id.as_str())
                        })
                        .cloned()
                        .collect(),
                )
            })
            .collect();

        let total_connections = self.settings.connections.len();
        let sidebar_drop_active = cx.has_active_drag();

        let mut list = v_flex().gap_2();

        for (group_id, group_name, connections) in groups {
            let collapsed = self.collapsed_groups.contains(&group_id);
            let count = connections.len();
            let group_drop_before = sidebar_drop_active
                && match self.sidebar_drop_target.as_ref() {
                    Some(SidebarDropTarget::Group { id, before: true }) => id == &group_id,
                    _ => false,
                };
            let group_drop_after = sidebar_drop_active
                && match self.sidebar_drop_target.as_ref() {
                    Some(SidebarDropTarget::Group { id, before: false }) => id == &group_id,
                    _ => false,
                };
            let group_drop_into = sidebar_drop_active
                && match self.sidebar_drop_target.as_ref() {
                    Some(SidebarDropTarget::GroupEnd {
                        group_id: target_group,
                    }) => target_group == &group_id,
                    _ => false,
                };

            let header = h_flex()
                .id(ElementId::Name(format!("group-{group_id}").into()))
                .relative()
                .gap_1()
                .px_2()
                .h(px(32.0))
                .rounded_md()
                .hover(|this| this.bg(colors.sidebar_accent))
                .when(group_drop_into, |this| {
                    this.bg(colors.primary.opacity(0.12))
                })
                .child(
                    Icon::new(if collapsed {
                        IconName::ChevronRight
                    } else {
                        IconName::ChevronDown
                    })
                    .xsmall()
                    .text_color(colors.muted_foreground),
                )
                .child(
                    Icon::new(if collapsed {
                        IconName::FolderClosed
                    } else {
                        IconName::FolderOpen
                    })
                    .xsmall()
                    .text_color(colors.muted_foreground),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .font_medium()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(group_name.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(colors.muted_foreground)
                        .child(count.to_string()),
                )
                .child(
                    Button::new(ElementId::Name(format!("group-add-{group_id}").into()))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Plus)
                        .tooltip("在分组中新建连接")
                        .on_click(cx.listener({
                            let group_id = group_id.clone();
                            move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.open_new_connection(Some(group_id.clone()), window, cx);
                            }
                        })),
                )
                .when(group_drop_before, |this| {
                    this.child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .right_0()
                            .h(px(2.0))
                            .bg(colors.primary),
                    )
                })
                .when(group_drop_after, |this| {
                    this.child(
                        div()
                            .absolute()
                            .bottom_0()
                            .left_0()
                            .right_0()
                            .h(px(2.0))
                            .bg(colors.primary),
                    )
                })
                .when(group_drop_into, |this| {
                    this.child(
                        div()
                            .absolute()
                            .bottom(px(3.0))
                            .left(px(8.0))
                            .right(px(8.0))
                            .h(px(2.0))
                            .rounded_full()
                            .bg(colors.primary),
                    )
                });

            let header = header
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener({
                        let group_id = group_id.clone();
                        let group_name = group_name.clone();
                        move |this, event: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.open_context_menu(
                                ContextTarget::Group {
                                    id: group_id.clone(),
                                    name: group_name.clone(),
                                },
                                event.position,
                                cx,
                            );
                        }
                    }),
                )
                .on_click(cx.listener({
                    let group_id = group_id.clone();
                    move |this, event: &ClickEvent, _, cx| {
                        if !event.standard_click() {
                            return;
                        }
                        this.context_menu = None;
                        this.toggle_group(group_id.clone(), cx);
                    }
                }))
                .on_drag_move(cx.listener({
                    let group_id = group_id.clone();
                    move |this, event: &DragMoveEvent<SidebarDragItem>, _, cx| {
                        // Every `on_drag_move` listener in the tree is invoked, so
                        // only react when the pointer is actually over this header.
                        if !event.bounds.contains(&event.event.position) {
                            return;
                        }
                        let before = event.event.position.y < event.bounds.center().y;
                        let target = match event.drag(cx) {
                            SidebarDragItem::Group { id, .. } => {
                                if id == &group_id {
                                    None
                                } else {
                                    let after =
                                        this.settings.normalize_group_drop(id, &group_id, !before);
                                    Some(SidebarDropTarget::Group {
                                        id: group_id.clone(),
                                        before: !after,
                                    })
                                }
                            }
                            SidebarDragItem::Connection { .. } => {
                                Some(SidebarDropTarget::GroupEnd {
                                    group_id: group_id.clone(),
                                })
                            }
                        };
                        this.set_sidebar_drop_target(target, cx);
                    }
                }))
                .on_drop(
                    cx.listener(|this, item: &SidebarDragItem, _, cx| {
                        this.drop_on_sidebar(item, cx)
                    }),
                )
                .on_drag(
                    SidebarDragItem::Group {
                        id: group_id.clone(),
                        name: group_name.clone(),
                    },
                    |payload, _, _, cx| {
                        let label: SharedString = payload.name().to_string().into();
                        let is_group = payload.is_group();
                        cx.new(|_| SidebarDragPreview { label, is_group })
                    },
                )
                .into_any_element();

            list = list.child(header);

            if collapsed {
                continue;
            }

            let mut group_box = v_flex()
                .id(ElementId::Name(format!("group-body-{group_id}").into()))
                .relative()
                .gap_1()
                .pl_5()
                .on_drag_move(cx.listener({
                    let group_id = group_id.clone();
                    move |this, event: &DragMoveEvent<SidebarDragItem>, _, cx| {
                        if !event.bounds.contains(&event.event.position) {
                            return;
                        }
                        // Dropping in the gap or empty space of the group adds
                        // the connection at the end.
                        let target = match event.drag(cx) {
                            SidebarDragItem::Connection { .. } => {
                                Some(SidebarDropTarget::GroupEnd {
                                    group_id: group_id.clone(),
                                })
                            }
                            _ => None,
                        };
                        this.set_sidebar_drop_target(target, cx);
                    }
                }))
                .on_drop(cx.listener(|this, item: &SidebarDragItem, _, cx| {
                    this.drop_on_sidebar(item, cx)
                }));
            if connections.is_empty() {
                group_box = group_box.child(
                    div()
                        .px_2()
                        .py_1()
                        .text_xs()
                        .text_color(colors.muted_foreground)
                        .child("这个分组还没有连接"),
                );
            }

            for connection in connections {
                let active = self.active_session.as_deref() == Some(connection.id.as_str());
                let connected = self.sessions.contains_key(&connection.id);
                let connecting = self.connecting.contains(&connection.id);
                let connection_drop_before = sidebar_drop_active
                    && match self.sidebar_drop_target.as_ref() {
                        Some(SidebarDropTarget::Connection { id, before: true }) => {
                            id == &connection.id
                        }
                        _ => false,
                    };
                let connection_drop_after = sidebar_drop_active
                    && match self.sidebar_drop_target.as_ref() {
                        Some(SidebarDropTarget::Connection { id, before: false }) => {
                            id == &connection.id
                        }
                        _ => false,
                    };

                let icon_background = if active || connecting {
                    colors.primary
                } else {
                    colors.secondary
                };
                let icon_color = if active || connecting {
                    colors.primary_foreground
                } else {
                    colors.muted_foreground
                };

                let row = h_flex()
                    .id(ElementId::Name(
                        format!("connection-{}", connection.id).into(),
                    ))
                    .relative()
                    .gap_2()
                    .px_2()
                    .py_1p5()
                    .rounded_md()
                    .when(active, |this| this.bg(colors.sidebar_accent))
                    .hover(|this| this.bg(colors.sidebar_accent))
                    .child(
                        div()
                            .size(px(26.0))
                            .rounded_md()
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(icon_background)
                            .child(Icon::new(IconName::Server).xsmall().text_color(icon_color)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0()
                            .child(
                                div()
                                    .text_sm()
                                    .font_medium()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .child(connection.name.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(colors.muted_foreground)
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .child(connection.host.clone()),
                            ),
                    )
                    .when(connected, |this| {
                        this.child(
                            div()
                                .size(px(8.0))
                                .flex_shrink_0()
                                .rounded_full()
                                .bg(colors.success),
                        )
                    })
                    .when(connection_drop_before, |this| {
                        this.child(
                            div()
                                .absolute()
                                .top_0()
                                .left_0()
                                .right_0()
                                .h(px(2.0))
                                .bg(colors.primary),
                        )
                    })
                    .when(connection_drop_after, |this| {
                        this.child(
                            div()
                                .absolute()
                                .bottom_0()
                                .left_0()
                                .right_0()
                                .h(px(2.0))
                                .bg(colors.primary),
                        )
                    })
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener({
                            let id = connection.id.clone();
                            let name = connection.name.clone();
                            move |this, event: &MouseDownEvent, _, cx| {
                                cx.stop_propagation();
                                this.open_context_menu(
                                    ContextTarget::Connection {
                                        id: id.clone(),
                                        name: name.clone(),
                                    },
                                    event.position,
                                    cx,
                                );
                            }
                        }),
                    )
                    .on_click(cx.listener({
                        let id = connection.id.clone();
                        let name = connection.name.clone();
                        move |this, event: &ClickEvent, _, cx| {
                            if !event.standard_click() || event.click_count() < 2 {
                                return;
                            }

                            this.context_menu = None;
                            if this.connecting.contains(&id) {
                                return;
                            }

                            if this.sessions.contains_key(&id) {
                                if this.active_session.as_deref() == Some(id.as_str()) {
                                    this.disconnect(cx);
                                } else {
                                    this.active_session = Some(id.clone());
                                    this.status = format!("已切换到 {name}").into();
                                    this.status_kind = StatusKind::Info;
                                    cx.notify();
                                }
                            } else {
                                this.connect_profile(&id, cx);
                            }
                        }
                    }))
                    .on_drag_move(cx.listener({
                        let connection_id = connection.id.clone();
                        move |this, event: &DragMoveEvent<SidebarDragItem>, _, cx| {
                            // See the group header: drag move listeners fire for
                            // every element, so filter by the pointer position.
                            if !event.bounds.contains(&event.event.position) {
                                return;
                            }
                            let before = event.event.position.y < event.bounds.center().y;
                            let target = match event.drag(cx) {
                                SidebarDragItem::Connection { id, .. } if id != &connection_id => {
                                    let after = this.settings.normalize_connection_drop(
                                        id,
                                        &connection_id,
                                        !before,
                                    );
                                    Some(SidebarDropTarget::Connection {
                                        id: connection_id.clone(),
                                        before: !after,
                                    })
                                }
                                _ => None,
                            };
                            this.set_sidebar_drop_target(target, cx);
                        }
                    }))
                    .on_drop(cx.listener(|this, item: &SidebarDragItem, _, cx| {
                        this.drop_on_sidebar(item, cx)
                    }))
                    .on_drag(
                        SidebarDragItem::Connection {
                            id: connection.id.clone(),
                            name: connection.name.clone(),
                        },
                        |payload, _, _, cx| {
                            let label: SharedString = payload.name().to_string().into();
                            let is_group = payload.is_group();
                            cx.new(|_| SidebarDragPreview { label, is_group })
                        },
                    );

                group_box = group_box.child(row);
            }

            if group_drop_into {
                group_box = group_box.child(
                    div()
                        .absolute()
                        .bottom_0()
                        .left(px(8.0))
                        .right(px(8.0))
                        .h(px(2.0))
                        .rounded_full()
                        .bg(colors.primary),
                );
            }

            list = list.child(group_box);
        }

        if self.settings.connections.is_empty() {
            list = list.child(
                v_flex()
                    .mt_4()
                    .mx_2()
                    .gap_2()
                    .items_center()
                    .child(
                        Icon::new(IconName::Cloud)
                            .size(px(30.0))
                            .text_color(colors.muted_foreground),
                    )
                    .child(div().text_sm().font_medium().child("还没有保存的连接"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(colors.muted_foreground)
                            .child("新建一个连接，开始传输文件"),
                    ),
            );
        }

        v_flex()
            .w(px(286.0))
            .h_full()
            .flex_shrink_0()
            .border_r_1()
            .border_color(colors.border)
            .bg(colors.sidebar)
            .on_drag_move(
                cx.listener(|this, _: &DragMoveEvent<SidebarDragItem>, _, cx| {
                    // Clear the indicator when the pointer leaves all drop targets.
                    this.set_sidebar_drop_target(None, cx);
                }),
            )
            .child(
                h_flex()
                    .px_3()
                    .pt_4()
                    .pb_2()
                    .gap_2()
                    .child(div().flex_1().text_sm().font_semibold().child("连接管理器"))
                    .child(
                        Button::new("add-group")
                            .ghost()
                            .xsmall()
                            .icon(IconName::FolderPlus)
                            .tooltip("新建分组")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_new_group(window, cx)),
                            ),
                    ),
            )
            .child(
                div().px_3().pb_3().child(
                    Button::new("add-connection")
                        .primary()
                        .small()
                        .icon(IconName::Plus)
                        .label("新建连接")
                        .w_full()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_new_connection(None, window, cx)
                        })),
                ),
            )
            .child(
                v_flex()
                    .id("sidebar-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_2()
                    .child(list),
            )
            .child(
                v_flex()
                    .gap_1()
                    .px_3()
                    .py_3()
                    .border_t_1()
                    .border_color(colors.border)
                    .child(
                        div()
                            .text_xs()
                            .text_color(colors.muted_foreground)
                            .child(format!("{total_connections} 个连接")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(colors.muted_foreground)
                            .child("拖拽文件到另一侧即可传输"),
                    )
                    .child(
                        div()
                            .pt_2()
                            .text_xs()
                            .text_color(colors.muted_foreground.opacity(0.8))
                            .child("Copyright (c) 2026 dousx"),
                    ),
            )
            .into_any_element()
    }

    fn render_main(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors;

        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(colors.background)
            .child(self.render_connection_bar(cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .gap_3()
                    .p_3()
                    .child(self.render_local_pane(cx))
                    .child(self.render_remote_pane(cx)),
            )
            .child(self.render_status_bar(cx))
            .into_any_element()
    }

    fn render_connection_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors;

        let mut bar = h_flex()
            .h(px(50.0))
            .px_4()
            .gap_3()
            .flex_shrink_0()
            .border_b_1()
            .border_color(colors.border)
            .bg(colors.background);

        let connecting_name = self
            .connecting
            .iter()
            .next()
            .and_then(|id| self.settings.connection(id))
            .map(|profile| profile.name.clone());

        let transferring = self.is_transferring();

        if let Some(session) = self.active_session_state() {
            let connected = session.info.clone();
            let session_count = self.sessions.len();
            bar =
                bar.child(div().size(px(8.0)).rounded_full().bg(colors.success))
                    .child(
                        v_flex()
                            .gap_0()
                            .child(div().text_sm().font_medium().child(connected.name.clone()))
                            .child(div().text_xs().text_color(colors.muted_foreground).child(
                                format!(
                                    "{}@{} · 已连接 · 当前 {} 个会话",
                                    connected.username, connected.endpoint, session_count
                                ),
                            )),
                    )
                    .child(div().flex_1())
                    .child(
                        h_flex()
                            .id("disconnect")
                            .h(px(28.0))
                            .px_2p5()
                            .gap_1p5()
                            .rounded_md()
                            .text_sm()
                            .when(transferring, |this| {
                                this.cursor_not_allowed()
                                    .opacity(0.45)
                                    .tooltip(|window, cx| {
                                        Tooltip::new("传输进行中，无法断开连接").build(window, cx)
                                    })
                            })
                            .when(!transferring, |this| {
                                this.cursor_pointer()
                                    .text_color(colors.muted_foreground)
                                    .hover(|this| {
                                        this.bg(colors.danger.opacity(0.14))
                                            .text_color(colors.danger)
                                    })
                                    .active(|this| {
                                        this.bg(colors.danger.opacity(0.22))
                                            .text_color(colors.danger)
                                    })
                            })
                            .child(Icon::new(IconName::Unplug).xsmall())
                            .child("断开")
                            .on_click(cx.listener(|this, _, _, cx| this.disconnect(cx))),
                    );
        } else if let Some(name) = connecting_name {
            bar = bar
                .child(Spinner::new().small().color(colors.primary))
                .child(
                    div()
                        .text_sm()
                        .text_color(colors.muted_foreground)
                        .child(format!("正在连接 {name} ...")),
                )
                .child(div().flex_1());
        } else {
            bar = bar
                .child(
                    Icon::new(IconName::ServerOff)
                        .small()
                        .text_color(colors.muted_foreground),
                )
                .child(
                    v_flex()
                        .gap_0()
                        .child(div().text_sm().font_medium().child("未连接"))
                        .child(
                            div()
                                .text_xs()
                                .text_color(colors.muted_foreground)
                                .child("从左侧选择一个服务器，或者新建连接"),
                        ),
                )
                .child(div().flex_1())
                .child(
                    Button::new("connection-bar-new")
                        .secondary()
                        .small()
                        .icon(IconName::Plus)
                        .label("新建连接")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_new_connection(None, window, cx)
                        })),
                );
        }

        bar.into_any_element()
    }

    fn render_local_pane(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors;

        v_flex()
            .id("local-pane")
            .flex_1()
            .min_w_0()
            .h_full()
            .rounded_lg()
            .border_1()
            .border_color(colors.border)
            .bg(colors.background)
            .overflow_hidden()
            .drag_over::<DragItem>(|this, _, _, cx| {
                this.bg(cx.theme().colors.primary.opacity(0.05))
                    .border_color(cx.theme().colors.primary)
            })
            .on_drop(cx.listener(|this, item: &DragItem, _, cx| {
                this.handle_drop_on_local(item.clone(), cx)
            }))
            .on_drop(cx.listener(|this, _: &ExternalPaths, _, cx| {
                this.status = "请把系统文件拖到右侧远程窗口以上传".into();
                this.status_kind = StatusKind::Info;
                cx.notify();
            }))
            .child(self.render_local_header(cx))
            .child(self.render_local_files(cx))
            .into_any_element()
    }

    fn render_local_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors;
        let mono = cx.theme().mono_font_family.clone();

        v_flex()
            .gap_2()
            .p_3()
            .border_b_1()
            .border_color(colors.border)
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_1p5()
                            .child(
                                Icon::new(IconName::HardDrive)
                                    .small()
                                    .text_color(colors.primary),
                            )
                            .child(div().text_sm().font_semibold().child("本地文件")),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("local-up")
                            .ghost()
                            .xsmall()
                            .icon(IconName::ArrowUp)
                            .tooltip("上级目录")
                            .on_click(cx.listener(|this, _, _, cx| this.local_up(cx))),
                    )
                    .child(
                        Button::new("local-home")
                            .ghost()
                            .xsmall()
                            .icon(IconName::House)
                            .tooltip("主目录")
                            .on_click(cx.listener(|this, _, _, cx| this.local_home(cx))),
                    )
                    .child(
                        Button::new("local-pick")
                            .ghost()
                            .xsmall()
                            .icon(IconName::FolderOpen)
                            .tooltip("选择本地目录")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.pick_local_folder(window, cx)
                            })),
                    )
                    .child(
                        Button::new("local-refresh")
                            .ghost()
                            .xsmall()
                            .icon(IconName::RefreshCw)
                            .tooltip("刷新")
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_local(cx))),
                    ),
            )
            .child(
                div()
                    .px_2()
                    .py_1()
                    .rounded_sm()
                    .bg(colors.muted)
                    .text_xs()
                    .font_family(mono)
                    .text_color(colors.muted_foreground)
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(self.local.path.display().to_string()),
            )
            .child(
                Input::new(&self.local_search).cleanable(true).prefix(
                    Icon::new(IconName::Search)
                        .xsmall()
                        .text_color(colors.muted_foreground),
                ),
            )
            .into_any_element()
    }

    fn render_local_files(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors;
        let entries = self.local.entries.clone();
        let view = cx.entity();
        let selected = self.local.selected;

        if let Some(error) = &self.local.error {
            return render_pane_error(error, colors.danger, colors);
        }

        if entries.is_empty() {
            if !self.local.all_entries.is_empty() && !self.local_query.trim().is_empty() {
                return render_empty_pane(
                    IconName::Search,
                    "没有匹配的文件",
                    "换个关键词，或清空搜索框",
                    colors,
                );
            }
            return render_empty_pane(
                IconName::FolderOpen,
                "这个目录是空的",
                "拖入文件或选择其他目录",
                colors,
            );
        }

        let count = entries.len();
        let list = uniform_list("local-files", count, move |range, _window, cx| {
            range
                .map(|index| {
                    render_file_row(
                        Side::Local,
                        index,
                        &entries[index],
                        selected == Some(index),
                        view.clone(),
                        cx,
                    )
                })
                .collect::<Vec<_>>()
        })
        .track_scroll(&self.local_scroll)
        .h_full()
        .w_full();

        v_flex()
            .flex_1()
            .min_h_0()
            .child(self.render_file_columns(colors))
            .child(
                div().relative().flex_1().min_h_0().child(list).child(
                    div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .bottom_0()
                        .w(px(12.0))
                        .child(
                            Scrollbar::vertical(&self.local_scroll)
                                .mode(ScrollbarMode::Always)
                                .viewport_from_layout(),
                        ),
                ),
            )
            .into_any_element()
    }

    fn render_remote_pane(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors;

        v_flex()
            .id("remote-pane")
            .flex_1()
            .min_w_0()
            .h_full()
            .rounded_lg()
            .border_1()
            .border_color(colors.border)
            .bg(colors.background)
            .overflow_hidden()
            .drag_over::<DragItem>(|this, _, _, cx| {
                this.bg(cx.theme().colors.primary.opacity(0.05))
                    .border_color(cx.theme().colors.primary)
            })
            .drag_over::<ExternalPaths>(|this, _, _, cx| {
                this.bg(cx.theme().colors.primary.opacity(0.05))
                    .border_color(cx.theme().colors.primary)
            })
            .on_drop(cx.listener(|this, item: &DragItem, _, cx| {
                this.handle_drop_on_remote(item.clone(), cx)
            }))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.upload_external_paths(paths.paths().to_vec(), cx)
            }))
            .child(self.render_remote_header(cx))
            .child(self.render_remote_files(cx))
            .into_any_element()
    }

    fn render_remote_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors;
        let mono = cx.theme().mono_font_family.clone();
        let connected = self.active_session_state().is_some();
        let transferring = self.is_transferring();

        let mut title = h_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_1p5()
                    .child(
                        Icon::new(IconName::Server)
                            .small()
                            .text_color(if connected {
                                colors.primary
                            } else {
                                colors.muted_foreground
                            }),
                    )
                    .child(div().text_sm().font_semibold().child("远程服务器")),
            )
            .child(div().flex_1());

        if connected {
            title = title
                .child(
                    Button::new("remote-up")
                        .ghost()
                        .xsmall()
                        .icon(IconName::ArrowUp)
                        .tooltip("上级目录")
                        .on_click(cx.listener(|this, _, _, cx| this.remote_up(cx))),
                )
                .child(
                    Button::new("remote-home")
                        .ghost()
                        .xsmall()
                        .icon(IconName::House)
                        .tooltip("登录目录")
                        .on_click(cx.listener(|this, _, _, cx| this.remote_home(cx))),
                )
                .child(
                    Button::new("remote-toggle-hidden")
                        .ghost()
                        .xsmall()
                        .icon(if self.show_hidden_files {
                            IconName::Eye
                        } else {
                            IconName::EyeOff
                        })
                        .tooltip(if self.show_hidden_files {
                            "隐藏以 . 开头的文件"
                        } else {
                            "显示隐藏文件"
                        })
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_hidden_files(cx))),
                )
                .child(
                    Button::new("remote-refresh")
                        .ghost()
                        .xsmall()
                        .icon(IconName::RefreshCw)
                        .tooltip("刷新")
                        .on_click(cx.listener(|this, _, _, cx| this.refresh_remote(cx))),
                )
                .child(
                    div()
                        .id("remote-disconnect")
                        .size(px(24.0))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_md()
                        .text_color(colors.muted_foreground)
                        .when(transferring, |this| {
                            this.cursor_not_allowed()
                                .opacity(0.45)
                                .tooltip(|window, cx| {
                                    Tooltip::new("传输进行中，无法断开连接").build(window, cx)
                                })
                        })
                        .when(!transferring, |this| {
                            this.cursor_pointer()
                                .hover(|this| {
                                    this.bg(colors.danger.opacity(0.16))
                                        .text_color(colors.danger)
                                })
                                .tooltip(|window, cx| Tooltip::new("断开连接").build(window, cx))
                        })
                        .on_click(cx.listener(|this, _, _, cx| this.disconnect(cx)))
                        .child(Icon::new(IconName::Unplug).xsmall()),
                );
        } else {
            title = title
                .child(
                    Button::new("remote-up-disabled")
                        .ghost()
                        .xsmall()
                        .icon(IconName::ArrowUp)
                        .disabled(true),
                )
                .child(
                    Button::new("remote-refresh-disabled")
                        .ghost()
                        .xsmall()
                        .icon(IconName::RefreshCw)
                        .disabled(true),
                );
        }

        let path = self
            .active_session_state()
            .map(|session| session.pane.path.clone())
            .unwrap_or_else(|| "连接服务器后显示远程目录".to_string());

        v_flex()
            .gap_2()
            .p_3()
            .border_b_1()
            .border_color(colors.border)
            .child(title)
            .child(
                div()
                    .px_2()
                    .py_1()
                    .rounded_sm()
                    .bg(colors.muted)
                    .text_xs()
                    .font_family(mono)
                    .text_color(colors.muted_foreground)
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(path),
            )
            .child(
                Input::new(&self.remote_search).cleanable(true).prefix(
                    Icon::new(IconName::Search)
                        .xsmall()
                        .text_color(colors.muted_foreground),
                ),
            )
            .into_any_element()
    }

    fn render_remote_files(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors;

        let Some(session) = self.active_session_state() else {
            let connecting_name = self
                .connecting
                .iter()
                .next()
                .and_then(|id| self.settings.connection(id))
                .map(|profile| profile.name.clone());
            if let Some(name) = connecting_name {
                return v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .child(Spinner::new().small().color(colors.primary))
                    .child(
                        div()
                            .text_sm()
                            .font_medium()
                            .child(format!("正在连接 {name} ...")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(colors.muted_foreground)
                            .child("正在建立 SSH 会话并启动 SFTP"),
                    )
                    .into_any_element();
            }

            return v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_2()
                .child(
                    Icon::new(IconName::Cloud)
                        .size(px(38.0))
                        .text_color(colors.muted_foreground),
                )
                .child(div().text_sm().font_medium().child("还没有连接服务器"))
                .child(
                    div()
                        .text_xs()
                        .text_color(colors.muted_foreground)
                        .child("从左侧选择一个连接，或者新建一个连接"),
                )
                .child(
                    Button::new("empty-new-connection")
                        .primary()
                        .small()
                        .icon(IconName::Plus)
                        .label("新建连接")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_new_connection(None, window, cx)
                        })),
                )
                .into_any_element();
        };

        if session.pane.loading {
            return v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_2()
                .child(Spinner::new().small().color(colors.primary))
                .child(
                    div()
                        .text_xs()
                        .text_color(colors.muted_foreground)
                        .child("正在读取远程目录..."),
                )
                .into_any_element();
        }

        if let Some(error) = &session.pane.error {
            return render_pane_error(error, colors.danger, colors);
        }

        if session.pane.entries.is_empty() {
            if !session.pane.all_entries.is_empty() && !self.remote_query.trim().is_empty() {
                return render_empty_pane(
                    IconName::Search,
                    "没有匹配的文件",
                    "换个关键词，或清空搜索框",
                    colors,
                );
            }
            return render_empty_pane(
                IconName::FolderOpen,
                "远程目录是空的",
                "把文件从左侧拖到这里即可上传",
                colors,
            );
        }

        let entries = session.pane.entries.clone();
        let view = cx.entity();
        let selected = session.pane.selected;
        let count = entries.len();

        let list = uniform_list("remote-files", count, move |range, _window, cx| {
            range
                .map(|index| {
                    render_file_row(
                        Side::Remote,
                        index,
                        &entries[index],
                        selected == Some(index),
                        view.clone(),
                        cx,
                    )
                })
                .collect::<Vec<_>>()
        })
        .track_scroll(&session.scroll)
        .h_full()
        .w_full();

        v_flex()
            .flex_1()
            .min_h_0()
            .child(self.render_file_columns(colors))
            .child(
                div().relative().flex_1().min_h_0().child(list).child(
                    div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .bottom_0()
                        .w(px(12.0))
                        .child(
                            Scrollbar::vertical(&session.scroll)
                                .mode(ScrollbarMode::Always)
                                .viewport_from_layout(),
                        ),
                ),
            )
            .into_any_element()
    }

    fn render_file_columns(&self, colors: gpui_kit::component::theme::ThemeColor) -> AnyElement {
        h_flex()
            .h(px(26.0))
            .pl_2()
            .pr_5()
            .gap_2()
            .flex_shrink_0()
            .border_b_1()
            .border_color(colors.border)
            .bg(colors.list_head)
            .text_xs()
            .text_color(colors.muted_foreground)
            .child(div().flex_1().child("名称"))
            .child(div().w(px(82.0)).flex_shrink_0().text_right().child("大小"))
            .child(
                div()
                    .w(px(118.0))
                    .flex_shrink_0()
                    .text_right()
                    .child("修改时间"),
            )
            .into_any_element()
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors;
        let status_bg: Hsla = if cx.theme().mode.is_dark() {
            rgb(0x2e2e2e).into()
        } else {
            rgb(0xf8f8f8).into()
        };

        v_flex()
            .flex_shrink_0()
            .border_t_1()
            .border_color(colors.status_bar_border)
            .bg(status_bg)
            .when_some(self.transfer.as_ref(), |this, transfer| {
                this.child(self.render_transfer_row(transfer, cx))
            })
            .into_any_element()
    }

    fn render_transfer_row(&self, transfer: &TransferView, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors;
        let accent = if transfer.cancelled {
            colors.muted_foreground
        } else if transfer.done && !transfer.ok {
            colors.danger
        } else {
            colors.primary
        };
        let pct = if transfer.done && !transfer.cancelled {
            100.0
        } else if transfer.total == 0 {
            0.0
        } else {
            (transfer.transferred as f64 / transfer.total as f64 * 100.0) as f32
        };

        h_flex()
            .h(px(42.0))
            .px_3()
            .gap_3()
            .border_b_1()
            .border_color(colors.status_bar_border)
            .child(
                Icon::new(match transfer.direction {
                    Direction::Upload => IconName::Upload,
                    Direction::Download => IconName::Download,
                })
                .small()
                .text_color(accent),
            )
            .child(
                div()
                    .w(px(180.0))
                    .flex_shrink_0()
                    .text_xs()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(format!("{} {}", transfer.direction.label(), transfer.name)),
            )
            .child(
                div().flex_1().child(
                    Progress::new("transfer-progress")
                        .value(pct)
                        .small()
                        .color(accent),
                ),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .gap_2()
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .text_left()
                            .text_color(colors.muted_foreground)
                            .child(if transfer.done || transfer.cancelling {
                                String::new()
                            } else {
                                format_speed(transfer.speed)
                            }),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .text_left()
                            .whitespace_nowrap()
                            .text_color(colors.muted_foreground)
                            .child(if transfer.done {
                                transfer.message.clone()
                            } else if transfer.cancelling {
                                "正在取消...".to_string()
                            } else {
                                format_progress(transfer.transferred, transfer.total)
                            }),
                    ),
            )
            .when(!transfer.done, |this| {
                this.child(
                    div()
                        .id("cancel-transfer")
                        .size(px(24.0))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_md()
                        .cursor_pointer()
                        .text_color(colors.muted_foreground)
                        .when(transfer.cancelling, |this| this.opacity(0.45))
                        .hover(|this| {
                            this.bg(colors.danger.opacity(0.16))
                                .text_color(colors.danger)
                        })
                        .tooltip(|window, cx| Tooltip::new("取消当前传输").build(window, cx))
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_current_transfer(cx)))
                        .child(Icon::new(IconName::X).xsmall()),
                )
            })
            .into_any_element()
    }

    fn render_context_menu(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !matches!(self.dialog, DialogState::None) {
            return None;
        }
        let menu = self.context_menu.as_ref()?;
        let colors = cx.theme().colors;

        let mut items = Vec::new();

        match &menu.target {
            ContextTarget::Group { id, name } => {
                items.push(context_menu_item(
                    "ctx-group-new",
                    IconName::Plus,
                    "在此分组新建连接",
                    false,
                    colors,
                    cx.listener({
                        let id = id.clone();
                        move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.context_menu = None;
                            this.open_new_connection(Some(id.clone()), window, cx);
                        }
                    }),
                ));
                items.push(context_menu_item(
                    "ctx-group-edit",
                    IconName::Pencil,
                    "重命名分组",
                    false,
                    colors,
                    cx.listener({
                        let id = id.clone();
                        move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.context_menu = None;
                            this.open_edit_group(&id, window, cx);
                        }
                    }),
                ));
                items.push(context_menu_item(
                    "ctx-group-delete",
                    IconName::Trash,
                    "删除分组",
                    true,
                    colors,
                    cx.listener({
                        let id = id.clone();
                        let name = name.clone();
                        move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.context_menu = None;
                            let has_connections = this.settings.connections.iter().any(|connection| {
                                connection.group_id.as_deref() == Some(id.as_str())
                            });
                            let next_group = this
                                .settings
                                .groups
                                .iter()
                                .find(|group| group.id != *id)
                                .map(|group| group.name.clone());
                            match (has_connections, next_group) {
                                (true, Some(next_name)) => {
                                    this.open_confirm(
                                        "删除分组",
                                        format!("确定删除分组“{name}”吗？分组内的连接会移到“{next_name}”。"),
                                        ConfirmAction::DeleteGroup(id.clone()),
                                        cx,
                                    );
                                }
                                (true, None) => {
                                    let message: SharedString =
                                        "这是最后一个分组，请先创建其他分组或删除其中的连接".into();
                                    this.status = message.clone();
                                    this.status_kind = StatusKind::Error;
                                    this.push_toast(ToastKind::Error, message, cx);
                                }
                                (false, _) => {
                                    this.open_confirm(
                                        "删除分组",
                                        format!("确定删除分组“{name}”吗？"),
                                        ConfirmAction::DeleteGroup(id.clone()),
                                        cx,
                                    );
                                }
                            }
                        }
                    }),
                ));
            }
            ContextTarget::File {
                side,
                session_id,
                path,
                name,
                is_dir,
                permissions,
            } => {
                items.push(context_menu_item(
                    "ctx-file-rename",
                    IconName::Pencil,
                    "重命名",
                    false,
                    colors,
                    cx.listener({
                        let side = *side;
                        let session_id = session_id.clone();
                        let path = path.clone();
                        let name = name.clone();
                        move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.open_rename_file(
                                side,
                                session_id.clone(),
                                path.clone(),
                                &name,
                                window,
                                cx,
                            );
                        }
                    }),
                ));

                if *side == Side::Remote
                    && let Some(session_id) = session_id.clone()
                {
                    let permissions = *permissions;

                    let chmod_session = session_id.clone();
                    let chmod_path = path.clone();
                    let chmod_name = name.clone();
                    items.push(context_menu_item(
                        "ctx-file-chmod",
                        IconName::Shield,
                        "修改权限",
                        false,
                        colors,
                        cx.listener(move |this, _, _window, cx| {
                            cx.stop_propagation();
                            this.open_chmod_file(
                                chmod_session.clone(),
                                chmod_path.clone(),
                                chmod_name.clone(),
                                permissions,
                                cx,
                            );
                        }),
                    ));

                    let info_session = session_id.clone();
                    let info_path = path.clone();
                    let info_name = name.clone();
                    items.push(context_menu_item(
                        "ctx-file-info",
                        IconName::Info,
                        "详细信息",
                        false,
                        colors,
                        cx.listener(move |this, _, _window, cx| {
                            cx.stop_propagation();
                            this.open_file_info(
                                info_session.clone(),
                                info_path.clone(),
                                info_name.clone(),
                                cx,
                            );
                        }),
                    ));
                }

                let delete_name = name.clone();
                let delete_message = if *is_dir {
                    format!("确定删除“{name}”及其中的所有内容吗？此操作不可撤销。")
                } else {
                    format!("确定删除“{name}”吗？此操作不可撤销。")
                };
                let action = match (*side, session_id.clone()) {
                    (Side::Local, _) => ConfirmAction::DeleteLocalFile {
                        path: path.clone(),
                        name: delete_name,
                        is_dir: *is_dir,
                    },
                    (Side::Remote, Some(session_id)) => ConfirmAction::DeleteRemoteFile {
                        session_id,
                        path: path.clone(),
                        name: delete_name,
                    },
                    (Side::Remote, None) => return None,
                };
                items.push(context_menu_item(
                    "ctx-file-delete",
                    IconName::Trash,
                    "删除",
                    true,
                    colors,
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.open_confirm("删除文件", delete_message.clone(), action.clone(), cx);
                    }),
                ));
            }
            ContextTarget::Connection { id, name } => {
                let connected = self.sessions.contains_key(id);
                let is_active = self.active_session.as_deref() == Some(id.as_str());

                if connected {
                    if !is_active {
                        items.push(context_menu_item(
                            "ctx-connection-switch",
                            IconName::Plug,
                            "切换到此服务器",
                            false,
                            colors,
                            cx.listener({
                                let id = id.clone();
                                let name = name.clone();
                                move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.context_menu = None;
                                    this.active_session = Some(id.clone());
                                    this.status = format!("已切换到 {name}").into();
                                    this.status_kind = StatusKind::Info;
                                    cx.notify();
                                }
                            }),
                        ));
                    }
                    if self.is_transferring() {
                        items.push(context_menu_item_disabled(
                            "ctx-connection-disconnect",
                            IconName::Unplug,
                            "断开连接（传输中）",
                            colors,
                        ));
                    } else {
                        items.push(context_menu_item(
                            "ctx-connection-disconnect",
                            IconName::Unplug,
                            "断开连接",
                            false,
                            colors,
                            cx.listener({
                                let id = id.clone();
                                move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.context_menu = None;
                                    this.disconnect_session(&id, cx);
                                }
                            }),
                        ));
                    }
                } else {
                    items.push(context_menu_item(
                        "ctx-connection-connect",
                        IconName::Plug,
                        "连接服务器",
                        false,
                        colors,
                        cx.listener({
                            let id = id.clone();
                            move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.context_menu = None;
                                this.connect_profile(&id, cx);
                            }
                        }),
                    ));
                }

                items.push(context_menu_item(
                    "ctx-connection-copy",
                    IconName::Copy,
                    "复制连接",
                    false,
                    colors,
                    cx.listener({
                        let id = id.clone();
                        move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.context_menu = None;
                            this.open_copy_connection(&id, window, cx);
                        }
                    }),
                ));
                items.push(context_menu_item(
                    "ctx-connection-edit",
                    IconName::Pencil,
                    "编辑连接",
                    false,
                    colors,
                    cx.listener({
                        let id = id.clone();
                        move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.context_menu = None;
                            this.open_edit_connection(&id, window, cx);
                        }
                    }),
                ));
                items.push(context_menu_item(
                    "ctx-connection-delete",
                    IconName::Trash,
                    "删除连接",
                    true,
                    colors,
                    cx.listener({
                        let id = id.clone();
                        let name = name.clone();
                        move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.context_menu = None;
                            this.open_confirm(
                                "删除连接",
                                format!("确定删除连接“{name}”吗？"),
                                ConfirmAction::DeleteConnection(id.clone()),
                                cx,
                            );
                        }
                    }),
                ));
            }
        }

        let item_count = items.len();
        let max_label_chars = items.iter().map(|(_, chars)| *chars).max().unwrap_or(4);
        let width = (52.0 + max_label_chars as f32 * 14.0).max(132.0);
        let height = 8.0 + item_count as f32 * 32.0;
        let viewport = window.viewport_size();
        let left = menu
            .position
            .x
            .as_f32()
            .clamp(8.0, (viewport.width.as_f32() - width - 8.0).max(8.0));
        let top = menu
            .position
            .y
            .as_f32()
            .clamp(8.0, (viewport.height.as_f32() - height - 8.0).max(8.0));

        let panel = v_flex()
            .id("context-menu")
            .absolute()
            .left(px(left))
            .top(px(top))
            .w(px(width))
            .p_1()
            .gap_0p5()
            .bg(colors.popover)
            .text_color(colors.popover_foreground)
            .rounded_md()
            .border_1()
            .border_color(colors.border)
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .children(items.into_iter().map(|(element, _)| element));

        Some(
            div()
                .id("context-menu-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .occlude()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| this.close_context_menu(cx)),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(|this, _, _, cx| this.close_context_menu(cx)),
                )
                .child(panel)
                .into_any_element(),
        )
    }

    fn render_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        match &self.dialog {
            DialogState::None => None,
            DialogState::Connection(form) => Some(self.render_connection_dialog(form, cx)),
            DialogState::Group(form) => Some(self.render_group_dialog(form, cx)),
            DialogState::Rename(form) => Some(self.render_rename_dialog(form, cx)),
            DialogState::Chmod(form) => Some(self.render_chmod_dialog(form, cx)),
            DialogState::FileInfo(form) => Some(self.render_file_info_dialog(form, cx)),
            DialogState::Confirm(confirm) => Some(self.render_confirm_dialog(confirm, cx)),
        }
    }

    fn render_dialog_shell(
        &self,
        title: &str,
        subtitle: Option<&str>,
        icon: IconName,
        body: AnyElement,
        footer: AnyElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors;

        let panel = v_flex()
            .id("dialog-panel")
            .w(px(570.0))
            .max_h(px(760.0))
            .bg(colors.popover)
            .text_color(colors.popover_foreground)
            .rounded_lg()
            .border_1()
            .border_color(colors.border)
            .shadow_lg()
            .overflow_hidden()
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                h_flex()
                    .gap_3()
                    .px_5()
                    .py_4()
                    .border_b_1()
                    .border_color(colors.border)
                    .child(
                        div()
                            .size(px(34.0))
                            .rounded_lg()
                            .bg(colors.primary.opacity(0.12))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(Icon::new(icon).small().text_color(colors.primary)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .gap_0()
                            .child(div().text_base().font_semibold().child(title.to_string()))
                            .when_some(subtitle, |this, subtitle| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .text_color(colors.muted_foreground)
                                        .child(subtitle.to_string()),
                                )
                            }),
                    )
                    .child(
                        Button::new("dialog-close")
                            .ghost()
                            .xsmall()
                            .icon(IconName::X)
                            .on_click(cx.listener(|this, _, _, cx| this.close_dialog(cx))),
                    ),
            )
            .child(
                div()
                    .id("dialog-body-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(body),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .px_5()
                    .py_3()
                    .border_t_1()
                    .border_color(colors.border)
                    .child(footer),
            );

        v_flex()
            .id("dialog-backdrop")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .items_center()
            .justify_center()
            .bg(hsla(0.0, 0.0, 0.0, 0.34))
            // The backdrop floats above the file lists and sidebar. Without
            // this, their hitboxes stay "hovered" through the overlay, so a
            // text selection drag inside a dialog input can start dragging the
            // file/connection row underneath the pointer.
            .occlude()
            .child(panel)
            .into_any_element()
    }

    fn render_connection_dialog(
        &self,
        form: &ConnectionForm,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors;
        let editing = form.editing_id.is_some();

        let group_chips = {
            let mut chips = h_flex().flex_wrap().gap_1();
            if self.settings.groups.is_empty() {
                chips = chips.child(
                    div()
                        .text_xs()
                        .text_color(colors.muted_foreground)
                        .child("还没有分组，请先创建一个分组"),
                );
            }
            for group in &self.settings.groups {
                let selected = form.group_id.as_deref() == Some(group.id.as_str());
                let group_id = group.id.clone();
                chips = chips.child(
                    Button::new(ElementId::Name(format!("group-chip-{}", group.id).into()))
                        .xsmall()
                        .label(group.name.clone())
                        .when(selected, |button| button.primary())
                        .when(!selected, |button| button.secondary())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let DialogState::Connection(form) = &mut this.dialog {
                                form.group_id = Some(group_id.clone());
                                cx.notify();
                            }
                        })),
                );
            }
            chips
        };

        let mut body = v_flex()
            .gap_3()
            .p_5()
            .child(labeled(
                "连接名称",
                Input::new(&form.name).cleanable(true),
                cx,
            ))
            .child(
                v_flex()
                    .gap_1()
                    .child(field_label("分组", colors))
                    .child(group_chips),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_start()
                    .child(div().flex_1().child(labeled(
                        "主机地址",
                        Input::new(&form.host).cleanable(true),
                        cx,
                    )))
                    .child(
                        div()
                            .w(px(92.0))
                            .child(labeled("端口", Input::new(&form.port), cx)),
                    ),
            )
            .child(labeled(
                "用户名",
                Input::new(&form.username).cleanable(true),
                cx,
            ))
            .child(
                v_flex()
                    .gap_1()
                    .child(field_label("认证方式", colors))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("auth-password")
                                    .xsmall()
                                    .label("密码")
                                    .icon(IconName::Lock)
                                    .when(!form.use_key, |button| button.primary())
                                    .when(form.use_key, |button| button.secondary())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if let DialogState::Connection(form) = &mut this.dialog {
                                            form.use_key = false;
                                            cx.notify();
                                        }
                                    })),
                            )
                            .child(
                                Button::new("auth-key")
                                    .xsmall()
                                    .label("公钥")
                                    .icon(IconName::Key)
                                    .when(form.use_key, |button| button.primary())
                                    .when(!form.use_key, |button| button.secondary())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if let DialogState::Connection(form) = &mut this.dialog {
                                            form.use_key = true;
                                            cx.notify();
                                        }
                                    })),
                            ),
                    ),
            );

        if form.use_key {
            body = body
                .child(
                    v_flex()
                        .gap_1()
                        .child(
                            h_flex()
                                .gap_2()
                                .child(div().flex_1().child(labeled(
                                    "私钥文件",
                                    Input::new(&form.key_path).cleanable(true),
                                    cx,
                                )))
                                .child(
                                    div().pt_5().child(
                                        Button::new("pick-key")
                                            .secondary()
                                            .small()
                                            .icon(IconName::FolderOpen)
                                            .label("选择")
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.pick_key_file(window, cx)
                                            })),
                                    ),
                                ),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(colors.muted_foreground)
                                .child("支持 OpenSSH / PEM 格式，口令留空表示私钥未加密"),
                        ),
                )
                .child(labeled(
                    "私钥口令",
                    Input::new(&form.passphrase).mask_toggle(),
                    cx,
                ));
        } else {
            body = body.child(labeled(
                "密码",
                Input::new(&form.password).mask_toggle(),
                cx,
            ));
        }

        body = body.child(labeled(
            "远程目录",
            Input::new(&form.remote_dir).cleanable(true),
            cx,
        ));

        if let Some(error) = &form.error {
            body = body.child(
                div()
                    .text_xs()
                    .text_color(colors.danger)
                    .child(error.clone()),
            );
        }

        let footer = h_flex()
            .gap_2()
            .child(
                Button::new("connection-cancel")
                    .ghost()
                    .small()
                    .label("取消")
                    .on_click(cx.listener(|this, _, _, cx| this.close_dialog(cx))),
            )
            .child(
                Button::new("connection-save")
                    .secondary()
                    .small()
                    .label("保存")
                    .on_click(cx.listener(|this, _, _, cx| this.save_connection(false, cx))),
            )
            .child(
                Button::new("connection-save-connect")
                    .primary()
                    .small()
                    .icon(IconName::Plug)
                    .label("保存并连接")
                    .on_click(cx.listener(|this, _, _, cx| this.save_connection(true, cx))),
            )
            .into_any_element();

        self.render_dialog_shell(
            if form.copying {
                "复制连接"
            } else if editing {
                "编辑连接"
            } else {
                "新建连接"
            },
            Some("密码或 SSH 公钥登录"),
            IconName::Server,
            body.into_any_element(),
            footer,
            cx,
        )
    }

    fn render_rename_dialog(&self, form: &RenameForm, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors;

        let mut body = v_flex()
            .gap_3()
            .p_5()
            .child(labeled(
                "新的名称",
                Input::new(&form.name).cleanable(true),
                cx,
            ))
            .child(div().text_xs().text_color(colors.muted_foreground).child(
                if form.side == Side::Local {
                    "重命名本地文件或文件夹"
                } else {
                    "重命名服务器上的文件或文件夹"
                },
            ));

        if let Some(error) = &form.error {
            body = body.child(
                div()
                    .text_xs()
                    .text_color(colors.danger)
                    .child(error.clone()),
            );
        }

        let footer = h_flex()
            .gap_2()
            .child(
                Button::new("rename-cancel")
                    .ghost()
                    .small()
                    .label("取消")
                    .on_click(cx.listener(|this, _, _, cx| this.close_dialog(cx))),
            )
            .child(
                Button::new("rename-save")
                    .primary()
                    .small()
                    .label("重命名")
                    .on_click(cx.listener(|this, _, _, cx| this.save_rename(cx))),
            )
            .into_any_element();

        self.render_dialog_shell(
            "重命名",
            Some("输入新的文件或文件夹名称"),
            IconName::Pencil,
            body.into_any_element(),
            footer,
            cx,
        )
    }

    fn render_chmod_dialog(&self, form: &ChmodForm, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors;
        let view = cx.entity();

        let rows = [("所有者", 6u32), ("用户组", 3u32), ("其他", 0u32)];
        let mut permissions = v_flex().gap_2();
        for (label, shift) in rows {
            let mut row = h_flex().gap_4().items_center();
            row = row.child(
                div()
                    .w(px(72.0))
                    .text_sm()
                    .text_color(colors.foreground)
                    .child(label),
            );
            for (bit, name) in [(4u32, "可读"), (2u32, "可写"), (1u32, "可执行")] {
                let mask = bit << shift;
                let checked = form.mode & mask != 0;
                let checkbox_view = view.clone();
                row = row.child(
                    Checkbox::new(ElementId::Name(format!("chmod-{shift}-{bit}").into()))
                        .checked(checked)
                        .label(name)
                        .on_click(move |checked: &bool, _, cx| {
                            let checked = *checked;
                            checkbox_view.update(cx, |this, cx| {
                                if let DialogState::Chmod(form) = &mut this.dialog {
                                    if checked {
                                        form.mode |= mask;
                                    } else {
                                        form.mode &= !mask;
                                    }
                                    cx.notify();
                                }
                            });
                        }),
                );
            }
            permissions = permissions.child(row);
        }

        let body = v_flex()
            .gap_3()
            .p_5()
            .child(
                div()
                    .text_xs()
                    .text_color(colors.muted_foreground)
                    .child(format!("文件：{}", form.name)),
            )
            .child(permissions)
            .child(
                div()
                    .text_xs()
                    .text_color(colors.muted_foreground)
                    .child(format!(
                        "当前权限：{}（{}）",
                        format_mode(form.mode),
                        format_mode_symbolic(form.mode)
                    )),
            )
            .into_any_element();

        let footer = h_flex()
            .gap_2()
            .child(
                Button::new("chmod-cancel")
                    .ghost()
                    .small()
                    .label("取消")
                    .on_click(cx.listener(|this, _, _, cx| this.close_dialog(cx))),
            )
            .child(
                Button::new("chmod-save")
                    .primary()
                    .small()
                    .label("应用")
                    .on_click(cx.listener(|this, _, _, cx| this.save_chmod(cx))),
            )
            .into_any_element();

        self.render_dialog_shell(
            "修改文件权限",
            Some("勾选所有者、用户组和其他用户的可读 / 可写 / 可执行权限"),
            IconName::Shield,
            body,
            footer,
            cx,
        )
    }

    fn render_file_info_dialog(&self, form: &FileInfoForm, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors;
        let mono = cx.theme().mono_font_family.clone();

        let mut body = v_flex().gap_3().p_5();

        if form.loading {
            body = body.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(Spinner::new().small().color(colors.primary))
                    .child(
                        div()
                            .text_sm()
                            .text_color(colors.muted_foreground)
                            .child(format!("正在读取 {} 的文件信息...", form.name)),
                    ),
            );
        } else if let Some(error) = &form.error {
            body = body.child(
                div()
                    .text_sm()
                    .text_color(colors.danger)
                    .child(format!("{}：{error}", form.name)),
            );
        } else if let Some(info) = &form.info {
            let size = if info.kind == "目录" {
                "—".to_string()
            } else {
                format!("{}（{} 字节）", format_size(info.size), info.size)
            };
            let permissions = match info.permissions {
                Some(mode) => format!("{}（{}）", format_mode(mode), format_mode_symbolic(mode)),
                None => "服务器未提供".to_string(),
            };
            let owner = match (&info.user, info.uid) {
                (Some(user), Some(uid)) => format!("{user}（uid {uid}）"),
                (Some(user), None) => user.clone(),
                (None, Some(uid)) => format!("uid {uid}"),
                (None, None) => "服务器未提供".to_string(),
            };
            let group = match (&info.group, info.gid) {
                (Some(group), Some(gid)) => format!("{group}（gid {gid}）"),
                (Some(group), None) => group.clone(),
                (None, Some(gid)) => format!("gid {gid}"),
                (None, None) => "服务器未提供".to_string(),
            };

            body = body
                .child(info_row("名称", info.name.clone(), false, colors, &mono))
                .child(info_row("路径", info.path.clone(), true, colors, &mono))
                .child(info_row("类型", info.kind.clone(), false, colors, &mono))
                .child(info_row("大小", size, false, colors, &mono))
                .child(info_row("权限", permissions, true, colors, &mono))
                .child(info_row("所属用户", owner, false, colors, &mono))
                .child(info_row("所属用户组", group, false, colors, &mono))
                .child(info_row(
                    "最后修改时间",
                    format_time(info.modified),
                    false,
                    colors,
                    &mono,
                ))
                .child(info_row(
                    "最后访问时间",
                    format_time(info.accessed),
                    false,
                    colors,
                    &mono,
                ));
        }

        let footer = h_flex().gap_2().child(
            Button::new("file-info-close")
                .primary()
                .small()
                .label("关闭")
                .on_click(cx.listener(|this, _, _, cx| this.close_dialog(cx))),
        );

        self.render_dialog_shell(
            "详细信息",
            Some("来自服务器的文件属性"),
            IconName::Info,
            body.into_any_element(),
            footer.into_any_element(),
            cx,
        )
    }

    fn render_group_dialog(&self, form: &GroupForm, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors;
        let editing = form.editing_id.is_some();

        let mut body = v_flex()
            .gap_3()
            .p_5()
            .child(labeled(
                "分组名称",
                Input::new(&form.name).cleanable(true),
                cx,
            ))
            .child(
                div()
                    .text_xs()
                    .text_color(colors.muted_foreground)
                    .child("分组只用于整理连接，不会影响服务器上的文件"),
            );

        if let Some(error) = &form.error {
            body = body.child(
                div()
                    .text_xs()
                    .text_color(colors.danger)
                    .child(error.clone()),
            );
        }

        let footer = h_flex()
            .gap_2()
            .child(
                Button::new("group-cancel")
                    .ghost()
                    .small()
                    .label("取消")
                    .on_click(cx.listener(|this, _, _, cx| this.close_dialog(cx))),
            )
            .child(
                Button::new("group-save")
                    .primary()
                    .small()
                    .label(if editing {
                        "保存修改"
                    } else {
                        "创建分组"
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.save_group(cx))),
            )
            .into_any_element();

        self.render_dialog_shell(
            if editing {
                "重命名分组"
            } else {
                "新建分组"
            },
            Some("在连接管理器中整理服务器"),
            IconName::FolderPlus,
            body.into_any_element(),
            footer,
            cx,
        )
    }

    fn render_confirm_dialog(&self, confirm: &ConfirmDialog, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors;

        let body = v_flex()
            .gap_3()
            .p_5()
            .child(
                div()
                    .text_sm()
                    .text_color(colors.foreground)
                    .child(confirm.message.clone()),
            )
            .into_any_element();

        let action = confirm.action.clone();
        let danger = confirm.danger;
        let confirm_label = confirm.confirm_label.clone();
        let footer = h_flex()
            .gap_2()
            .child(
                Button::new("confirm-cancel")
                    .ghost()
                    .small()
                    .label("取消")
                    .on_click(cx.listener(|this, _, _, cx| this.close_dialog(cx))),
            )
            .child(
                Button::new("confirm-action")
                    .when(danger, |button| button.danger())
                    .when(!danger, |button| button.primary())
                    .small()
                    .icon(if danger {
                        IconName::Trash
                    } else {
                        IconName::Upload
                    })
                    .label(confirm_label)
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.apply_confirm(action.clone(), cx)),
                    ),
            )
            .into_any_element();

        self.render_dialog_shell(
            confirm.title.as_ref(),
            None,
            if danger {
                IconName::TriangleAlert
            } else {
                IconName::Info
            },
            body,
            footer,
            cx,
        )
    }
}

impl Render for RgFtpApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;
        let context_menu = self.render_context_menu(window, cx);
        let dialog = self.render_dialog(cx);
        let toasts = if self.toasts.is_empty() {
            None
        } else {
            Some(self.render_toasts(cx))
        };

        div()
            .size_full()
            .relative()
            .bg(colors.background)
            .text_color(colors.foreground)
            .child(
                v_flex()
                    .size_full()
                    .child(self.render_header(window, cx))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_h_0()
                            .child(self.render_sidebar(cx))
                            .child(self.render_main(cx)),
                    ),
            )
            .on_drag_move(cx.listener(|this, _: &DragMoveEvent<DragItem>, _, cx| {
                this.set_sidebar_drop_target(None, cx);
            }))
            .on_drop(cx.listener(|this, _: &SidebarDragItem, _, cx| {
                this.set_sidebar_drop_target(None, cx);
            }))
            .when_some(context_menu, |this, menu| this.child(menu))
            .when_some(dialog, |this, dialog| this.child(dialog))
            .when_some(toasts, |this, toasts| this.child(toasts))
    }
}

struct DragPreview {
    label: SharedString,
    is_dir: bool,
}

impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;

        h_flex()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(colors.popover)
            .text_color(colors.popover_foreground)
            .border_1()
            .border_color(colors.primary)
            .shadow_md()
            .child(
                Icon::new(if self.is_dir {
                    IconName::Folder
                } else {
                    IconName::File
                })
                .xsmall()
                .text_color(colors.primary),
            )
            .child(
                div()
                    .text_xs()
                    .max_w(px(220.0))
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(self.label.clone()),
            )
    }
}

struct SidebarDragPreview {
    label: SharedString,
    is_group: bool,
}

impl Render for SidebarDragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;

        h_flex()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(colors.popover)
            .text_color(colors.popover_foreground)
            .border_1()
            .border_color(colors.primary)
            .shadow_md()
            .child(
                Icon::new(if self.is_group {
                    IconName::FolderOpen
                } else {
                    IconName::Server
                })
                .xsmall()
                .text_color(colors.primary),
            )
            .child(
                div()
                    .text_xs()
                    .max_w(px(220.0))
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(self.label.clone()),
            )
    }
}

fn make_input(
    window: &mut Window,
    cx: &mut App,
    placeholder: &str,
    value: &str,
    masked: bool,
) -> Entity<InputState> {
    let placeholder = placeholder.to_string();
    let value = value.to_string();
    cx.new(|cx| {
        let mut state = InputState::new(window, cx).placeholder(placeholder);
        if masked {
            state = state.masked(true);
        }
        if !value.is_empty() {
            state = state.default_value(value);
        }
        state
    })
}

fn labeled(label: &'static str, input: impl IntoElement, cx: &App) -> AnyElement {
    let colors = cx.theme().colors;
    v_flex()
        .gap_1()
        .child(field_label(label, colors))
        .child(input)
        .into_any_element()
}

fn field_label(label: &'static str, colors: gpui_kit::component::theme::ThemeColor) -> AnyElement {
    div()
        .text_xs()
        .font_medium()
        .text_color(colors.muted_foreground)
        .child(label)
        .into_any_element()
}

/// Filters a directory listing by the hidden-file toggle and the fuzzy
/// search box. Directory rows are kept in the original (sorted) order.
fn filter_entries(entries: &[FileEntry], query: &str, show_hidden: bool) -> Vec<FileEntry> {
    let query = query.trim();
    entries
        .iter()
        .filter(|entry| show_hidden || !entry.hidden)
        .filter(|entry| query.is_empty() || crate::util::fuzzy_match(&entry.name, query))
        .cloned()
        .collect()
}

fn context_menu_item(
    id: &'static str,
    icon: IconName,
    label: &'static str,
    danger: bool,
    colors: gpui_kit::component::theme::ThemeColor,
    handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> (AnyElement, usize) {
    let label_chars = label.chars().count();
    let element = h_flex()
        .id(id)
        .h(px(32.0))
        .w_full()
        .px_2()
        .gap_2()
        .rounded_sm()
        .text_sm()
        .text_color(if danger {
            colors.danger
        } else {
            colors.foreground
        })
        .hover(move |this| {
            this.bg(if danger {
                colors.danger.opacity(0.12)
            } else {
                colors.secondary_hover
            })
            .text_color(if danger {
                colors.danger
            } else {
                colors.foreground
            })
        })
        .child(Icon::new(icon).xsmall().text_color(if danger {
            colors.danger
        } else {
            colors.muted_foreground
        }))
        .child(label)
        .on_click(handler)
        .into_any_element();
    (element, label_chars)
}

fn context_menu_item_disabled(
    id: &'static str,
    icon: IconName,
    label: &'static str,
    colors: gpui_kit::component::theme::ThemeColor,
) -> (AnyElement, usize) {
    let label_chars = label.chars().count();
    let element = h_flex()
        .id(id)
        .h(px(32.0))
        .w_full()
        .px_2()
        .gap_2()
        .rounded_sm()
        .text_sm()
        .opacity(0.45)
        .cursor_not_allowed()
        .text_color(colors.muted_foreground)
        .child(Icon::new(icon).xsmall().text_color(colors.muted_foreground))
        .child(label)
        .into_any_element();
    (element, label_chars)
}

fn info_row(
    label: &'static str,
    value: String,
    monospace: bool,
    colors: gpui_kit::component::theme::ThemeColor,
    mono_font: &SharedString,
) -> AnyElement {
    let value_style = div().flex_1().min_w_0().text_sm().child(value);
    let value_style = if monospace {
        value_style
            .text_xs()
            .font_family(mono_font.clone())
            .text_color(colors.foreground)
    } else {
        value_style.text_color(colors.foreground)
    };

    h_flex()
        .gap_3()
        .items_start()
        .child(
            div()
                .w(px(104.0))
                .flex_shrink_0()
                .text_xs()
                .text_color(colors.muted_foreground)
                .child(label),
        )
        .child(value_style)
        .into_any_element()
}

fn render_pane_error(
    message: &str,
    color: Hsla,
    colors: gpui_kit::component::theme::ThemeColor,
) -> AnyElement {
    v_flex()
        .flex_1()
        .items_center()
        .justify_center()
        .gap_2()
        .p_4()
        .child(
            Icon::new(IconName::CircleAlert)
                .size(px(30.0))
                .text_color(color),
        )
        .child(
            div()
                .max_w(px(360.0))
                .text_xs()
                .text_color(colors.muted_foreground)
                .child(message.to_string()),
        )
        .into_any_element()
}

fn render_empty_pane(
    icon: IconName,
    title: &'static str,
    hint: &'static str,
    colors: gpui_kit::component::theme::ThemeColor,
) -> AnyElement {
    v_flex()
        .flex_1()
        .items_center()
        .justify_center()
        .gap_2()
        .child(
            Icon::new(icon)
                .size(px(34.0))
                .text_color(colors.muted_foreground),
        )
        .child(div().text_sm().font_medium().child(title))
        .child(
            div()
                .text_xs()
                .text_color(colors.muted_foreground)
                .child(hint),
        )
        .into_any_element()
}

fn render_file_row(
    side: Side,
    index: usize,
    entry: &FileEntry,
    selected: bool,
    view: Entity<RgFtpApp>,
    cx: &mut App,
) -> AnyElement {
    let colors = cx.theme().colors;
    let payload = DragItem {
        source: side,
        path: entry.path.clone(),
        name: entry.name.clone(),
        is_dir: entry.is_dir,
    };

    let click_view = view.clone();
    let menu_view = view.clone();
    let menu_path = entry.path.clone();
    let row = h_flex()
        .id(match side {
            Side::Local => ("local-file", index),
            Side::Remote => ("remote-file", index),
        })
        .h(px(30.0))
        .w_full()
        .pl_2()
        .pr_5()
        .gap_2()
        .rounded_sm()
        .cursor_pointer()
        .when(selected, |this| this.bg(colors.list_active))
        .hover(|this| this.bg(colors.list_hover))
        .child(
            Icon::new(if entry.is_dir {
                IconName::Folder
            } else {
                IconName::File
            })
            .small()
            .text_color(if entry.is_dir {
                colors.primary
            } else {
                colors.muted_foreground
            }),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_sm()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(entry.name.clone()),
        )
        .child(
            div()
                .w(px(82.0))
                .flex_shrink_0()
                .text_xs()
                .text_right()
                .text_color(colors.muted_foreground)
                .child(if entry.is_dir {
                    "—".to_string()
                } else {
                    format_size(entry.size)
                }),
        )
        .child(
            div()
                .w(px(118.0))
                .flex_shrink_0()
                .text_xs()
                .text_right()
                .text_color(colors.muted_foreground)
                .child(format_time(entry.modified)),
        )
        .on_click(move |event, _window, cx| {
            let click_count = event.click_count();
            click_view.update(cx, |app, cx| {
                app.on_entry_click(side, index, click_count, cx);
            });
        })
        .on_mouse_down(MouseButton::Right, move |event, _, cx| {
            let position = event.position;
            menu_view.update(cx, |app, cx| {
                app.open_file_context_menu(side, menu_path.clone(), position, cx);
            });
        })
        .on_drag(payload, |payload, _offset, _window, cx| {
            let label: SharedString = payload.name.clone().into();
            let is_dir = payload.is_dir;
            cx.new(|_| DragPreview { label, is_dir })
        });

    row.into_any_element()
}
