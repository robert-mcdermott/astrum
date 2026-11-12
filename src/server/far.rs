//! Tabs on other machines: the server's side of `crate::remote`.
//!
//! For each machine this session has shells on there is one `Far`: the
//! `ssh` to it while one is running, the way to the program keeping the
//! shells once that is on the line, and a small terminal that shows what
//! `ssh` asks and what Astrum has to say while a pane's own shell cannot
//! be shown (it is being reached, or the connection has dropped).

use super::{Ev, Focus, Server, Tab, panes};
use crate::remote::reach::{Reach, Told};
use super::session::AgentSession;
use crate::pane::{HeldOpts, Pane};
use crate::remote::{self, Down, Frame, LINK, Machine, Up, Way};
use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::{Duration, Instant};

/// How often the other side is asked whether it is still there, and how
/// long it may say nothing at all before the connection is given up for
/// lost. (A connection that has merely stopped, with nothing closed, is
/// not noticed by `ssh` for a long time, or ever.)
const PING_EVERY: Duration = Duration::from_secs(10);
const SILENCE: Duration = Duration::from_secs(35);

/// What a pane waiting for its machine offers to click.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum FarButton {
    Yes,
    No,
    Again,
}

pub enum Stage {
    /// `ssh` is running; the line run there has not been heard from yet.
    Calling,
    /// The user is being asked whether to put Astrum there: the build it
    /// would be, and the version it would take the place of.
    Asking { asset: String, replaces: Option<String> },
    /// The build is being found (and its checksum verified).
    Finding,
    /// It has been sent; the other side is putting it in place.
    Putting,
    /// The keeper is on the line and has been greeted.
    Greeting,
    Up,
    /// Not connected.
    Down,
}

struct Size(usize, usize);
impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.1
    }
    fn screen_lines(&self) -> usize {
        self.1
    }
    fn columns(&self) -> usize {
        self.0
    }
}

pub struct Far {
    /// The machine, as the user named it to `ssh`.
    pub host: String,
    reach: Option<Reach>,
    pub way: Option<Way>,
    /// The machine as it described itself, once it has.
    pub there: Option<Machine>,
    pub stage: Stage,
    /// What is shown in place of a pane's shell meanwhile.
    pub cover: Term<VoidListener>,
    parser: Processor<StdSyncHandler>,
    heard: Instant,
    pinged: Instant,
    /// It has been reached before: a connection lost is tried again.
    been_up: bool,
    tries: u32,
    again_at: Option<Instant>,
    /// Said once: its shells end at the last logout.
    warned: bool,
}

impl Far {
    fn new(host: &str, size: (u16, u16)) -> Far {
        let cover = Term::new(TermConfig { scrolling_history: 200, ..TermConfig::default() }, &Size(size.0.max(20) as usize, size.1.max(4) as usize), VoidListener);
        let now = Instant::now();
        Far { host: host.to_string(), reach: None, way: None, there: None, stage: Stage::Down, cover, parser: Processor::new(), heard: now, pinged: now, been_up: false, tries: 0, again_at: None, warned: false }
    }

    fn show(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.cover, bytes);
    }

    /// A line of Astrum's own among what `ssh` says.
    fn note(&mut self, text: &str) {
        // (At the start of a line, whatever `ssh` left unfinished.)
        let lead = if self.cover.grid().cursor.point.column.0 > 0 { "\r\n" } else { "" };
        self.show(format!("{lead}\x1b[1m{}\x1b[0m\r\n", text.replace('\n', "\r\n")).as_bytes());
    }

    /// The name to call the machine by: its own, once it has said.
    pub fn name(&self) -> String {
        match &self.there {
            Some(there) if !there.host.is_empty() => there.host.clone(),
            _ => self.host.split_whitespace().last().unwrap_or(&self.host).to_string(),
        }
    }

    /// The buttons there are to click right now.
    pub fn buttons(&self) -> &'static [(FarButton, &'static str)] {
        match self.stage {
            Stage::Asking { .. } => &[(FarButton::Yes, "Yes, copy it there"), (FarButton::No, "No")],
            Stage::Down => &[(FarButton::Again, "Try again")],
            _ => &[],
        }
    }

    fn hang_up(&mut self) {
        if let Some(way) = self.way.take() {
            way.lost();
        }
        self.reach = None;
        self.stage = Stage::Down;
    }
}

