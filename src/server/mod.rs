//! The session server: owns the shells, the agent conversations and the
//! whole interface, and streams rendered frames to whichever clients are
//! attached. It keeps running when every client has gone away, which is what
//! makes sessions persistent.

mod agent_ops;
pub mod far;
pub mod mcp_ops;
mod skills_ops;
pub mod agents_ops;
pub mod panes;
mod pictures;
pub mod session;
pub mod restore;
pub mod settings;
pub mod store;

use crate::agent::AgentEv;
use crate::config::{Chord, Config};
use crate::keys::{Action, Keymap};
use crate::input;
use crate::pane::{Pane, PaneEvent, SpawnOpts};
use crate::paths;
use crate::proto::{self, ClientMsg, Hello, ServerFrame, ServerMsg, SessionInfo};
use crate::theme::{self, Theme};
use crate::ui;
use alacritty_terminal::selection::SelectionType;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vte::ansi::Rgb;
use anyhow::{Context, Result};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use session::AgentSession;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime};
use tokio::net::UnixListener;
use tokio::sync::mpsc;

const MIN_FRAME: Duration = Duration::from_millis(6);
/// How many frames may be on their way to a terminal before it is sent no
/// more until it has taken them.
const MAX_UNSENT: usize = 2;
const TICK: Duration = Duration::from_millis(80);
/// How long the prefix key waits for the key (or click) that follows it.
/// Moving the pointer starts the wait again.
const PREFIX_TIMEOUT: Duration = Duration::from_secs(10);

/// What the status bar's hint line and the menu offer: the action, its
/// short name on the hint line, and its name in the menu. The key shown
/// with each is whichever the keymap has for it.
pub const OFFERED: &[(Action, &str, &str)] = &[
    (Action::NewTab, "new tab", "New tab"),
    (Action::RemoteTab, "remote tab", "New tab on another machine…"),
    (Action::SplitRight, "split right", "Split right"),
    (Action::SplitDown, "split down", "Split down"),
    (Action::Zoom, "zoom", "Zoom pane"),
    (Action::Close, "close", "Close pane"),
    (Action::NextTab, "next", "Next tab"),
    (Action::PrevTab, "prev", "Previous tab"),
    (Action::Rename, "rename", "Rename tab"),
    (Action::Find, "find", "Find in scrollback"),
    (Action::CopyMode, "copy", "Select with the keyboard"),
    (Action::AgentPanel, "agent", "Agent panel"),
    (Action::Agents, "agents", "Every agent at a glance"),
    (Action::Explain, "explain", "Explain the last command"),
    (Action::Fix, "fix", "Fix the last command"),
    (Action::Watch, "watch", "Watch the running command"),
    (Action::AgentHere, "agent in", "Take the agent into this shell (ssh, sudo…)"),
    (Action::Model, "model", "Choose a model"),
    (Action::Theme, "theme", "Choose a theme"),
    (Action::Settings, "settings", "Settings"),
    (Action::Detach, "detach", "Detach (keep everything running)"),
    (Action::Help, "help", "Help"),
];

pub enum Ev {
    Join { id: u64, hello: Hello, tx: mpsc::UnboundedSender<ServerFrame>, unsent: Arc<AtomicUsize> },
    /// Everything sent to this terminal has been written to it.
    CaughtUp { id: u64 },
    Client { id: u64, msg: ClientMsg },
    Gone { id: u64 },
    Pty { pane: u64, data: Vec<u8> },
    PtyExit { pane: u64, killed: bool },
    Agent { tab: u64, turn: u64, ev: AgentEv },
    Models { result: Vec<(String, String)>, errors: Vec<String> },
    /// Keys read from the system's password store: provider and key, if any.
    KeysLoaded(Vec<(String, Option<String>)>),
    /// `/key` finished: the provider, the key now stored for it (`None`
    /// after removing one), or what went wrong; reported in tab `tab`.
    KeyChanged { tab: u64, provider: String, result: Result<Option<String>, String> },
    /// The config file was written to.
    ConfigChanged,
    /// Something from an `ssh` to a machine where shells are kept.
    Remote { reach: u64, told: crate::remote::reach::Told },
    /// Something from an MCP server, or from starting one.
    Mcp { server: String, round: u64, ev: mcp_ops::McpEv },
    Shutdown,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Focus {
    Shell,
    Agent,
}

pub struct Tab {
    pub id: u64,
    pub panes: panes::Panes,
    pub name: Option<String>,
    pub agent: AgentSession,
    pub focus: Focus,
    /// Something happened here while another tab was in front.
    pub activity: bool,
    /// The terminal this tab's panes are sized for: the one that last used
    /// the tab, of those looking at it. Another terminal looking at the
    /// same tab sees it at that size.
    pub owner: Option<u64>,
    /// The space the panes were last laid out in (columns, rows).
    pub sized: (u16, u16),
}

/// Everything clickable, recorded while drawing a frame.
#[derive(Clone, PartialEq, Debug)]
pub enum Hit {
    Tab(usize),
    CloseTab(usize),
    NewTab,
    Mode,
    Autonomy,
    Model,
    Help,
    /// The menu of everything the prefix key offers.
    Menu,
    /// Something a key also does: an entry of the hint line shown after
    /// the prefix key.
    Do(Action),
    /// The search bar of a pane: its text, the older and newer buttons.
    FindBox,
    FindOlder,
    FindNewer,
    /// The button that ends looking back through a pane.
    CopyClose,
    /// A row of the settings screen, and its close button.
    Setting(usize),
    SettingsClose,
    /// A button of the walkthrough.
    Walk(Walk),
    Explain,
    Fix,
    Watch,
    /// The pane the keyboard is in.
    Pane,
    /// Another pane of the same tab.
    OtherPane(u64),
    /// The divider of a split, by the split's number.
    Border(usize),
    Divider,
    PanelClose,
    Panel,
    Choice(usize),
    Slash(usize),
    PickerItem(usize),
    /// An entry of a right-click menu.
    MenuItem(usize),
    /// A button of a pane that is waiting for its machine.
    Far(far::FarButton),
    /// A tab's row of the agents overview, and its "yes" to a question.
    AgentRow(usize),
    AgentYes(usize),
    Overlay,
}

#[derive(Clone, Copy, PartialEq)]
pub enum PickerKind {
    Model,
    Theme,
    Conversation,
    Mcp,
    Skills,
}

const CONVERSATIONS_NOTE: &str = "Type to filter · Enter to open · Del to delete · Esc to cancel";

pub struct PickItem {
    pub label: String,
    pub detail: String,
    pub value: String,
}

pub struct Picker {
    pub kind: PickerKind,
    pub title: String,
    pub items: Vec<PickItem>,
    pub filter: String,
    pub selected: usize,
    pub loading: bool,
    pub note: String,
    /// Theme to restore if a live preview is abandoned.
    pub original_theme: Option<Theme>,
    /// A saved conversation whose deletion is waiting for a second Del.
    pub pending_delete: Option<String>,
}

impl Picker {
    /// Indices of the items that match the filter.
    pub fn visible(&self) -> Vec<usize> {
        let needle = self.filter.to_lowercase();
        self.items
            .iter()
            .enumerate()
            .filter(|(_, it)| needle.is_empty() || it.label.to_lowercase().contains(&needle) || it.detail.to_lowercase().contains(&needle))
            .map(|(i, _)| i)
            .collect()
    }
}

pub enum Overlay {
    Picker(Picker),
    Help,
    Rename { text: String },
    /// Naming the machine a new tab is to be on.
    Remote { text: String },
    /// Every tab's agent (`agents_ops`), with the keyboard on one of them.
    Agents { selected: usize },
    Menu(Menu),
    Settings(settings::SettingsView),
    /// The walkthrough, at one of its cards.
    Welcome { step: usize },
}

/// How many cards the walkthrough has.
pub const WALKTHROUGH_STEPS: usize = 4;

/// The buttons of the walkthrough.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Walk {
    Back,
    Next,
    Skip,
    /// "Choose a model", on the card about the agent.
    Model,
}

/// What an entry of a right-click menu does.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum MenuAction {
    /// Tabs are named by their id, not their place: by the time an entry
    /// is picked, another terminal may have moved or closed tabs.
    RenameTab(u64),
    /// Move a tab one place left (-1) or right (1).
    MoveTab(u64, isize),
    CloseTab(u64),
    NewTab,
    SplitRight,
    SplitDown,
    Zoom,
    ClosePane,
    /// What a key also does.
    Do(Action),
    /// Open, or copy, the link the menu was opened on.
    OpenLink,
    CopyLink,
}

/// A small menu opened at the pointer.
pub struct Menu {
    /// Where it was asked for (the cell under the pointer).
    pub at: (u16, u16),
    pub items: Vec<(String, MenuAction)>,
    pub selected: usize,
}

enum Drag {
    Select,
    Divider,
    Forward,
    /// Moving the divider of a split between panes.
    Border(usize),
    /// Carrying a tab along the tab bar: where it is now.
    Tab(usize),
}

pub struct Ui {
    /// The link the pointer is over, in the pane the keyboard is in.
    pub link: Option<crate::pane::Link>,
    /// The link a right-click menu was opened on.
    menu_link: Option<String>,
    /// Where the pointer was last seen.
    pointer: (u16, u16),
    pub prefix: Option<Instant>,
    pub overlay: Option<Overlay>,
    pub hits: Vec<(Rect, Hit)>,
    pub hover: Option<Hit>,
    drag: Option<Drag>,
    pub toast: Option<(String, Instant)>,
    /// Share of the window height given to the agent panel.
    pub panel_pct: u16,
    /// Share of the window width given to the agent panel when it is beside
    /// the shell, in thousandths.
    pub side_share: u16,
    last_click: Option<(Instant, u16, u16, u8)>,
    pub spinner: usize,
}

impl Ui {
    fn new() -> Self {
        Ui {
            link: None,
            menu_link: None,
            pointer: (0, 0),
            prefix: None,
            overlay: None,
            hits: Vec::new(),
            hover: None,
            drag: None,
            toast: None,
            panel_pct: 45,
            side_share: 400,
            last_click: None,
            spinner: 0,
        }
    }

    /// A fresh one that keeps another's layout preferences, and a notice
    /// that has not been read yet (one raised before anyone was attached).
    fn like(other: &Ui) -> Self {
        Ui { panel_pct: other.panel_pct, side_share: other.side_share, toast: other.toast.clone(), ..Ui::new() }
    }
}

/// What one attached terminal is looking at: which tab is in front, and
/// the state of its own menus, pointer and prompts.
struct View {
    active: usize,
    ui: Ui,
}

struct ClientConn {
    id: u64,
    tx: mpsc::UnboundedSender<ServerFrame>,
    cols: u16,
    rows: u16,
    attached: bool,
    /// Whether this client's terminal takes 24-bit color.
    truecolor: bool,
    /// Its terminal's own text and background colors, if it said.
    colors: (Option<[u8; 3]>, Option<[u8; 3]>),
    /// It can be asked to open a link.
    opens_links: bool,
    /// How its terminal shows a notification, if it has a way.
    notify: Option<String>,
    /// Its view, while that is not the one loaded into the server (see
    /// `Server::load_view`).
    view: Option<View>,
    /// What this client's screen currently shows, for diffing.
    prev: Buffer,
    /// Needs a full repaint (just attached, or its screen is in an unknown state).
    fresh: bool,
    cursor_shape: Option<u8>,
    /// The ways its terminal can show a picture, and the size of one of
    /// its cells in pixels (zero where it did not say).
    images: crate::image::Caps,
    cell_px: (u16, u16),
    /// The pictures on its screen.
    pictures: pictures::Pictures,
    /// Frames sent to it that have not been written out yet, and whether
    /// one was not drawn because of them (see `MAX_UNSENT`).
    unsent: Arc<AtomicUsize>,
    held_back: bool,
    /// Where its cursor was last put; `None` while it is hidden.
    cursor_at: Option<(u16, u16)>,
}

pub struct Server {
    pub session: String,
    pub cfg: Config,
    pub theme: Theme,
    /// Every theme there is to choose from: the built-in ones and the user's.
    pub themes: Vec<Theme>,
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub cols: u16,
    pub rows: u16,
    pub ui: Ui,
    pub prefix: Chord,
    pub agent_key: Chord,
    /// Which keys do what.
    pub keymap: Keymap,
    /// Autopilot was turned on in this session, not by the config file.
    pub(super) autopilot_here: bool,
    /// The bell has been passed on since the loop last came round.
    rang: bool,
    pub http: reqwest::Client,
    clients: Vec<ClientConn>,
    /// The terminal used most recently. Between events its view is the one
    /// in `active`, `ui`, `cols` and `rows`.
    lead: Option<u64>,
    /// Whose view is in those fields right now.
    loaded: Option<u64>,
    /// The frame of the busy animation, the same in every view.
    spin: usize,
    next_id: u64,
    pub tx: mpsc::Sender<Ev>,
    force_redraw: bool,
    dirty: bool,
    quit: Option<String>,
    shell_dir: PathBuf,
    started: SystemTime,
    /// What providers have said about their models' context windows, by
    /// (provider, model); `None` where a provider had no answer.
    windows: std::collections::HashMap<(String, String), Option<u64>>,
    born: Instant,
    /// Keeping the session on disk, to bring it back after a restart.
    saving: restore::Saving,
    /// Started by a service manager: it waits for someone to attach for as
    /// long as it takes.
    service: bool,
    /// When to look up the directories of shells that do not report them.
    cwd_due: Option<Instant>,
    /// When a picture that has stopped moving is to be drawn.
    pictures_due: Option<Instant>,
    /// What has been spent on reading output and drawing, for measuring.
    work: proto::Work,
    /// Keys whose press went to a program that wants to hear of releases.
    keys_down: Vec<KeyCode>,
    /// The session is being stopped by `astrum kill`.
    stopped_on_purpose: bool,
    /// The other machines this session has shells on.
    pub fars: Vec<far::Far>,
    /// The MCP servers the agent may use.
    pub mcp: mcp_ops::McpState,
    next_reach: u64,
}

