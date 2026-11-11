//! The session server: owns the shells, the agent conversations and the
//! whole interface, and streams rendered frames to whichever clients are
//! attached. It keeps running when every client has gone away, which is what
//! makes sessions persistent.

mod agent_ops;
pub mod session;

use crate::agent::AgentEv;
use crate::config::{Chord, Config};
use crate::input;
use crate::pane::{Pane, PaneEvent, SpawnOpts};
use crate::paths;
use crate::proto::{self, ClientMsg, Hello, ServerFrame, ServerMsg, SessionInfo};
use crate::theme::{self, Theme};
use crate::ui;
use alacritty_terminal::selection::SelectionType;
use alacritty_terminal::term::TermMode;
use anyhow::{Context, Result};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use session::AgentSession;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};
use tokio::net::UnixListener;
use tokio::sync::mpsc;

const MIN_FRAME: Duration = Duration::from_millis(6);
const TICK: Duration = Duration::from_millis(80);
const PREFIX_TIMEOUT: Duration = Duration::from_secs(3);

pub enum Ev {
    Join { id: u64, hello: Hello, tx: mpsc::UnboundedSender<ServerFrame> },
    Client { id: u64, msg: ClientMsg },
    Gone { id: u64 },
    Pty { pane: u64, data: Vec<u8> },
    PtyExit { pane: u64 },
    Agent { tab: u64, turn: u64, ev: AgentEv },
    Models { result: Vec<(String, String)>, errors: Vec<String> },
    Shutdown,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Focus {
    Shell,
    Agent,
}

pub struct Tab {
    pub id: u64,
    pub pane: Pane,
    pub name: Option<String>,
    pub agent: AgentSession,
    pub focus: Focus,
    /// Something happened here while another tab was in front.
    pub activity: bool,
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
    Pane,
    Divider,
    PanelClose,
    Panel,
    Choice(usize),
    Slash(usize),
    PickerItem(usize),
    Overlay,
}

#[derive(Clone, Copy, PartialEq)]
pub enum PickerKind {
    Model,
    Theme,
}

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
}

enum Drag {
    Select,
    Divider,
    Forward,
}

pub struct Ui {
    pub prefix: Option<Instant>,
    pub overlay: Option<Overlay>,
    pub hits: Vec<(Rect, Hit)>,
    pub hover: Option<Hit>,
    drag: Option<Drag>,
    pub toast: Option<(String, Instant)>,
    /// Share of the window height given to the agent panel.
    pub panel_pct: u16,
    last_click: Option<(Instant, u16, u16, u8)>,
    pub spinner: usize,
}

struct ClientConn {
    id: u64,
    tx: mpsc::UnboundedSender<ServerFrame>,
    cols: u16,
    rows: u16,
    attached: bool,
}

pub struct Server {
    pub session: String,
    pub cfg: Config,
    pub theme: Theme,
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub cols: u16,
    pub rows: u16,
    pub ui: Ui,
    pub prefix: Chord,
    pub agent_key: Chord,
    pub http: reqwest::Client,
    clients: Vec<ClientConn>,
    next_id: u64,
    pub tx: mpsc::Sender<Ev>,
    prev: Buffer,
    force_redraw: bool,
    dirty: bool,
    quit: Option<String>,
    shell_dir: PathBuf,
    started: SystemTime,
    born: Instant,
    last_cursor: Option<(u16, u16, u8)>,
}