/// How long to wait before trying a lost connection again, the `n`th time.
fn wait_before(n: u32) -> Duration {
    Duration::from_secs(match n {
        0 => 1,
        1 => 3,
        2 => 8,
        3 => 20,
        _ => 60,
    })
}

impl Server {
    pub fn far(&self, host: &str) -> Option<&Far> {
        self.fars.iter().find(|f| f.host == host)
    }

    fn far_mut(&mut self, host: &str) -> Option<&mut Far> {
        self.fars.iter_mut().find(|f| f.host == host)
    }

    /// The machine whose word is shown in place of this pane's shell, when
    /// the shell itself cannot be.
    pub fn cover_of(&self, pane: &Pane) -> Option<&Far> {
        pane.there().filter(|t| !t.up).and_then(|t| self.far(&t.host))
    }

    /// Who this session is to a keeper of shells: a name for this machine
    /// (made up once, and kept) and the session's.
    fn owner(&self) -> String {
        let file = crate::paths::data_dir().join("machine-id");
        let id = std::fs::read_to_string(&file).ok().map(|s| s.trim().to_string()).filter(|s| s.len() >= 16 && s.chars().all(|c| c.is_ascii_hexdigit()));
        let id = id.unwrap_or_else(|| {
            let id = crate::pane::random_hex();
            let _ = std::fs::create_dir_all(crate::paths::data_dir());
            let _ = std::fs::write(&file, &id);
            id
        });
        format!("{id}/{}", self.session)
    }

    /// Panes showing shells on `host`: (tab, pane id).
    fn panes_on(&self, host: &str) -> Vec<(usize, u64)> {
        let mut found = Vec::new();
        for (idx, tab) in self.tabs.iter().enumerate() {
            for pane in tab.panes.iter() {
                if pane.there().is_some_and(|t| t.host == host) {
                    found.push((idx, pane.id));
                }
            }
        }
        found
    }

    fn pane_by_key(&mut self, host: &str, key: u64) -> Option<(usize, &mut Pane)> {
        for (idx, tab) in self.tabs.iter_mut().enumerate() {
            for pane in tab.panes.iter_mut() {
                if pane.there().is_some_and(|t| t.host == host && t.key == key) {
                    return Some((idx, pane));
                }
            }
        }
        None
    }

    /// Start reaching `host`, unless that is under way.
    pub(super) fn far_call(&mut self, host: &str) {
        let size = self.panes_on(host).first().and_then(|(idx, id)| self.tabs[*idx].panes.get(*id)).map_or((80, 24), |p| (p.cols, p.rows));
        if self.far(host).is_none() {
            self.fars.push(Far::new(host, size));
        }
        let id = self.next_reach;
        self.next_reach += 1;
        let tx = self.tx.clone();
        let Some(far) = self.far_mut(host) else { return };
        if far.reach.is_some() {
            return;
        }
        far.again_at = None;
        far.cover.resize(Size(size.0.max(20) as usize, size.1.max(4) as usize));
        far.show(b"\x1b[2J\x1b[H");
        far.note(&format!("Reaching {} over ssh…", far.host));
        match Reach::start(id, host, size, tx) {
            Ok(reach) => {
                far.reach = Some(reach);
                far.stage = Stage::Calling;
            }
            Err(e) => {
                far.note(&format!("ssh could not be started: {e:#}"));
                far.stage = Stage::Down;
            }
        }
        self.dirty = true;
    }

    /// A pane for a new shell on `host`, started there as soon as the
    /// machine is reached (now, if it has been).
    fn held_pane(&mut self, host: &str, cols: u16, rows: u16, cwd: Option<String>) -> Pane {
        let id = self.next_id;
        self.next_id += 1;
        let key = u64::from_str_radix(&crate::pane::random_hex()[..16], 16).unwrap_or(id);
        let mut pane = Pane::held(HeldOpts { id, cols, rows, scrollback: self.cfg.scrollback, host: host.to_string(), key, cwd });
        let (fg, bg) = self.term_colors();
        pane.default_fg = fg.unwrap_or(pane.default_fg);
        pane.default_bg = bg.unwrap_or(pane.default_bg);
        let up = self.far(host).filter(|f| matches!(f.stage, Stage::Up)).and_then(|f| f.way.clone());
        match up {
            Some(way) => Server::start_there(&mut pane, way, &self.session, self.cfg.scrollback),
            None => self.far_call(host),
        }
        pane
    }

