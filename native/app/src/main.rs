//! The native Shepherd app (milestone 1): sidebar with spaces and agents, tab bar, and the live terminal
//! surface from the server. Everything that is not drawing lives in `shepherd-app-core`.

mod commands;
mod config_panel;
mod editor;
mod graph_view;
mod grid;
mod keys;
mod palette;
mod panel;
mod pattern_editor;
mod prism;
mod sessions;
mod theme;

use iced::keyboard;
use iced::widget::canvas::Canvas;
use iced::widget::{
    button, column, container, mouse_area, row, scrollable, text, text_input, tooltip,
};
use iced::{Background, Border, Color, Element, Length, Size, Subscription, Task, Theme};
use shepherd_app_core::api;
use shepherd_app_core::data::Data;
use shepherd_app_core::endpoint::{Endpoint, EndpointSender, Geometry, SNAPSHOT};
use shepherd_app_core::snapshot::Snapshot;
use shepherd_app_core::watch::StateWatcher;
use shepherd_app_core::wire::{ClientMouseButton, ClientMouseKind, Incoming, PaneSurface};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use theme::{
    item_style, mini_button_style, pad, primary_button_style, status_color, status_name, ACCENT,
    BG, DIM, LINE, PANEL,
};

const SIDEBAR_W: f32 = 232.0;
const SIDEBAR_COLLAPSED_W: f32 = 48.0;
const TABBAR_H: f32 = 42.0;
const STATUS_H: f32 = 30.0;
const TOPBAR_H: f32 = 46.0;
const STRIP_H: f32 = 6.0;
/// Margin between the terminal and what surrounds it, and the 1 px border of its frame (both sides).
const TERM_PAD: f32 = 8.0;
const TERM_FRAME: f32 = 2.0 * TERM_PAD + 2.0;
const START: Size = Size::new(1280.0, 800.0);

fn main() -> iced::Result {
    iced::application(App::boot, App::update, App::view)
        .title("Shepherd")
        .font(theme::FONT_FILES[0])
        .font(theme::FONT_FILES[1])
        .font(theme::FONT_FILES[2])
        .font(theme::FONT_FILES[3])
        .default_font(theme::MONO)
        .subscription(App::subscription)
        .theme(App::theme)
        .window_size(START)
        .run()
}

/// The session the app was switched to (see `Message::SwitchSession`); the connection thread reads it on every
/// (re)connect, so a switch is: set this, close the current connection, and let the reader reconnect.
static SOCKET_OVERRIDE: Mutex<Option<PathBuf>> = Mutex::new(None);
static CLOSE_DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static NEW_DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static SWITCH_DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn refresh_sessions() -> Task<Message> {
    Task::perform(async { sessions::list() }, Message::Sessions)
}

fn client_socket() -> PathBuf {
    if let Some(path) = SOCKET_OVERRIDE.lock().ok().and_then(|o| o.clone()) {
        return path;
    }
    if let Some(path) = std::env::var_os("SHEPHERD_CLIENT_SOCKET_PATH") {
        return PathBuf::from(path);
    }
    match std::env::var_os("SHEPHERD_SOCKET_PATH").map(PathBuf::from) {
        Some(api) => sessions::client_socket_for(&api),
        None => PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
            .join(".config/shepherd-dev/shepherd-client.sock"),
    }
}

#[derive(Clone)]
struct Shared(Arc<Mutex<EndpointSender>>);

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Shared(sender)")
    }
}

/// Things the user can ask the server to do from the sidebar and the tab bar.
#[derive(Debug, Clone)]
pub(crate) enum Action {
    FocusSpace(String),
    CloseSpace(String, bool),
    FocusTab(String),
    CloseTab(String),
    FocusAgent(String),
    FocusPane(String),
    NewTab(String),
    NewSpace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PendingClose {
    Workspace(String),
    LastTab {
        tab_id: String,
        workspace_id: String,
    },
}

fn focus_action_for_pane(pane: String) -> Action {
    Action::FocusPane(pane)
}

#[derive(Debug, Clone)]
enum Message {
    Connected(Shared),
    Incoming(Box<Incoming>),
    Snapshot(Box<Snapshot>),
    Lost(String),
    Key(keyboard::Event),
    Resized(Size),
    /// Next step of the prism animation.
    Tick,
    /// The sessions known to the `shepherd` binary.
    Sessions(Result<Vec<sessions::Session>, String>),
    /// Leave the current session for the one whose client socket this is.
    SwitchSession(PathBuf),
    /// The `+` of the session row: open the name box.
    NewSessionOpen,
    NewSessionInput(String),
    NewSessionSubmit,
    NewSessionCancel,
    /// A started session answered (its client socket), or why it did not.
    SessionStarted(Result<PathBuf, String>),
    /// The `×` of a session: the first click warns, the second one stops it (or deletes it when it is stopped).
    CloseSession(String),
    /// What closing a session came to.
    SessionClosed(Result<String, String>),
    Act(Action),
    Acted(Result<(), String>),
    Grid(grid::Event),
    Panel(panel::PanelMsg),
    Reload,
    DataLoaded(Box<Data>),
    GoToPane(String),
    SetMode(Mode),
    StateChanged,
    TogglePalette,
    PaletteQuery(String),
    PaletteMove(isize),
    PaletteRun,
    Shoot,
    Shot(iced::window::Screenshot),
    ArmClose(PendingClose),
    ToggleSidebar,
    ContextMenuOpened(ContextMenuTarget),
    ContextMenuClosed,
    RenameStarted(String),
    RenameInput(String),
    RenameSubmitted,
    RenameCancelled,
    MouseMoved(iced::Point),
}

/// The open command palette: what was typed and which hit is highlighted.
struct Palette {
    query: String,
    selected: usize,
}

const PALETTE_INPUT: &str = "palette-input";
const SESSION_INPUT: &str = "session-input";

/// What the central area shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Terminal,
    Shepherd,
}

impl Mode {
    /// The other one: what `Ctrl+Shift+S` switches to.
    fn other(self) -> Mode {
        match self {
            Mode::Terminal => Mode::Shepherd,
            Mode::Shepherd => Mode::Terminal,
        }
    }
}

/// Target of a context menu (for rename).
#[derive(Debug, Clone)]
enum ContextMenuTarget {
    Tab { tab_id: String, label: String },
    Workspace { workspace_id: String, label: String },
}

/// Active rename state.
#[derive(Debug, Clone)]
struct RenameState {
    target: ContextMenuTarget,
    input: String,
}

struct App {
    palette: Option<Palette>,
    mode: Mode,
    panel: panel::Panel,
    data: Data,
    sender: Option<Shared>,
    surface: Option<PaneSurface>,
    snapshot: Snapshot,
    status: String,
    pending_close: Option<PendingClose>,
    sidebar_collapsed: bool,
    /// Animation step of the prism canvases.
    frame: u32,
    sessions: Vec<sessions::Session>,
    /// The name being typed for a new session, while the box is open, and what is wrong with it.
    new_session: Option<(String, Option<String>)>,
    /// The session whose `×` was clicked once, and the animation step it happened at.
    closing: Option<(String, u32)>,
    /// What the session row has to say (a warning, a result), shown in the status bar; not the connection status.
    session_note: Option<(String, Color)>,
    window: Size,
    sent: Option<(u16, u16)>,
    api_socket: PathBuf,
    context_menu: Option<(iced::Point, ContextMenuTarget)>,
    rename: Option<RenameState>,
    mouse_position: iced::Point,
}