/// Write the embedded shell-integration scripts where shells can source them.
pub fn install_shell_files() -> Result<PathBuf> {
    let dir = paths::data_dir().join("shell");
    let zsh = dir.join("zsh");
    std::fs::create_dir_all(&zsh)?;
    let files: [(PathBuf, &str); 4] = [
        (zsh.join(".zshenv"), include_str!("../../shell/zshenv")),
        (dir.join("astrum.zsh"), include_str!("../../shell/astrum.zsh")),
        (dir.join("astrum.bash"), include_str!("../../shell/astrum.bash")),
        (dir.join("astrum.fish"), include_str!("../../shell/astrum.fish")),
    ];
    for (path, body) in files {
        if std::fs::read_to_string(&path).ok().as_deref() != Some(body) {
            std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
        }
    }
    Ok(dir)
}

/// Run a session's server. `service` says a service manager started it
/// (see `service.rs`) rather than a client that is about to attach.
/// Set when the server process itself is told to stop (SIGTERM, SIGINT).
static TOLD_TO_STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub async fn serve(session: String, service: bool) -> Result<()> {
    let sock = paths::socket_path(&session)?;
    // A live server answers on its socket; anything else is a stale file.
    while tokio::net::UnixStream::connect(&sock).await.is_ok() {
        if !service {
            anyhow::bail!("session `{session}` is already running");
        }
        // The session is running outside the service. Wait for it to stop
        // rather than fail and be started over and over.
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).with_context(|| format!("binding {}", sock.display()))?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600))?;
    }
    let shell_dir = install_shell_files()?;
    let (cfg, warnings) = Config::load();
    for w in &warnings {
        eprintln!("astrum: config: {w}");
    }

    // Saved conversations nobody has used for a long time are cleared out.
    let keep_days = cfg.agent.keep_conversations_days;
    if keep_days > 0 {
        tokio::task::spawn_blocking(move || store::prune(keep_days, &[]));
    }

    // What programs write, and everything else that is not from a
    // terminal. Kept short: a shell pouring out text waits here for the
    // loop to catch up, and whatever another pane writes meanwhile (the
    // echo of a key, say) is never far behind it in the line.
    let (tx, mut rx) = mpsc::channel::<Ev>(128);
    // What terminals send (keys, the mouse, coming and going) has a line
    // of its own, taken first: a key is not kept waiting behind output.
    let (in_tx, mut in_rx) = mpsc::channel::<Ev>(256);

    // Accept loop: one reader and one writer task per client.
    let accept_tx = in_tx;
    tokio::spawn(async move {
        let mut next: u64 = 1;
        loop {
            let Ok((stream, _)) = listener.accept().await else { break };
            let id = next;
            next += 1;
            let tx = accept_tx.clone();
            tokio::spawn(async move {
                let (mut rd, mut wr) = stream.into_split();
                let Ok(ClientMsg::Hello(hello)) = proto::read_client_msg(&mut rd).await else { return };
                let (out_tx, mut out_rx) = mpsc::unbounded_channel::<ServerFrame>();
                // How many frames are on their way to this terminal and
                // not yet written to it.
                let unsent = Arc::new(AtomicUsize::new(0));
                if tx.send(Ev::Join { id, hello, tx: out_tx, unsent: unsent.clone() }).await.is_err() {
                    return;
                }
                let caught_up = tx.clone();
                let writer = tokio::spawn(async move {
                    while let Some(frame) = out_rx.recv().await {
                        let last = matches!(frame, ServerFrame::Msg(ServerMsg::Bye { .. }));
                        let drawing = matches!(frame, ServerFrame::Output(_));
                        if proto::write_server_frame(&mut wr, &frame).await.is_err() || last {
                            break;
                        }
                        // The last frame on its way has gone: if any were
                        // held back meanwhile, there is room for one now.
                        if drawing && unsent.fetch_sub(1, Ordering::SeqCst) == 1 {
                            let _ = caught_up.send(Ev::CaughtUp { id }).await;
                        }
                    }
                });
                while let Ok(msg) = proto::read_client_msg(&mut rd).await {
                    if tx.send(Ev::Client { id, msg }).await.is_err() {
                        break;
                    }
                }
                let _ = tx.send(Ev::Gone { id }).await;
                let _ = writer.await;
            });
        }
    });

    // Watch the config file: a change to it takes effect in the running
    // session. (By looking at it once a second; nothing else is needed.)
    let config_tx = tx.clone();
    tokio::spawn(async move {
        let path = paths::config_file();
        let stamp = |path: &std::path::Path| std::fs::metadata(path).ok().and_then(|m| m.modified().ok().map(|at| (at, m.len())));
        let mut seen = stamp(&path);
        loop {
            tokio::time::sleep(Duration::from_millis(1000)).await;
            if stamp(&path) != seen {
                // Give whatever is writing it a moment to finish.
                tokio::time::sleep(Duration::from_millis(150)).await;
                seen = stamp(&path);
                if config_tx.send(Ev::ConfigChanged).await.is_err() {
                    break;
                }
            }
        }
    });

    // Terminate cleanly on signals; SIGHUP is ignored so closing the terminal
    // that launched us does not end the session.
    let sig_tx = tx.clone();
    tokio::spawn(async move {
        use tokio::signal::unix::{SignalKind, signal};
        let (Ok(mut term), Ok(mut int), Ok(mut hup)) =
            (signal(SignalKind::terminate()), signal(SignalKind::interrupt()), signal(SignalKind::hangup()))
        else {
            return;
        };
        loop {
            tokio::select! {
                _ = term.recv() => break,
                _ = int.recv() => break,
                _ = hup.recv() => continue,
            }
        }
        TOLD_TO_STOP.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = sig_tx.send(Ev::Shutdown).await;
    });

    let (themes, theme_warnings) = theme::all_with(&cfg.themes);
    for w in &theme_warnings {
        eprintln!("astrum: config: {w}");
    }
    let warnings: Vec<String> = warnings.into_iter().chain(theme_warnings).collect();
    let theme = theme::find(&themes, &cfg.theme).unwrap_or_else(theme::default_theme);
    let prefix = Chord::parse(&cfg.prefix).unwrap_or(Chord { code: KeyCode::Char('b'), mods: KeyModifiers::CONTROL });
    let agent_key = Chord::parse(&cfg.agent_key).unwrap_or(Chord { code: KeyCode::Char(' '), mods: KeyModifiers::CONTROL });
    let (keymap, key_warnings) = Keymap::new(&cfg.keys);
    for w in &key_warnings {
        eprintln!("astrum: config: {w}");
    }
    let warnings: Vec<String> = warnings.into_iter().chain(key_warnings).collect();
    let cfg_mcp = cfg.mcp.clone();
    let mut server = Server {
        session,
        cfg,
        theme,
        themes,
        tabs: Vec::new(),
        active: 0,
        cols: 80,
        rows: 24,
        ui: Ui::new(),
        prefix,
        agent_key,
        keymap,
        autopilot_here: false,
        rang: false,
        http: crate::agent::llm::http_client(),
        clients: Vec::new(),
        lead: None,
        loaded: None,
        spin: 0,
        next_id: 1,
        tx,
        force_redraw: true,
        dirty: true,
        quit: None,
        shell_dir,
        started: SystemTime::now(),
        born: Instant::now(),
        windows: Default::default(),
        saving: Default::default(),
        service,
        cwd_due: None,
        pictures_due: None,
        work: proto::Work::default(),
        keys_down: Vec::new(),
        stopped_on_purpose: false,
        fars: Vec::new(),
        next_reach: 1,
        mcp: mcp_ops::McpState::new(&cfg_mcp),
    };
    if let Some(w) = warnings.first() {
        server.toast(format!("Config problem: {w}"));
    }
    server.load_stored_keys();

    let mut last_render = Instant::now() - Duration::from_secs(1);
    loop {
        let wait = if server.dirty {
            MIN_FRAME.saturating_sub(last_render.elapsed())
        } else if server.animating() {
            TICK
        } else {
            Duration::from_secs(3600)
        };
        // Wake in time to write the session out, if that is due, or to
        // look up a shell's directory.
        let wait = match [server.saving.due, server.cwd_due, server.pictures_due, server.far_due()].into_iter().flatten().min() {
            Some(due) => wait.min(due.saturating_duration_since(Instant::now())),
            None => wait,
        };
        tokio::select! {
            // What a terminal sends is taken first.
            biased;
            Some(ev) = in_rx.recv() => server.handle(ev),
            ev = rx.recv() => {
                let Some(ev) = ev else { break };
                server.handle(ev);
            }
            _ = tokio::time::sleep(wait) => {}
        }
        // Take what else has arrived before drawing. Everything from the
        // terminals; and of what programs wrote, as much as there is
        // until a frame is due, so that output pouring past is taken in
        // bulk and the screen still keeps up with it.
        let began = Instant::now();
        loop {
            if let Ok(ev) = in_rx.try_recv() {
                server.handle(ev);
                continue;
            }
            let due = server.dirty && last_render.elapsed() >= MIN_FRAME;
            if due || began.elapsed() >= Duration::from_millis(12) {
                break;
            }
            match rx.try_recv() {
                Ok(ev) => server.handle(ev),
                Err(_) => break,
            }
        }
        server.tick();
        if server.quit.is_some() {
            break;
        }
        // (Only a frame that was sent counts toward the pace: when
        // nothing on any screen had changed, the next change is not kept
        // waiting for it.)
        if server.dirty && last_render.elapsed() >= MIN_FRAME && server.render() {
            last_render = Instant::now();
        }
    }

    // A turn cut short by the shutdown keeps what it had got to.
    for idx in 0..server.tabs.len() {
        server.save_conversation(idx);
    }
    let finished = server.finished();
    if finished {
        restore::set_aside(&server.session);
    } else {
        server.save_at_exit(server.stopped_on_purpose);
    }
    let reason = server.quit.take().unwrap_or_else(|| "server stopped".into());
    for c in &server.clients {
        let _ = c.tx.send(ServerFrame::Msg(ServerMsg::Bye { reason: reason.clone() }));
    }
    // Shells on other machines end with the session only when it was told
    // to stop; otherwise they are there to come back to.
    if !server.stopped_on_purpose {
        server.keep_far_shells();
    }
    let far = server.fars.iter().any(|f| f.way.is_some());
    server.mcp_close_all();
    server.tabs.clear();
    let _ = std::fs::remove_file(&sock);
    if far && server.stopped_on_purpose {
        // Shells on other machines have just been told to end: that has
        // further to go than a word to a terminal on this one.
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    // Give writer tasks a moment to flush the goodbye.
    tokio::time::sleep(Duration::from_millis(60)).await;
    if finished {
        // Closed, or going down with the machine? If we are told to stop
        // as well within a moment, it was the machine: keep the session.
        tokio::time::sleep(Duration::from_millis(400)).await;
        if TOLD_TO_STOP.load(std::sync::atomic::Ordering::SeqCst) {
            restore::put_back(&server.session);
        } else {
            restore::discard_aside(&server.session);
        }
    }
    Ok(())
}