    /// Ask the keeper for this pane's shell: a new one, or the one it had.
    fn start_there(pane: &mut Pane, way: Way, session: &str, scrollback: usize) {
        let (cols, rows, px) = pane.size_told();
        let cwd = pane.cwd.clone();
        let session = session.to_string();
        let Some(there) = pane.there_mut() else { return };
        way.send(match there.started {
            true => Up::Attach { key: there.key, had: (!there.fresh).then_some(there.had), cols, rows, px },
            false => Up::Open { key: there.key, cols, rows, px, cwd, session, scrollback },
        });
        there.way = Some(way);
    }

    /// Open a tab on another machine.
    pub fn new_remote_tab(&mut self, host: &str) {
        let host = host.split_whitespace().collect::<Vec<_>>().join(" ");
        if host.is_empty() {
            return;
        }
        let (cols, rows) = self.pane_size(false);
        let pane = self.held_pane(&host, cols, rows, None);
        let id = pane.id;
        self.tabs.push(Tab { id, panes: panes::Panes::new(pane), name: None, agent: AgentSession::new(), focus: Focus::Shell, activity: false, owner: None, sized: (0, 0) });
        self.active = self.tabs.len() - 1;
        self.dirty = true;
    }

    /// A pane beside one on another machine is on that machine too.
    pub(super) fn held_like(&mut self, like: (String, Option<String>), cols: u16, rows: u16) -> Pane {
        self.held_pane(&like.0, cols, rows, like.1)
    }

    /// A pane for a shell this session had on `host` before it was
    /// restarted: found again there if it is still running.
    pub(super) fn held_again(&mut self, host: &str, key: u64, cols: u16, rows: u16, cwd: Option<String>) -> Pane {
        let id = self.next_id;
        self.next_id += 1;
        let mut pane = Pane::held(HeldOpts { id, cols, rows, scrollback: self.cfg.scrollback, host: host.to_string(), key, cwd });
        if let Some(there) = pane.there_mut() {
            there.started = true;
        }
        self.far_call(host);
        pane
    }

    /// When something here is next due, for the loop to wake in time.
    pub(super) fn far_due(&self) -> Option<Instant> {
        self.fars.iter().filter_map(|f| if f.way.is_some() { Some(f.pinged + PING_EVERY) } else { f.again_at }).min()
    }

    pub(super) fn far_tick(&mut self, now: Instant) {
        let mut lost = Vec::new();
        let mut again = Vec::new();
        for far in &mut self.fars {
            if let Some(way) = &far.way {
                if now.duration_since(far.heard) > SILENCE {
                    lost.push(far.host.clone());
                } else if now.duration_since(far.pinged) >= PING_EVERY {
                    far.pinged = now;
                    way.send(Up::Ping);
                }
            } else if far.again_at.is_some_and(|at| now >= at) {
                again.push(far.host.clone());
            }
        }
        for host in lost {
            self.far_lost(&host, "Nothing has been heard from it for a while.");
        }
        self.drop_unused_fars();
        for host in again {
            // (Not for a machine nothing is shown from any more.)
            if self.panes_on(&host).is_empty() {
                self.fars.retain(|f| f.host != host);
            } else {
                self.far_call(&host);
            }
        }
    }

    /// The connection to `host` is gone. Its shells are not.
    fn far_lost(&mut self, host: &str, why: &str) {
        for (idx, id) in self.panes_on(host) {
            if let Some(pane) = self.tabs[idx].panes.get_mut(id) {
                if let Some(there) = pane.there_mut() {
                    there.way = None;
                    there.up = false;
                    there.job = None;
                }
                pane.redraw();
            }
        }
        let Some(far) = self.far_mut(host) else { return };
        let was_up = matches!(far.stage, Stage::Up | Stage::Greeting);
        far.hang_up();
        if was_up {
            far.show(b"\x1b[2J\x1b[H");
            far.note(&format!("The connection to {} was lost. {why}\nThe shells there are still running, and what they print meanwhile is kept.", far.name()));
        } else if !why.is_empty() {
            far.note(why);
        }
        if far.been_up {
            let wait = wait_before(far.tries);
            far.tries += 1;
            far.again_at = Some(Instant::now() + wait);
            far.note(&format!("Trying again in {}s.", wait.as_secs()));
        }
        self.dirty = true;
    }