/// Terminal cells that fit in the window once the sidebar, tab bar and status line are taken out.
fn terminal_cells(window: Size, sidebar_width: f32) -> (u16, u16) {
    let cols = ((window.width - sidebar_width - TERM_FRAME) / grid::CELL_W)
        .floor()
        .max(20.0) as u16;
    let chrome = STRIP_H + TOPBAR_H + TABBAR_H + STATUS_H + TERM_FRAME;
    let rows = ((window.height - chrome) / grid::CELL_H).floor().max(5.0) as u16;
    (cols, rows)
}

impl App {
    fn boot() -> (Self, Task<Message>) {
        let app = Self {
            palette: std::env::var("SHEPHERD_NATIVE_PALETTE")
                .ok()
                .map(|query| Palette { query, selected: 0 }), // for pictures (make native-shot PALETTE=…)
            mode: if std::env::var_os("SHEPHERD_NATIVE_MODE").is_some_and(|m| m == "shepherd") {
                Mode::Shepherd
            } else {
                Mode::Terminal
            },
            panel: panel::Panel::default(),
            data: Data::default(),
            sender: None,
            surface: None,
            snapshot: Snapshot::default(),
            status: "connecting…".into(),
            pending_close: None,
            sidebar_collapsed: std::env::var_os("SHEPHERD_NATIVE_SIDEBAR_COLLAPSED").is_some(),
            window: START,
            sent: None,
            api_socket: api::api_socket_for(&client_socket()),
            context_menu: None,
            rename: None,
            mouse_position: iced::Point::ORIGIN,
            frame: 0,
            sessions: Vec::new(),
            new_session: None,
            closing: None,
            session_note: None,
        };
        let folder = app.folder();
        let api = app.api_socket.clone();
        (
            app,
            Task::batch([
                Task::perform(async move { Data::load(&api, &folder) }, |d| {
                    Message::DataLoaded(Box::new(d))
                }),
                refresh_sessions(),
            ]),
        )
    }

    /// The folder the panels are about: where the focused pane is, else where the app started.
    fn folder(&self) -> PathBuf {
        self.snapshot
            .panes
            .iter()
            .find(|p| p.focused)
            .and_then(|p| p.cwd.clone())
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
    }

    fn reload(&self) -> Task<Message> {
        let folder = self.folder();
        let api = self.api_socket.clone();
        Task::perform(async move { Data::load(&api, &folder) }, |d| {
            Message::DataLoaded(Box::new(d))
        })
    }

    fn theme(&self) -> Theme {
        theme::app_theme()
    }

    fn geometry(&self) -> Geometry {
        let sidebar_width = if self.sidebar_collapsed {
            SIDEBAR_COLLAPSED_W
        } else {
            SIDEBAR_W
        };
        let (cols, rows) = terminal_cells(self.window, sidebar_width);
        Geometry {
            cols,
            rows,
            cell_width_px: grid::CELL_W.round() as u32,
            cell_height_px: grid::CELL_H.round() as u32,
        }
    }

    /// Tells the server the size of the terminal area when it changed.
    fn sync_size(&mut self) {
        let (Some(shared), geometry) = (self.sender.clone(), self.geometry()) else {
            return;
        };
        if self.sent == Some((geometry.cols, geometry.rows)) {
            return;
        }
        let resized = shared
            .0
            .lock()
            .map(|mut sender| sender.resize(geometry).is_ok())
            .unwrap_or(false);
        if resized {
            self.sent = Some((geometry.cols, geometry.rows));
        }
    }

    fn focused_pane(&self) -> Option<String> {
        self.surface
            .as_ref()?
            .panes
            .iter()
            .find(|p| p.focused)
            .map(|p| p.pane_id.clone())
    }