impl Server {
    /// Everything on every terminal is to be drawn again.
    pub(super) fn force_redraw(&mut self) {
        self.force_redraw = true;
        self.dirty = true;
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn toast(&mut self, text: impl Into<String>) {
        self.ui.toast = Some((text.into(), Instant::now()));
        self.dirty = true;
    }

    pub fn tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    pub fn tab_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active)
    }

    pub fn tab_index(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    /// Every view: the one loaded, then those put away.
    fn uis(&self) -> impl Iterator<Item = &Ui> {
        std::iter::once(&self.ui).chain(self.clients.iter().filter_map(|c| c.view.as_ref().map(|v| &v.ui)))
    }

    fn animating(&self) -> bool {
        self.uis().any(|ui| ui.prefix.is_some() || ui.toast.is_some() || matches!(&ui.overlay, Some(Overlay::Picker(p)) if p.loading))
            || self.tabs.iter().any(|t| t.agent.busy || t.agent.wait.is_some() || t.panes.iter().any(|p| p.sync_pending() || p.carrying()))
            || (!self.service && self.tabs.is_empty() && self.clients.iter().all(|c| !c.attached))
    }

    fn tick(&mut self) {
        let now = Instant::now();
        self.rang = false;
        // A shell the agent was being carried into that never answered.
        let mut gave_up = None;
        for pane in self.tabs.iter_mut().flat_map(|t| t.panes.iter_mut()) {
            if pane.carry_overdue(now) {
                gave_up = pane.carried.take();
            }
        }
        if let Some(how) = gave_up {
            self.say_carried(how);
        }
        if self.saving.due.is_some_and(|due| now >= due) {
            self.save_session();
        }
        if self.pictures_due.is_some_and(|due| now >= due) {
            self.pictures_due = None;
            self.dirty = true;
        }
        if self.cwd_due.is_some_and(|due| now >= due) {
            self.cwd_due = None;
            let mut moved = false;
            for pane in self.tabs.iter_mut().flat_map(|t| t.panes.iter_mut()).filter(|p| p.cwd_stale) {
                moved |= pane.refresh_cwd();
            }
            self.dirty |= moved;
        }
        // In every view: a prefix nobody followed up, a notice that has been read.
        for ui in std::iter::once(&mut self.ui).chain(self.clients.iter_mut().filter_map(|c| c.view.as_mut().map(|v| &mut v.ui))) {
            if ui.prefix.is_some_and(|t| now.duration_since(t) > PREFIX_TIMEOUT) {
                ui.prefix = None;
                self.dirty = true;
            }
            if ui.toast.as_ref().is_some_and(|(_, t)| now.duration_since(*t) > Duration::from_secs(4)) {
                ui.toast = None;
                self.dirty = true;
            }
        }
        for tab in &mut self.tabs {
            for pane in tab.panes.iter_mut() {
                if pane.flush_stale_sync() {
                    self.dirty = true;
                }
            }
        }
        self.agent_tick(now);
        self.far_tick(now);
        if self.tabs.iter().any(|t| t.agent.busy && t.agent.visible) || self.uis().any(|ui| matches!(&ui.overlay, Some(Overlay::Picker(p)) if p.loading)) {
            let frame = (self.born.elapsed().as_millis() / 80) as usize;
            if frame != self.spin {
                self.spin = frame;
                self.dirty = true;
            }
        }
        // A server nobody ever attached to should not linger.
        if !self.service && self.tabs.is_empty() && self.clients.iter().all(|c| !c.attached) && self.born.elapsed() > Duration::from_secs(20) {
            self.quit = Some("no client attached".into());
        }
    }

    fn handle(&mut self, ev: Ev) {
        // Whatever this is, the session may differ afterwards from what is on disk.
        self.schedule_save();
        match ev {
            Ev::Join { id, hello, tx, unsent } => self.on_join(id, hello, tx, unsent),
            Ev::CaughtUp { id } => {
                // A frame was held back while this terminal was busy with the last.
                if let Some(c) = self.clients.iter_mut().find(|c| c.id == id) {
                    self.dirty |= std::mem::take(&mut c.held_back);
                }
            }
            Ev::Client { id, msg } => self.on_client(id, msg),
            Ev::Gone { id } => {
                self.drop_client(id);
                self.recompute_size();
            }
            Ev::Pty { pane, data } => self.on_pty(pane, &data),
            Ev::PtyExit { pane, killed } => {
                if let Some(idx) = self.find_pane(pane) {
                    // A shell killed from outside may be the first of all
                    // of them, with the machine going down: keep what is saved.
                    self.hold_saves();
                    self.saving.killed_now = killed;
                    self.remove_pane(idx, pane);
                    self.saving.killed_now = false;
                }
            }
            Ev::Agent { tab, turn, ev } => self.on_agent_ev(tab, turn, ev),
            Ev::Models { result, errors } => {
                // The list goes to whichever terminal is waiting for it.
                let waiting = |ui: &Ui| matches!(&ui.overlay, Some(Overlay::Picker(p)) if p.loading);
                let asked = self.clients.iter().find(|c| c.view.as_ref().is_some_and(|v| waiting(&v.ui))).map(|c| c.id);
                if let Some(id) = asked.filter(|_| !waiting(&self.ui)) {
                    self.load_view(id);
                }
                self.on_models(result, errors);
                if let Some(lead) = self.lead {
                    self.load_view(lead);
                }
            }
            Ev::KeysLoaded(keys) => {
                for (provider, key) in keys {
                    if let Some(p) = self.cfg.providers.get_mut(&provider) {
                        p.stored_key = key;
                    }
                }
                self.dirty = true;
            }
            Ev::KeyChanged { tab, provider, result } => self.on_key_changed(tab, provider, result),
            Ev::ConfigChanged => match self.reload_config() {
                Ok(notes) => match notes.first() {
                    Some(note) => self.toast(format!("Settings reloaded. Config problem: {note}")),
                    None => self.toast("Settings reloaded from the config file"),
                },
                Err(problem) => self.toast(format!("Config problem, nothing was changed: {problem}")),
            },
            Ev::Remote { reach, told } => self.on_remote(reach, told),
            Ev::Mcp { server, round, ev } => self.on_mcp(server, round, ev),
            Ev::Shutdown => self.quit = Some("server terminated".into()),
        }
    }

    // ---- clients ------------------------------------------------------------

    fn on_join(&mut self, id: u64, hello: Hello, tx: mpsc::UnboundedSender<ServerFrame>, unsent: Arc<AtomicUsize>) {
        if hello.version != proto::VERSION {
            let _ = tx.send(ServerFrame::Msg(ServerMsg::Bye {
                reason: format!(
                    "this astrum client speaks protocol {} but the running session uses {}; run `astrum kill` and start again",
                    hello.version,
                    proto::VERSION
                ),
            }));
            return;
        }
        let attached = !hello.control;
        let (cols, rows) = (hello.cols.max(20), hello.rows.max(5));
        self.clients.push(ClientConn {
            id,
            tx,
            cols,
            rows,
            attached,
            truecolor: self.cfg.truecolor.or(hello.truecolor).unwrap_or(true),
            colors: (hello.fg, hello.bg),
            opens_links: hello.opens_links,
            notify: hello.notify.clone().filter(|n| matches!(n.as_str(), "osc9" | "osc99" | "osc777")),
            // A terminal that joins others starts on the tab in front, with
            // a view of its own. The first one takes over the view the
            // session was left with.
            view: (attached && self.loaded.is_some()).then(|| View { active: self.active, ui: Ui::like(&self.ui) }),
            prev: Buffer::empty(Rect::new(0, 0, cols, rows)),
            fresh: true,
            cursor_shape: None,
            images: hello.images,
            cell_px: hello.cell_px,
            pictures: pictures::Pictures::default(),
            unsent,
            held_back: false,
            cursor_at: None,
        });
        if !attached {
            return;
        }
        // The terminal you just opened is the one you are looking at.
        if self.loaded.is_none() {
            // Whatever menu the last terminal left open is not this one's.
            self.ui = Ui::like(&self.ui);
        }
        self.lead = Some(id);
        self.recompute_size();
        if self.tabs.is_empty() && !self.restore_session() {
            self.new_tab(hello.cwd.as_deref());
        }
        // The first time Astrum is used here: say what is where.
        // (To a person, that is: a script driving a session has no use for it.)
        if !hello.scripted && std::mem::take(&mut self.cfg.first_run) && !self.cfg.state.welcomed {
            self.open_walkthrough();
        }
        self.force_redraw = true;
        self.dirty = true;
    }

    fn on_client(&mut self, id: u64, msg: ClientMsg) {
        match msg {
            ClientMsg::Hello(_) => {}
            ClientMsg::Event(ev) => self.on_event(id, ev),
            ClientMsg::Detach => self.detach_client(id, "detached"),
            ClientMsg::Info => {
                let info = SessionInfo {
                    session: self.session.clone(),
                    pid: std::process::id(),
                    tabs: self.tabs.iter().map(ui::tab_title).collect(),
                    clients: self.clients.iter().filter(|c| c.attached).count(),
                    started_unix: self.started.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
                    work: self.work,
                };
                if let Some(c) = self.clients.iter().find(|c| c.id == id) {
                    let _ = c.tx.send(ServerFrame::Msg(ServerMsg::Info(info)));
                }
            }
            ClientMsg::Kill => {
                self.stopped_on_purpose = true;
                self.quit = Some("session killed".into());
            }
            ClientMsg::Reload { env } => {
                let report = self.reload_providers(Some(env));
                self.detach_client(id, &report);
            }
        }
    }

    /// Forget a client. If its view was the one loaded, that view stays
    /// behind for whoever comes next.
    fn drop_client(&mut self, id: u64) {
        self.clients.retain(|c| c.id != id);
        if self.loaded == Some(id) {
            self.loaded = None;
        }
    }

    fn detach_client(&mut self, id: u64, reason: &str) {
        if let Some(c) = self.clients.iter().find(|c| c.id == id) {
            let _ = c.tx.send(ServerFrame::Msg(ServerMsg::Bye { reason: reason.into() }));
        }
        self.drop_client(id);
        self.recompute_size();
    }

    fn detach_all(&mut self) {
        for c in self.clients.drain(..) {
            let _ = c.tx.send(ServerFrame::Msg(ServerMsg::Bye { reason: "detached".into() }));
        }
        self.loaded = None;
    }

    /// Bring a terminal's view into the server's own fields (`active`,
    /// `ui`, `cols`, `rows`), putting away the one that was there. Every
    /// attached terminal has a view of its own: which tab is in front, its
    /// menus, its pointer. Everything that acts on "the view" acts on the
    /// one loaded: a terminal's own while its event is handled or its frame
    /// drawn, and otherwise that of the terminal used most recently.
    fn load_view(&mut self, id: u64) {
        if self.loaded == Some(id) {
            return;
        }
        let Some(target) = self.clients.iter().position(|c| c.id == id && c.attached) else { return };
        if let Some(was) = self.loaded.and_then(|was| self.clients.iter().position(|c| c.id == was)) {
            let ui = std::mem::replace(&mut self.ui, Ui::new());
            self.clients[was].view = Some(View { active: self.active, ui });
        }
        // (With nothing put away for it, it takes over what is there.)
        if let Some(view) = self.clients[target].view.take() {
            self.active = view.active;
            self.ui = view.ui;
        }
        self.cols = self.clients[target].cols;
        self.rows = self.clients[target].rows;
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
        self.loaded = Some(id);
    }

    /// Is this tab in front in any terminal's view?
    pub(super) fn shown(&self, idx: usize) -> bool {
        self.active == idx || self.clients.iter().any(|c| c.attached && c.view.as_ref().is_some_and(|v| v.active == idx))
    }

    /// Is a terminal other than the one loaded looking at this tab?
    fn client_shows(&self, client: u64, idx: usize) -> bool {
        self.clients.iter().any(|c| c.id == client && c.attached && c.view.as_ref().is_some_and(|v| v.active == idx))
    }

    /// The size another terminal has this view's tab laid out for, when it
    /// is that terminal's and not this one's.
    /// (When both have room for it at the same size there is nothing to
    /// tell apart, and this view is drawn and clicked like any other.)
    pub fn foreign_size(&self) -> Option<(u16, u16)> {
        let tab = self.tab()?;
        let area = self.panes_area();
        let foreign = tab.owner.is_some() && tab.owner != self.loaded && tab.sized != (0, 0) && tab.sized != (area.width, area.height);
        foreign.then_some(tab.sized)
    }

    /// The terminal whose view is loaded takes the tab it has in front:
    /// the panes are sized for it from now on. True if that changes their
    /// layout, so that what the terminal shows no longer matches.
    fn own_tab(&mut self) -> bool {
        let Some(me) = self.loaded else { return false };
        let area = self.panes_area();
        let Some(tab) = self.tabs.get_mut(self.active) else { return false };
        if tab.owner == Some(me) {
            return false;
        }
        let was = tab.sized;
        tab.owner = Some(me);
        // The shells take the new size now, not at the next frame: what
        // this terminal types next must find them at the size it sees.
        tab.sized = (area.width, area.height);
        for (pane_id, rect) in tab.panes.place(area).0 {
            if let Some(pane) = tab.panes.get_mut(pane_id) {
                pane.resize(rect.width, rect.height);
            }
        }
        self.dirty = true;
        was != (0, 0) && was != (area.width, area.height)
    }

    /// After terminals have come, gone or changed size: make sure the one
    /// in the lead is still there (else the newest takes over), and that
    /// its view is the one loaded.
    fn recompute_size(&mut self) {
        let attached = |c: &&ClientConn| c.attached;
        if !self.clients.iter().filter(attached).any(|c| Some(c.id) == self.lead) {
            self.lead = self.clients.iter().filter(attached).last().map(|c| c.id);
        }
        let Some(lead) = self.lead else { return };
        self.load_view(lead);
        if let Some(c) = self.clients.iter().find(|c| c.id == lead) {
            self.cols = c.cols;
            self.rows = c.rows;
        }
        self.retheme();
        self.dirty = true;
    }

    /// `client` is the terminal being used now: it takes the lead.
    fn take_lead(&mut self, client: u64) {
        if self.clients.iter().any(|c| c.id == client && c.attached) {
            let changed = self.lead != Some(client);
            self.lead = Some(client);
            self.load_view(client);
            if changed {
                self.retheme();
            }
        }
    }

    fn broadcast(&mut self, frame_bytes: Vec<u8>) {
        let mut dead = Vec::new();
        for c in self.clients.iter().filter(|c| c.attached) {
            c.unsent.fetch_add(1, Ordering::SeqCst);
            if c.tx.send(ServerFrame::Output(frame_bytes.clone())).is_err() {
                dead.push(c.id);
            }
        }
        if !dead.is_empty() {
            for id in dead {
                self.drop_client(id);
            }
            self.recompute_size();
        }
    }

    pub fn copy_to_clipboard(&mut self, text: String) {
        for c in self.clients.iter().filter(|c| c.attached) {
            let _ = c.tx.send(ServerFrame::Msg(ServerMsg::Clipboard(text.clone())));
        }
    }

    // ---- tabs ---------------------------------------------------------------

    /// Is the agent panel docked beside the shell, at this window's width?
    fn panel_beside(&self) -> bool {
        match self.cfg.agent_panel {
            crate::config::Dock::Side => true,
            crate::config::Dock::Bottom => false,
            crate::config::Dock::Auto => self.cols >= crate::config::WIDE_TERMINAL,
        }
    }

    /// Where everything goes in the window, with the agent panel open or not.
    pub fn layout(&self, panel_visible: bool) -> ui::Layout {
        let side = self.panel_beside().then_some(self.ui.side_share);
        ui::layout(self.cols, self.rows, panel_visible, self.ui.panel_pct, side, self.cfg.status_bar)
    }

    fn pane_size(&self, panel_visible: bool) -> (u16, u16) {
        let l = self.layout(panel_visible);
        (l.pane.width, l.pane.height)
    }

    /// Start a shell in a new pane, in `cwd` or else where the focused pane is.
    /// The text and background colors a program in a pane is told it has,
    /// when it asks: the theme's, if the theme recolors the terminal's
    /// content; otherwise those of the terminal being used, if known.
    fn term_colors(&self) -> (Option<Rgb>, Option<Rgb>) {
        let rgb = |c: [u8; 3]| Rgb { r: c[0], g: c[1], b: c[2] };
        if self.cfg.theme_terminal && !self.theme.flat {
            let of = |c: ratatui::style::Color| match c {
                ratatui::style::Color::Rgb(r, g, b) => Some(Rgb { r, g, b }),
                _ => None,
            };
            return (of(self.theme.term_fg), of(self.theme.term_bg));
        }
        let lead = self.clients.iter().find(|c| Some(c.id) == self.lead);
        let said = lead.into_iter().chain(self.clients.iter().filter(|c| c.attached)).map(|c| c.colors).find(|(fg, bg)| fg.is_some() || bg.is_some());
        match said {
            Some((fg, bg)) => (fg.map(rgb), bg.map(rgb)),
            None => (None, None),
        }
    }

    fn spawn_pane(&mut self, cwd: Option<&str>, cols: u16, rows: u16) -> Option<Pane> {
        let id = self.next_id;
        self.next_id += 1;
        // (Not from a shell on another machine: its directory is not one of ours.)
        let inherited = self.tab().filter(|t| t.panes.focused().there().is_none()).and_then(|t| t.panes.focused().cwd.clone());
        let cwd = cwd.map(str::to_string).or(inherited);
        let opts = SpawnOpts {
            id,
            cols,
            rows,
            cwd: cwd.as_deref(),
            shell: self.cfg.shell.as_deref(),
            shell_dir: &self.shell_dir,
            session: &self.session,
            scrollback: self.cfg.scrollback,
        };
        match Pane::spawn(opts, self.tx.clone()) {
            Ok(mut pane) => {
                let (fg, bg) = self.term_colors();
                pane.default_fg = fg.unwrap_or(pane.default_fg);
                pane.default_bg = bg.unwrap_or(pane.default_bg);
                Some(pane)
            }
            Err(e) => {
                self.toast(format!("Could not start a shell: {e}"));
                None
            }
        }
    }

    pub fn new_tab(&mut self, cwd: Option<&str>) {
        let (cols, rows) = self.pane_size(false);
        let Some(pane) = self.spawn_pane(cwd, cols, rows) else { return };
        // A tab is known by the id of the pane it began with.
        let id = pane.id;
        self.tabs.push(Tab { id, panes: panes::Panes::new(pane), name: None, agent: AgentSession::new(), focus: Focus::Shell, activity: false, owner: None, sized: (0, 0) });
        self.active = self.tabs.len() - 1;
        self.dirty = true;
    }

    // ---- panes --------------------------------------------------------------

    /// The part of the window the front tab's panes share.
    fn panes_area(&self) -> Rect {
        let panel = self.tab().is_some_and(|t| t.agent.visible);
        self.layout(panel).pane
    }

    /// Where the focused pane is drawn.
    fn focused_rect(&self) -> Rect {
        let area = self.panes_area();
        let Some(tab) = self.tab() else { return area };
        tab.panes.place(area).0.into_iter().find(|(id, _)| *id == tab.panes.focus).map(|(_, r)| r).unwrap_or(area)
    }

    /// The tab a pane belongs to.
    fn find_pane(&self, pane: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.panes.get(pane).is_some())
    }

    /// Split the focused pane in two, with a new shell in the new half.
    pub fn split_pane(&mut self, side_by_side: bool) {
        let area = self.panes_area();
        let Some(tab) = self.tab() else { return };
        if tab.panes.zoomed {
            self.toggle_zoom();
        }
        let Some(tab) = self.tab() else { return };
        if !tab.panes.room_to_split(area, side_by_side) {
            self.toast("Not enough room to split this pane");
            return;
        }
        let from = self.focused_rect();
        let (cols, rows) = ((from.width / 2).max(2), (from.height / 2).max(1));
        // Beside a shell on another machine: another shell on that machine.
        let like = self.tab().and_then(|t| t.panes.focused().there().map(|there| (there.host.clone(), t.panes.focused().cwd.clone())));
        let pane = match like {
            Some(like) => Some(self.held_like(like, cols, rows)),
            None => self.spawn_pane(None, cols, rows),
        };
        let Some(pane) = pane else { return };
        let id = pane.id;
        if let Some(tab) = self.tab_mut() {
            tab.panes.add(pane, side_by_side);
            tab.focus = Focus::Shell;
        }
        self.focus_pane(id);
    }

    /// Move the keyboard to a pane of the front tab. The agent goes with
    /// it, unless it is in the middle of something in the pane it has.
    pub fn focus_pane(&mut self, id: u64) {
        let Some(tab) = self.tab_mut() else { return };
        if tab.panes.get(id).is_none() {
            return;
        }
        tab.panes.focus = id;
        if !tab.agent.busy && !tab.agent.detached {
            tab.panes.agent = id;
        }
        self.dirty = true;
    }

    /// Move the keyboard to the pane next door. False if there is none.
    fn move_focus(&mut self, dir: panes::Dir) -> bool {
        let area = self.panes_area();
        let Some(tab) = self.tab() else { return false };
        if tab.panes.zoomed {
            return false;
        }
        match tab.panes.neighbour(area, dir) {
            Some(id) => {
                self.focus_pane(id);
                if let Some(tab) = self.tab_mut() {
                    tab.focus = Focus::Shell;
                }
                true
            }
            None => false,
        }
    }

    /// Let the focused pane fill the tab, or put the others back.
    pub fn toggle_zoom(&mut self) {
        let Some(tab) = self.tab_mut() else { return };
        if tab.panes.len() < 2 {
            self.toast("Zoom is for a tab with more than one pane");
            return;
        }
        tab.panes.zoomed = !tab.panes.zoomed;
        self.dirty = true;
    }

    /// Close a pane: its shell ends and its neighbour takes the space. The
    /// last pane of a tab takes the tab with it.
    fn remove_pane(&mut self, idx: usize, pane: u64) {
        let Some(tab) = self.tabs.get(idx) else { return };
        if tab.panes.len() < 2 {
            self.remove_tab(idx);
            return;
        }
        if tab.panes.agent == pane && (tab.agent.busy || tab.agent.detached) {
            self.cancel_turn(idx, "The pane the agent was working in has closed.");
            self.tabs[idx].agent.detached = false;
        }
        let tab = &mut self.tabs[idx];
        tab.panes.remove(pane);
        // (An agent still following a command of its own stays with that pane.)
        if !tab.agent.busy && !tab.agent.detached {
            tab.panes.agent = tab.panes.focus;
        }
        self.dirty = true;
    }

    /// The machine the pane with the keyboard is waiting for, if its
    /// shell is on one that is not reached (yet, or any more).
    fn waiting_for(&self) -> Option<String> {
        let tab = self.tab().filter(|t| t.focus == Focus::Shell)?;
        self.cover_of(tab.panes.focused()).map(|far| far.host.clone())
    }

    /// Ask which machine a new tab is to be on.
    pub fn do_remote_prompt(&mut self) {
        self.ui.overlay = Some(Overlay::Remote { text: String::new() });
        self.dirty = true;
    }

    /// Close the pane the keyboard is in.
    pub fn close_pane(&mut self) {
        if let Some(pane) = self.tab().map(|t| t.panes.focus) {
            self.remove_pane(self.active, pane);
        }
    }

    /// Move a tab to another place in the bar. The tab in front stays in front.
    pub fn move_tab(&mut self, from: usize, to: usize) {
        if from == to || from >= self.tabs.len() || to >= self.tabs.len() {
            return;
        }
        let front = self.tabs[self.active].id;
        let others = self.views_tabs();
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        self.active = self.tab_index(front).unwrap_or(0);
        self.repoint_views(others, None);
        self.dirty = true;
    }

    /// Open the right-click menu for a tab.
    fn open_tab_menu(&mut self, idx: usize, at: (u16, u16)) {
        let Some(id) = self.tabs.get(idx).map(|t| t.id) else { return };
        let mut items = vec![("Rename".to_string(), MenuAction::RenameTab(id))];
        if idx > 0 {
            items.push(("Move left".into(), MenuAction::MoveTab(id, -1)));
        }
        if idx + 1 < self.tabs.len() {
            items.push(("Move right".into(), MenuAction::MoveTab(id, 1)));
        }
        items.push(("New tab".into(), MenuAction::NewTab));
        items.push(("Close tab".into(), MenuAction::CloseTab(id)));
        self.ui.overlay = Some(Overlay::Menu(Menu { at, items, selected: 0 }));
        self.dirty = true;
    }

    /// Open the right-click menu for the pane the keyboard is in.
    /// The menu for the pane the keyboard is in, opened at `at`; `link` is
    /// the link under the pointer, if it is on one.
    fn open_pane_menu(&mut self, at: (u16, u16), link: Option<String>) {
        let Some(tab) = self.tab() else { return };
        let mut items = Vec::new();
        if let Some(uri) = &link {
            // A link that is not opened (a file, say) can still be copied.
            if crate::pane::openable(uri) {
                items.push(("Open link".to_string(), MenuAction::OpenLink));
            }
            items.push(("Copy link".to_string(), MenuAction::CopyLink));
        }
        items.extend([("Split right".to_string(), MenuAction::SplitRight), ("Split down".into(), MenuAction::SplitDown)]);
        items.push(("Find…".into(), MenuAction::Do(Action::Find)));
        if tab.panes.len() > 1 {
            items.push((if tab.panes.zoomed { "Unzoom" } else { "Zoom" }.into(), MenuAction::Zoom));
        }
        items.push((if tab.panes.len() > 1 { "Close pane" } else { "Close tab" }.into(), MenuAction::ClosePane));
        self.ui.menu_link = link;
        self.ui.overlay = Some(Overlay::Menu(Menu { at, items, selected: 0 }));
        self.dirty = true;
    }

    /// The link at a cell of the window, if that cell is in the pane the
    /// keyboard is in and has one.
    fn link_under(&mut self, col: u16, row: u16) -> Option<crate::pane::Link> {
        let rect = self.focused_rect();
        if col < rect.x || row < rect.y || col >= rect.x + rect.width || row >= rect.y + rect.height {
            return None;
        }
        self.tab_mut()?.panes.focused_mut().link_at(col - rect.x, row - rect.y)
    }

    /// Have the terminal being used open a link.
    fn open_link(&mut self, uri: &str) {
        if !crate::pane::openable(uri) {
            self.toast("That link is not one Astrum opens (only web and mail links); right-click to copy it");
            return;
        }
        let client = self.clients.iter().find(|c| Some(c.id) == self.lead && c.attached);
        match client {
            Some(c) if c.opens_links => {
                let _ = c.tx.send(ServerFrame::Msg(ServerMsg::OpenUrl(uri.to_string())));
                self.toast(format!("Opening {}", ui::text::truncate(uri, 60)));
            }
            // A client from before links could be opened: hand it the address instead.
            _ => {
                self.copy_to_clipboard(uri.to_string());
                self.toast("Copied the link (restart this terminal's astrum to open links directly)");
            }
        }
    }

    /// The menu of everything the prefix key offers, for the mouse: opened
    /// from the status bar, it sits just above it.
    fn open_main_menu(&mut self, col: u16) {
        let Some(tab) = self.tab() else { return };
        let (several, zoomed, showing) = (tab.panes.len() > 1, tab.panes.zoomed, tab.agent.visible);
        let prefix = self.prefix.label();
        let mut items = Vec::new();
        for &(action, _, name) in OFFERED {
            let name = match action {
                Action::Zoom if !several => continue,
                Action::Zoom if zoomed => "Unzoom pane",
                Action::Close if !several => "Close tab",
                Action::AgentPanel if showing => "Hide the agent panel",
                Action::AgentPanel => "Talk to the agent",
                _ => name,
            };
            // After the tab: the key that does the same, drawn at the right edge.
            items.push((format!("{name}\t{}", self.keymap.label(action, &prefix)), MenuAction::Do(action)));
        }
        let height = items.len() as u16 + 2;
        let at = (col, self.rows.saturating_sub(height + 2));
        self.ui.overlay = Some(Overlay::Menu(Menu { at, items, selected: 0 }));
        self.dirty = true;
    }

    /// Do something a key also does, asked for with the mouse.
    fn act(&mut self, action: Action) {
        let Some(client) = self.lead else { return };
        self.do_action(client, action);
    }

    fn run_menu_action(&mut self, action: MenuAction) {
        self.ui.overlay = None;
        self.dirty = true;
        match action {
            // (A tab that has gone in the meantime: nothing to do.)
            MenuAction::RenameTab(id) | MenuAction::MoveTab(id, _) | MenuAction::CloseTab(id) if self.tab_index(id).is_none() => {}
            MenuAction::RenameTab(id) => {
                let idx = self.tab_index(id).unwrap_or(0);
                self.select_tab(idx);
                let current = self.tab().and_then(|t| t.name.clone()).unwrap_or_default();
                self.ui.overlay = Some(Overlay::Rename { text: current });
            }
            MenuAction::MoveTab(id, by) => {
                let idx = self.tab_index(id).unwrap_or(0);
                self.move_tab(idx, idx.saturating_add_signed(by));
            }
            MenuAction::CloseTab(id) => {
                let idx = self.tab_index(id).unwrap_or(0);
                self.remove_tab(idx);
            }
            MenuAction::NewTab => self.new_tab(None),
            MenuAction::SplitRight => self.split_pane(true),
            MenuAction::SplitDown => self.split_pane(false),
            MenuAction::Zoom => self.toggle_zoom(),
            MenuAction::ClosePane => self.close_pane(),
            MenuAction::Do(action) => self.act(action),
            MenuAction::OpenLink => {
                if let Some(uri) = self.ui.menu_link.take() {
                    self.open_link(&uri);
                }
            }
            MenuAction::CopyLink => {
                if let Some(uri) = self.ui.menu_link.take() {
                    self.copy_to_clipboard(uri);
                    self.toast("Copied the link");
                }
            }
        }
    }

    pub fn select_tab(&mut self, idx: usize) {
        if idx < self.tabs.len() && idx != self.active {
            self.active = idx;
            self.tabs[idx].activity = false;
            self.tabs[idx].agent.unseen = false;
            self.dirty = true;
        }
    }

    /// For each view that is put away: the tab it has in front, by id and
    /// by position.
    fn views_tabs(&self) -> Vec<(u64, Option<u64>, usize)> {
        self.clients.iter().filter_map(|c| c.view.as_ref().map(|v| (c.id, self.tabs.get(v.active).map(|t| t.id), v.active))).collect()
    }

    /// After tabs have moved, or the one at `removed` has gone: keep each
    /// put-away view on the tab it was on, or on its neighbour.
    fn repoint_views(&mut self, were: Vec<(u64, Option<u64>, usize)>, removed: Option<usize>) {
        let last = self.tabs.len().saturating_sub(1);
        for (client, tab, at) in were {
            let now = tab.and_then(|id| self.tab_index(id)).unwrap_or_else(|| match removed {
                Some(gone) if at > gone || at > last => at.saturating_sub(1).min(last),
                _ => at.min(last),
            });
            if let Some(view) = self.clients.iter_mut().find(|c| c.id == client).and_then(|c| c.view.as_mut()) {
                view.active = now;
            }
        }
    }

    fn remove_tab(&mut self, idx: usize) {
        if idx >= self.tabs.len() {
            return;
        }
        if let Some(task) = self.tabs[idx].agent.task.take() {
            task.abort();
        }
        let others = self.views_tabs();
        self.tabs.remove(idx);
        self.repoint_views(others, Some(idx));
        if self.tabs.is_empty() {
            // The last shell was closed: the session is over, and there is
            // nothing to bring back. Unless that shell was killed from
            // outside, or the server is itself on its way down and the
            // shells are going with it.
            if self.quit.is_none() {
                self.saving.finished = !self.saving.killed_now;
                self.quit = Some("exited".into());
            }
            return;
        }
        if self.active > idx || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1).min(self.tabs.len() - 1);
        }
        self.dirty = true;
    }

    fn on_pty(&mut self, pane_id: u64, data: &[u8]) {
        let Some(idx) = self.find_pane(pane_id) else { return };
        let mut events = Vec::new();
        let began = Instant::now();
        let mut carried = None;
        if let Some(pane) = self.tabs[idx].panes.get_mut(pane_id) {
            pane.feed(data, &mut events);
            carried = pane.carried.take();
        }
        if let Some(how) = carried {
            self.say_carried(how);
        }
        self.work.read += data.len() as u64;
        self.work.read_us += began.elapsed().as_micros() as u64;
        // A moment after output, when a `cd` will have happened.
        self.cwd_due.get_or_insert(Instant::now() + Duration::from_millis(150));
        // Only the pane the agent works in tells it about commands.
        let agents = self.tabs[idx].panes.agent == pane_id;
        let shown = self.shown(idx);
        if shown {
            self.dirty = true;
        }
        for ev in events {
            match ev {
                PaneEvent::Bell => {
                    if !shown {
                        self.tabs[idx].activity = true;
                        self.dirty = true;
                    }
                    // One ring for a run of them: a file full of bell
                    // characters is not a frame apiece.
                    if !std::mem::replace(&mut self.rang, true) {
                        self.broadcast(vec![0x07]);
                    }
                }
                PaneEvent::Clipboard(text) => self.copy_to_clipboard(text),
                PaneEvent::CommandStarted if agents => self.agent_command_started(idx),
                PaneEvent::CommandStarted => {}
                PaneEvent::CommandFinished(block) => {
                    if !shown {
                        self.tabs[idx].activity = true;
                    }
                    if agents {
                        self.agent_command_finished(idx, block);
                    }
                    self.dirty = true;
                }
                PaneEvent::PromptReady => self.dirty = true,
            }
        }
    }

    // ---- input --------------------------------------------------------------

    /// An input event from a terminal. It is handled in that terminal's own
    /// view; afterwards the view of the terminal in the lead is back.
    fn on_event(&mut self, client: u64, ev: Event) {
        if !self.clients.iter().any(|c| c.id == client && c.attached) {
            return;
        }
        self.load_view(client);
        self.on_event_in_view(client, ev);
        // (A pointer passing over another window does not take the lead.)
        if let Some(lead) = self.lead {
            self.load_view(lead);
        }
    }

    fn on_event_in_view(&mut self, client: u64, ev: Event) {
        match ev {
            Event::Key(key) => {
                self.take_lead(client);
                self.on_key(client, key)
            }
            Event::Mouse(m) => {
                // Merely moving the pointer over another window does nothing
                // to the session; clicking or scrolling does.
                if !matches!(m.kind, MouseEventKind::Moved) {
                    self.take_lead(client);
                }
                // A tab laid out for another terminal comes over at the
                // first click or scroll inside it, and that is all that
                // click does: what it was aimed at has moved.
                let inside = matches!(self.hit_at(m.column, m.row), Some((_, Hit::Pane)));
                let uses = matches!(m.kind, MouseEventKind::Down(_) | MouseEventKind::ScrollUp | MouseEventKind::ScrollDown);
                if inside && uses && self.own_tab() {
                    return;
                }
                self.on_mouse(m)
            }
            Event::Paste(text) => {
                self.take_lead(client);
                self.own_tab();
                self.on_paste(text)
            }
            Event::Resize(cols, rows) => {
                if let Some(c) = self.clients.iter_mut().find(|c| c.id == client) {
                    c.cols = cols.max(20);
                    c.rows = rows.max(5);
                }
                self.recompute_size();
            }
            Event::FocusGained | Event::FocusLost => {
                let gained = matches!(ev, Event::FocusGained);
                if gained {
                    self.take_lead(client);
                }
                if let Some(tab) = self.tab() {
                    if tab.focus == Focus::Shell && tab.panes.focused().mode().contains(TermMode::FOCUS_IN_OUT) {
                        tab.panes.focused().write(if gained { &b"\x1b[I"[..] } else { &b"\x1b[O"[..] });
                    }
                }
            }
        }
    }

    fn on_paste(&mut self, text: String) {
        if let Some(Overlay::Rename { text: name } | Overlay::Remote { text: name }) = &mut self.ui.overlay {
            name.push_str(text.lines().next().unwrap_or(""));
            self.dirty = true;
            return;
        }
        if self.ui.overlay.is_some() {
            return;
        }
        if let Some(host) = self.waiting_for() {
            self.far_paste(&host, &text);
            return;
        }
        let Some(tab) = self.tab_mut() else { return };
        match tab.focus {
            // Into a search being typed, a paste is more of the text searched for.
            Focus::Shell if tab.panes.focused().copy.as_ref().is_some_and(|c| c.typing) => {
                let pane = tab.panes.focused_mut();
                let mut query = pane.copy.as_ref().and_then(|c| c.find.as_ref()).map(|f| f.query.clone()).unwrap_or_default();
                query.push_str(text.lines().next().unwrap_or(""));
                pane.find_set(query);
            }
            Focus::Shell => {
                tab.panes.focused_mut().leave_copy();
                tab.panes.focused_mut().scroll_to_bottom();
                tab.panes.focused_mut().note_paste(&text);
                tab.agent.last_typed = Some(Instant::now());
                let bytes = input::encode_paste(&text, tab.panes.focused_mut().mode());
                tab.panes.focused_mut().write(bytes);
            }
            Focus::Agent => self.agent_paste(&text),
        }
        self.dirty = true;
    }

    fn on_key(&mut self, client: u64, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            // A key being let go is news only to the program that was sent
            // its press, and only if it asked to hear of such things.
            if let Some(at) = self.keys_down.iter().position(|code| *code == key.code) {
                self.keys_down.swap_remove(at);
                if let Some(tab) = self.tab().filter(|t| t.focus == Focus::Shell && t.panes.focused().copy.is_none()) {
                    if let Some(bytes) = input::encode_key(&key, tab.panes.focused().mode()) {
                        tab.panes.focused().write(bytes);
                    }
                }
            }
            return;
        }
        self.dirty = true;
        if self.ui.overlay.is_some() {
            self.overlay_key(key);
            return;
        }
        if self.ui.prefix.take().is_some() {
            self.prefix_key(client, key);
            return;
        }
        if self.prefix.matches(&key) {
            self.ui.prefix = Some(Instant::now());
            return;
        }
        if self.agent_key.matches(&key) {
            self.toggle_focus();
            return;
        }
        // A key that is Astrum's own without the prefix.
        if let Some(action) = self.keymap.direct(&key) {
            self.do_action(client, action);
            return;
        }
        // From here on the key is for the tab in front: if another terminal
        // had it, it comes over to this one.
        self.own_tab();
        // A pane whose machine is being reached: the key answers `ssh`,
        // or Astrum's question about it.
        if let Some(host) = self.waiting_for() {
            self.far_key(&host, key);
            return;
        }
        // A pane being looked back through takes the keys itself.
        if self.tab().is_some_and(|t| t.focus == Focus::Shell && t.panes.focused().copy.is_some()) {
            // (Typing all the same: a question from the agent that
            // appears now is not to be answered by the next of these keys.)
            if let Some(tab) = self.tabs.get_mut(self.active) {
                tab.agent.last_typed = Some(Instant::now());
            }
            self.copy_key(key);
            return;
        }
        // Enter on a line meant for the agent sends it there, not to the shell.
        if key.code == KeyCode::Enter && key.modifiers.is_empty() && self.tab().is_some_and(|t| t.focus == Focus::Shell) {
            let cfg = &self.cfg.agent;
            let asked = self.tab().and_then(|t| t.panes.focused().typed_line()).and_then(|line| {
                crate::agent::ask::question(&line, &cfg.ask_prefix, cfg.natural_language)
            });
            if let Some(question) = asked {
                self.ask_from_shell(self.active, question);
                return;
            }
        }
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        match tab.focus {
            Focus::Agent => self.agent_key_input(key),
            Focus::Shell => {
                // Shift+PageUp/PageDown page through scrollback like a
                // regular terminal, unless a full-screen program is running.
                let alt_screen = tab.panes.focused_mut().mode().contains(TermMode::ALT_SCREEN);
                if key.modifiers.contains(KeyModifiers::SHIFT) && !alt_screen {
                    let page = tab.panes.focused_mut().rows.saturating_sub(1) as i32;
                    match key.code {
                        KeyCode::PageUp => return tab.panes.focused_mut().scroll(page),
                        KeyCode::PageDown => return tab.panes.focused_mut().scroll(-page),
                        _ => {}
                    }
                }
                if let Some(bytes) = input::encode_key(&key, tab.panes.focused_mut().mode()) {
                    if tab.panes.focused().mode().contains(TermMode::REPORT_EVENT_TYPES) && !self.keys_down.contains(&key.code) && self.keys_down.len() < 32 {
                        self.keys_down.push(key.code);
                    }
                    tab.panes.focused_mut().scroll_to_bottom();
                    tab.panes.focused_mut().clear_selection();
                    tab.panes.focused_mut().note_key(&key);
                    // A question from the agent takes the keyboard from the
                    // shell; keys already on their way must not answer it.
                    tab.agent.last_typed = Some(Instant::now());
                    tab.panes.focused_mut().write(bytes);
                }
            }
        }
    }

    /// A key for the pane the keyboard is looking back through: the search
    /// text while that is being typed, otherwise the cursor.
    fn copy_key(&mut self, key: KeyEvent) {
        use alacritty_terminal::vi_mode::ViMotion as M;
        let Some(tab) = self.tab_mut() else { return };
        let pane = tab.panes.focused_mut();
        let Some(copy) = &mut pane.copy else { return };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let page = pane.rows.saturating_sub(1).max(1) as i32;

        if copy.typing {
            let mut query = copy.find.as_ref().map(|f| f.query.clone()).unwrap_or_default();
            match key.code {
                // Opened as a search, Escape closes it; opened from the
                // cursor, it goes back to the cursor.
                KeyCode::Esc if copy.search_only => pane.leave_copy(),
                KeyCode::Esc => copy.typing = false,
                KeyCode::Enter if copy.search_only => pane.find_step(true),
                KeyCode::Enter => copy.typing = false,
                KeyCode::Tab => {
                    copy.typing = false;
                    copy.search_only = false;
                }
                KeyCode::Up => pane.find_step(true),
                KeyCode::Down => pane.find_step(false),
                KeyCode::PageUp => pane.scroll(page),
                KeyCode::PageDown => pane.scroll(-page),
                KeyCode::Backspace => {
                    query.pop();
                    pane.find_set(query);
                }
                KeyCode::Char('u') if ctrl => pane.find_set(String::new()),
                KeyCode::Char(c) if !ctrl && !alt => {
                    query.push(c);
                    pane.find_set(query);
                }
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Esc if pane.clear_selection() => {}
            KeyCode::Esc | KeyCode::Char('q') => pane.leave_copy(),
            KeyCode::Left | KeyCode::Char('h') => pane.copy_motion(M::Left),
            KeyCode::Right | KeyCode::Char('l') => pane.copy_motion(M::Right),
            KeyCode::Up | KeyCode::Char('k') => pane.copy_motion(M::Up),
            KeyCode::Down | KeyCode::Char('j') => pane.copy_motion(M::Down),
            KeyCode::Char('u') if ctrl => pane.copy_scroll(page / 2 + 1),
            KeyCode::Char('d') if ctrl => pane.copy_scroll(-(page / 2 + 1)),
            KeyCode::Char('v') if ctrl => pane.copy_select(SelectionType::Block),
            KeyCode::PageUp => pane.copy_scroll(page),
            KeyCode::PageDown => pane.copy_scroll(-page),
            KeyCode::Home | KeyCode::Char('0') => pane.copy_motion(M::First),
            KeyCode::End | KeyCode::Char('$') => pane.copy_motion(M::Last),
            KeyCode::Char('^') => pane.copy_motion(M::FirstOccupied),
            KeyCode::Char('w') => pane.copy_motion(M::SemanticRight),
            KeyCode::Char('b') => pane.copy_motion(M::SemanticLeft),
            KeyCode::Char('e') => pane.copy_motion(M::SemanticRightEnd),
            KeyCode::Char('W') => pane.copy_motion(M::WordRight),
            KeyCode::Char('B') => pane.copy_motion(M::WordLeft),
            KeyCode::Char('E') => pane.copy_motion(M::WordRightEnd),
            KeyCode::Char('H') => pane.copy_motion(M::High),
            KeyCode::Char('M') => pane.copy_motion(M::Middle),
            KeyCode::Char('L') => pane.copy_motion(M::Low),
            KeyCode::Char('{') => pane.copy_motion(M::ParagraphUp),
            KeyCode::Char('}') => pane.copy_motion(M::ParagraphDown),
            KeyCode::Char('%') => pane.copy_motion(M::Bracket),
            KeyCode::Char('g') => pane.copy_end(true),
            KeyCode::Char('G') => pane.copy_end(false),
            KeyCode::Char('v') | KeyCode::Char(' ') => pane.copy_select(SelectionType::Simple),
            KeyCode::Char('V') => pane.copy_select(SelectionType::Lines),
            KeyCode::Char('/') | KeyCode::Char('?') => pane.enter_copy(true),
            KeyCode::Char('n') => pane.find_step(true),
            KeyCode::Char('N') => pane.find_step(false),
            KeyCode::Char('y') | KeyCode::Enter => {
                let text = pane.selection_text();
                pane.leave_copy();
                if let Some(text) = text {
                    let n = text.chars().count();
                    self.copy_to_clipboard(text);
                    self.toast(format!("Copied {n} character{}", if n == 1 { "" } else { "s" }));
                }
            }
            _ => {}
        }
    }

    /// Switch between typing in the shell and talking to the agent.
    pub fn toggle_focus(&mut self) {
        let Some(tab) = self.tab_mut() else { return };
        match tab.focus {
            Focus::Shell => {
                tab.focus = Focus::Agent;
                tab.agent.visible = true;
                // (The slash menu offers the skills there are now.)
                let idx = self.active;
                self.skills_for(idx);
            }
            Focus::Agent => tab.focus = Focus::Shell,
        }
        self.dirty = true;
    }

    pub fn hide_panel(&mut self) {
        if let Some(tab) = self.tab_mut() {
            tab.agent.visible = false;
            tab.focus = Focus::Shell;
        }
        self.dirty = true;
    }

    fn prefix_key(&mut self, client: u64, key: KeyEvent) {
        // Prefix twice sends the prefix key itself to the shell.
        if self.prefix.matches(&key) {
            if let Some(tab) = self.tab() {
                if let Some(bytes) = input::encode_key(&key, tab.panes.focused().mode()) {
                    tab.panes.focused().write(bytes);
                }
            }
            return;
        }
        if let Some(action) = self.keymap.after_prefix(&key) {
            self.do_action(client, action);
        }
    }

    /// Carry out one of the things Astrum's keys do. `client` is the
    /// terminal that asked.
    fn do_action(&mut self, client: u64, action: Action) {
        let n = self.tabs.len().max(1);
        match action {
            Action::NewTab => self.new_tab(None),
            Action::NextTab => self.select_tab((self.active + 1) % n),
            Action::PrevTab => self.select_tab((self.active + n - 1) % n),
            Action::Tab(i) => self.select_tab(i as usize),
            Action::SplitRight => self.split_pane(true),
            Action::SplitDown => self.split_pane(false),
            // Sideways, past the last pane, is the next tab.
            Action::FocusRight => {
                if !self.move_focus(panes::Dir::Right) {
                    self.select_tab((self.active + 1) % n);
                }
            }
            Action::FocusLeft => {
                if !self.move_focus(panes::Dir::Left) {
                    self.select_tab((self.active + n - 1) % n);
                }
            }
            Action::FocusUp => {
                self.move_focus(panes::Dir::Up);
            }
            Action::FocusDown => {
                self.move_focus(panes::Dir::Down);
            }
            Action::Zoom => self.toggle_zoom(),
            Action::Close => self.close_pane(),
            Action::MoveTabLeft => self.move_tab(self.active, self.active.saturating_sub(1)),
            Action::MoveTabRight => self.move_tab(self.active, (self.active + 1).min(n - 1)),
            Action::Detach => self.detach_client(client, "detached"),
            Action::DetachAll => self.detach_all(),
            Action::AgentPanel => {
                let showing = self.tab().is_some_and(|t| t.agent.visible);
                if showing { self.hide_panel() } else { self.toggle_focus() }
            }
            Action::Rename => {
                let current = self.tab().and_then(|t| t.name.clone()).unwrap_or_default();
                self.ui.overlay = Some(Overlay::Rename { text: current });
            }
            Action::Explain => self.explain_last(self.active, false),
            Action::Fix => self.explain_last(self.active, true),
            Action::Watch => self.watch_command(self.active, ""),
            Action::AgentHere => self.carry_agent_in(),
            Action::RemoteTab => self.do_remote_prompt(),
            Action::Agents => self.open_agents(),
            Action::Model => self.open_model_picker(),
            Action::Theme => self.open_theme_picker(),
            Action::Help => self.ui.overlay = Some(Overlay::Help),
            Action::Find => {
                if let Some(tab) = self.tab_mut() {
                    tab.focus = Focus::Shell;
                    tab.panes.focused_mut().enter_copy(true);
                }
            }
            Action::CopyMode => {
                if let Some(tab) = self.tab_mut() {
                    tab.focus = Focus::Shell;
                    let pane = tab.panes.focused_mut();
                    if pane.copy.is_some() { pane.leave_copy() } else { pane.enter_copy(false) }
                }
            }
            Action::PageUp | Action::PageDown => {
                if let Some(tab) = self.tab_mut() {
                    let page = tab.panes.focused_mut().rows.saturating_sub(1) as i32;
                    tab.panes.focused_mut().scroll(if action == Action::PageUp { page } else { -page });
                }
            }
            Action::Menu => self.open_main_menu(self.cols),
            Action::Settings => self.open_settings(),
        }
        self.dirty = true;
    }

    /// The walkthrough for someone new: a few cards on what is where.
    pub fn open_walkthrough(&mut self) {
        self.ui.overlay = Some(Overlay::Welcome { step: 0 });
        self.dirty = true;
    }

    /// A button of the walkthrough (or the key that stands for it).
    /// Carry the agent into the shell the pane with the keyboard has
    /// reached: the other end of `ssh`, a container, a `sudo -s`.
    pub(super) fn carry_agent_in(&mut self) {
        let Some(tab) = self.tab_mut() else { return };
        let begun = tab.panes.focused_mut().carry_in();
        match begun {
            Ok(()) => self.toast("Asking the shell there what it is…"),
            Err(why) => self.toast(why),
        }
    }

    /// Say how carrying the agent into a shell went.
    fn say_carried(&mut self, how: Result<String, String>) {
        match how {
            Ok(place) => self.toast(format!("The agent now works in this shell: {place}")),
            Err(why) => self.toast(why),
        }
        self.dirty = true;
    }

    /// Put away whatever is open over the screen. (The walkthrough, put
    /// away like this, has been seen all the same.)
    fn close_overlay(&mut self) {
        if matches!(self.ui.overlay, Some(Overlay::Welcome { .. })) {
            self.walk(Walk::Skip);
        }
        self.ui.overlay = None;
    }

    fn walk(&mut self, button: Walk) {
        let Some(Overlay::Welcome { step }) = &mut self.ui.overlay else { return };
        let done = match button {
            Walk::Back => {
                *step = step.saturating_sub(1);
                false
            }
            Walk::Next if *step + 1 < WALKTHROUGH_STEPS => {
                *step += 1;
                false
            }
            Walk::Next | Walk::Skip | Walk::Model => true,
        };
        if done {
            self.ui.overlay = None;
            // Seen once is enough; the settings can show it again.
            if !self.cfg.state.welcomed {
                self.cfg.state.welcomed = true;
                self.cfg.save_state();
            }
            if button == Walk::Model {
                self.open_model_picker();
            }
        }
        self.dirty = true;
    }

    fn overlay_key(&mut self, key: KeyEvent) {
        let Some(overlay) = &mut self.ui.overlay else { return };
        match overlay {
            Overlay::Help => self.ui.overlay = None,
            Overlay::Agents { .. } => self.agents_key(key),
            Overlay::Settings(_) => self.settings_key(key),
            Overlay::Welcome { .. } => match key.code {
                KeyCode::Left | KeyCode::Backspace | KeyCode::Char('b') => self.walk(Walk::Back),
                KeyCode::Esc | KeyCode::Char('q') => self.walk(Walk::Skip),
                KeyCode::Right | KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Char('n') | KeyCode::Tab => self.walk(Walk::Next),
                _ => {}
            },
            Overlay::Menu(menu) => {
                let count = menu.items.len().max(1);
                match key.code {
                    KeyCode::Up | KeyCode::BackTab => menu.selected = (menu.selected + count - 1) % count,
                    KeyCode::Down | KeyCode::Tab => menu.selected = (menu.selected + 1) % count,
                    KeyCode::Enter => {
                        if let Some((_, action)) = menu.items.get(menu.selected).cloned() {
                            self.run_menu_action(action);
                        }
                    }
                    // In the menu of prefix actions, a key picks its entry.
                    _ if self.keymap.after_prefix(&key).is_some_and(|action| menu.items.iter().any(|(_, a)| *a == MenuAction::Do(action))) => {
                        if let Some(action) = self.keymap.after_prefix(&key) {
                            self.run_menu_action(MenuAction::Do(action));
                        }
                    }
                    _ => self.ui.overlay = None,
                }
            }
            Overlay::Remote { text } => match key.code {
                KeyCode::Esc => self.ui.overlay = None,
                KeyCode::Enter => {
                    let host = text.trim().to_string();
                    self.ui.overlay = None;
                    self.new_remote_tab(&host);
                }
                KeyCode::Backspace => {
                    text.pop();
                }
                KeyCode::Char(c) if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => text.push(c),
                _ => {}
            },
            Overlay::Rename { text } => match key.code {
                KeyCode::Esc => self.ui.overlay = None,
                KeyCode::Enter => {
                    let name = text.trim().to_string();
                    self.ui.overlay = None;
                    if let Some(tab) = self.tab_mut() {
                        tab.name = if name.is_empty() { None } else { Some(name) };
                    }
                }
                KeyCode::Backspace => {
                    text.pop();
                }
                KeyCode::Char(c) if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => text.push(c),
                _ => {}
            },
            Overlay::Picker(p) => {
                let visible = p.visible();
                let count = visible.len();
                if p.kind == PickerKind::Conversation {
                    // Deleting takes two presses of Del; anything else calls it off.
                    let target = visible.get(p.selected).map(|&i| p.items[i].value.clone());
                    let confirmed = key.code == KeyCode::Delete && target.is_some() && p.pending_delete == target;
                    p.pending_delete = None;
                    p.note = CONVERSATIONS_NOTE.into();
                    if let (KeyCode::Delete, Some(id)) = (key.code, target) {
                        if self.tabs.iter().any(|t| t.agent.saved.as_ref().is_some_and(|s| s.id == id)) {
                            p.note = "That conversation is open in a tab; /clear it there first.".into();
                        } else if confirmed {
                            match store::delete(&id) {
                                Ok(()) => {
                                    p.items.retain(|it| it.value != id);
                                    p.selected = p.selected.min(p.visible().len().saturating_sub(1));
                                    p.note = "Deleted.".into();
                                }
                                Err(e) => p.note = format!("Could not delete it: {e}"),
                            }
                        } else {
                            p.note = "Press Del again to delete this conversation for good.".into();
                            p.pending_delete = Some(id);
                        }
                        return;
                    }
                }
                match key.code {
                    KeyCode::Esc => return self.close_picker(false),
                    KeyCode::Enter => return self.close_picker(true),
                    KeyCode::Up | KeyCode::BackTab => p.selected = (p.selected + count.max(1) - 1) % count.max(1),
                    KeyCode::Down | KeyCode::Tab => p.selected = (p.selected + 1) % count.max(1),
                    KeyCode::PageUp => p.selected = p.selected.saturating_sub(8),
                    KeyCode::PageDown => p.selected = (p.selected + 8).min(count.saturating_sub(1)),
                    KeyCode::Backspace => {
                        p.filter.pop();
                        p.selected = 0;
                    }
                    KeyCode::Char(c) if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                        p.filter.push(c);
                        p.selected = 0;
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return self.close_picker(false),
                    _ => {}
                }
                self.preview_picker();
            }
        }
    }

    // ---- pickers ------------------------------------------------------------

    pub fn open_theme_picker(&mut self) {
        let items: Vec<PickItem> =
            self.themes.iter().map(|t| PickItem { label: t.label.to_string(), detail: t.name.to_string(), value: t.name.to_string() }).collect();
        let selected = items.iter().position(|i| i.value == self.theme.name).unwrap_or(0);
        self.ui.overlay = Some(Overlay::Picker(Picker {
            kind: PickerKind::Theme,
            title: "Theme".into(),
            items,
            filter: String::new(),
            selected,
            loading: false,
            note: "Previewing as you move · Enter to keep · Esc to cancel".into(),
            original_theme: Some(self.theme.clone()),
            pending_delete: None,
        }));
        self.dirty = true;
    }

    /// Apply the highlighted theme immediately so browsing is a live preview.
    fn preview_picker(&mut self) {
        let Some(Overlay::Picker(p)) = &self.ui.overlay else { return };
        if p.kind != PickerKind::Theme {
            return;
        }
        let visible = p.visible();
        if let Some(&i) = visible.get(p.selected) {
            if let Some(t) = theme::find(&self.themes, &p.items[i].value) {
                if t.name != self.theme.name {
                    self.theme = t;
                    self.force_redraw = true;
                }
            }
        }
    }

    fn close_picker(&mut self, accept: bool) {
        let Some(Overlay::Picker(p)) = self.ui.overlay.take() else { return };
        self.dirty = true;
        let chosen = if accept { p.visible().get(p.selected).map(|&i| p.items[i].value.clone()) } else { None };
        match p.kind {
            PickerKind::Theme => match chosen.and_then(|name| theme::find(&self.themes, &name)) {
                Some(t) => self.set_theme(t),
                None => {
                    if let Some(orig) = p.original_theme {
                        self.theme = orig;
                        self.force_redraw = true;
                    }
                }
            },
            PickerKind::Model => {
                if let Some(value) = chosen {
                    if let Some((provider, model)) = value.split_once('\u{1f}') {
                        self.set_model(provider, model);
                    }
                }
            }
            PickerKind::Conversation => {
                if let Some(id) = chosen {
                    let _ = self.load_conversation(self.active, &id);
                }
            }
            PickerKind::Mcp => {
                if let Some(name) = chosen {
                    self.mcp_picked(&name);
                }
            }
            PickerKind::Skills => {
                if let Some(name) = chosen {
                    self.skill_picked(&name);
                }
            }
        }
    }

    pub fn set_theme(&mut self, t: Theme) {
        // With one theme for a dark terminal and another for a light one,
        // the choice is for whichever the terminal is now.
        if self.host_is_light() && self.cfg.theme_light.is_some() {
            self.cfg.theme_light = Some(t.name.to_string());
            self.cfg.state.theme_light = Some(t.name.to_string());
        } else {
            self.cfg.theme = t.name.to_string();
            self.cfg.state.theme = Some(t.name.to_string());
        }
        self.theme = t;
        self.force_redraw = true;
        self.dirty = true;
        self.cfg.save_state();
    }

    /// Is the terminal in use showing a light background?
    fn host_is_light(&self) -> bool {
        let lead = self.clients.iter().find(|c| Some(c.id) == self.lead && c.attached);
        lead.and_then(|c| c.colors.1).is_some_and(|[r, g, b]| {
            // Perceived brightness, near enough for telling day from night.
            (r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000 > 140
        })
    }

    /// Put up the theme the settings ask for, for the terminal in use: the
    /// light one if there is one and its background is light. Not while a
    /// theme is being tried out in the list.
    pub(super) fn retheme(&mut self) {
        if matches!(&self.ui.overlay, Some(Overlay::Picker(p)) if p.kind == PickerKind::Theme) {
            return;
        }
        let name = match &self.cfg.theme_light {
            Some(light) if self.host_is_light() => light.clone(),
            _ => self.cfg.theme.clone(),
        };
        let wanted = theme::find(&self.themes, &name).unwrap_or_else(theme::default_theme);
        if wanted.name != self.theme.name || wanted.bg != self.theme.bg || wanted.text != self.theme.text {
            self.theme = wanted;
            self.force_redraw = true;
            self.dirty = true;
        }
    }

    pub fn set_model(&mut self, provider: &str, model: &str) {
        self.cfg.agent.provider = provider.to_string();
        self.cfg.agent.model = model.to_string();
        self.cfg.state.provider = Some(provider.to_string());
        self.cfg.state.model = Some(model.to_string());
        self.cfg.save_state();
        self.toast(format!("Model: {model} ({provider})"));
    }

    // ---- mouse --------------------------------------------------------------

    fn hit_at(&self, col: u16, row: u16) -> Option<(Rect, Hit)> {
        self.ui.hits.iter().rev().find(|(r, _)| col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height).cloned()
    }

    fn on_mouse(&mut self, ev: MouseEvent) {
        let (col, row) = (ev.column, ev.row);
        let hit = self.hit_at(col, row);

        self.ui.pointer = (col, row);
        // A pane that is waiting for its machine shows that machine's
        // word, not its shell: there is nothing under the pointer to
        // select, scroll or pass a click to. (Its buttons are their own.)
        if matches!(hit, Some((_, Hit::Pane))) && self.ui.overlay.is_none() && self.tab().is_some_and(|t| self.cover_of(t.panes.focused()).is_some()) {
            match ev.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(tab) = self.tab_mut() {
                        tab.focus = Focus::Shell;
                    }
                    self.dirty = true;
                }
                MouseEventKind::Down(MouseButton::Right) => self.open_pane_menu((col, row), None),
                _ => {}
            }
            return;
        }
        if let MouseEventKind::Moved = ev.kind {
            // Someone on their way to click the hint line gets the time to reach it.
            if self.ui.prefix.is_some() {
                self.ui.prefix = Some(Instant::now());
            }
            let hover = hit.as_ref().map(|(_, h)| h.clone()).filter(|h| !matches!(h, Hit::Pane | Hit::Panel | Hit::Overlay));
            if hover != self.ui.hover {
                self.ui.hover = hover;
                self.dirty = true;
            }
            // Over a link, say where it goes.
            let here = matches!(hit, Some((_, Hit::Pane))) && self.ui.overlay.is_none() && self.foreign_size().is_none();
            let link = if here { self.link_under(col, row) } else { None };
            if link != self.ui.link {
                self.ui.link = link;
                self.dirty = true;
            }
            // Pass plain motion to programs that asked for it. (Not over a
            // tab laid out for another terminal: the cells do not line up.)
            if let (Some((rect, Hit::Pane)), Some(tab)) = (&hit, self.tab().filter(|_| self.foreign_size().is_none())) {
                if tab.panes.focused().mode().contains(TermMode::MOUSE_MOTION) {
                    if let Some(bytes) = input::encode_mouse(&ev, col - rect.x, row - rect.y, tab.panes.focused().mode()) {
                        tab.panes.focused().write(bytes);
                    }
                }
            }
            return;
        }
        self.dirty = true;

        // An in-progress drag keeps its target even when the pointer leaves it.
        if let Some(drag) = &self.ui.drag {
            match ev.kind {
                MouseEventKind::Drag(MouseButton::Left) => {
                    match drag {
                        Drag::Divider if self.panel_beside() => {
                            let cols = self.cols.max(1);
                            let right = cols.saturating_sub(col + 1);
                            self.ui.side_share = (((right as u32 * 1000 + cols as u32 / 2) / cols as u32) as u16).clamp(200, 700);
                        }
                        Drag::Divider => {
                            let rows = self.rows.max(1);
                            let below = rows.saturating_sub(row + 1);
                            self.ui.panel_pct = ((below as u32 * 100 / rows as u32) as u16).clamp(20, 80);
                        }
                        Drag::Select => {
                            let pane_rect = self.focused_rect();
                            if let Some(tab) = self.tab_mut() {
                                // Dragging past the edge scrolls the history.
                                if row < pane_rect.y {
                                    tab.panes.focused_mut().scroll(1);
                                } else if row >= pane_rect.y + pane_rect.height {
                                    tab.panes.focused_mut().scroll(-1);
                                }
                                let c = col.saturating_sub(pane_rect.x).min(pane_rect.width.saturating_sub(1));
                                let r = row.saturating_sub(pane_rect.y).min(pane_rect.height.saturating_sub(1));
                                tab.panes.focused_mut().update_selection(c, r);
                            }
                        }
                        Drag::Forward => self.forward_mouse(&ev),
                        Drag::Tab(from) => {
                            // Over another tab: the carried tab takes its place.
                            let from = *from;
                            if let Some((_, Hit::Tab(to) | Hit::CloseTab(to))) = self.hit_at(col, row) {
                                if to != from {
                                    self.move_tab(from, to);
                                    self.ui.drag = Some(Drag::Tab(to));
                                }
                            }
                        }
                        Drag::Border(split) => {
                            let (split, area) = (*split, self.panes_area());
                            if let Some(tab) = self.tab_mut() {
                                tab.panes.drag_divider(area, split, col, row);
                            }
                        }
                    }
                    return;
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    match self.ui.drag.take() {
                        Some(Drag::Select) => {
                            let text = self.tab().and_then(|t| t.panes.focused().selection_text());
                            match text {
                                Some(text) if self.cfg.copy_on_select => {
                                    let n = text.chars().count();
                                    self.copy_to_clipboard(text);
                                    self.toast(format!("Copied {n} character{}", if n == 1 { "" } else { "s" }));
                                }
                                Some(_) => {}
                                None => {
                                    if let Some(tab) = self.tab_mut() {
                                        tab.panes.focused_mut().clear_selection();
                                    }
                                }
                            }
                        }
                        Some(Drag::Forward) => self.forward_mouse(&ev),
                        _ => {}
                    }
                    return;
                }
                _ => {}
            }
        }

        let Some((rect, hit)) = hit else { return };
        match ev.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = matches!(ev.kind, MouseEventKind::ScrollUp);
                match hit {
                    Hit::Pane => self.scroll_pane(&ev, col - rect.x, row - rect.y, up),
                    // The wheel scrolls the pane it is over, without moving the keyboard there.
                    Hit::OtherPane(id) => {
                        if let Some(pane) = self.tab_mut().and_then(|t| t.panes.get_mut(id)) {
                            pane.scroll(if up { 3 } else { -3 });
                        }
                    }
                    Hit::Panel | Hit::Choice(_) | Hit::Slash(_) | Hit::Divider => {
                        if let Some(tab) = self.tab_mut() {
                            let a = &mut tab.agent;
                            // Over a diff waiting for approval, the wheel moves the diff.
                            match a.approval.as_mut().filter(|ap| ap.edit.is_some()) {
                                Some(ap) if up => ap.scroll = ap.scroll.saturating_sub(3),
                                Some(ap) => ap.scroll = (ap.scroll + 3).min(ap.preview.len().saturating_sub(1)),
                                None => a.scroll = if up { a.scroll + 3 } else { a.scroll.saturating_sub(3) },
                            }
                        }
                    }
                    // The wheel goes through the settings a row at a time,
                    // which brings into view what a short window cuts off.
                    Hit::Setting(_) | Hit::SettingsClose | Hit::Overlay if matches!(self.ui.overlay, Some(Overlay::Settings(_))) => {
                        let rows = settings::rows().len().max(1);
                        if let Some(Overlay::Settings(view)) = &mut self.ui.overlay {
                            view.selected = if up { view.selected.saturating_sub(1) } else { (view.selected + 1).min(rows - 1) };
                        }
                    }
                    Hit::PickerItem(_) | Hit::Overlay => {
                        if let Some(Overlay::Picker(p)) = &mut self.ui.overlay {
                            let count = p.visible().len().max(1);
                            p.selected = if up { p.selected.saturating_sub(1) } else { (p.selected + 1).min(count - 1) };
                            self.preview_picker();
                        }
                    }
                    Hit::Tab(_) | Hit::CloseTab(_) | Hit::NewTab => {
                        let n = self.tabs.len().max(1);
                        self.select_tab(if up { (self.active + n - 1) % n } else { (self.active + 1) % n });
                    }
                    _ => {}
                }
            }
            MouseEventKind::Down(MouseButton::Middle) => {
                if let Hit::Tab(i) | Hit::CloseTab(i) = hit {
                    self.remove_tab(i);
                } else if hit == Hit::Pane {
                    self.forward_or_ignore(&ev, &rect);
                }
            }
            MouseEventKind::Down(MouseButton::Right) => {
                if self.ui.overlay.is_some() {
                    self.close_overlay();
                    return;
                }
                match hit {
                    Hit::Tab(i) | Hit::CloseTab(i) => self.open_tab_menu(i, (col, row)),
                    // A program that uses the mouse gets the click (hold Shift for the menu).
                    Hit::Pane if self.tab().is_some_and(|t| t.panes.focused().wants_mouse()) && !ev.modifiers.contains(KeyModifiers::SHIFT) => {
                        self.forward_or_ignore(&ev, &rect);
                    }
                    Hit::Pane => {
                        let link = self.link_under(col, row).map(|l| l.uri);
                        self.open_pane_menu((col, row), link);
                    }
                    Hit::OtherPane(id) => {
                        self.focus_pane(id);
                        let link = self.link_under(col, row).map(|l| l.uri);
                        self.open_pane_menu((col, row), link);
                    }
                    _ => {}
                }
            }
            MouseEventKind::Down(MouseButton::Left) => self.on_click(ev, rect, hit),
            MouseEventKind::Up(_) => {
                if hit == Hit::Pane {
                    self.forward_or_ignore(&ev, &rect);
                }
            }
            _ => {}
        }
    }

    fn forward_mouse(&mut self, ev: &MouseEvent) {
        let pane_rect = self.focused_rect();
        self.forward_or_ignore(ev, &pane_rect);
    }

    fn forward_or_ignore(&mut self, ev: &MouseEvent, rect: &Rect) {
        let Some(tab) = self.tab() else { return };
        let mode = tab.panes.focused().mode();
        if !tab.panes.focused().wants_mouse() || ev.modifiers.contains(KeyModifiers::SHIFT) {
            return;
        }
        let col = ev.column.saturating_sub(rect.x).min(rect.width.saturating_sub(1));
        let row = ev.row.saturating_sub(rect.y).min(rect.height.saturating_sub(1));
        if let Some(bytes) = input::encode_mouse(ev, col, row, mode) {
            tab.panes.focused().write(bytes);
        }
    }

    fn scroll_pane(&mut self, ev: &MouseEvent, col: u16, row: u16, up: bool) {
        let Some(tab) = self.tab_mut() else { return };
        let mode = tab.panes.focused_mut().mode();
        if tab.panes.focused().wants_mouse() && !ev.modifiers.contains(KeyModifiers::SHIFT) {
            if let Some(bytes) = input::encode_mouse(ev, col, row, mode) {
                tab.panes.focused_mut().write(bytes);
            }
        } else if mode.contains(TermMode::ALT_SCREEN) {
            // Pagers and editors without mouse support: turn the wheel into arrows.
            let key = KeyEvent::new(if up { KeyCode::Up } else { KeyCode::Down }, KeyModifiers::NONE);
            if let Some(bytes) = input::encode_key(&key, mode) {
                tab.panes.focused_mut().write(bytes.repeat(3));
            }
        } else {
            tab.panes.focused_mut().scroll(if up { 3 } else { -3 });
        }
    }

    fn on_click(&mut self, ev: MouseEvent, rect: Rect, hit: Hit) {
        // Clicking outside a menu closes it.
        if self.ui.overlay.is_some() && !matches!(hit, Hit::PickerItem(_) | Hit::MenuItem(_) | Hit::Overlay | Hit::Setting(_) | Hit::SettingsClose | Hit::Walk(_) | Hit::AgentRow(_) | Hit::AgentYes(_)) {
            match &self.ui.overlay {
                Some(Overlay::Picker(_)) => self.close_picker(false),
                _ => self.close_overlay(),
            }
            return;
        }
        // After the prefix key, a click on the hint line does what the key would;
        // a click anywhere else lets go of the prefix and does what it always does.
        if self.ui.prefix.take().is_some() {
            if let Hit::Do(action) = hit {
                self.act(action);
                return;
            }
        }
        match hit {
            Hit::Tab(i) => {
                self.select_tab(i);
                // Held and moved, the tab comes along.
                self.ui.drag = Some(Drag::Tab(i));
            }
            Hit::MenuItem(i) => {
                if let Some(Overlay::Menu(menu)) = &self.ui.overlay {
                    if let Some((_, action)) = menu.items.get(i).cloned() {
                        self.run_menu_action(action);
                    }
                }
            }
            Hit::CloseTab(i) => self.remove_tab(i),
            Hit::NewTab => self.new_tab(None),
            Hit::Mode => self.toggle_focus(),
            Hit::Autonomy => self.cycle_autonomy(),
            Hit::Model => self.open_model_picker(),
            Hit::Help => self.ui.overlay = Some(Overlay::Help),
            Hit::Menu => self.open_main_menu(ev.column),
            Hit::Setting(row) => self.settings_click(row),
            Hit::SettingsClose => self.ui.overlay = None,
            Hit::Walk(button) => self.walk(button),
            Hit::FindBox | Hit::FindOlder | Hit::FindNewer | Hit::CopyClose => {
                let Some(tab) = self.tab_mut() else { return };
                tab.focus = Focus::Shell;
                let pane = tab.panes.focused_mut();
                match hit {
                    Hit::FindBox => pane.enter_copy(true),
                    Hit::FindOlder => pane.find_step(true),
                    Hit::FindNewer => pane.find_step(false),
                    _ => pane.leave_copy(),
                }
            }
            Hit::Do(action) => self.act(action),
            Hit::AgentRow(idx) => self.go_to_agent(idx),
            Hit::AgentYes(idx) => self.yes_from_overview(idx),
            Hit::Far(button) => {
                // (Wherever the keyboard is: the button is the pane's.)
                let host = self.tab().and_then(|t| self.cover_of(t.panes.focused())).map(|far| far.host.clone());
                if let Some(host) = host {
                    self.far_button(&host, button);
                }
            }
            Hit::Explain => self.explain_last(self.active, false),
            Hit::Fix => self.explain_last(self.active, true),
            Hit::Watch => self.watch_command(self.active, ""),
            Hit::PanelClose => self.hide_panel(),
            Hit::Divider => self.ui.drag = Some(Drag::Divider),
            Hit::Border(split) => self.ui.drag = Some(Drag::Border(split)),
            Hit::OtherPane(id) => {
                self.focus_pane(id);
                if let Some(tab) = self.tab_mut() {
                    tab.focus = Focus::Shell;
                }
            }
            Hit::Panel => {
                if let Some(tab) = self.tab_mut() {
                    tab.focus = Focus::Agent;
                }
            }
            Hit::Choice(i) => {
                if let Some(tab) = self.tab_mut() {
                    tab.focus = Focus::Agent;
                }
                self.choose_approval(i);
            }
            Hit::Slash(i) => {
                if let Some(tab) = self.tab_mut() {
                    tab.agent.slash_sel = i;
                }
                self.agent_submit();
            }
            Hit::PickerItem(i) => {
                if let Some(Overlay::Picker(p)) = &mut self.ui.overlay {
                    p.selected = i;
                }
                self.close_picker(true);
            }
            Hit::Overlay => {
                if matches!(self.ui.overlay, Some(Overlay::Help)) {
                    self.ui.overlay = None;
                }
            }
            // Ctrl+click on a link opens it.
            Hit::Pane if ev.modifiers.contains(KeyModifiers::CONTROL) && self.link_under(ev.column, ev.row).is_some() => {
                if let Some(link) = self.link_under(ev.column, ev.row) {
                    self.open_link(&link.uri);
                }
            }
            Hit::Pane => {
                let (col, row) = (ev.column - rect.x, ev.row - rect.y);
                let now = Instant::now();
                let clicks = match self.ui.last_click {
                    Some((t, c, r, n)) if now.duration_since(t) < Duration::from_millis(400) && c == ev.column && r == ev.row => n % 3 + 1,
                    _ => 1,
                };
                self.ui.last_click = Some((now, ev.column, ev.row, clicks));
                let Some(tab) = self.tab_mut() else { return };
                tab.focus = Focus::Shell;
                let mode = tab.panes.focused_mut().mode();
                if tab.panes.focused().wants_mouse() && !ev.modifiers.contains(KeyModifiers::SHIFT) {
                    if let Some(bytes) = input::encode_mouse(&ev, col, row, mode) {
                        tab.panes.focused_mut().write(bytes);
                    }
                    self.ui.drag = Some(Drag::Forward);
                } else {
                    let ty = match clicks {
                        2 => SelectionType::Semantic,
                        3 => SelectionType::Lines,
                        _ => SelectionType::Simple,
                    };
                    tab.panes.focused_mut().clear_selection();
                    // Looking back with the keyboard, a click also moves its cursor.
                    if let Some(copy) = &mut tab.panes.focused_mut().copy {
                        copy.typing = false;
                        tab.panes.focused_mut().copy_goto(col, row);
                    }
                    tab.panes.focused_mut().start_selection(col, row, ty);
                    if clicks > 1 {
                        tab.panes.focused_mut().update_selection(col, row);
                    }
                    self.ui.drag = Some(Drag::Select);
                }
            }
        }
    }

    // ---- rendering ----------------------------------------------------------

    /// Draw every attached terminal its view. True if any was sent anything.
    fn render(&mut self) -> bool {
        self.dirty = false;
        if self.clients.iter().all(|c| !c.attached) || self.tabs.is_empty() {
            return false;
        }
        // Programs that ask for the terminal's colors are told the current ones.
        let (fg, bg) = self.term_colors();
        // And those that ask about pictures, what the terminal in use can show.
        let lead = self.clients.iter().find(|c| Some(c.id) == self.lead && c.attached).or_else(|| self.clients.iter().find(|c| c.attached));
        let (caps, cell_px) = lead.filter(|_| self.cfg.images).map(|c| (c.images, c.cell_px)).unwrap_or_default();
        for pane in self.tabs.iter_mut().flat_map(|t| t.panes.iter_mut()) {
            pane.default_fg = fg.unwrap_or(pane.default_fg);
            pane.default_bg = bg.unwrap_or(pane.default_bg);
            pane.set_image_support(caps, cell_px);
        }
        let repaint_all = std::mem::take(&mut self.force_redraw);
        // Everything is to be drawn afresh (new colors, most often):
        // nothing kept of how a pane was last drawn can be used.
        if repaint_all {
            for pane in self.tabs.iter_mut().flat_map(|t| t.panes.iter_mut()) {
                pane.redraw();
            }
        }

        // Each attached terminal is drawn its own view: the terminal in the
        // lead first, so that a tab two of them are looking at for the
        // first time is sized for that one.
        let mut order: Vec<u64> = self.clients.iter().filter(|c| c.attached).map(|c| c.id).collect();
        order.sort_by_key(|id| Some(*id) != self.lead);
        let mut dead = Vec::new();
        let mut sent = false;
        for id in order {
            // A terminal that has not taken the last frames yet (a slow
            // connection, a window that is busy) is not sent more to
            // queue up behind them: it gets one frame, of how things are
            // by then, when it has caught up.
            if let Some(c) = self.clients.iter_mut().find(|c| c.id == id).filter(|c| c.unsent.load(Ordering::SeqCst) >= MAX_UNSENT) {
                c.held_back = true;
                c.fresh |= repaint_all;
                continue;
            }
            self.load_view(id);
            if self.loaded == Some(id) {
                match self.render_view(id, repaint_all) {
                    Sent::Frame => sent = true,
                    Sent::Nothing => {}
                    Sent::Gone => dead.push(id),
                }
            }
        }
        if dead.is_empty() {
            if let Some(lead) = self.lead {
                self.load_view(lead);
            }
        } else {
            for id in dead {
                self.drop_client(id);
            }
            self.recompute_size();
        }        sent
    }

    /// Draw the loaded view and send its terminal what changed, if anything did.
    fn render_view(&mut self, id: u64, repaint_all: bool) -> Sent {
        self.ui.spinner = self.spin;
        // What is under a pointer that has not moved may have changed.
        if self.ui.link.is_some() {
            let (col, row) = self.ui.pointer;
            self.ui.link = if self.ui.overlay.is_none() { self.link_under(col, row) } else { None };
        }
        // The tab in front is sized for one terminal at a time: the one
        // that last used it, as long as it is still looking at it. Keep
        // each of its shells the size of the space it is drawn in.
        let area = self.panes_area();
        let idx = self.active;
        let held = self.tabs[idx].owner.is_some_and(|owner| owner != id && self.client_shows(owner, idx));
        if !held {
            let tab = &mut self.tabs[idx];
            tab.owner = Some(id);
            tab.sized = (area.width, area.height);
            for (pane_id, rect) in tab.panes.place(area).0 {
                if let Some(pane) = tab.panes.get_mut(pane_id) {
                    pane.resize(rect.width, rect.height);
                    pane.find_refresh();
                }
            }
        }

        let view_area = Rect::new(0, 0, self.cols, self.rows);
        let began = Instant::now();
        let mut frame = Buffer::empty(view_area);
        let cursor = ui::draw(self, &mut frame);
        let drawn = Instant::now();
        self.work.frames += 1;
        self.work.draw_us += drawn.duration_since(began).as_micros() as u64;
        let wanted = self.settle_pictures(id, &mut frame);
        let kept: std::collections::HashSet<u32> = self.tabs.iter().flat_map(|t| t.panes.iter()).flat_map(|p| p.images.iter().map(|i| i.id)).collect();

        let Some(c) = self.clients.iter_mut().find(|c| c.id == id) else { return Sent::Gone };
        if !c.truecolor {
            reduce_to_256_colors(&mut frame);
        }
        let mut out: Vec<u8> = Vec::with_capacity(8192);
        // Paint inside a synchronized update with the cursor hidden so
        // the host terminal never shows a half-drawn frame.
        out.extend_from_slice(b"\x1b[?2026h\x1b[?25l");
        let cleared = repaint_all || c.fresh || c.prev.area != view_area;
        if cleared {
            out.extend_from_slice(b"\x1b[0m\x1b[2J");
            c.pictures.cleared(c.images.kitty, &mut out);
            c.prev = Buffer::empty(view_area);
            c.fresh = false;
            c.cursor_shape = None;
        }
        c.pictures.before(&wanted, &mut c.prev);
        let before = out.len();
        {
            use ratatui::backend::{Backend, CrosstermBackend};
            let updates = changed_cells(&c.prev, &frame);
            if !updates.is_empty() {
                let mut backend = CrosstermBackend::new(&mut out);
                let _ = backend.draw(updates.into_iter());
                let _ = Backend::flush(&mut backend);
            }
        }
        c.pictures.after(wanted, |picture| kept.contains(&picture), &mut out);
        let cursor = cursor.filter(|cur| cur.x < c.cols && cur.y < c.rows);
        let at = cursor.as_ref().map(|cur| (cur.x, cur.y));
        // Nothing on this terminal's screen is to change, and its cursor
        // is where it was: there is nothing to send it. (A key that has
        // gone to a shell changes nothing until the shell answers.)
        if !cleared && out.len() == before && at == c.cursor_at && cursor.as_ref().is_none_or(|cur| c.cursor_shape == Some(cur.shape)) {
            c.prev = frame;
            return Sent::Nothing;
        }
        c.cursor_at = at;
        if let Some(cur) = cursor.as_ref() {
            if c.cursor_shape != Some(cur.shape) {
                out.extend_from_slice(format!("\x1b[{} q", cur.shape).as_bytes());
                c.cursor_shape = Some(cur.shape);
            }
            out.extend_from_slice(format!("\x1b[{};{}H\x1b[?25h", cur.y + 1, cur.x + 1).as_bytes());
        }
        out.extend_from_slice(b"\x1b[?2026l");
        c.prev = frame;
        self.work.sent += out.len() as u64;
        self.work.send_us += drawn.elapsed().as_micros() as u64;
        c.unsent.fetch_add(1, Ordering::SeqCst);
        if c.tx.send(ServerFrame::Output(out)).is_ok() { Sent::Frame } else { Sent::Gone }
    }
}