    pub(super) fn on_remote(&mut self, reach: u64, told: Told) {
        // (Something from an `ssh` that has been given up on is of no interest.)
        let Some(host) = self.fars.iter().find(|f| f.reach.as_ref().is_some_and(|r| r.id == reach)).map(|f| f.host.clone()) else { return };
        match told {
            Told::Frame(Frame::Bytes(key, data)) => {
                if let Some(far) = self.far_mut(&host) {
                    far.heard = Instant::now();
                }
                if let Some(id) = self.pane_by_key(&host, key).map(|(_, pane)| pane.id) {
                    self.on_pty(id, &data);
                }
                return;
            }
            Told::Frame(frame) => {
                if let Some(far) = self.far_mut(&host) {
                    far.heard = Instant::now();
                }
                self.on_far_frame(&host, frame);
            }
            Told::Tty(bytes) => {
                if let Some(far) = self.far_mut(&host) {
                    far.show(&bytes);
                }
            }
            Told::Found { link, system, machine, version } => self.on_far_found(&host, link, &system, &machine, version),
            Told::Build(found) => {
                let Some(far) = self.far_mut(&host).filter(|f| matches!(f.stage, Stage::Finding)) else { return };
                match found {
                    Ok(build) => {
                        far.note(&format!("Copying it there ({:.1} MB, from {}; its checksum matches).", build.bytes.len() as f64 / 1e6, build.from));
                        far.stage = Stage::Putting;
                        if let Some(reach) = &far.reach {
                            reach.put(build.bytes);
                        }
                    }
                    Err(why) => {
                        far.hang_up();
                        far.note(&format!("Astrum was not copied: {why}"));
                    }
                }
            }
            Told::Failed => {
                if let Some(far) = self.far_mut(&host) {
                    far.hang_up();
                    far.note("The copy could not be put in ~/.local/bin there, or would not run. Nothing that was there has been changed.");
                }
            }
            Told::Ready => {
                let owner = self.owner();
                let Some(far) = self.far_mut(&host) else { return };
                let Some(reach) = &far.reach else { return };
                let way = reach.way();
                way.send(Up::Hello { link: LINK, version: env!("CARGO_PKG_VERSION").into(), owner });
                far.way = Some(way);
                far.stage = Stage::Greeting;
                far.heard = Instant::now();
                far.pinged = far.heard;
            }
            Told::Closed => {
                let calling = self.far(&host).is_some_and(|f| matches!(f.stage, Stage::Calling | Stage::Putting | Stage::Finding | Stage::Asking { .. }));
                self.far_lost(&host, if calling { "ssh ended before Astrum there was reached." } else { "" });
            }
        }
        self.dirty = true;
    }

    /// The line run on the other machine has said what is there.
    fn on_far_found(&mut self, host: &str, link: Option<u32>, system: &str, machine: &str, version: Option<String>) {
        let Some(far) = self.far_mut(host) else { return };
        let this = env!("CARGO_PKG_VERSION");
        match link {
            Some(n) if n == LINK => {
                if let Some(reach) = &far.reach {
                    reach.go();
                }
            }
            // Never an older one in a newer one's place.
            Some(n) if n > LINK => {
                far.hang_up();
                far.note(&format!(
                    "The Astrum there ({}) is newer than this one ({this}) and the two no longer speak the same language. Update Astrum on this machine.",
                    version.as_deref().unwrap_or("a later version")
                ));
            }
            _ => match remote::asset(system, machine) {
                Some(asset) => {
                    let what = match (&version, link) {
                        (Some(v), Some(_)) => format!("The Astrum there ({v}) is older than this one ({this}), and the two do not speak the same language."),
                        (Some(v), None) => format!("The Astrum there ({v}) is from before tabs on other machines."),
                        _ => "Astrum is not on that machine.".to_string(),
                    };
                    let place = if version.is_some() { "put this version in ~/.local/bin/astrum there" } else { "copy it to ~/.local/bin/astrum there" };
                    far.note(&format!(
                        "{what}\nShall Astrum {place}? It is one file in your home folder ({system} {machine}), sent over this connection: nothing is installed system-wide and that machine needs no internet access.\n\nPress y to copy it, n to leave things as they are."
                    ));
                    far.stage = Stage::Asking { asset, replaces: version };
                }
                None => {
                    far.hang_up();
                    far.note(&format!(
                        "There is no build of Astrum for that machine ({system} {machine}), so its shells cannot be kept there.\nWhat still works: open an ordinary tab, run ssh in it, and take the agent in (the status bar offers it)."
                    ));
                }
            },
        }
        self.dirty = true;
    }