/// Write the embedded shell-integration scripts where shells can source them.
fn install_shell_files() -> Result<PathBuf> {
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

pub async fn serve(session: String) -> Result<()> {
    let sock = paths::socket_path(&session)?;
    // A live server answers on its socket; anything else is a stale file.
    if tokio::net::UnixStream::connect(&sock).await.is_ok() {
        anyhow::bail!("session `{session}` is already running");
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

    let (tx, mut rx) = mpsc::channel::<Ev>(2048);

    // Accept loop: one reader and one writer task per client.
    let accept_tx = tx.clone();
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
                if tx.send(Ev::Join { id, hello, tx: out_tx }).await.is_err() {
                    return;
                }
                let writer = tokio::spawn(async move {
                    while let Some(frame) = out_rx.recv().await {
                        let last = matches!(frame, ServerFrame::Msg(ServerMsg::Bye { .. }));
                        if proto::write_server_frame(&mut wr, &frame).await.is_err() || last {
                            break;
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
        let _ = sig_tx.send(Ev::Shutdown).await;
    });

    let theme = theme::by_name(&cfg.theme).unwrap_or_else(theme::default_theme);
    let prefix = Chord::parse(&cfg.prefix).unwrap_or(Chord { code: KeyCode::Char('b'), mods: KeyModifiers::CONTROL });
    let agent_key = Chord::parse(&cfg.agent_key).unwrap_or(Chord { code: KeyCode::Char(' '), mods: KeyModifiers::CONTROL });
    let mut server = Server {
        session,
        cfg,
        theme,
        tabs: Vec::new(),
        active: 0,
        cols: 80,
        rows: 24,
        ui: Ui {
            prefix: None,
            overlay: None,
            hits: Vec::new(),
            hover: None,
            drag: None,
            toast: None,
            panel_pct: 45,
            last_click: None,
            spinner: 0,
        },
        prefix,
        agent_key,
        http: crate::agent::llm::http_client(),
        clients: Vec::new(),
        next_id: 1,
        tx,
        prev: Buffer::empty(Rect::new(0, 0, 80, 24)),
        force_redraw: true,
        dirty: true,
        quit: None,
        shell_dir,
        started: SystemTime::now(),
        born: Instant::now(),
        last_cursor: None,
    };
    if let Some(w) = warnings.first() {
        server.toast(format!("Config problem: {w}"));
    }

    let mut last_render = Instant::now() - Duration::from_secs(1);
    loop {
        let wait = if server.dirty {
            MIN_FRAME.saturating_sub(last_render.elapsed())
        } else if server.animating() {
            TICK
        } else {
            Duration::from_secs(3600)
        };
        tokio::select! {
            ev = rx.recv() => {
                let Some(ev) = ev else { break };
                server.handle(ev);
                // Drain a burst before drawing, but never starve the screen.
                let began = Instant::now();
                while began.elapsed() < Duration::from_millis(12) {
                    match rx.try_recv() {
                        Ok(ev) => server.handle(ev),
                        Err(_) => break,
                    }
                }
            }
            _ = tokio::time::sleep(wait) => {}
        }
        server.tick();
        if server.quit.is_some() {
            break;
        }
        if server.dirty && last_render.elapsed() >= MIN_FRAME {
            server.render();
            last_render = Instant::now();
        }
    }

    let reason = server.quit.take().unwrap_or_else(|| "server stopped".into());
    for c in &server.clients {
        let _ = c.tx.send(ServerFrame::Msg(ServerMsg::Bye { reason: reason.clone() }));
    }
    server.tabs.clear();
    let _ = std::fs::remove_file(&sock);
    // Give writer tasks a moment to flush the goodbye.
    tokio::time::sleep(Duration::from_millis(60)).await;
    Ok(())
}

impl Server {
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

    fn animating(&self) -> bool {
        self.ui.prefix.is_some()
            || self.ui.toast.is_some()
            || matches!(&self.ui.overlay, Some(Overlay::Picker(p)) if p.loading)
            || self.tabs.iter().any(|t| t.agent.busy || t.agent.wait.is_some() || t.pane.sync_pending())
            || (self.tabs.is_empty() && self.clients.iter().all(|c| !c.attached))
    }

    fn tick(&mut self) {
        let now = Instant::now();
        if self.ui.prefix.is_some_and(|t| now.duration_since(t) > PREFIX_TIMEOUT) {
            self.ui.prefix = None;
            self.dirty = true;
        }
        if self.ui.toast.as_ref().is_some_and(|(_, t)| now.duration_since(*t) > Duration::from_secs(4)) {
            self.ui.toast = None;
            self.dirty = true;
        }
        for tab in &mut self.tabs {
            if tab.pane.flush_stale_sync() {
                self.dirty = true;
            }
        }
        self.agent_tick(now);
        if self.tabs.iter().any(|t| t.agent.busy && t.agent.visible)
            || matches!(&self.ui.overlay, Some(Overlay::Picker(p)) if p.loading)
        {
            let frame = (self.born.elapsed().as_millis() / 80) as usize;
            if frame != self.ui.spinner {
                self.ui.spinner = frame;
                self.dirty = true;
            }
        }
        // A server nobody ever attached to should not linger.
        if self.tabs.is_empty() && self.clients.iter().all(|c| !c.attached) && self.born.elapsed() > Duration::from_secs(20) {
            self.quit = Some("no client attached".into());
        }
    }

    fn handle(&mut self, ev: Ev) {
        match ev {
            Ev::Join { id, hello, tx } => self.on_join(id, hello, tx),
            Ev::Client { id, msg } => self.on_client(id, msg),
            Ev::Gone { id } => {
                self.clients.retain(|c| c.id != id);
                self.recompute_size();
            }
            Ev::Pty { pane, data } => self.on_pty(pane, &data),
            Ev::PtyExit { pane } => {
                if let Some(idx) = self.tab_index(pane) {
                    self.remove_tab(idx);
                }
            }
            Ev::Agent { tab, turn, ev } => self.on_agent_ev(tab, turn, ev),
            Ev::Models { result, errors } => self.on_models(result, errors),
            Ev::Shutdown => self.quit = Some("server terminated".into()),
        }
    }

    // ---- clients ------------------------------------------------------------

    fn on_join(&mut self, id: u64, hello: Hello, tx: mpsc::UnboundedSender<ServerFrame>) {
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
        self.clients.push(ClientConn { id, tx, cols: hello.cols.max(20), rows: hello.rows.max(5), attached });
        if !attached {
            return;
        }
        self.recompute_size();
        if self.tabs.is_empty() {
            self.new_tab(hello.cwd.as_deref());
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
                };
                if let Some(c) = self.clients.iter().find(|c| c.id == id) {
                    let _ = c.tx.send(ServerFrame::Msg(ServerMsg::Info(info)));
                }
            }
            ClientMsg::Kill => self.quit = Some("session killed".into()),
        }
    }

    fn detach_client(&mut self, id: u64, reason: &str) {
        if let Some(pos) = self.clients.iter().position(|c| c.id == id) {
            let c = self.clients.remove(pos);
            let _ = c.tx.send(ServerFrame::Msg(ServerMsg::Bye { reason: reason.into() }));
        }
        self.recompute_size();
    }

    fn detach_all(&mut self) {
        for c in self.clients.drain(..) {
            let _ = c.tx.send(ServerFrame::Msg(ServerMsg::Bye { reason: "detached".into() }));
        }
    }

    /// The shared view is as large as the smallest attached client.
    fn recompute_size(&mut self) {
        let attached: Vec<&ClientConn> = self.clients.iter().filter(|c| c.attached).collect();
        if attached.is_empty() {
            return;
        }
        let cols = attached.iter().map(|c| c.cols).min().unwrap_or(80);
        let rows = attached.iter().map(|c| c.rows).min().unwrap_or(24);
        if cols != self.cols || rows != self.rows {
            self.cols = cols;
            self.rows = rows;
        }
        self.force_redraw = true;
        self.dirty = true;
    }

    fn broadcast(&mut self, frame_bytes: Vec<u8>) {
        let mut dead = Vec::new();
        for c in self.clients.iter().filter(|c| c.attached) {
            if c.tx.send(ServerFrame::Output(frame_bytes.clone())).is_err() {
                dead.push(c.id);
            }
        }
        if !dead.is_empty() {
            self.clients.retain(|c| !dead.contains(&c.id));
            self.recompute_size();
        }
    }

    pub fn copy_to_clipboard(&mut self, text: String) {
        for c in self.clients.iter().filter(|c| c.attached) {
            let _ = c.tx.send(ServerFrame::Msg(ServerMsg::Clipboard(text.clone())));
        }
    }

    // ---- tabs ---------------------------------------------------------------

    fn pane_size(&self, panel_visible: bool) -> (u16, u16) {
        let l = ui::layout(self.cols, self.rows, panel_visible, self.ui.panel_pct, self.cfg.status_bar);
        (l.pane.width, l.pane.height)
    }

    pub fn new_tab(&mut self, cwd: Option<&str>) {
        let id = self.next_id;
        self.next_id += 1;
        let inherited = self.tab().and_then(|t| t.pane.cwd.clone());
        let cwd = cwd.map(str::to_string).or(inherited);
        let (cols, rows) = self.pane_size(false);
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
            Ok(pane) => {
                self.tabs.push(Tab { id, pane, name: None, agent: AgentSession::new(), focus: Focus::Shell, activity: false });
                self.active = self.tabs.len() - 1;
                self.dirty = true;
            }
            Err(e) => self.toast(format!("Could not start a shell: {e}")),
        }
    }

    pub fn select_tab(&mut self, idx: usize) {
        if idx < self.tabs.len() && idx != self.active {
            self.active = idx;
            self.tabs[idx].activity = false;
            self.dirty = true;
        }
    }

    fn remove_tab(&mut self, idx: usize) {
        if idx >= self.tabs.len() {
            return;
        }
        if let Some(task) = self.tabs[idx].agent.task.take() {
            task.abort();
        }
        self.tabs.remove(idx);
        if self.tabs.is_empty() {
            self.quit = Some("exited".into());
            return;
        }
        if self.active > idx || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1).min(self.tabs.len() - 1);
        }
        self.dirty = true;
    }

    fn on_pty(&mut self, pane_id: u64, data: &[u8]) {
        let Some(idx) = self.tab_index(pane_id) else { return };
        let mut events = Vec::new();
        self.tabs[idx].pane.feed(data, &mut events);
        if idx == self.active {
            self.dirty = true;
        }
        for ev in events {
            match ev {
                PaneEvent::Bell => {
                    if idx != self.active {
                        self.tabs[idx].activity = true;
                        self.dirty = true;
                    }
                    self.broadcast(vec![0x07]);
                }
                PaneEvent::Clipboard(text) => self.copy_to_clipboard(text),
                PaneEvent::CommandStarted => self.agent_command_started(idx),
                PaneEvent::CommandFinished(block) => {
                    if idx != self.active {
                        self.tabs[idx].activity = true;
                    }
                    self.agent_command_finished(idx, block);
                    self.dirty = true;
                }
                PaneEvent::PromptReady => self.dirty = true,
            }
        }
    }

    // ---- input --------------------------------------------------------------

    fn on_event(&mut self, client: u64, ev: Event) {
        match ev {
            Event::Key(key) => self.on_key(client, key),
            Event::Mouse(m) => self.on_mouse(m),
            Event::Paste(text) => self.on_paste(text),
            Event::Resize(cols, rows) => {
                if let Some(c) = self.clients.iter_mut().find(|c| c.id == client) {
                    c.cols = cols.max(20);
                    c.rows = rows.max(5);
                }
                self.recompute_size();
            }
            Event::FocusGained | Event::FocusLost => {
                let gained = matches!(ev, Event::FocusGained);
                if let Some(tab) = self.tab() {
                    if tab.focus == Focus::Shell && tab.pane.mode().contains(TermMode::FOCUS_IN_OUT) {
                        tab.pane.write(if gained { &b"\x1b[I"[..] } else { &b"\x1b[O"[..] });
                    }
                }
            }
        }
    }

    fn on_paste(&mut self, text: String) {
        if let Some(Overlay::Rename { text: name }) = &mut self.ui.overlay {
            name.push_str(text.lines().next().unwrap_or(""));
            self.dirty = true;
            return;
        }
        if self.ui.overlay.is_some() {
            return;
        }
        let Some(tab) = self.tab_mut() else { return };
        match tab.focus {
            Focus::Shell => {
                tab.pane.scroll_to_bottom();
                let bytes = input::encode_paste(&text, tab.pane.mode());
                tab.pane.write(bytes);
            }
            Focus::Agent => self.agent_paste(&text),
        }
        self.dirty = true;
    }

    fn on_key(&mut self, client: u64, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
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
        let Some(tab) = self.tab_mut() else { return };
        match tab.focus {
            Focus::Agent => self.agent_key_input(key),
            Focus::Shell => {
                // Shift+PageUp/PageDown page through scrollback like a
                // regular terminal, unless a full-screen program is running.
                let alt_screen = tab.pane.mode().contains(TermMode::ALT_SCREEN);
                if key.modifiers.contains(KeyModifiers::SHIFT) && !alt_screen {
                    let page = tab.pane.rows.saturating_sub(1) as i32;
                    match key.code {
                        KeyCode::PageUp => return tab.pane.scroll(page),
                        KeyCode::PageDown => return tab.pane.scroll(-page),
                        _ => {}
                    }
                }
                if let Some(bytes) = input::encode_key(&key, tab.pane.mode()) {
                    tab.pane.scroll_to_bottom();
                    tab.pane.clear_selection();
                    tab.pane.write(bytes);
                }
            }
        }
    }

    /// Switch between typing in the shell and talking to the agent.
    pub fn toggle_focus(&mut self) {
        let Some(tab) = self.tab_mut() else { return };
        match tab.focus {
            Focus::Shell => {
                tab.focus = Focus::Agent;
                tab.agent.visible = true;
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
                if let Some(bytes) = input::encode_key(&key, tab.pane.mode()) {
                    tab.pane.write(bytes);
                }
            }
            return;
        }
        let n = self.tabs.len();
        match key.code {
            KeyCode::Char('c') | KeyCode::Char('t') => self.new_tab(None),
            KeyCode::Char('n') | KeyCode::Right | KeyCode::Tab => self.select_tab((self.active + 1) % n.max(1)),
            KeyCode::Char('p') | KeyCode::Left | KeyCode::BackTab => self.select_tab((self.active + n.max(1) - 1) % n.max(1)),
            KeyCode::Char(c @ '1'..='9') => self.select_tab(c as usize - '1' as usize),
            KeyCode::Char('x') | KeyCode::Char('w') => self.remove_tab(self.active),
            KeyCode::Char('d') | KeyCode::Char('q') => self.detach_client(client, "detached"),
            KeyCode::Char('D') => self.detach_all(),
            KeyCode::Char('a') => {
                let showing = self.tab().is_some_and(|t| t.agent.visible);
                if showing { self.hide_panel() } else { self.toggle_focus() }
            }
            KeyCode::Char(',') | KeyCode::Char('r') => {
                let current = self.tab().and_then(|t| t.name.clone()).unwrap_or_default();
                self.ui.overlay = Some(Overlay::Rename { text: current });
            }
            KeyCode::Char('m') => self.open_model_picker(),
            KeyCode::Char('T') | KeyCode::Char('s') => self.open_theme_picker(),
            KeyCode::Char('?') | KeyCode::Char('h') => self.ui.overlay = Some(Overlay::Help),
            KeyCode::PageUp | KeyCode::Char('[') => {
                if let Some(tab) = self.tab_mut() {
                    let page = tab.pane.rows.saturating_sub(1) as i32;
                    tab.pane.scroll(page);
                }
            }
            KeyCode::PageDown => {
                if let Some(tab) = self.tab_mut() {
                    let page = tab.pane.rows.saturating_sub(1) as i32;
                    tab.pane.scroll(-page);
                }
            }
            _ => {}
        }
    }

    fn overlay_key(&mut self, key: KeyEvent) {
        let Some(overlay) = &mut self.ui.overlay else { return };
        match overlay {
            Overlay::Help => self.ui.overlay = None,
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
        let items: Vec<PickItem> = theme::all()
            .into_iter()
            .map(|t| PickItem { label: t.label.to_string(), detail: t.name.to_string(), value: t.name.to_string() })
            .collect();
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
            if let Some(t) = theme::by_name(&p.items[i].value) {
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
            PickerKind::Theme => match chosen.and_then(|name| theme::by_name(&name)) {
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
        }
    }

    pub fn set_theme(&mut self, t: Theme) {
        self.cfg.theme = t.name.to_string();
        self.theme = t;
        self.force_redraw = true;
        self.dirty = true;
        self.cfg.save_state();
    }

    pub fn set_model(&mut self, provider: &str, model: &str) {
        self.cfg.agent.provider = provider.to_string();
        self.cfg.agent.model = model.to_string();
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

        if let MouseEventKind::Moved = ev.kind {
            let hover = hit.as_ref().map(|(_, h)| h.clone()).filter(|h| !matches!(h, Hit::Pane | Hit::Panel | Hit::Overlay));
            if hover != self.ui.hover {
                self.ui.hover = hover;
                self.dirty = true;
            }
            // Pass plain motion to programs that asked for it.
            if let (Some((rect, Hit::Pane)), Some(tab)) = (&hit, self.tab()) {
                if tab.pane.mode().contains(TermMode::MOUSE_MOTION) {
                    if let Some(bytes) = input::encode_mouse(&ev, col - rect.x, row - rect.y, tab.pane.mode()) {
                        tab.pane.write(bytes);
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
                        Drag::Divider => {
                            let rows = self.rows.max(1);
                            let below = rows.saturating_sub(row + 1);
                            self.ui.panel_pct = ((below as u32 * 100 / rows as u32) as u16).clamp(20, 80);
                        }
                        Drag::Select => {
                            let pane_rect = ui::layout(
                                self.cols,
                                self.rows,
                                self.tab().is_some_and(|t| t.agent.visible),
                                self.ui.panel_pct,
                                self.cfg.status_bar,
                            )
                            .pane;
                            if let Some(tab) = self.tab_mut() {
                                // Dragging past the edge scrolls the history.
                                if row < pane_rect.y {
                                    tab.pane.scroll(1);
                                } else if row >= pane_rect.y + pane_rect.height {
                                    tab.pane.scroll(-1);
                                }
                                let c = col.saturating_sub(pane_rect.x).min(pane_rect.width.saturating_sub(1));
                                let r = row.saturating_sub(pane_rect.y).min(pane_rect.height.saturating_sub(1));
                                tab.pane.update_selection(c, r);
                            }
                        }
                        Drag::Forward => self.forward_mouse(&ev),
                    }
                    return;
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    match self.ui.drag.take() {
                        Some(Drag::Select) => {
                            let text = self.tab().and_then(|t| t.pane.selection_text());
                            match text {
                                Some(text) if self.cfg.copy_on_select => {
                                    let n = text.chars().count();
                                    self.copy_to_clipboard(text);
                                    self.toast(format!("Copied {n} character{}", if n == 1 { "" } else { "s" }));
                                }
                                Some(_) => {}
                                None => {
                                    if let Some(tab) = self.tab_mut() {
                                        tab.pane.clear_selection();
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
                    Hit::Panel | Hit::Choice(_) | Hit::Slash(_) | Hit::Divider => {
                        if let Some(tab) = self.tab_mut() {
                            let a = &mut tab.agent;
                            a.scroll = if up { a.scroll + 3 } else { a.scroll.saturating_sub(3) };
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
                if hit == Hit::Pane {
                    self.forward_or_ignore(&ev, &rect);
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
        let pane_rect =
            ui::layout(self.cols, self.rows, self.tab().is_some_and(|t| t.agent.visible), self.ui.panel_pct, self.cfg.status_bar).pane;
        self.forward_or_ignore(ev, &pane_rect);
    }

    fn forward_or_ignore(&mut self, ev: &MouseEvent, rect: &Rect) {
        let Some(tab) = self.tab() else { return };
        let mode = tab.pane.mode();
        if !input::wants_mouse(mode) || ev.modifiers.contains(KeyModifiers::SHIFT) {
            return;
        }
        let col = ev.column.saturating_sub(rect.x).min(rect.width.saturating_sub(1));
        let row = ev.row.saturating_sub(rect.y).min(rect.height.saturating_sub(1));
        if let Some(bytes) = input::encode_mouse(ev, col, row, mode) {
            tab.pane.write(bytes);
        }
    }

    fn scroll_pane(&mut self, ev: &MouseEvent, col: u16, row: u16, up: bool) {
        let Some(tab) = self.tab_mut() else { return };
        let mode = tab.pane.mode();
        if input::wants_mouse(mode) && !ev.modifiers.contains(KeyModifiers::SHIFT) {
            if let Some(bytes) = input::encode_mouse(ev, col, row, mode) {
                tab.pane.write(bytes);
            }
        } else if mode.contains(TermMode::ALT_SCREEN) {
            // Pagers and editors without mouse support: turn the wheel into arrows.
            let key = KeyEvent::new(if up { KeyCode::Up } else { KeyCode::Down }, KeyModifiers::NONE);
            if let Some(bytes) = input::encode_key(&key, mode) {
                tab.pane.write(bytes.repeat(3));
            }
        } else {
            tab.pane.scroll(if up { 3 } else { -3 });
        }
    }

    fn on_click(&mut self, ev: MouseEvent, rect: Rect, hit: Hit) {
        // Clicking outside a menu closes it.
        if self.ui.overlay.is_some() && !matches!(hit, Hit::PickerItem(_) | Hit::Overlay) {
            match &self.ui.overlay {
                Some(Overlay::Picker(_)) => self.close_picker(false),
                _ => self.ui.overlay = None,
            }
            return;
        }
        match hit {
            Hit::Tab(i) => self.select_tab(i),
            Hit::CloseTab(i) => self.remove_tab(i),
            Hit::NewTab => self.new_tab(None),
            Hit::Mode => self.toggle_focus(),
            Hit::Autonomy => self.cycle_autonomy(),
            Hit::Model => self.open_model_picker(),
            Hit::Help => self.ui.overlay = Some(Overlay::Help),
            Hit::PanelClose => self.hide_panel(),
            Hit::Divider => self.ui.drag = Some(Drag::Divider),
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
                let mode = tab.pane.mode();
                if input::wants_mouse(mode) && !ev.modifiers.contains(KeyModifiers::SHIFT) {
                    if let Some(bytes) = input::encode_mouse(&ev, col, row, mode) {
                        tab.pane.write(bytes);
                    }
                    self.ui.drag = Some(Drag::Forward);
                } else {
                    let ty = match clicks {
                        2 => SelectionType::Semantic,
                        3 => SelectionType::Lines,
                        _ => SelectionType::Simple,
                    };
                    tab.pane.clear_selection();
                    tab.pane.start_selection(col, row, ty);
                    if clicks > 1 {
                        tab.pane.update_selection(col, row);
                    }
                    self.ui.drag = Some(Drag::Select);
                }
            }
        }
    }

    // ---- rendering ----------------------------------------------------------

    fn render(&mut self) {
        self.dirty = false;
        if self.clients.iter().all(|c| !c.attached) || self.tabs.is_empty() {
            return;
        }
        // Keep the front tab's shell sized to the space it is drawn in.
        let panel = self.tabs[self.active].agent.visible;
        let (pc, pr) = self.pane_size(panel);
        self.tabs[self.active].pane.resize(pc, pr);

        let area = Rect::new(0, 0, self.cols, self.rows);
        let mut buf = Buffer::empty(area);
        let cursor = ui::draw(self, &mut buf);

        let mut out: Vec<u8> = Vec::with_capacity(8192);
        // Paint inside a synchronized update with the cursor hidden so the
        // host terminal never shows a half-drawn frame.
        out.extend_from_slice(b"\x1b[?2026h\x1b[?25l");
        if self.force_redraw || self.prev.area != area {
            out.extend_from_slice(b"\x1b[0m\x1b[2J");
            self.prev = Buffer::empty(area);
            self.force_redraw = false;
            self.last_cursor = None;
        }
        {
            use ratatui::backend::{Backend, CrosstermBackend};
            let updates = self.prev.diff(&buf);
            let mut backend = CrosstermBackend::new(&mut out);
            let _ = backend.draw(updates.into_iter());
            let _ = Backend::flush(&mut backend);
        }
        match cursor {
            Some(c) => {
                if self.last_cursor.map(|l| l.2) != Some(c.shape) {
                    out.extend_from_slice(format!("\x1b[{} q", c.shape).as_bytes());
                }
                out.extend_from_slice(format!("\x1b[{};{}H\x1b[?25h", c.y + 1, c.x + 1).as_bytes());
                self.last_cursor = Some((c.x, c.y, c.shape));
            }
            None => self.last_cursor = None,
        }
        out.extend_from_slice(b"\x1b[?2026l");
        self.prev = buf;
        self.broadcast(out);
    }
}