/// The cells to send a terminal that shows `prev`, for it to show `next`.
///
/// Rows that are the same in both are passed over whole, which is most of
/// them in most frames; the rest are compared cell by cell, as the drawing
/// library does (it knows what a double-width character does to the cell
/// after it). Rows can be taken one at a time because no character runs
/// from one into the next: see `ui::pair_wide_cells`.
fn changed_cells<'a>(prev: &Buffer, next: &'a Buffer) -> Vec<(u16, u16, &'a ratatui::buffer::Cell)> {
    let width = next.area.width as usize;
    if prev.area != next.area || width == 0 {
        return prev.diff(next);
    }
    let changed: Vec<usize> = next.content.chunks(width).zip(prev.content.chunks(width)).enumerate().filter(|(_, (new, old))| new != old).map(|(row, _)| row).collect();
    // With much of the screen changed there is nothing to save by it.
    if changed.len() * 3 > next.area.height as usize {
        return prev.diff(next);
    }
    let mut cells = Vec::new();
    for row in changed {
        let area = Rect::new(next.area.x, next.area.y + row as u16, next.area.width, 1);
        let line = |of: &Buffer| Buffer { area, content: of.content[row * width..(row + 1) * width].to_vec() };
        for (x, y, _) in line(prev).diff(&line(next)) {
            cells.push((x, y, &next[(x, y)]));
        }
    }
    cells
}