    fn on_far_frame(&mut self, host: &str, frame: Frame<Down>) {
        match frame {
            Frame::Bytes(..) => {}
            Frame::Screen(key, screen) => {
                if let Some((_, pane)) = self.pane_by_key(host, key) {
                    let at_prompt = pane.there_mut().and_then(|t| t.at_prompt.take());
                    pane.shown_as(&screen, at_prompt);
                }
            }
            Frame::Msg(Down::Hello { there, shells, .. }) => {
                let Some(far) = self.far_mut(host) else { return };
                let Some(way) = far.way.clone() else { return };
                far.stage = Stage::Up;
                far.been_up = true;
                far.tries = 0;
                let warn = there.outlasts_logout == Some(false) && !std::mem::replace(&mut far.warned, true);
                let name = if there.host.is_empty() { host.to_string() } else { there.host.clone() };
                far.there = Some(there);
                let (session, scrollback) = (self.session.clone(), self.cfg.scrollback);
                let (mut ended, mut claimed) = (Vec::new(), Vec::new());
                for (idx, id) in self.panes_on(host) {
                    let Some(pane) = self.tabs[idx].panes.get_mut(id) else { continue };
                    if let Some(t) = pane.there_mut().filter(|t| t.started && !shells.contains(&t.key)) {
                        // It was running there and no longer is. If any of
                        // it is shown here, that cannot be continued: the
                        // pane goes, as it would have had the connection
                        // been there. If none is (the tab is being brought
                        // back), a new shell takes its place.
                        if !t.fresh {
                            ended.push(id);
                            continue;
                        }
                        t.started = false;
                        if let Some((text, note)) = t.before.take() {
                            pane.preload(&text, &note);
                        }
                    }
                    claimed.extend(pane.there().map(|t| t.key));
                    Server::start_there(pane, way.clone(), &session, scrollback);
                }
                for id in ended {
                    if let Some(idx) = self.find_pane(id) {
                        self.remove_pane(idx, id);
                        self.toast(format!("A shell on {name} ended while the connection was down"));
                    }
                }
                // Shells of this session's that no pane shows any more
                // (their tabs were closed, or forgotten, while the
                // connection was down) are not left running for nobody.
                for key in shells.into_iter().filter(|k| !claimed.contains(k)) {
                    way.send(Up::Close { key });
                }
                if warn {
                    self.toast(format!("Shells on {name} end when you are logged out of it everywhere. `loginctl enable-linger` there keeps them."));
                }
            }
            Frame::Msg(Down::Shell { key, nonce, shell, cwd, at, screen, replay, at_prompt }) => {
                if let Some((_, pane)) = self.pane_by_key(host, key) {
                    pane.begin(nonce, shell, cwd, at, screen, replay);
                    if let Some(there) = pane.there_mut() {
                        there.started = true;
                        there.fresh = false;
                        there.at_prompt = at_prompt;
                    }
                    pane.tell_size();
                }
            }
            Frame::Msg(Down::Gone { key, why }) => {
                let Some((idx, pane)) = self.pane_by_key(host, key) else { return };
                let id = pane.id;
                let name = self.far(host).map(|f| f.name()).unwrap_or_default();
                self.remove_pane(idx, id);
                self.toast(match why {
                    Some(why) => format!("Could not start a shell on {name}: {why}"),
                    None => format!("That shell on {name} is no longer running"),
                });
            }
            Frame::Msg(Down::Exited { key }) => {
                if let Some((idx, pane)) = self.pane_by_key(host, key) {
                    let id = pane.id;
                    self.remove_pane(idx, id);
                }
            }
            Frame::Msg(Down::Doing { key, cwd, job }) => {
                if let Some((_, pane)) = self.pane_by_key(host, key) {
                    // (A shell that says where it is itself is believed first.)
                    if !pane.integrated && cwd.is_some() {
                        pane.cwd = cwd;
                    }
                    if let Some(there) = pane.there_mut() {
                        there.job = job;
                    }
                }
            }
            Frame::Msg(Down::Answer { n, answer }) => {
                if let Some(way) = self.far(host).and_then(|f| f.way.as_ref()) {
                    way.answered(n, answer);
                }
            }
            Frame::Msg(Down::Pong) => {}
        }
    }