    fn with_sender(&mut self, send: impl FnOnce(&mut EndpointSender) -> std::io::Result<()>) {
        let Some(shared) = self.sender.clone() else {
            return;
        };
        let Ok(mut sender) = shared.0.lock() else {
            return;
        };
        if let Err(e) = send(&mut sender) {
            self.status = format!("could not send: {e}");
        }
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Connected(sender) => {
                self.sender = Some(sender);
                self.sent = None;
                self.status = "connected".into();
                self.sync_size();
                // verification aid: `SHEPHERD_NATIVE_NEWSESSION=<name>` types that name in the `+` box and submits it
                if let Some(name) = std::env::var_os("SHEPHERD_NATIVE_NEWSESSION") {
                    if !NEW_DONE.swap(true, std::sync::atomic::Ordering::SeqCst) {
                        return Task::done(Message::NewSessionInput(
                            name.to_string_lossy().into_owned(),
                        ))
                        .chain(Task::done(Message::NewSessionSubmit));
                    }
                }
                // verification aid: `SHEPHERD_NATIVE_SWITCH=<client socket>` leaves for that session once connected
                if let Some(target) = std::env::var_os("SHEPHERD_NATIVE_SWITCH") {
                    if !SWITCH_DONE.swap(true, std::sync::atomic::Ordering::SeqCst) {
                        return Task::done(Message::SwitchSession(PathBuf::from(target)));
                    }
                }
                if std::env::var_os("SHEPHERD_NATIVE_SHOT").is_some() {
                    // verification aid: let the first frames arrive, then save the window and exit
                    return Task::batch([
                        Task::perform(
                            async { std::thread::sleep(Duration::from_millis(2500)) },
                            |_| Message::Shoot,
                        ),
                        refresh_sessions(),
                    ]);
                }
                return refresh_sessions();
            }
            Message::Lost(why) => {
                self.sender = None;
                self.status = format!("disconnected ({why}); reconnecting…");
            }
            Message::Snapshot(snapshot) => {
                let before = self.folder();
                self.snapshot = *snapshot;
                if self.folder() != before {
                    return self.reload();
                }
            }
            Message::Incoming(incoming) => match *incoming {
                Incoming::Surface(surface) => self.surface = Some(*surface),
                Incoming::Patch(patch) => {
                    let mismatch = self
                        .surface
                        .as_mut()
                        .is_some_and(|surface| surface.apply(*patch).is_err());
                    if mismatch {
                        // out of step: asking for the size again makes the server send a full surface
                        self.sent = None;
                        self.sync_size();
                    }
                }
                _ => {}
            },
            Message::Key(event) => {
                if let keyboard::Event::KeyPressed { key, modifiers, .. } = &event {
                    let is_p =
                        matches!(key, keyboard::Key::Character(c) if c.eq_ignore_ascii_case("p"));
                    if is_p && modifiers.control() && modifiers.shift() {
                        return Task::done(Message::TogglePalette);
                    }
                    let is_s =
                        matches!(key, keyboard::Key::Character(c) if c.eq_ignore_ascii_case("s"));
                    if is_s && modifiers.control() && modifiers.shift() {
                        return Task::done(Message::SetMode(self.mode.other()));
                    }
                    let is_b = matches!(
                        key,
                        keyboard::Key::Character(c) if c.eq_ignore_ascii_case("b")
                    );
                    if is_b && modifiers.control() {
                        return Task::done(Message::ToggleSidebar);
                    }
                    if self.mode == Mode::Shepherd && modifiers.control() {
                        let tab = match key {
                            keyboard::Key::Character(c) => match c.as_str() {
                                "1" => Some(panel::Tab::Overview),
                                "2" => Some(panel::Tab::Runs),
                                "3" => Some(panel::Tab::Agents),
                                "4" => Some(panel::Tab::Tasks),
                                "5" => Some(panel::Tab::Patterns),
                                "6" => Some(panel::Tab::Monitor),
                                "7" => Some(panel::Tab::Git),
                                "8" => Some(panel::Tab::Events),
                                "9" => Some(panel::Tab::Models),
                                _ => None,
                            },
                            _ => None,
                        };
                        if let Some(tab) = tab {
                            return Task::done(Message::Panel(panel::PanelMsg::Tab(tab)));
                        }
                    }
                    if self.new_session.is_some()
                        && matches!(key, keyboard::Key::Named(keyboard::key::Named::Escape))
                    {
                        return Task::done(Message::NewSessionCancel);
                    }
                    if self.palette.is_some() {
                        return match key {
                            keyboard::Key::Named(keyboard::key::Named::Escape) => {
                                Task::done(Message::TogglePalette)
                            }
                            keyboard::Key::Named(keyboard::key::Named::ArrowDown) => {
                                Task::done(Message::PaletteMove(1))
                            }
                            keyboard::Key::Named(keyboard::key::Named::ArrowUp) => {
                                Task::done(Message::PaletteMove(-1))
                            }
                            _ => Task::none(),
                        };
                    }
                }
                if self.palette.is_some() || self.mode != Mode::Terminal {
                    return Task::none();
                }
                if let (
                    keyboard::Event::KeyPressed {
                        key,
                        modifiers,
                        text,
                        ..
                    },
                    Some(pane),
                ) = (&event, self.focused_pane())
                {
                    if let Some(input) = keys::map_key(key, *modifiers, text.as_deref()) {
                        self.with_sender(|s| match input {
                            keys::Input::Key(code, bits) => s.send_key(&pane, code, bits),
                            keys::Input::Text(text) => s.send_text(&pane, &text),
                        });
                    }
                }
            }
            Message::Tick => {
                self.frame = self.frame.wrapping_add(1);
                // an unconfirmed `×` goes back to normal after ~5 s
                if let Some((_, since)) = &self.closing {
                    if self.frame.wrapping_sub(*since) > 60 {
                        self.closing = None;
                        self.session_note = None;
                    }
                }
                // sessions start and stop outside the app: look again every ~8 s while the window is open
                if self.frame.is_multiple_of(100) {
                    return refresh_sessions();
                }
            }
            Message::NewSessionOpen => {
                self.closing = None;
                self.new_session = Some((String::new(), None));
                return iced::widget::operation::focus(SESSION_INPUT);
            }
            Message::NewSessionInput(text) => self.new_session = Some((text, None)),
            Message::NewSessionCancel => self.new_session = None,
            Message::NewSessionSubmit => {
                let Some((name, _)) = self.new_session.take() else {
                    return Task::none();
                };
                let name = name.trim().to_owned();
                if let Err(why) = sessions::validate_name(&name, &self.sessions) {
                    self.new_session = Some((name, Some(why)));
                    return Task::none();
                }
                self.session_note = Some((format!("starting session `{name}`…"), DIM));
                return Task::perform(
                    async move { sessions::start(&name, Duration::from_secs(15)) },
                    Message::SessionStarted,
                );
            }
            Message::SessionStarted(Ok(socket)) => {
                return Task::batch([
                    refresh_sessions(),
                    Task::done(Message::SwitchSession(socket)),
                ]);
            }
            Message::SessionStarted(Err(why)) => {
                self.session_note =
                    Some((format!("could not start the session: {why}"), theme::BAD));
                return refresh_sessions();
            }
            Message::CloseSession(name) => {
                let Some(session) = self.sessions.iter().find(|s| s.name == name).cloned() else {
                    return Task::none();
                };
                if self.closing.as_ref().map(|(n, _)| n) != Some(&name) {
                    let current = session.client_socket() == client_socket();
                    self.session_note =
                        Some((sessions::close_hint(&session, current), theme::WARN));
                    self.closing = Some((name, self.frame));
                    return Task::none();
                }
                self.closing = None;
                let running = session.running;
                self.session_note = Some((format!("closing `{name}`…"), DIM));
                return Task::perform(
                    async move {
                        if running {
                            sessions::stop(&name).map(|_| format!("stopped `{name}`"))
                        } else {
                            sessions::delete(&name).map(|_| format!("deleted `{name}`"))
                        }
                    },
                    Message::SessionClosed,
                );
            }
            Message::SessionClosed(result) => {
                self.session_note = Some(match result {
                    Ok(done) => (done, theme::OK),
                    Err(why) => (format!("could not close the session: {why}"), theme::BAD),
                });
                return refresh_sessions();
            }
            Message::Sessions(Ok(list)) => {
                self.sessions = list;
                // verification aid: `SHEPHERD_NATIVE_CLOSE=<name>` clicks that session's `×` twice, `<name>:arm` once
                if let Some(spec) = std::env::var_os("SHEPHERD_NATIVE_CLOSE") {
                    if !CLOSE_DONE.swap(true, std::sync::atomic::Ordering::SeqCst) {
                        let spec = spec.to_string_lossy().into_owned();
                        let (name, arm_only) = match spec.strip_suffix(":arm") {
                            Some(name) => (name.to_owned(), true),
                            None => (spec, false),
                        };
                        let first = Task::done(Message::CloseSession(name.clone()));
                        return if arm_only {
                            first
                        } else {
                            first.chain(Task::done(Message::CloseSession(name)))
                        };
                    }
                }
            }
            // without a `shepherd` binary to ask there is simply no session row
            Message::Sessions(Err(_)) => self.sessions.clear(),
            Message::SwitchSession(socket) => {
                if socket == client_socket() {
                    return Task::none();
                }
                self.closing = None;
                if let Ok(mut current) = SOCKET_OVERRIDE.lock() {
                    *current = Some(socket.clone());
                }
                self.api_socket = api::api_socket_for(&socket);
                self.panel.session_changed();
                if let Some(shared) = self.sender.take() {
                    if let Ok(sender) = shared.0.lock() {
                        // wakes the reader, which reconnects to the socket set above
                        let _ = sender.close();
                    }
                }
                self.snapshot = Snapshot::default();
                self.surface = None;
                self.data = Data::default();
                self.sent = None;
                self.status = "switching session…".into();
                return Task::batch([self.reload(), refresh_sessions()]);
            }
            Message::Resized(size) => {
                if size.width > 0.0 {
                    self.window = size;
                    self.sync_size();
                }
            }
            Message::Act(action) => {
                self.pending_close = None;
                let (method, params) = match action {
                    Action::FocusSpace(id) => {
                        ("workspace.focus", serde_json::json!({ "workspace_id": id }))
                    }
                    Action::CloseSpace(id, close_group) => (
                        "workspace.close",
                        serde_json::json!({
                            "workspace_id": id,
                            "close_group": close_group
                        }),
                    ),
                    Action::FocusTab(id) => ("tab.focus", serde_json::json!({ "tab_id": id })),
                    Action::CloseTab(id) => ("tab.close", serde_json::json!({ "tab_id": id })),
                    Action::FocusAgent(id) => ("agent.focus", serde_json::json!({ "target": id })),
                    Action::FocusPane(id) => ("pane.focus", serde_json::json!({ "pane_id": id })),
                    Action::NewTab(workspace) => (
                        "tab.create",
                        serde_json::json!({ "workspace_id": workspace, "focus": true }),
                    ),
                    Action::NewSpace => ("workspace.create", serde_json::json!({ "focus": true })),
                };
                let socket = self.api_socket.clone();
                return Task::perform(
                    async move { api::call(&socket, method, params).map(|_| ()) },
                    Message::Acted,
                );
            }
            Message::ArmClose(pending) => {
                self.pending_close = Some(pending);
                self.status = "Press close again to confirm closing the group.".into();
            }
            Message::ToggleSidebar => {
                self.sidebar_collapsed = !self.sidebar_collapsed;
                self.sent = None;
                self.sync_size();
            }
            Message::ContextMenuOpened(target) => {
                self.context_menu = Some((self.mouse_position, target));
            }
            Message::ContextMenuClosed => {
                self.context_menu = None;
            }
            Message::RenameStarted(label) => {
                if let Some((_, target)) = self.context_menu.take() {
                    self.rename = Some(RenameState {
                        target,
                        input: label,
                    });
                }
            }
            Message::MouseMoved(position) => {
                self.mouse_position = position;
            }
            Message::RenameInput(value) => {
                if let Some(rename) = self.rename.as_mut() {
                    rename.input = value;
                }
            }
            Message::RenameSubmitted => {
                let Some(rename) = self.rename.take() else {
                    return Task::none();
                };
                let new_label = rename.input;
                if new_label.is_empty() {
                    return Task::none();
                }
                let (method, params) = match rename.target {
                    ContextMenuTarget::Tab { tab_id, .. } => (
                        "tab.rename",
                        serde_json::json!({ "tab_id": tab_id, "label": new_label }),
                    ),
                    ContextMenuTarget::Workspace { workspace_id, .. } => (
                        "workspace.rename",
                        serde_json::json!({ "workspace_id": workspace_id, "label": new_label }),
                    ),
                };
                let socket = self.api_socket.clone();
                return Task::perform(
                    async move { api::call(&socket, method, params).map(|_| ()) },
                    Message::Acted,
                );
            }
            Message::RenameCancelled => {
                self.rename = None;
            }
            Message::Reload | Message::StateChanged => return self.reload(),
            Message::DataLoaded(data) => {
                self.data = *data;
                let ctx = panel::Ctx {
                    api: self.api_socket.clone(),
                    data: &self.data,
                    snapshot: &self.snapshot,
                    frame: self.frame,
                };
                if self.mode == Mode::Shepherd && self.panel.tab == panel::Tab::Monitor {
                    return self.panel.load_feed(&ctx);
                }
                if self.mode == Mode::Shepherd && self.panel.tab == panel::Tab::Events {
                    return Task::batch([self.panel.on_data(&ctx), self.panel.refresh_events()]);
                }
                return self.panel.on_data(&ctx);
            }
            Message::SetMode(mode) => self.mode = mode,
            Message::GoToPane(pane) => {
                self.mode = Mode::Terminal;
                return Task::done(Message::Act(focus_action_for_pane(pane)));
            }
            Message::Panel(msg) => {
                let ctx = panel::Ctx {
                    api: self.api_socket.clone(),
                    data: &self.data,
                    snapshot: &self.snapshot,
                    frame: self.frame,
                };
                return self.panel.update(msg, &ctx);
            }
            Message::TogglePalette => {
                if self.palette.take().is_none() {
                    self.palette = Some(Palette {
                        query: String::new(),
                        selected: 0,
                    });
                    return iced::widget::operation::focus(PALETTE_INPUT);
                }
            }
            Message::PaletteQuery(query) => {
                if let Some(palette) = self.palette.as_mut() {
                    palette.query = query;
                    palette.selected = 0;
                }
            }
            Message::PaletteMove(step) => {
                let hits = self
                    .palette
                    .as_ref()
                    .map(|p| commands::filter(&commands::build(&self.snapshot), &p.query).len())
                    .unwrap_or(0);
                if let (Some(palette), true) = (self.palette.as_mut(), hits > 0) {
                    palette.selected =
                        (palette.selected as isize + step).rem_euclid(hits as isize) as usize;
                }
            }
            Message::PaletteRun => {
                let Some(palette) = self.palette.take() else {
                    return Task::none();
                };
                let all = commands::build(&self.snapshot);
                let run = commands::filter(&all, &palette.query)
                    .get(palette.selected)
                    .map(|c| c.run.clone());
                return Task::batch(run.unwrap_or_default().into_iter().map(Task::done));
            }
            Message::Shoot => {
                return iced::window::latest()
                    .and_then(iced::window::screenshot)
                    .map(Message::Shot)
            }
            Message::Shot(shot) => {
                if let Some(path) = std::env::var_os("SHEPHERD_NATIVE_SHOT") {
                    let mut out =
                        format!("P6\n{} {}\n255\n", shot.size.width, shot.size.height).into_bytes();
                    out.extend(shot.rgba.chunks(4).flat_map(|px| px[..3].iter().copied()));
                    let _ = std::fs::write(path, out);
                }
                return iced::exit();
            }
            Message::Acted(Err(e)) => self.status = e,
            Message::Acted(Ok(())) => {
                self.pending_close = None;
            }
            Message::Grid(event) => match event {
                grid::Event::Press {
                    pane,
                    focused,
                    mouse_reporting,
                    column,
                    row,
                } => {
                    if !focused {
                        return Task::done(Message::Act(Action::FocusPane(pane)));
                    }
                    if mouse_reporting {
                        self.with_sender(|s| {
                            s.send_mouse(
                                &pane,
                                ClientMouseKind::Down(ClientMouseButton::Left),
                                column,
                                row,
                                0,
                                1,
                            )
                        });
                    }
                }
                grid::Event::Release {
                    pane,
                    mouse_reporting,
                    column,
                    row,
                } => {
                    if mouse_reporting {
                        self.with_sender(|s| {
                            s.send_mouse(
                                &pane,
                                ClientMouseKind::Up(ClientMouseButton::Left),
                                column,
                                row,
                                0,
                                1,
                            )
                        });
                    }
                }
                grid::Event::Scroll {
                    pane,
                    up,
                    column,
                    row,
                } => {
                    let kind = if up {
                        ClientMouseKind::ScrollUp
                    } else {
                        ClientMouseKind::ScrollDown
                    };
                    self.with_sender(|s| s.send_mouse(&pane, kind, column, row, 0, 3));
                }
            },
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let body: Element<'_, Message> = match self.mode {
            Mode::Shepherd => {
                let ctx = panel::Ctx {
                    api: self.api_socket.clone(),
                    data: &self.data,
                    snapshot: &self.snapshot,
                    frame: self.frame,
                };
                self.panel.view(&ctx)
            }
            Mode::Terminal => self.terminal_pane(),
        };
        let status_color_val = if self.status.starts_with("connected") {
            theme::OK
        } else if self.status.contains("disconnected")
            || self.status.starts_with("could not")
            || self.status.contains("error")
        {
            theme::BAD
        } else {
            DIM
        };
        let base = self.app_window(body, status_color_val);
        let content: Element<'_, Message> = match &self.palette {
            Some(palette) => iced::widget::stack![base, self.palette_view(palette)].into(),
            None => base,
        };
        // Overlay: rename dialog takes precedence, then context menu
        if self.rename.is_some() {
            iced::widget::stack![content, self.rename_view()].into()
        } else if self.context_menu.is_some() {
            iced::widget::stack![content, self.context_menu_view()].into()
        } else {
            content
        }
    }

    /// One button per session (`.sess`): a dot for running or stopped, the current one lifted. Only a running
    /// session other than the current one can be switched to.
    fn session_buttons(&self) -> Element<'_, Message> {
        let current = client_socket();
        let mut buttons = row![].spacing(4);
        for session in &self.sessions {
            let here = session.client_socket() == current;
            let label = row![
                text("●")
                    .size(8)
                    .color(if session.running { theme::OK } else { DIM }),
                text(session.name.clone()).size(13),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center);
            let session_button = button(container(label).center_y(Length::Fill))
                .padding([0, 10])
                .height(30)
                .style(item_style(here));
            buttons = buttons.push(if session.running && !here {
                session_button.on_press(Message::SwitchSession(session.client_socket()))
            } else {
                session_button
            });
            // `×`: stop it (running) or delete it (stopped); the default session cannot be deleted
            if session.running || !session.default {
                let armed = self.closing.as_ref().map(|(n, _)| n) == Some(&session.name);
                let close = if armed {
                    button(
                        container(text(if session.running { "stop?" } else { "delete?" }).size(11))
                            .center(Length::Fill),
                    )
                    .padding([0, 8])
                    .height(30)
                    .style(theme::danger_button_style())
                } else {
                    button(container(text("×").size(13)).center(Length::Fill))
                        .width(24)
                        .height(30)
                        .style(theme::outline_button_style(true))
                };
                buttons = buttons.push(close.on_press(Message::CloseSession(session.name.clone())));
            }
        }
        // `+`: a new session. It opens a small name box; Enter starts it, Esc closes the box.
        match &self.new_session {
            None => {
                buttons = buttons.push(
                    button(container(text("+").size(14)).center(Length::Fill))
                        .width(30)
                        .height(30)
                        .style(theme::outline_button_style(true))
                        .on_press(Message::NewSessionOpen),
                );
            }
            Some((name, problem)) => {
                buttons = buttons.push(
                    text_input("session name", name)
                        .id(SESSION_INPUT)
                        .on_input(Message::NewSessionInput)
                        .on_submit(Message::NewSessionSubmit)
                        .style(theme::input_style)
                        .size(13)
                        .padding([5, 10])
                        .width(160),
                );
                if let Some(problem) = problem {
                    buttons = buttons.push(text(problem.clone()).size(11).color(theme::BAD));
                }
                buttons = buttons.push(
                    button(container(text("✕").size(11)).center(Length::Fill))
                        .width(30)
                        .height(30)
                        .style(theme::outline_button_style(true))
                        .on_press(Message::NewSessionCancel),
                );
            }
        }
        container(buttons.align_y(iced::Alignment::Center))
            .padding([0, 6])
            .into()
    }

    /// The two buttons that switch between the terminal and the Shepherd panel: the same place in both modes, the
    /// active one lifted. `Ctrl+Shift+S` does the same from the keyboard.
    fn mode_switch(&self) -> Element<'_, Message> {
        let segment = |glyph: &'static str, label: &'static str, mode: Mode| {
            tooltip(
                button(
                    container(
                        row![text(glyph).size(12), text(label).size(13)]
                            .spacing(8)
                            .align_y(iced::Alignment::Center),
                    )
                    .center_y(Length::Fill),
                )
                .padding([0, 14])
                .height(30)
                .style(item_style(self.mode == mode))
                .on_press(Message::SetMode(mode)),
                text("Ctrl+Shift+S").size(11),
                tooltip::Position::Bottom,
            )
        };
        container(
            row![
                segment(">_", "Terminal", Mode::Terminal),
                segment("◆", "Shepherd", Mode::Shepherd),
            ]
            .spacing(2),
        )
        .padding(2)
        .style(|_| container::Style {
            background: Some(Background::Color(theme::SURFACE)),
            border: Border {
                color: theme::LINE,
                width: 1.0,
                radius: 6.0.into(),
            },
            ..container::Style::default()
        })
        .into()
    }

    /// The terminal: the tabs of the workspace, and the screen in a thin frame with some margin.
    fn terminal_pane(&self) -> Element<'_, Message> {
        let screen: Element<'_, Message> = match &self.surface {
            Some(surface) => Canvas::new(grid::Grid { surface })
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
            None => container(text("waiting for the server…").color(DIM))
                .center(Length::Fill)
                .into(),
        };
        column![
            self.tab_bar(),
            container(
                container(screen)
                    .padding(1)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .style(|_| container::Style {
                        background: Some(Background::Color(BG)),
                        border: Border {
                            color: theme::LINE,
                            width: 1.0,
                            radius: 6.0.into(),
                        },
                        ..container::Style::default()
                    }),
            )
            .padding(TERM_PAD)
            .width(Length::Fill)
            .height(Length::Fill),
        ]
        .into()
    }

    /// `workspace › tab` of what the terminal shows now.
    fn terminal_place(&self) -> String {
        let workspace = self.snapshot.workspaces.iter().find(|w| w.focused);
        let tab = workspace.and_then(|w| {
            self.snapshot
                .tabs_of(&w.workspace_id)
                .find(|t| t.focused)
                .map(|t| t.label.clone())
        });
        match (workspace, tab) {
            (Some(w), Some(t)) => format!("{} › {t}", w.label),
            (Some(w), None) => w.label.clone(),
            _ => "no workspace".to_owned(),
        }
    }

    /// The window of both modes: prism strip, top bar with the switch, the rail, the body (the terminal or the
    /// Shepherd panel) and the status bar.
    fn app_window<'a>(
        &'a self,
        body: Element<'a, Message>,
        status_color_val: Color,
    ) -> Element<'a, Message> {
        // the strip and the logo animate in both modes: they belong to the shared window
        let strip = Canvas::new(prism::Prism {
            preset: prism::Preset::STRIP,
            frame: self.frame,
        })
        .width(Length::Fill)
        .height(STRIP_H);
        let connected = self.status.starts_with("connected");
        let top_bar = container(
            row![
                Canvas::new(prism::Prism {
                    preset: prism::Preset::logo(theme::BAR),
                    frame: self.frame + 3,
                })
                .width(34)
                .height(20),
                text("SHEPHERD")
                    .font(theme::PIXEL_BOLD)
                    .size(15)
                    .color(theme::HI),
                self.mode_switch(),
                self.session_buttons(),
                container(text("")).width(Length::Fill),
                text("●").size(9).color(status_color_val),
                text(if connected {
                    "connected".to_owned()
                } else {
                    self.status.clone()
                })
                .size(12)
                .color(DIM),
            ]
            .spacing(14)
            .align_y(iced::Alignment::Center),
        )
        .height(TOPBAR_H)
        .align_y(iced::alignment::Vertical::Center)
        .padding([0, 14])
        .width(Length::Fill)
        .style(theme::bar_style);
        // left of the status bar: the panel's own notice, or where the terminal is and how big it is
        let left: Element<'a, Message> = match self.mode {
            Mode::Shepherd => self.panel.notice(),
            Mode::Terminal => {
                let g = self.geometry();
                row![
                    text(self.terminal_place()).size(11.5).color(theme::FG),
                    text(format!("{}×{}", g.cols, g.rows)).size(11.5).color(DIM),
                ]
                .spacing(14)
                .into()
            }
        };
        let status_bar = container(
            row![
                left,
                text(
                    self.session_note
                        .as_ref()
                        .map(|(note, _)| note.clone())
                        .unwrap_or_default()
                )
                .size(11.5)
                .color(self.session_note.as_ref().map_or(DIM, |(_, color)| *color)),
                container(text("")).width(Length::Fill),
                text(if connected {
                    "● connected"
                } else {
                    "○ offline"
                })
                .size(11.5)
                .color(status_color_val),
                text(self.panel.config_path().unwrap_or_default())
                    .size(11.5)
                    .color(DIM),
                text(concat!("v", env!("CARGO_PKG_VERSION")))
                    .size(11.5)
                    .color(DIM),
            ]
            .spacing(16)
            .align_y(iced::Alignment::Center),
        )
        .height(STATUS_H)
        .align_y(iced::alignment::Vertical::Center)
        .padding([0, 14])
        .width(Length::Fill)
        .style(theme::bar_style);
        container(column![
            strip,
            top_bar,
            row![self.sidebar(), container(body).width(Length::Fill)].height(Length::Fill),
            status_bar,
        ])
        .style(|_| container::Style {
            background: Some(Background::Color(PANEL)),
            ..container::Style::default()
        })
        .into()
    }

    /// The context menu overlay (right-click menu).
    fn context_menu_view(&self) -> Element<'_, Message> {
        let Some((position, target)) = &self.context_menu else {
            return container(text("")).into();
        };

        let label = match target {
            ContextMenuTarget::Tab { label, .. } => label.clone(),
            ContextMenuTarget::Workspace { label, .. } => label.clone(),
        };

        let rename_btn = button(
            container(text(format!("Rename \"{}\"", label)).size(12))
                .padding([8, 16])
                .width(Length::Fill),
        )
        .padding(0)
        .width(180.0)
        .style(item_style(false))
        .on_press(Message::RenameStarted(label));

        let menu_content = container(column![rename_btn].spacing(0))
            .padding(4)
            .style(|_| container::Style {
                background: Some(Background::Color(PANEL)),
                border: Border {
                    color: Color { a: 0.5, ..LINE },
                    width: 0.5,
                    radius: 0.0.into(),
                },
                ..container::Style::default()
            });

        // Click overlay to close menu
        let overlay = mouse_area(container(text("")).width(Length::Fill).height(Length::Fill))
            .on_press(Message::ContextMenuClosed);

        // Position the menu at the click point
        let positioned = container(menu_content)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(iced::Padding {
                top: position.y.max(4.0),
                left: position.x.max(4.0),
                right: 0.0,
                bottom: 0.0,
            });

        container(iced::widget::stack![overlay, positioned])
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    /// The command palette, drawn over the window (not a separate window): a box to type in and the matches.
    fn palette_view<'a>(&'a self, palette: &'a Palette) -> Element<'a, Message> {
        let all = commands::build(&self.snapshot);
        let hits = commands::filter(&all, &palette.query);
        let mut list = column![].spacing(1);
        for (index, hit) in hits.iter().take(12).enumerate() {
            list = list.push(
                container(text(hit.label.clone()).size(12))
                    .width(Length::Fill)
                    .padding([4, 8])
                    .style(move |_| container::Style {
                        background: (index == palette.selected).then_some(Background::Color(
                            iced::Color {
                                a: 0.2,
                                ..theme::ACCENT
                            },
                        )),
                        ..container::Style::default()
                    }),
            );
        }
        if hits.is_empty() {
            list = list
                .push(container(text("No command matches.").size(12).color(DIM)).padding([4, 8]));
        }
        let input = text_input("Type a command…", &palette.query)
            .id(PALETTE_INPUT)
            .on_input(Message::PaletteQuery)
            .on_submit(Message::PaletteRun)
            .padding(6)
            .size(13);
        let shortcuts = row![
            text("↑↓ navigate").size(10).color(DIM),
            text("Enter run").size(10).color(DIM),
            text("Esc close").size(10).color(DIM),
        ]
        .spacing(10);
        let view_shortcuts = text("Ctrl+1–5 workspace · Ctrl+6–9 inspect")
            .size(10)
            .color(DIM);
        let boxed = container(column![input, list, shortcuts, view_shortcuts].spacing(4))
            .width(480)
            .padding(6)
            .style(|_| container::Style {
                background: Some(Background::Color(PANEL)),
                border: Border {
                    color: LINE,
                    width: 0.5,
                    radius: 0.0.into(),
                },
                ..container::Style::default()
            });
        container(boxed)
            .center_x(Length::Fill)
            .padding(iced::Padding {
                top: 70.0,
                ..iced::Padding::ZERO
            })
            .into()
    }

    fn tab_bar(&self) -> Element<'_, Message> {
        // Fluid tabs: no borders, seamless blend with terminal area
        let Some(workspace) = self.snapshot.focused_workspace_id.as_deref() else {
            return container(row![].padding([0, 12]))
                .height(TABBAR_H)
                .width(Length::Fill)
                .style(theme::bar_style)
                .into();
        };

        let mut tabs = row![].spacing(0);
        for tab in self.snapshot.tabs_of(workspace) {
            let label = tab.label.clone();
            let tab_id = tab.tab_id.clone();
            let tab_content = row![
                text(label.clone()).size(13),
                button(text("✕").size(9))
                    .padding([2, 4])
                    .width(18.0)
                    .height(18.0)
                    .style(mini_button_style())
                    .on_press(self.close_tab_message(tab)),
            ]
            .align_y(iced::Alignment::Center)
            .spacing(6);

            let tab_button = button(container(tab_content).center_y(Length::Fill))
                .padding([0, 12])
                .height(34)
                .style(item_style(tab.focused))
                .on_press(Message::Act(Action::FocusTab(tab_id.clone())));

            // Wrap with mouse_area for right-click
            let tab_with_context = mouse_area(tab_button).on_right_press(
                Message::ContextMenuOpened(ContextMenuTarget::Tab { tab_id, label }),
            );

            tabs = tabs.push(tab_with_context);
        }
        tabs = tabs.push(
            button(container(text("+").size(14)).center(Length::Fill))
                .width(30)
                .height(34)
                .style(mini_button_style())
                .on_press(Message::Act(Action::NewTab(workspace.to_owned()))),
        );

        let content = row![
            scrollable(tabs).direction(scrollable::Direction::Horizontal(
                scrollable::Scrollbar::new().width(0).scroller_width(0),
            )),
        ]
        .width(Length::Fill);

        container(content)
            .height(TABBAR_H)
            .align_y(iced::alignment::Vertical::Center)
            .padding([0, 10])
            .width(Length::Fill)
            .style(theme::bar_style)
            .into()
    }

    fn sidebar(&self) -> Element<'_, Message> {
        if self.sidebar_collapsed {
            // Orca-style collapsed sidebar: icon-only, very narrow
            let working = self
                .snapshot
                .agents
                .iter()
                .filter(|agent| {
                    matches!(agent.status(), shepherd_app_core::snapshot::Status::Working)
                })
                .count();
            let _blocked = self
                .snapshot
                .agents
                .iter()
                .filter(|agent| {
                    matches!(agent.status(), shepherd_app_core::snapshot::Status::Blocked)
                })
                .count();

            let mut items = column![].spacing(0).align_x(iced::Alignment::Center);

            // Top section: Shepherd button
            items = items.push(
                container(
                    column![tooltip(
                        button(text("◆").size(15))
                            .padding(6)
                            .width(32.0)
                            .height(32.0)
                            .style(item_style(self.mode == Mode::Shepherd))
                            .on_press(Message::SetMode(Mode::Shepherd)),
                        text("Shepherd").size(11),
                        tooltip::Position::Right,
                    ),]
                    .spacing(4),
                )
                .padding(pad(10.0, 0.0, 6.0, 0.0)),
            );

            items = items.push(
                container(
                    column![
                        tooltip(
                            button(text("⌂").size(15))
                                .padding(6)
                                .width(32.0)
                                .height(32.0)
                                .style(mini_button_style())
                                .on_press(Message::ToggleSidebar),
                            text("Workspaces").size(11),
                            tooltip::Position::Right,
                        ),
                        tooltip(
                            button(
                                container(text("◉").size(11).color(if working > 0 {
                                    ACCENT
                                } else {
                                    DIM
                                }))
                                .width(32.0)
                                .align_x(iced::Alignment::Center)
                                .height(32.0)
                            )
                            .padding(0)
                            .width(32.0)
                            .height(32.0)
                            .style(mini_button_style())
                            .on_press(Message::Panel(panel::PanelMsg::Tab(panel::Tab::Agents))),
                            text(format!("Agents ({working})")).size(11),
                            tooltip::Position::Right,
                        ),
                    ]
                    .spacing(0),
                )
                .padding([6, 0]),
            );

            // Bottom: expand button
            items = items.push(container(text("")).height(Length::Fill));
            items = items.push(tooltip(
                button(text("›").size(15))
                    .padding(6)
                    .width(32.0)
                    .height(32.0)
                    .style(mini_button_style())
                    .on_press(Message::ToggleSidebar),
                text("Expand sidebar").size(11),
                tooltip::Position::Right,
            ));

            return container(items)
                .width(SIDEBAR_COLLAPSED_W)
                .height(Length::Fill)
                .style(|_| container::Style {
                    background: Some(Background::Color(PANEL)),
                    ..container::Style::default()
                })
                .into();
        }

        // Rail (`.rail`): pixel-font section labels, square bullets, agents with a state square.
        let bullet = |selected: bool| {
            container(text(""))
                .width(6)
                .height(6)
                .style(move |_| container::Style {
                    background: Some(Background::Color(if selected {
                        theme::ACCENT
                    } else {
                        theme::LINE2
                    })),
                    border: Border {
                        radius: 1.0.into(),
                        ..Border::default()
                    },
                    ..container::Style::default()
                })
        };
        let heading = |label: &'static str| {
            container(theme::label(label))
                .padding(pad(10.0, 8.0, 4.0, 8.0))
                .width(Length::Fill)
        };

        let mut spaces = column![heading("WORKSPACES")].spacing(6);
        for workspace in &self.snapshot.workspaces {
            let label = workspace.label.clone();
            let workspace_id = workspace.workspace_id.clone();
            let ws_btn_inner = button(
                container(
                    row![
                        bullet(workspace.focused),
                        text(label.clone()).size(13).width(Length::Fill),
                        text(
                            self.snapshot
                                .agents
                                .iter()
                                .filter(|a| a.workspace_id == workspace.workspace_id)
                                .count()
                                .to_string()
                        )
                        .size(11)
                        .color(DIM),
                    ]
                    .align_y(iced::Alignment::Center)
                    .spacing(10)
                    .width(Length::Fill),
                )
                .center_y(Length::Fill),
            )
            .width(Length::Fill)
            .height(36)
            .padding([0, 8])
            .style(item_style(workspace.focused))
            .on_press(Message::Act(Action::FocusSpace(workspace_id.clone())));

            // Wrap with mouse_area for right-click context menu
            let ws_btn = mouse_area(ws_btn_inner).on_right_press(Message::ContextMenuOpened(
                ContextMenuTarget::Workspace {
                    workspace_id,
                    label,
                },
            ));

            let close_btn = button(text("✕").size(9))
                .padding([2, 4])
                .width(18.0)
                .height(18.0)
                .style(mini_button_style())
                .on_press(self.close_workspace_message(workspace));

            spaces = spaces.push(
                row![ws_btn, close_btn]
                    .align_y(iced::Alignment::Center)
                    .spacing(2),
            );
        }

        spaces = spaces.push(
            button(text("+ New workspace").size(13).color(theme::ACCENT))
                .width(Length::Fill)
                .padding([6, 8])
                .style(mini_button_style())
                .on_press(Message::Act(Action::NewSpace)),
        );

        let mut agents =
            column![container(heading("AGENTS")).padding(pad(10.0, 0.0, 0.0, 0.0))].spacing(2);
        if self.snapshot.agents.is_empty() {
            agents = agents
                .push(container(text("No agents running").size(11).color(DIM)).padding([6, 8]));
        }
        for (status, group) in self.snapshot.agents_by_state() {
            let color = status_color(status);
            for agent in group {
                let square = container(
                    text(theme::status_glyph(status))
                        .size(10)
                        .font(theme::MONO_BOLD)
                        .color(BG),
                )
                .width(18)
                .height(18)
                .center_x(18)
                .center_y(18)
                .style(move |_| container::Style {
                    background: Some(Background::Color(color)),
                    border: Border {
                        radius: 3.0.into(),
                        ..Border::default()
                    },
                    ..container::Style::default()
                });
                let agent_body = row![
                    square,
                    text(agent.display_name()).size(12).width(Length::Fill),
                    text(status_name(status)).size(11).color(color),
                ]
                .align_y(iced::Alignment::Center)
                .spacing(10)
                .width(Length::Fill);

                agents = agents.push(
                    button(container(agent_body).center_y(Length::Fill))
                        .width(Length::Fill)
                        .height(34)
                        .padding([0, 8])
                        .style(item_style(agent.focused))
                        .on_press(Message::Act(Action::FocusAgent(agent.pane_id.clone()))),
                );
            }
        }

        // the brand and the switch live in the top bar; here only the toggle that gives the terminal more room
        let brand: Element<'_, Message> = if self.mode == Mode::Shepherd {
            container(text("")).height(0).into()
        } else {
            row![
                container(text("")).width(Length::Fill),
                button(text("‹").size(13))
                    .padding(5)
                    .style(mini_button_style())
                    .on_press(Message::ToggleSidebar),
            ]
            .align_y(iced::Alignment::Center)
            .padding(pad(6.0, 10.0, 0.0, 10.0))
            .into()
        };
        container(scrollable(
            column![
                brand,
                container(column![spaces, agents].spacing(6)).padding([14, 10])
            ]
            .spacing(0),
        ))
        .width(SIDEBAR_W)
        .height(Length::Fill)
        .style(theme::bar_style)
        .into()
    }

    /// The rename dialog overlay.
    fn rename_view(&self) -> Element<'_, Message> {
        let Some(rename) = &self.rename else {
            return container(text("")).into();
        };
        let title = match &rename.target {
            ContextMenuTarget::Tab { .. } => "Rename Tab",
            ContextMenuTarget::Workspace { .. } => "Rename Workspace",
        };

        let input = text_input("New name", &rename.input)
            .on_input(Message::RenameInput)
            .on_submit(Message::RenameSubmitted)
            .padding(8)
            .size(13);

        let submit_btn = button(text("Rename").size(12))
            .padding([6, 16])
            .style(primary_button_style())
            .on_press(Message::RenameSubmitted);

        let cancel_btn = button(text("Cancel").size(12))
            .padding([6, 16])
            .style(mini_button_style())
            .on_press(Message::RenameCancelled);

        let dialog = container(
            column![
                container(text(title).size(14)).padding(pad(12.0, 16.0, 8.0, 16.0)),
                input,
                row![cancel_btn, submit_btn]
                    .spacing(8)
                    .padding(pad(8.0, 16.0, 12.0, 16.0)),
            ]
            .spacing(0),
        )
        .width(320)
        .padding(0)
        .style(|_| container::Style {
            background: Some(Background::Color(PANEL)),
            border: Border {
                color: Color { a: 0.5, ..LINE },
                width: 0.5,
                radius: 0.0.into(),
            },
            ..container::Style::default()
        });

        // Dark overlay background
        let overlay = container(text(""))
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_| container::Style {
                background: Some(Background::Color(Color {
                    a: 0.4,
                    ..Color::BLACK
                })),
                ..container::Style::default()
            });

        container(iced::widget::stack![
            overlay,
            container(dialog).center(Length::Fill)
        ])
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }

    fn workspace_has_linked_children(&self, workspace_id: &str) -> bool {
        let Some(workspace) = self
            .snapshot
            .workspaces
            .iter()
            .find(|workspace| workspace.workspace_id == workspace_id)
        else {
            return false;
        };
        let Some(worktree) = workspace
            .worktree
            .as_ref()
            .filter(|worktree| !worktree.is_linked_worktree)
        else {
            return false;
        };
        self.snapshot.workspaces.iter().any(|candidate| {
            candidate.workspace_id != workspace_id
                && candidate.worktree.as_ref().is_some_and(|candidate| {
                    candidate.key == worktree.key && candidate.is_linked_worktree
                })
        })
    }

    fn close_workspace_message(
        &self,
        workspace: &shepherd_app_core::snapshot::Workspace,
    ) -> Message {
        let pending = PendingClose::Workspace(workspace.workspace_id.clone());
        if self.workspace_has_linked_children(&workspace.workspace_id) {
            if self.pending_close.as_ref() == Some(&pending) {
                Message::Act(Action::CloseSpace(workspace.workspace_id.clone(), true))
            } else {
                Message::ArmClose(pending)
            }
        } else {
            Message::Act(Action::CloseSpace(workspace.workspace_id.clone(), false))
        }
    }

    fn close_tab_message(&self, tab: &shepherd_app_core::snapshot::Tab) -> Message {
        let workspace_id = tab.workspace_id.clone();
        let last_tab = self.snapshot.tabs_of(&workspace_id).count() <= 1;
        if last_tab && self.workspace_has_linked_children(&workspace_id) {
            let pending = PendingClose::LastTab {
                tab_id: tab.tab_id.clone(),
                workspace_id: workspace_id.clone(),
            };
            if self.pending_close.as_ref() == Some(&pending) {
                Message::Act(Action::CloseSpace(workspace_id, true))
            } else {
                Message::ArmClose(pending)
            }
        } else {
            Message::Act(Action::CloseTab(tab.tab_id.clone()))
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            // ~12 fps for the prism strip and logo, which both modes show in the shared top bar
            Subscription::run(ticks),
            Subscription::run(connection),
            Subscription::run_with(self.folder(), watch_files),
            keyboard::listen().map(Message::Key),
            iced::window::resize_events().map(|(_, size)| Message::Resized(size)),
            iced::event::listen_with(|event, _status, _window_id| match event {
                iced::event::Event::Mouse(iced::mouse::Event::CursorMoved { position }) => {
                    Some(Message::MouseMoved(position))
                }
                _ => None,
            }),
        ])
    }
}