/// What came of drawing a terminal its view.
enum Sent {
    /// A frame went to it.
    Frame,
    /// Nothing it shows had changed.
    Nothing,
    /// It is no longer there.
    Gone,
}

/// The closest entry in the xterm 256-color palette to an RGB color.
fn nearest_256(r: u8, g: u8, b: u8) -> u8 {
    const LEVELS: [i32; 6] = [0, 95, 135, 175, 215, 255];
    let step = |v: u8| (0..6).min_by_key(|&i| (LEVELS[i] - v as i32).abs()).unwrap_or(0);
    let (ri, gi, bi) = (step(r), step(g), step(b));
    let dist = |cr: i32, cg: i32, cb: i32| (cr - r as i32).pow(2) + (cg - g as i32).pow(2) + (cb - b as i32).pow(2);
    let cube = dist(LEVELS[ri], LEVELS[gi], LEVELS[bi]);
    // The 24-step gray ramp often matches dark, low-saturation colors better.
    let avg = (r as i32 + g as i32 + b as i32) / 3;
    let gray_i = ((avg - 8 + 5) / 10).clamp(0, 23);
    let gray_v = 8 + 10 * gray_i;
    if dist(gray_v, gray_v, gray_v) < cube { (232 + gray_i) as u8 } else { (16 + 36 * ri + 6 * gi + bi) as u8 }
}