    /// A button of a waiting pane was clicked, or its key pressed.
    pub(super) fn far_button(&mut self, host: &str, button: FarButton) {
        let (base, http, tx) = (self.cfg.download_base.clone(), self.http.clone(), self.tx.clone());
        let Some(far) = self.far_mut(host) else { return };
        match (button, &far.stage) {
            (FarButton::Yes, Stage::Asking { asset, .. }) => {
                let Some(id) = far.reach.as_ref().map(|r| r.id) else { return };
                let asset = asset.clone();
                far.note(&format!("Finding {asset}…"));
                far.stage = Stage::Finding;
                tokio::spawn(async move {
                    let found = remote::copy::find(&asset, base.as_deref(), &http).await;
                    let _ = tx.send(Ev::Remote { reach: id, told: Told::Build(found) }).await;
                });
            }
            (FarButton::No, Stage::Asking { .. }) => {
                far.hang_up();
                far.note("Nothing was copied. Close this tab, or put Astrum there yourself and try again.");
            }
            (FarButton::Again, Stage::Down) => {
                far.tries = 0;
                self.far_call(host);
            }
            _ => {}
        }
        self.dirty = true;
    }

    /// A key for a pane that is waiting for its machine: an answer to
    /// `ssh`, or to Astrum's own question.
    pub(super) fn far_key(&mut self, host: &str, key: KeyEvent) {
        let Some(far) = self.far_mut(host) else { return };
        let plain = !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match far.stage {
            Stage::Asking { .. } => match key.code {
                KeyCode::Char('y' | 'Y') | KeyCode::Enter if plain => self.far_button(host, FarButton::Yes),
                KeyCode::Char('n' | 'N') | KeyCode::Esc if plain => self.far_button(host, FarButton::No),
                _ => {}
            },
            Stage::Down => {
                if matches!(key.code, KeyCode::Enter | KeyCode::Char('r' | 'R')) && plain {
                    self.far_button(host, FarButton::Again);
                }
            }
            Stage::Calling | Stage::Putting | Stage::Finding | Stage::Greeting => {
                if let (Some(reach), Some(bytes)) = (far.reach.as_mut(), crate::input::encode_key(&key, Default::default())) {
                    reach.typed(&bytes);
                }
            }
            Stage::Up => {}
        }
    }

    /// Text pasted into a waiting pane: for `ssh`, if it is asking.
    pub(super) fn far_paste(&mut self, host: &str, text: &str) {
        if let Some(far) = self.far_mut(host).filter(|f| matches!(f.stage, Stage::Calling)) {
            if let Some(reach) = far.reach.as_mut() {
                reach.typed(text.lines().next().unwrap_or("").as_bytes());
            }
        }
    }

    /// The session is going down without having been told to stop its
    /// shells: the ones on other machines are left running, to be found
    /// again when it is back.
    pub(super) fn keep_far_shells(&mut self) {
        for pane in self.tabs.iter_mut().flat_map(|t| t.panes.iter_mut()) {
            if let Some(there) = pane.there_mut() {
                there.keep = true;
            }
        }
    }

    /// Machines nothing is shown from any more are let go.
    pub(super) fn drop_unused_fars(&mut self) {
        let used: Vec<String> = self.tabs.iter().flat_map(|t| t.panes.iter()).filter_map(|p| p.there().map(|t| t.host.clone())).collect();
        self.fars.retain(|f| used.contains(&f.host));
    }
}