/// Tells the app when a file the panels read changes (agents, runs, tasks, bus), so they never poll.
/// One `Tick` every 83 ms (iced's own timer needs an async runtime feature this app does not enable).
fn ticks() -> impl iced::futures::Stream<Item = Message> {
    iced::stream::channel(1, async move |mut output| {
        use iced::futures::{channel::mpsc, SinkExt, StreamExt};
        let (tx, mut rx) = mpsc::unbounded::<()>();
        std::thread::spawn(move || {
            // ends by itself once the subscription is dropped and nobody receives
            while tx.unbounded_send(()).is_ok() {
                std::thread::sleep(Duration::from_millis(83));
            }
        });
        while rx.next().await.is_some() {
            if output.send(Message::Tick).await.is_err() {
                break;
            }
        }
    })
}

#[allow(clippy::ptr_arg)] // iced hands the subscription data over as &PathBuf
fn watch_files(folder: &PathBuf) -> impl iced::futures::Stream<Item = Message> {
    let folder = folder.clone();
    iced::stream::channel(16, async move |mut output| {
        use iced::futures::{channel::mpsc, SinkExt, StreamExt};
        let (tx, mut rx) = mpsc::unbounded::<()>();
        let watcher = StateWatcher::start(move || {
            let _ = tx.unbounded_send(());
        });
        let _keep_alive = match watcher {
            Ok(mut watcher) => {
                let _ = watcher.watch(
                    &shepherd_app_core::shepherd_core::config::state_dir(),
                    false,
                );
                let _ = watcher.watch(
                    &shepherd_app_core::data::project_root_at(&folder).join(".shepherd"),
                    true,
                );
                Some(watcher)
            }
            Err(_) => None,
        };
        while rx.next().await.is_some() {
            if output.send(Message::StateChanged).await.is_err() {
                break;
            }
        }
    })
}