/// For terminals without 24-bit color: map every RGB color to the 256-color palette.
fn reduce_to_256_colors(frame: &mut Buffer) {
    use ratatui::style::Color;
    for cell in frame.content.iter_mut() {
        if let Color::Rgb(r, g, b) = cell.fg {
            cell.fg = Color::Indexed(nearest_256(r, g, b));
        }
        if let Color::Rgb(r, g, b) = cell.bg {
            cell.bg = Color::Indexed(nearest_256(r, g, b));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Style};

    #[test]
    fn palette_mapping() {
        assert_eq!(nearest_256(0, 0, 0), 16);
        assert_eq!(nearest_256(255, 255, 255), 231);
        assert_eq!(nearest_256(255, 0, 0), 196);
        assert_eq!(nearest_256(95, 135, 175), 67);
        // Near-black theme backgrounds land on the gray ramp, not on pure black.
        assert_eq!(nearest_256(0x1b, 0x1d, 0x2e), 235);
        assert!((232..=255).contains(&nearest_256(128, 128, 128)));
    }

    #[test]
    fn frames_are_reduced_for_terminals_without_full_color() {
        let mut frame = Buffer::empty(Rect::new(0, 0, 6, 3));
        frame.set_string(0, 0, "abcdef", Style::default().fg(Color::Rgb(255, 0, 0)).bg(Color::Indexed(4)));
        frame.set_string(0, 2, "bottom", Style::default());

        let mut reduced = frame.clone();
        reduce_to_256_colors(&mut reduced);
        assert_eq!(reduced.cell((0, 0)).unwrap().fg, Color::Indexed(196));
        assert_eq!(reduced.cell((0, 0)).unwrap().bg, Color::Indexed(4));
        assert_eq!(reduced.cell((0, 2)).unwrap().fg, Color::Reset);
    }
}