/// Reads the server on its own thread (the protocol is blocking), reconnecting when it goes away, and
/// forwards what arrives.
fn connection() -> impl iced::futures::Stream<Item = Message> {
    iced::stream::channel(256, async |mut output| {
        use iced::futures::{channel::mpsc, SinkExt, StreamExt};
        let (tx, mut rx) = mpsc::unbounded::<Message>();
        std::thread::spawn(move || loop {
            let geometry = Geometry {
                cols: 120,
                rows: 36,
                cell_width_px: grid::CELL_W.round() as u32,
                cell_height_px: grid::CELL_H.round() as u32,
            };
            match Endpoint::connect(&client_socket(), geometry) {
                Err(e) => {
                    let _ = tx.unbounded_send(Message::Lost(e.to_string()));
                }
                Ok(mut endpoint) => {
                    if let Ok(sender) = endpoint.sender() {
                        let _ = tx.unbounded_send(Message::Connected(Shared(Arc::new(
                            Mutex::new(sender),
                        ))));
                    }
                    loop {
                        let message = match endpoint.read() {
                            Ok(Incoming::Control { kind, data }) if kind == SNAPSHOT => {
                                match Snapshot::parse(&data) {
                                    Ok(snapshot) => Message::Snapshot(Box::new(snapshot)),
                                    Err(_) => continue,
                                }
                            }
                            Ok(incoming) => Message::Incoming(Box::new(incoming)),
                            Err(e) => {
                                let _ = tx.unbounded_send(Message::Lost(e.to_string()));
                                break;
                            }
                        };
                        if tx.unbounded_send(message).is_err() {
                            return;
                        }
                    }
                }
            }
            std::thread::sleep(Duration::from_secs(1));
        });
        while let Some(message) = rx.next().await {
            if output.send(message).await.is_err() {
                break;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_shortcut_toggles_between_the_two_modes() {
        assert_eq!(Mode::Terminal.other(), Mode::Shepherd);
        assert_eq!(Mode::Shepherd.other(), Mode::Terminal);
        assert_eq!(Mode::Terminal.other().other(), Mode::Terminal);
    }

    use super::*;

    #[test]
    fn the_terminal_area_is_what_the_window_leaves() {
        assert_eq!(
            terminal_cells(Size::new(1280.0, 800.0), SIDEBAR_W),
            (132, 38)
        );
        assert_eq!(
            terminal_cells(Size::new(100.0, 100.0), SIDEBAR_W),
            (20, 5),
            "never smaller than a usable minimum"
        );
    }

    #[test]
    fn go_to_terminal_focuses_the_pane_not_the_agent() {
        assert!(matches!(
            focus_action_for_pane("pane-1".into()),
            Action::FocusPane(id) if id == "pane-1"
        ));
    }
}
