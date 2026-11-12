//! A pane: one shell running on a PTY plus the terminal emulation state that
//! mirrors its screen.

use crate::inner::{self, Inner};
use crate::image::{self, Caps, Data, Dim, Image, KittyCmd, PLACEHOLDER, Piece, Protocol, Splitter};
use crate::osc::{self, Mark, Scanner};
use crate::server::Ev;
use alacritty_terminal::event::{Event as TermEvent, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Boundary, Column, Direction, Line, Point};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::{Cell, Flags, LineLength};
use alacritty_terminal::term::search::{Match, RegexIter, RegexSearch};
use alacritty_terminal::term::{Config as TermConfig, Term, TermMode, viewport_to_point};
use alacritty_terminal::vi_mode::ViMotion;
use alacritty_terminal::vte::ansi::{Color as CellColor, Handler, NamedColor, Processor, Rgb};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc as std_mpsc;
use std::time::Instant;
use tokio::sync::mpsc;

const MAX_BLOCKS: usize = 40;

struct Dims {
    cols: usize,
    lines: usize,
}

impl Dimensions for Dims {
    fn total_lines(&self) -> usize {
        self.lines
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

#[derive(Clone)]
pub struct Listener(std_mpsc::Sender<TermEvent>);

impl EventListener for Listener {
    fn send_event(&self, event: TermEvent) {
        let _ = self.0.send(event);
    }
}

#[derive(Debug, Clone)]
pub enum ShellState {
    /// No shell integration seen (yet); we cannot tell what the shell is doing.
    Unknown,
    /// Sitting at a prompt.
    Prompt,
    /// Running a command.
    Running { cmd: Option<String>, since: Instant },
}

/// A finished command with its output, as recorded by shell integration.
#[derive(Debug, Clone)]
pub struct Block {
    pub cmd: String,
    pub exit: Option<i32>,
    pub output: String,
    pub cwd: Option<String>,
}

pub enum PaneEvent {
    Bell,
    Clipboard(String),
    CommandStarted,
    CommandFinished(Block),
    PromptReady,
}

pub struct Pane {
    pub id: u64,
    /// How many of this pane's finished commands the agent has been shown.
    pub seen_seq: u64,
    pub term: Term<Listener>,
    parser: Processor,
    scanner: Scanner,
    master: Box<dyn MasterPty + Send>,
    writer: std_mpsc::Sender<Vec<u8>>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    term_events: std_mpsc::Receiver<TermEvent>,
    pub shell_name: String,
    pub osc_title: Option<String>,
    pub cwd: Option<String>,
    pub state: ShellState,
    pub integrated: bool,
    pub blocks: VecDeque<Block>,
    /// Total number of commands finished in this pane.
    pub block_seq: u64,
    pub cols: u16,
    pub rows: u16,
    pub default_fg: Rgb,
    pub default_bg: Rgb,
    /// What the cursor's line held when the user began typing at the current
    /// prompt, i.e. the prompt itself. Forgotten at each new prompt.
    prompt_text: Option<String>,
    /// When the shell or a program in it last wrote anything.
    pub last_output: Instant,
    /// The line typed at the current prompt and where the cursor is in it
    /// (a byte offset), for as long as that can be followed from the keys
    /// alone: typing, deleting and moving the cursor. `None` once something
    /// only the shell knows the effect of was pressed (history, completion).
    typed: Option<(String, usize)>,
    /// A line was sent off and the next prompt has not appeared yet.
    awaiting_prompt: bool,
    /// Keys were sent before the current prompt appeared; the shell may
    /// have put them on its line.
    typeahead: bool,
    /// Set while the keyboard is looking back through this pane.
    pub copy: Option<CopyMode>,
    /// The pattern for web addresses, built the first time one is looked for.
    addresses: Option<RegexSearch>,
    /// The shell's process.
    pid: Option<i32>,
    /// There has been output since the directory was last looked up.
    pub cwd_stale: bool,
    /// Takes pictures out of what the shell writes, before the emulator.
    splitter: Splitter,
    /// The pictures on this pane's screen and in its history, oldest first.
    pub images: Vec<Arc<Image>>,
    /// Pictures a program handed over in kitty's way, to place when it says.
    loaded: Vec<Loaded>,
    /// One of those still arriving in pieces: its command, and its data so far.
    arriving: Option<(KittyCmd, Vec<u8>)>,
    /// A file arriving in pieces in iTerm2's way: what has come of it.
    file_parts: Option<Vec<u8>>,
    /// How many pictures there have to be before the history is looked
    /// through for ones no longer in it.
    look_through_at: usize,
    /// Which ways of showing a picture the terminal in use understands, and
    /// the size of one of its cells in pixels (zero if it did not say).
    /// Programs that ask are answered from these.
    pub image_caps: Caps,
    pub cell_px: (u16, u16),
    /// The pane as it was last drawn, and what has changed since.
    pub drawn: std::cell::RefCell<Drawn>,
    /// The shells inside this pane's own that Astrum has been carried
    /// into, outermost first (see `crate::inner`).
    inner: Vec<Inner>,
    /// One it is being carried into now.
    carrying: Option<Carrying>,
    /// How the last carrying went, until the server has said so: where it
    /// arrived, or why it did not.
    pub carried: Option<Result<String, String>>,
}

/// Astrum on its way into a shell inside the pane's own.
struct Carrying {
    /// The secret typed in with it, which that shell's marks will carry.
    secret: String,
    /// What kind of shell it is, once it has said.
    kind: Option<inner::Kind>,
    /// When to give up waiting for its next answer.
    until: Instant,
    /// The row the first line was typed on (see `Pane::row_counted`).
    from_row: Option<usize>,
    /// The last line, the marks themselves, has been sent.
    sent: bool,
}

/// How long a shell is given to answer each line typed into it. (A slow
/// connection to the other side of the world is well inside this.)
const CARRY_WAIT: std::time::Duration = std::time::Duration::from_secs(8);

fn carry_wait() -> std::time::Duration {
    // (A test of a shell that never answers cannot wait that long.)
    if cfg!(debug_assertions) {
        if let Some(ms) = std::env::var("ASTRUM_CARRY_WAIT_MS").ok().and_then(|v| v.parse().ok()) {
            return std::time::Duration::from_millis(ms);
        }
    }
    CARRY_WAIT
}

/// A pane's cells as they were last drawn into a frame, kept so that the
/// next frame only has to work out the rows a program has written to
/// since. (Most frames change a line or two: a key echoed, a clock.)
#[derive(Default)]
pub struct Drawn {
    /// What the cells were made for: the size of the space they were
    /// drawn in and how colors were being shown. `None` when they are
    /// not to be relied on.
    pub made_for: Option<(u16, u16, bool, bool)>,
    pub cells: Vec<ratatui::buffer::Cell>,
    /// The rows written to since; every row, if `all`.
    pub rows: Vec<bool>,
    pub all: bool,
}

/// A picture handed over in kitty's way: the number the program gave it,
/// what it is, and the pictures on screen that are placements of it.
struct Loaded {
    number: u32,
    data: Arc<Data>,
    px: (u32, u32),
    placed: Vec<u32>,
}

/// Looking back through a pane with the keyboard: a cursor of its own that
/// moves over the screen and the scrollback, a selection it can stretch,
/// and a search.
pub struct CopyMode {
    pub find: Option<Find>,
    /// Keys go into the search text.
    pub typing: bool,
    /// It was opened to search, not to move a cursor about: Enter goes on
    /// to the next match and Escape closes it.
    pub search_only: bool,
}

/// A search through a pane's screen and scrollback.
pub struct Find {
    pub query: String,
    regex: Option<RegexSearch>,
    /// Where the search started from; each change to the text looks again
    /// from here.
    origin: Point,
    /// The matches on the part of the pane being shown.
    pub visible: Vec<Match>,
    /// How many matches there are in all, up to `MAX_COUNTED`.
    pub total: usize,
    /// Which of them the cursor is on, counting from the oldest; 0 for none.
    pub index: usize,
    counted: Instant,
}

/// Something in a pane that can be opened: a hyperlink a program marked as
/// one, or a web address that is simply there in the text.
#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub uri: String,
    /// The cells of an address found in the text. `None` for a marked
    /// hyperlink, which is every cell that carries the same target.
    pub cells: Option<Match>,
}

/// What an address in running text looks like.
const ADDRESS: &str = r#"(https?://|mailto:)[^\s<>"'`{}|\\^]+"#;

/// Can this be handed to the system to open? A web page or a mail address,
/// and nothing else. Not a file: what the text of a link says and where it
/// leads are both up to whatever printed it, and opening a file can mean
/// running it.
pub fn openable(uri: &str) -> bool {
    let lower = uri.to_ascii_lowercase();
    ["http://", "https://", "mailto:"].iter().any(|scheme| lower.starts_with(scheme)) && !uri.chars().any(|c| c.is_control() || c.is_whitespace())
}

/// Counting stops here; a search with more matches says "9999+".
pub const MAX_COUNTED: usize = 9999;

pub struct SpawnOpts<'a> {
    pub id: u64,
    pub cols: u16,
    pub rows: u16,
    pub cwd: Option<&'a str>,
    pub shell: Option<&'a str>,
    pub shell_dir: &'a Path,
    pub session: &'a str,
    pub scrollback: usize,
}

/// Where the cursor is left once a picture has been drawn.
#[derive(Clone, Copy, PartialEq)]
enum After {
    /// Just past it, on its last row (iTerm2's way, and kitty's).
    Beside,
    /// At the start of the line below it (sixel).
    Below,
    /// Where it was (kitty, when a program asks).
    Stay,
}

/// The picture, row and column a cell stands for, if it is one of a
/// picture's cells (see `image::PLACEHOLDER`).
pub fn marked(cell: &Cell) -> Option<(u32, u16, u16)> {
    match (cell.c, cell.fg, cell.bg) {
        (PLACEHOLDER, CellColor::Spec(fg), CellColor::Spec(bg)) => Some(image::unmark([fg.r, fg.g, fg.b], [bg.r, bg.g, bg.b])),
        _ => None,
    }
}

/// Text taken from the screen, without the marks pictures leave in it.
fn without_pictures(text: String) -> String {
    if text.contains(PLACEHOLDER) { text.replace(PLACEHOLDER, " ") } else { text }
}

impl Pane {
    pub fn spawn(o: SpawnOpts, tx: mpsc::Sender<Ev>) -> anyhow::Result<Pane> {
        let (cols, rows) = (o.cols.max(2), o.rows.max(1));
        let pair = native_pty_system().openpty(PtySize { rows, cols, pixel_width: cols.saturating_mul(8), pixel_height: rows.saturating_mul(16) })?;

        let shell = o
            .shell
            .map(str::to_string)
            .or_else(|| std::env::var("SHELL").ok().filter(|s| !s.is_empty()))
            .unwrap_or_else(|| "/bin/sh".to_string());
        let shell_name =
            Path::new(&shell).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "sh".into());

        let mut cmd = CommandBuilder::new(&shell);
        match shell_name.as_str() {
            "zsh" => {
                if let Ok(orig) = std::env::var("ZDOTDIR") {
                    cmd.env("ASTRUM_ORIG_ZDOTDIR", orig);
                }
                cmd.env("ZDOTDIR", o.shell_dir.join("zsh"));
                cmd.arg("-l");
            }
            "bash" => {
                cmd.arg("--rcfile");
                cmd.arg(o.shell_dir.join("astrum.bash"));
            }
            "fish" => {
                cmd.arg("-l");
                cmd.arg("--init-command");
                cmd.arg(format!("source '{}'", o.shell_dir.join("astrum.fish").display()));
            }
            _ => cmd.arg("-l"),
        }
        cmd.env("ASTRUM_SHELL_DIR", o.shell_dir);
        let nonce = random_hex();
        cmd.env("ASTRUM_NONCE", &nonce);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "astrum");
        cmd.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
        cmd.env("ASTRUM", "1");
        cmd.env("ASTRUM_SESSION", o.session);
        cmd.env("ASTRUM_TAB", o.id.to_string());
        // Don't let programs believe they talk to the host terminal directly.
        for var in [
            "ITERM_SESSION_ID",
            "LC_TERMINAL",
            "LC_TERMINAL_VERSION",
            "TERM_SESSION_ID",
            "WEZTERM_PANE",
            "KITTY_WINDOW_ID",
            "KITTY_PID",
            "TMUX",
            "TMUX_PANE",
            "STY",
            "ZELLIJ",
            "ZELLIJ_SESSION_NAME",
            "WT_SESSION",
            "COLUMNS",
            "LINES",
        ] {
            cmd.env_remove(var);
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
        let cwd = o.cwd.filter(|d| Path::new(d).is_dir()).unwrap_or(&home);
        cmd.cwd(cwd);
        // Keep the logical path ($PWD) the shell shows consistent with where we start it.
        cmd.env("PWD", cwd);

        let mut child = pair.slave.spawn_command(cmd)?;
        let pid = child.process_id().map(|p| p as i32);
        drop(pair.slave);
        let killer = child.clone_killer();
        let master = pair.master;
        let id = o.id;

        // Reader: PTY -> server loop. The bounded channel gives backpressure,
        // so a flood of output cannot outrun parsing.
        let mut reader = master.try_clone_reader()?;
        let tx_out = tx.clone();
        std::thread::Builder::new().name(format!("pty-read-{id}")).spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx_out.blocking_send(Ev::Pty { pane: id, data: buf[..n].to_vec() }).is_err() {
                            break;
                        }
                    }
                }
            }
        })?;

        // Writer: a blocked child must never stall the server loop.
        let mut pty_writer = master.take_writer()?;
        let (wtx, wrx) = std_mpsc::channel::<Vec<u8>>();
        std::thread::Builder::new().name(format!("pty-write-{id}")).spawn(move || {
            while let Ok(bytes) = wrx.recv() {
                if pty_writer.write_all(&bytes).is_err() || pty_writer.flush().is_err() {
                    break;
                }
            }
        })?;

        let tx_exit = tx;
        std::thread::Builder::new().name(format!("pty-wait-{id}")).spawn(move || {
            // A shell that left of its own accord says so with an exit
            // code; one that was killed has a signal instead.
            let killed = child.wait().map(|status| status.signal().is_some()).unwrap_or(false);
            let _ = tx_exit.blocking_send(Ev::PtyExit { pane: id, killed });
        })?;

        let (etx, erx) = std_mpsc::channel();
        // (Programs may ask for enhanced key reports; see `input::encode_key`.)
        let config = TermConfig { scrolling_history: o.scrollback, kitty_keyboard: true, ..TermConfig::default() };
        let term = Term::new(config, &Dims { cols: cols as usize, lines: rows as usize }, Listener(etx));

        Ok(Pane {
            id,
            seen_seq: 0,
            term,
            parser: Processor::new(),
            scanner: Scanner::with_nonce(nonce),
            master,
            writer: wtx,
            killer,
            term_events: erx,
            shell_name,
            osc_title: None,
            cwd: Some(cwd.to_string()),
            state: ShellState::Unknown,
            integrated: false,
            blocks: VecDeque::new(),
            block_seq: 0,
            cols,
            rows,
            default_fg: Rgb { r: 0xd8, g: 0xd8, b: 0xe0 },
            default_bg: Rgb { r: 0x12, g: 0x12, b: 0x1a },
            prompt_text: None,
            last_output: Instant::now(),
            typed: None,
            awaiting_prompt: false,
            typeahead: false,
            copy: None,
            addresses: None,
            pid,
            cwd_stale: false,
            splitter: Splitter::default(),
            images: Vec::new(),
            loaded: Vec::new(),
            arriving: None,
            file_parts: None,
            look_through_at: 8,
            image_caps: Caps::default(),
            cell_px: (0, 0),
            drawn: std::cell::RefCell::new(Drawn { all: true, ..Drawn::default() }),
            inner: Vec::new(),
            carrying: None,
            carried: None,
        })
    }

    pub fn write(&self, bytes: impl Into<Vec<u8>>) {
        let _ = self.writer.send(bytes.into());
    }

    pub fn mode(&self) -> TermMode {
        *self.term.mode()
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        let (cols, rows) = (cols.max(2), rows.max(1));
        if cols == self.cols && rows == self.rows {
            return;
        }
        self.cols = cols;
        self.rows = rows;
        self.term.resize(Dims { cols: cols as usize, lines: rows as usize });
        self.redraw();
        let _ = self.master.resize(self.pty_size());
    }

    /// Nothing kept of how the pane was last drawn is to be relied on.
    pub fn redraw(&mut self) {
        self.drawn.get_mut().all = true;
    }

    /// Take note of the rows the emulator has written to since it was last
    /// asked. (It keeps count as it goes; this empties its list into ours.)
    fn note_changes(&mut self) {
        use alacritty_terminal::term::TermDamage;
        let drawn = self.drawn.get_mut();
        // Looking back through the history, rows are not where they were.
        let scrolled = self.term.grid().display_offset() != 0;
        match self.term.damage() {
            TermDamage::Full => drawn.all = true,
            TermDamage::Partial(_) if scrolled => drawn.all = true,
            TermDamage::Partial(lines) => {
                for line in lines {
                    if drawn.rows.len() <= line.line {
                        drawn.rows.resize(line.line + 1, false);
                    }
                    drawn.rows[line.line] = true;
                }
            }
        }
        self.term.reset_damage();
    }

    /// The size of a cell as programs are told it: what the terminal in
    /// use said, or a usual size where it said nothing. (Never nothing:
    /// there are programs that divide by it.)
    fn cell_told(&self) -> (u16, u16) {
        if self.cell_px.0 > 0 && self.cell_px.1 > 0 { self.cell_px } else { (8, 16) }
    }

    /// The size programs are told: in cells, and in pixels.
    fn pty_size(&self) -> PtySize {
        let (w, h) = self.cell_told();
        PtySize { rows: self.rows, cols: self.cols, pixel_width: self.cols.saturating_mul(w), pixel_height: self.rows.saturating_mul(h) }
    }

    /// What the terminal in use can show, and the size of its cells.
    pub fn set_image_support(&mut self, caps: Caps, cell_px: (u16, u16)) {
        self.image_caps = caps;
        if cell_px != self.cell_px {
            self.cell_px = cell_px;
            let _ = self.master.resize(self.pty_size());
        }
    }

    /// Feed PTY output through the emulator and the shell-integration scanner.
    pub fn feed(&mut self, data: &[u8], out: &mut Vec<PaneEvent>) {
        self.last_output = Instant::now();
        self.cwd_stale = true;
        // Pictures are taken out first: the emulator would drop them.
        for piece in self.splitter.feed(data) {
            match piece {
                Piece::Text(range) => self.feed_text(&data[range], out),
                Piece::Held(bytes) => self.feed_text(&bytes, out),
                Piece::Picture(protocol, body) => self.picture(protocol, body),
                Piece::AsksCellSize => {
                    let (width, height) = self.cell_told();
                    self.write(format!("\x1b[6;{height};{width}t").into_bytes());
                }
            }
        }
        self.answer_the_emulator(out);
    }

    fn feed_text(&mut self, data: &[u8], out: &mut Vec<PaneEvent>) {
        self.parser.advance(&mut self.term, data);
        self.note_changes();

        let mut marks = Vec::new();
        self.scanner.feed(data, &mut marks);
        for mark in marks {
            self.integrated = true;
            // Whose mark it is: the pane's own shell's, or that of a shell inside it.
            let (level, mark) = match mark {
                Mark::Inner { level, mark } => (level, *mark),
                own => (0, own),
            };
            // A shell further out is heard from: the ones inside it have ended.
            if level < self.inner.len() {
                self.inner.truncate(level);
                self.scanner.keep_inner(level);
                self.carrying = None;
            }
            match mark {
                Mark::PromptStart => {
                    self.prompt_text = None;
                    self.typed = (!std::mem::take(&mut self.typeahead)).then(|| (String::new(), 0));
                    self.awaiting_prompt = false;
                    self.state = ShellState::Prompt;
                    out.push(PaneEvent::PromptReady);
                }
                Mark::OutputStart { cmd } => {
                    self.prompt_text = None;
                    self.state = ShellState::Running { cmd, since: Instant::now() };
                    out.push(PaneEvent::CommandStarted);
                }
                Mark::CmdEnd { exit, output } => {
                    if let ShellState::Running { cmd, .. } = &self.state {
                        let block = Block {
                            cmd: cmd.clone().unwrap_or_default(),
                            exit,
                            output: output.map(|c| c.text()).unwrap_or_default(),
                            cwd: self.cwd_shown().map(str::to_string),
                        };
                        let mut stored = block.clone();
                        stored.output = osc::truncate_lines(&stored.output, 30, 30);
                        if self.blocks.len() >= MAX_BLOCKS {
                            self.blocks.pop_front();
                        }
                        self.blocks.push_back(stored);
                        self.block_seq += 1;
                        out.push(PaneEvent::CommandFinished(block));
                    }
                    self.state = ShellState::Prompt;
                }
                // The directory of a shell inside is that shell's, on
                // whatever machine it runs: the pane's own stays what it was.
                Mark::Cwd(path) => match level.checked_sub(1).and_then(|at| self.inner.get_mut(at)) {
                    Some(inner) => inner.cwd = Some(path),
                    None => self.cwd = Some(path),
                },
                Mark::Inner { .. } => {}
            }
        }

    }

    /// What the emulator wants said back to the program, or passed on.
    fn answer_the_emulator(&mut self, out: &mut Vec<PaneEvent>) {
        while let Ok(ev) = self.term_events.try_recv() {
            match ev {
                // "What kind of terminal are you?" The emulator's answer says
                // nothing of pictures; a program looking for sixel reads it there.
                TermEvent::PtyWrite(text) if text == "\x1b[?6c" && self.image_caps.sixel => self.write(b"\x1b[?62;4c".to_vec()),
                TermEvent::PtyWrite(text) => self.write(text.into_bytes()),
                TermEvent::Title(t) => self.osc_title = Some(t),
                TermEvent::ResetTitle => self.osc_title = None,
                TermEvent::ClipboardStore(_, text) => out.push(PaneEvent::Clipboard(text)),
                TermEvent::ColorRequest(index, fmt) => {
                    let color = self.term.colors()[index].unwrap_or(match index {
                        256 | 258 => self.default_fg,
                        257 => self.default_bg,
                        _ => self.default_fg,
                    });
                    self.write(fmt(color).into_bytes());
                }
                TermEvent::TextAreaSizeRequest(fmt) => {
                    let (cell_width, cell_height) = self.cell_told();
                    let size = WindowSize { num_lines: self.rows, num_cols: self.cols, cell_width, cell_height };
                    self.write(fmt(size).into_bytes());
                }
                TermEvent::Bell => out.push(PaneEvent::Bell),
                _ => {}
            }
        }
    }

    // ---- a shell inside this one ------------------------------------------------

    /// The innermost shell Astrum has been carried into, if the pane's own
    /// shell has led to one (see `crate::inner`).
    pub fn inner(&self) -> Option<&Inner> {
        self.inner.last()
    }

    /// The directory of the shell that has the prompt: on another machine,
    /// when that shell is.
    pub fn cwd_shown(&self) -> Option<&str> {
        match self.inner.last() {
            Some(inner) => inner.cwd.as_deref(),
            None => self.cwd.as_deref(),
        }
    }

    pub fn carrying(&self) -> bool {
        self.carrying.is_some()
    }

    /// Whether to offer, unasked, to carry Astrum into what is running:
    /// a program known to end in another shell, and nothing begun yet.
    pub fn offers_inner(&self) -> bool {
        self.carrying.is_none() && !self.mode().contains(TermMode::ALT_SCREEN) && matches!(&self.state, ShellState::Running { cmd: Some(cmd), .. } if inner::leads_to_a_shell(cmd))
    }

    /// Carry Astrum into the shell the pane has reached: type the line that
    /// asks what shell it is. The rest follows from its answers.
    pub fn carry_in(&mut self) -> Result<(), &'static str> {
        if self.carrying.is_some() {
            return Err("Already on its way: waiting for the shell there to answer");
        }
        if self.mode().contains(TermMode::ALT_SCREEN) {
            return Err("A full-screen program has this pane: leave it first");
        }
        match self.state {
            ShellState::Running { .. } => {}
            ShellState::Prompt => return Err("The agent already works in the shell at this prompt. Start `ssh` (or whatever leads to the other shell) first"),
            ShellState::Unknown => return Err("This pane's own shell is not followed, so one inside it cannot be"),
        }
        let secret = random_hex();
        // Whatever is half typed there goes first.
        let mut typed = vec![0x15];
        typed.extend_from_slice(inner::probe(&secret).as_bytes());
        typed.push(b'\r');
        self.scroll_to_bottom();
        self.write(typed);
        self.carrying = Some(Carrying { secret, kind: None, until: Instant::now() + carry_wait(), from_row: self.row_counted(), sent: false });
        Ok(())
    }

    /// Which row the cursor is on, counting every row there has been: the
    /// same number for the same row however far it has scrolled. (Once
    /// the history is full, rows are dropped as others are added and the
    /// count stops rising; what is worked out from it is then too little,
    /// which here means less is tidied away than might be.)
    fn row_counted(&self) -> Option<usize> {
        let grid = self.term.grid();
        Some(grid.history_size() + grid.cursor.point.line.0.max(0) as usize)
    }

    /// The shell being carried into has answered something typed. (An
    /// answer has to carry the secret that was typed in with the question.)
    fn carrying_answer(&mut self, answer: &str) {
        let Some(carrying) = &mut self.carrying else { return };
        let Some(answer) = answer.strip_prefix("k=").and_then(|rest| rest.strip_prefix(carrying.secret.as_str())).and_then(|rest| rest.strip_prefix(';')) else { return };
        carrying.until = Instant::now() + carry_wait();
        let secret = carrying.secret.clone();
        if let (Some(rest), Some(kind)) = (answer.strip_prefix("ok;"), carrying.kind) {
            // The marks are in place there. What was typed to put them
            // there is of no more use to anyone: the rows it took are
            // cleared, and the shell's next prompt lands where the first
            // of them was. (Only in this pane's own picture of the screen;
            // the shell there is none the wiser, and need not be.)
            let from = carrying.from_row;
            if let (Some(from), Some(now)) = (from, self.row_counted()) {
                let up = now.saturating_sub(from).min(self.rows as usize - 1);
                let tidy = if up > 0 { format!("\x1b[{up}A\r\x1b[J") } else { "\r\x1b[J".to_string() };
                self.parser.advance(&mut self.term, tidy.as_bytes());
                self.note_changes();
            }
            let arrived = inner::arrived(kind, rest);
            self.carried = Some(Ok(arrived.label()));
            self.scanner.arrived(secret);
            self.inner.push(arrived);
            self.carrying = None;
            // The line that did this is the last thing that shell was
            // running: its prompt is next, and says so itself in a moment.
            self.state = ShellState::Prompt;
        } else if answer == "ready" {
            // It is reading, and not showing what it reads: the marks
            // themselves. (Sent once, however often it says it is ready.)
            if let Some(kind) = carrying.kind.filter(|_| !std::mem::replace(&mut carrying.sent, true)) {
                let mut line = inner::script(kind, &secret).into_bytes();
                line.push(b'\r');
                self.write(line);
            }
        } else if carrying.kind.is_none() {
            // What kind of shell it is.
            match inner::kind_from(answer) {
                Some(kind) => {
                    carrying.kind = Some(kind);
                    let mut line = inner::reader(kind, &secret).into_bytes();
                    line.push(b'\r');
                    self.write(line);
                }
                None => self.carry_failed("The shell there is not bash, zsh or fish, which are the ones the agent can work in"),
            }
        }
    }

    fn carry_failed(&mut self, why: &str) {
        self.carrying = None;
        self.carried = Some(Err(why.to_string()));
    }

    /// Give up on a shell that has not answered in time. True if that
    /// happened just now.
    pub fn carry_overdue(&mut self, now: Instant) -> bool {
        let overdue = self.carrying.as_ref().is_some_and(|carrying| now >= carrying.until);
        if overdue {
            self.carry_failed("Nothing there answered. The agent can be carried into bash, zsh or fish at a prompt; a line was typed that may need clearing");
        }
        overdue
    }

    // ---- pictures -------------------------------------------------------------

    /// A picture sequence a program wrote.
    fn picture(&mut self, protocol: Protocol, body: Vec<u8>) {
        // Text a synchronized update is holding back was written first.
        if self.parser.sync_bytes_count() > 0 {
            self.parser.stop_sync(&mut self.term);
            self.note_changes();
        }
        match protocol {
            Protocol::Iterm => self.file(&body),
            // The same file in several sequences: what it is, then its
            // data piece by piece, then the end.
            Protocol::ItermBegin => {
                let mut whole = body;
                whole.push(b':');
                self.file_parts = Some(whole);
            }
            Protocol::ItermPart => {
                if let Some(mut whole) = self.file_parts.take().filter(|whole| whole.len() + body.len() <= image::MAX_PICTURE) {
                    whole.extend_from_slice(&body);
                    self.file_parts = Some(whole);
                }
            }
            Protocol::ItermEnd => {
                if let Some(whole) = self.file_parts.take() {
                    self.file(&whole);
                }
            }
            Protocol::Sixel => {
                let px = image::sixel_size(&body);
                if px.0 == 0 || px.1 == 0 || !image::is_sixel(&body) {
                    return;
                }
                let room = self.room(false);
                // It is drawn pixel for pixel. Where that cannot be made to
                // fit the cells, it is given room and never drawn (no cell
                // size is one that any terminal has).
                let (cells, measured) = match image::sixel_footprint(px, self.cell_px, room) {
                    Some(cells) => (cells, self.cell_px),
                    None => (image::footprint(px, self.cell_px, (Dim::Auto, Dim::Auto), room), (0, 0)),
                };
                self.show(Arc::new(Data::Sixel { body }), px, cells, measured, After::Below);
            }
            Protocol::Kitty => self.kitty(KittyCmd::parse(&body)),
            // (Not a picture at all: see `Protocol::Answer`.)
            Protocol::Answer => self.carrying_answer(&String::from_utf8_lossy(&body)),
        }
    }

    /// A file sent in iTerm2's way, to be shown where the cursor is.
    fn file(&mut self, body: &[u8]) {
        let Some(file) = image::parse_iterm(body) else { return };
        let cells = image::footprint(file.px, self.cell_px, (file.width, file.height), self.room(false));
        let data = Arc::new(Data::File { b64: file.b64, kind: file.kind });
        self.show(data, file.px, cells, self.cell_px, After::Beside);
    }

    /// The columns and rows there are for a picture at the cursor. One that
    /// leaves the cursor where it is has to fit below it; any other may
    /// scroll the screen, and gets all but one row of it.
    fn room(&self, stay: bool) -> (u16, u16) {
        let at = self.term.grid().cursor.point;
        let cols = self.cols.saturating_sub(at.column.0 as u16).max(1);
        let rows = if stay { self.rows.saturating_sub(at.line.0.max(0) as u16) } else { self.rows.saturating_sub(1) };
        (cols, rows.max(1))
    }

    /// Keep a picture and mark the cells it covers, from the cursor.
    fn show(&mut self, data: Arc<Data>, px: (u32, u32), cells: (u16, u16), cell_px: (u16, u16), after: After) -> u32 {
        let image = Arc::new(Image { id: image::next_id(), data, px, cells, cell_px });
        let id = image.id;
        // (Cells are written straight into the grid below, which the
        // emulator keeps no count of.)
        self.redraw();
        // What the agent reads of a command's output says a picture was here.
        if let Some(capture) = &mut self.scanner.capture {
            capture.note(&format!("[{}]\n", image.label()));
        }
        self.keep(image);

        let (cols, rows) = cells;
        let start = self.term.grid().cursor.point;
        let left = start.column.0;
        for row in 0..rows {
            let line = self.term.grid().cursor.point.line;
            for col in 0..cols {
                let (fg, bg) = image::mark(id, row, col);
                let Some(at) = Some(left + col as usize).filter(|at| *at < self.cols as usize) else { break };
                let cell = &mut self.term.grid_mut()[line][Column(at)];
                *cell = Cell::default();
                cell.c = PLACEHOLDER;
                cell.fg = CellColor::Spec(Rgb { r: fg[0], g: fg[1], b: fg[2] });
                cell.bg = CellColor::Spec(Rgb { r: bg[0], g: bg[1], b: bg[2] });
            }
            if row + 1 < rows {
                if after == After::Stay {
                    self.term.grid_mut().cursor.point.line = line + 1;
                } else {
                    // As a real terminal does: the screen scrolls to make room.
                    self.term.linefeed();
                }
            }
        }
        let cursor = &mut self.term.grid_mut().cursor;
        cursor.input_needs_wrap = false;
        match after {
            After::Stay => cursor.point = start,
            After::Beside => {
                // Past the last column there is nowhere to be but on the
                // picture's own last cell: what is written next goes to
                // the next line, as it would after text that filled the row.
                let past = left + cols as usize;
                cursor.point.column = Column(past.min(self.cols as usize - 1));
                cursor.input_needs_wrap = past >= self.cols as usize;
            }
            After::Below => {
                self.term.linefeed();
                self.term.grid_mut().cursor.point.column = Column(0);
            }
        }
        id
    }

    /// Add to the pictures kept, letting go of those no longer anywhere in
    /// the pane and, past the limits, of the oldest.
    fn keep(&mut self, image: Arc<Image>) {
        // (While a full-screen program has the screen, the one behind it
        // cannot be looked through: nothing is let go on that account.)
        // (And not for every picture: the whole history is looked through,
        // so it is done when there are twice as many as were left the last time.)
        if self.images.len() >= self.look_through_at && !self.mode().contains(TermMode::ALT_SCREEN) {
            let grid = self.term.grid();
            let mut used = std::collections::HashSet::new();
            for line in grid.topmost_line().0..=grid.bottommost_line().0 {
                let row = &grid[Line(line)];
                for col in 0..grid.columns() {
                    if let Some((id, _, _)) = marked(&row[Column(col)]) {
                        used.insert(id);
                    }
                }
            }
            self.images.retain(|image| used.contains(&image.id));
            self.look_through_at = (self.images.len() * 2).max(8);
        }
        self.images.push(image);
        let total = |images: &[Arc<Image>]| images.iter().map(|i| i.bytes()).sum::<usize>();
        while self.images.len() > image::MAX_KEPT || (self.images.len() > 1 && total(&self.images) > image::MAX_KEPT_BYTES) {
            self.images.remove(0);
        }
        // Placements that are no longer kept are no longer remembered.
        let images = &self.images;
        for loaded in &mut self.loaded {
            loaded.placed.retain(|id| images.iter().any(|image| image.id == *id));
        }
    }

    /// A command in kitty's graphics protocol.
    fn kitty(&mut self, cmd: KittyCmd) {
        // The rest of a picture that is arriving in pieces.
        if let Some((first, mut data)) = self.arriving.take() {
            data.extend_from_slice(&cmd.payload);
            if data.len() > image::MAX_PICTURE {
                return;
            }
            if cmd.number('m') == 1 {
                self.arriving = Some((first, data));
            } else {
                self.kitty_whole(first, data);
            }
            return;
        }
        if cmd.number('m') == 1 {
            let data = cmd.payload.clone();
            self.arriving = Some((cmd, data));
            return;
        }
        let data = cmd.payload.clone();
        self.kitty_whole(cmd, data);
    }

    fn kitty_whole(&mut self, cmd: KittyCmd, data: Vec<u8>) {
        let action = cmd.letter('a', 't');
        let (number, quiet) = (cmd.number('i'), cmd.number('q'));
        // A program is answered only about pictures it gave a number to, and
        // not at all if it asked for quiet.
        let reply = |pane: &Pane, text: &str| {
            let ok = text == "OK";
            if number != 0 && quiet < if ok { 1 } else { 2 } {
                pane.write(format!("\x1b_Gi={number};{text}\x1b\\").into_bytes());
            }
        };
        match action {
            // "Can you show pictures?" Not answering is how a terminal says
            // no, and the program then draws with characters instead.
            'q' if self.image_caps.any() => match self.kitty_data(&cmd, data) {
                Ok(_) => reply(self, "OK"),
                Err(why) => reply(self, why),
            },
            't' | 'T' => {
                let (data, px) = match self.kitty_data(&cmd, data) {
                    Ok(read) => read,
                    Err(why) => return reply(self, why),
                };
                // A picture sent again under a number in use takes the
                // place of the old one, wherever that had been put.
                if number != 0 {
                    let old: Vec<u32> = self.loaded.iter().filter(|l| l.number == number).flat_map(|l| l.placed.iter().copied()).collect();
                    if !old.is_empty() {
                        self.clear_pictures(Some(&old));
                    }
                }
                self.loaded.retain(|l| number == 0 || l.number != number);
                self.loaded.push(Loaded { number, data: Arc::new(data), px, placed: Vec::new() });
                let size = |loaded: &[Loaded]| loaded.iter().map(|l| l.data.bytes()).sum::<usize>();
                while self.loaded.len() > image::MAX_KEPT || (self.loaded.len() > 1 && size(&self.loaded) > image::MAX_KEPT_BYTES) {
                    self.loaded.remove(0);
                }
                if action == 'T' {
                    self.kitty_place(self.loaded.len() - 1, &cmd);
                    // One with no number can never be placed again.
                    if number == 0 {
                        self.loaded.pop();
                    }
                }
                reply(self, "OK");
            }
            'p' => match self.loaded.iter().position(|l| number != 0 && l.number == number) {
                Some(at) => {
                    self.kitty_place(at, &cmd);
                    reply(self, "OK");
                }
                None => reply(self, "ENOENT:no such image"),
            },
            'd' => {
                let what = cmd.letter('d', 'a');
                let ids: Option<Vec<u32>> = match what.to_ascii_lowercase() {
                    'a' => None,
                    'i' => Some(self.loaded.iter().filter(|l| l.number == number).flat_map(|l| l.placed.iter().copied()).collect()),
                    // By position, by layer and the rest: not followed here.
                    _ => return,
                };
                self.clear_pictures(ids.as_deref());
                // A capital letter also gives the picture itself up.
                if what.is_ascii_uppercase() {
                    self.loaded.retain(|l| !(what == 'A' || l.number == number));
                }
            }
            _ => {}
        }
    }

    /// What a kitty command carries: the picture, and its size in pixels.
    fn kitty_data(&self, cmd: &KittyCmd, b64: Vec<u8>) -> Result<(Data, (u32, u32)), &'static str> {
        // Only sent along the terminal's own connection: a file or shared
        // memory named by a program is not read.
        if cmd.letter('t', 'd') != 'd' {
            return Err("ENOTSUP:only direct transmission is supported");
        }
        if !image::is_base64(&b64) {
            return Err("EINVAL:the data is not base64");
        }
        match cmd.text('f').map_or(32, |f| f.parse().unwrap_or(0)) {
            100 => {
                let (kind, px) = image::file_size(&b64);
                if kind != image::FileKind::Png {
                    return Err("EBADPNG:not a PNG");
                }
                Ok((Data::File { b64, kind }, px))
            }
            format @ (24 | 32) => {
                let px = (cmd.number('s'), cmd.number('v'));
                if px.0 == 0 || px.1 == 0 {
                    return Err("EINVAL:the size is missing");
                }
                Ok((Data::Pixels { b64, alpha: format == 32, compressed: cmd.letter('o', ' ') == 'z' }, px))
            }
            _ => Err("EINVAL:unknown format"),
        }
    }

    /// Put a picture that was handed over where the cursor is.
    fn kitty_place(&mut self, at: usize, cmd: &KittyCmd) {
        // A placement inside other text (a "virtual" one) is not followed.
        if cmd.number('U') == 1 {
            return;
        }
        let stay = cmd.number('C') == 1;
        let cells = |n: u32| if n == 0 { Dim::Auto } else { Dim::Cells(n) };
        let (data, px) = (self.loaded[at].data.clone(), self.loaded[at].px);
        let size = image::footprint(px, self.cell_px, (cells(cmd.number('c')), cells(cmd.number('r'))), self.room(stay));
        let id = self.show(data, px, size, self.cell_px, if stay { After::Stay } else { After::Beside });
        self.loaded[at].placed.push(id);
    }

    /// Take pictures off the screen: these, or all of them. (Not out of the
    /// history, which a program cannot reach either.)
    fn clear_pictures(&mut self, ids: Option<&[u32]>) {
        self.redraw();
        let (lines, cols) = (self.term.grid().screen_lines(), self.term.grid().columns());
        for line in 0..lines as i32 {
            for col in 0..cols {
                let cell = &mut self.term.grid_mut()[Line(line)][Column(col)];
                if marked(cell).is_some_and(|(id, _, _)| ids.is_none_or(|ids| ids.contains(&id))) {
                    *cell = Cell::default();
                }
            }
        }
    }

    /// The picture with this number, if this pane has it.
    pub fn image(&self, id: u32) -> Option<&Arc<Image>> {
        self.images.iter().find(|image| image.id == id)
    }

    /// A program can open a synchronized update and never close it; flush the
    /// buffered bytes once the emulator's timeout has passed.
    pub fn flush_stale_sync(&mut self) -> bool {
        if self.parser.sync_bytes_count() == 0 {
            return false;
        }
        match self.parser.sync_timeout().sync_timeout() {
            Some(deadline) if deadline <= Instant::now() => {
                self.parser.stop_sync(&mut self.term);
                self.note_changes();
                true
            }
            _ => false,
        }
    }

    pub fn sync_pending(&self) -> bool {
        self.parser.sync_bytes_count() > 0
    }

    /// Output captured so far from the command that is still running.
    pub fn partial_output(&self) -> String {
        if self.mode().contains(TermMode::ALT_SCREEN) {
            return self.recent_text(self.rows as usize);
        }
        self.scanner.capture.as_ref().map(|c| c.text()).unwrap_or_default()
    }

    pub fn is_busy(&self) -> bool {
        matches!(self.state, ShellState::Running { .. })
    }

    pub fn running_cmd(&self) -> Option<&str> {
        match &self.state {
            ShellState::Running { cmd, .. } => cmd.as_deref(),
            _ => None,
        }
    }

    // ---- scrollback ---------------------------------------------------------

    pub fn scroll(&mut self, delta: i32) {
        self.term.scroll_display(Scroll::Delta(delta));
    }

    pub fn scroll_to_bottom(&mut self) {
        if self.term.grid().display_offset() != 0 {
            self.term.scroll_display(Scroll::Bottom);
        }
    }

    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    // ---- selection ----------------------------------------------------------

    fn point_at(&self, col: u16, row: u16) -> Point {
        let col = (col as usize).min(self.cols as usize - 1);
        let row = (row as usize).min(self.rows as usize - 1);
        viewport_to_point(self.display_offset(), Point::new(row, Column(col)))
    }

    pub fn start_selection(&mut self, col: u16, row: u16, ty: SelectionType) {
        let p = self.point_at(col, row);
        self.term.selection = Some(Selection::new(ty, p, Direction::Left));
    }

    pub fn update_selection(&mut self, col: u16, row: u16) {
        let p = self.point_at(col, row);
        if let Some(sel) = &mut self.term.selection {
            sel.update(p, Direction::Right);
        }
    }

    pub fn clear_selection(&mut self) -> bool {
        self.term.selection.take().is_some()
    }

    pub fn selection_text(&self) -> Option<String> {
        self.term.selection_to_string().map(without_pictures).filter(|s| !s.trim().is_empty())
    }

    // ---- copy mode and search -------------------------------------------------

    /// Should the mouse go to the program in this pane?
    pub fn wants_mouse(&self) -> bool {
        self.copy.is_none() && crate::input::wants_mouse(self.mode())
    }

    /// Start looking back through the pane; with `find`, start by searching.
    pub fn enter_copy(&mut self, find: bool) {
        if self.copy.is_none() {
            self.term.selection = None;
            if !self.term.mode().contains(TermMode::VI) {
                self.term.toggle_vi_mode();
            }
            self.copy = Some(CopyMode { find: None, typing: false, search_only: find });
        }
        let origin = self.term.vi_mode_cursor.point;
        let Some(copy) = &mut self.copy else { return };
        if find {
            let find = copy.find.get_or_insert_with(|| Find {
                query: String::new(),
                regex: None,
                origin,
                visible: Vec::new(),
                total: 0,
                index: 0,
                counted: Instant::now(),
            });
            find.origin = origin;
            copy.typing = true;
        }
    }

    /// Back to the shell, at the bottom.
    pub fn leave_copy(&mut self) {
        if self.copy.take().is_none() {
            return;
        }
        if self.term.mode().contains(TermMode::VI) {
            self.term.toggle_vi_mode();
        }
        self.term.selection = None;
        self.scroll_to_bottom();
    }

    pub fn copy_motion(&mut self, motion: ViMotion) {
        self.term.vi_motion(motion);
        let point = self.term.vi_mode_cursor.point;
        self.term.scroll_to_point(point);
    }

    /// Move the cursor and the view by a number of lines (up for positive).
    pub fn copy_scroll(&mut self, lines: i32) {
        // Where the cursor ends up: the same row of the window, that many
        // lines on. The view moves by the same amount, so going there
        // afterwards moves it no further (except at either end).
        let moved = self.term.vi_mode_cursor.scroll(&self.term, lines);
        self.term.scroll_display(Scroll::Delta(lines));
        self.term.vi_goto_point(moved.point);
    }

    /// To the oldest line kept, or back to the newest.
    pub fn copy_end(&mut self, top: bool) {
        let grid = self.term.grid();
        let point = if top { Point::new(grid.topmost_line(), Column(0)) } else { Point::new(grid.bottommost_line(), Column(0)) };
        self.term.vi_goto_point(point);
    }

    /// Put the cursor on a cell of the pane as it is shown.
    pub fn copy_goto(&mut self, col: u16, row: u16) {
        let point = self.point_at(col, row);
        self.term.vi_goto_point(point);
    }

    /// Start selecting from the cursor, or stop if that is what is going on.
    pub fn copy_select(&mut self, ty: SelectionType) {
        let point = self.term.vi_mode_cursor.point;
        match &mut self.term.selection {
            Some(sel) if sel.ty == ty && !sel.is_empty() => self.term.selection = None,
            Some(sel) if !sel.is_empty() => sel.ty = ty,
            _ => self.term.selection = Some(Selection::new(ty, point, Direction::Left)),
        }
        // A selection of one cell is still a selection.
        if let Some(sel) = &mut self.term.selection {
            sel.include_all();
        }
    }

    /// Change what is being searched for, and go to the nearest match at or
    /// before where the search began.
    pub fn find_set(&mut self, query: String) {
        let Some(find) = self.copy.as_mut().and_then(|c| c.find.as_mut()) else { return };
        // The text is searched for as written, not as a pattern. Without a
        // capital letter in it, the search ignores case.
        find.regex = if query.is_empty() { None } else { RegexSearch::new(&escape_pattern(&query)).ok() };
        find.query = query;
        // Where the search began may no longer exist: the pane has been
        // made narrower or shorter since, or its history cleared.
        let grid = self.term.grid();
        find.origin.line = find.origin.line.clamp(grid.topmost_line(), grid.bottommost_line());
        find.origin.column = find.origin.column.min(grid.last_column());
        let origin = find.origin;
        let Some(regex) = find.regex.as_mut() else {
            find.total = 0;
            find.index = 0;
            find.visible.clear();
            return;
        };
        // (A match that begins right where the search began counts.)
        if let Some(m) = self.term.search_next(regex, origin, Direction::Left, Direction::Left, None) {
            let start = self.settled(*m.start());
            self.show_match(start);
        }
        self.find_count();
    }

    /// A search backwards and a search forwards do not always agree on
    /// where matches are: looking for `==` in `====`, one finds a match at
    /// every position and the other at every second. The count and the
    /// highlighting go forwards, so a match found going backwards is moved
    /// to the forward match it falls in.
    fn settled(&mut self, start: Point) -> Point {
        let Some(regex) = self.copy.as_mut().and_then(|c| c.find.as_mut()).and_then(|f| f.regex.as_mut()) else { return start };
        let from = self.term.line_search_left(start);
        let from = Point::new(from.line.max(Line(start.line.0 - 20)), Column(0));
        RegexIter::new(from, start, Direction::Right, &self.term, regex)
            .take_while(|m| *m.start() <= start)
            .last()
            .filter(|m| *m.end() >= start)
            .map(|m| *m.start())
            .unwrap_or(start)
    }

    /// Put the cursor on a match. One that is out of sight, or under the
    /// search bar on the top row, is brought to the middle of the pane.
    fn show_match(&mut self, point: Point) {
        let offset = self.term.grid().display_offset() as i32;
        let rows = self.rows as i32;
        let row = point.line.0 + offset;
        if row < 1 || row >= rows {
            let history = self.term.grid().history_size() as i32;
            let wanted = (rows / 2 - point.line.0).clamp(0, history);
            self.term.scroll_display(Scroll::Delta(wanted - offset));
        }
        self.term.vi_goto_point(point);
    }

    /// Go to the next older match (`older`) or the next newer one, wrapping
    /// round at either end.
    pub fn find_step(&mut self, older: bool) {
        let Some(find) = self.copy.as_mut().and_then(|c| c.find.as_mut()) else { return };
        let Some(regex) = find.regex.as_mut() else { return };
        let cursor = self.term.vi_mode_cursor.point;
        let (origin, direction) = if older {
            (cursor.sub(&self.term, Boundary::None, 1), Direction::Left)
        } else {
            (cursor.add(&self.term, Boundary::None, 1), Direction::Right)
        };
        let found = self.term.search_next(regex, origin, direction, Direction::Left, None).map(|m| *m.start());
        if let Some(start) = found {
            let start = if older { self.settled(start) } else { start };
            if let Some(find) = self.copy.as_mut().and_then(|c| c.find.as_mut()) {
                find.origin = start;
            }
            self.show_match(start);
        }
        self.find_count();
    }

    /// Count the matches and find which one the cursor is on.
    fn find_count(&mut self) {
        let Some(find) = self.copy.as_mut().and_then(|c| c.find.as_mut()) else { return };
        find.counted = Instant::now();
        find.total = 0;
        find.index = 0;
        let Some(regex) = find.regex.as_mut() else { return };
        let cursor = self.term.vi_mode_cursor.point;
        let grid = self.term.grid();
        let (start, end) = (Point::new(grid.topmost_line(), Column(0)), Point::new(grid.bottommost_line(), grid.last_column()));
        for m in RegexIter::new(start, end, Direction::Right, &self.term, regex).take(MAX_COUNTED + 1) {
            find.total += 1;
            if *m.start() == cursor {
                find.index = find.total;
            }
        }
    }

    /// Bring what the search shows up to date with the pane: the matches in
    /// view, and now and then the count. Called before drawing.
    pub fn find_refresh(&mut self) {
        let stale = self.last_output;
        let Some(find) = self.copy.as_mut().and_then(|c| c.find.as_mut()) else { return };
        find.visible.clear();
        let Some(regex) = find.regex.as_mut() else { return };
        let offset = self.term.grid().display_offset() as i32;
        let top = Point::new(Line(-offset), Column(0));
        let bottom = Point::new(Line(self.rows as i32 - 1 - offset), self.term.grid().last_column());
        find.visible.extend(RegexIter::new(top, bottom, Direction::Right, &self.term, regex).take(2000));
        // New output moves the matches about; count again, but not on every frame.
        if stale > find.counted && find.counted.elapsed() > std::time::Duration::from_millis(400) {
            self.find_count();
        }
    }

    /// The cell the copy-mode cursor is on.
    pub fn copy_cursor(&self) -> Option<Point> {
        self.copy.as_ref().map(|_| self.term.vi_mode_cursor.point)
    }

    // ---- links ----------------------------------------------------------------

    /// The link under a cell of the pane as it is shown, if there is one.
    pub fn link_at(&mut self, col: u16, row: u16) -> Option<Link> {
        let point = self.point_at(col, row);
        if let Some(marked) = self.term.grid()[point].hyperlink() {
            return Some(Link { uri: marked.uri().to_string(), cells: None });
        }
        if self.addresses.is_none() {
            self.addresses = RegexSearch::new(ADDRESS).ok();
        }
        let regex = self.addresses.as_mut()?;
        // The line the cell is on, over the rows it wraps onto; but no more
        // than a few rows either way, however long the line (this runs each
        // time the pointer moves).
        let (start, end) = (self.term.line_search_left(point), self.term.line_search_right(point));
        let start = if start.line.0 < point.line.0 - 6 { Point::new(Line(point.line.0 - 6), Column(0)) } else { start };
        let end = if end.line.0 > point.line.0 + 6 { Point::new(Line(point.line.0 + 6), self.term.grid().last_column()) } else { end };
        let found = RegexIter::new(start, end, Direction::Right, &self.term, regex).take_while(|m| *m.start() <= point).find(|m| m.contains(&point))?;
        let text: String = self.term.bounds_to_string(*found.start(), *found.end()).chars().filter(|c| !c.is_control()).collect();
        // Punctuation that ends the sentence, not the address.
        let mut uri = text.as_str();
        loop {
            let trimmed = uri.trim_end_matches(['.', ',', ';', ':', '!', '?', '\'', '"']);
            let trimmed = if trimmed.ends_with(')') && !trimmed.contains('(') { &trimmed[..trimmed.len() - 1] } else { trimmed };
            if trimmed.len() == uri.len() {
                break;
            }
            uri = trimmed;
        }
        let dropped = text.chars().count() - uri.chars().count();
        let last = found.end().sub(&self.term, Boundary::None, dropped);
        (point <= last && openable(uri)).then(|| Link { uri: uri.to_string(), cells: Some(*found.start()..=last) })
    }

    // ---- the line being typed -------------------------------------------------

    /// The cursor's line from its start to the cursor, following it back
    /// over rows it has wrapped onto.
    fn line_to_cursor(&self) -> String {
        let grid = self.term.grid();
        let cursor = grid.cursor.point;
        let last = grid.last_column();
        let mut first = cursor.line;
        while first > grid.topmost_line() && grid[Line(first.0 - 1)][last].flags.contains(Flags::WRAPLINE) {
            first = Line(first.0 - 1);
        }
        let mut text = String::new();
        let mut line = first;
        while line <= cursor.line {
            // A cursor waiting to wrap sits on the last character, not after it.
            let end = if line < cursor.line || grid.cursor.input_needs_wrap { last.0 + 1 } else { cursor.column.0 };
            for col in 0..end {
                let cell = &grid[line][Column(col)];
                if !cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    text.push(if cell.c == '\0' { ' ' } else { cell.c });
                }
            }
            line = Line(line.0 + 1);
        }
        text
    }

    /// Is a pager (less, more, man) sitting on its prompt, waiting for a
    /// key? Told by what the cursor's line says, the way their prompts do.
    pub fn pager_waiting(&self) -> bool {
        let line = self.line_to_cursor();
        let line = line.trim();
        line == ":" || line.ends_with("(END)") || line.contains("--More--") || line.starts_with("Manual page ") || {
            // "lines 1-24", "lines 1-24/310 7%"
            line.strip_prefix("lines ").is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()) && rest.contains('-'))
        }
    }

    /// The process group in the terminal's foreground: the shell at a
    /// prompt, otherwise the job it is running.
    pub fn foreground_group(&self) -> Option<i32> {
        self.master.process_group_leader()
    }

    /// Look up where the shell is working by asking the system about its
    /// process, for when the shell does not say so itself: a shell without
    /// the integration, or another shell started inside the pane's own
    /// (`sudo -s`, `sh`). True if the directory turned out to have changed.
    pub fn refresh_cwd(&mut self) -> bool {
        self.cwd_stale = false;
        let Some(shell) = self.pid else { return false };
        // A shell in the foreground that is not the pane's own is the one being typed into.
        let front = self.foreground_group().filter(|group| *group != shell);
        let nested = front.filter(|pid| crate::procinfo::name_of(*pid).is_some_and(|name| crate::procinfo::is_shell(&name)));
        let asked = match nested {
            Some(pid) => pid,
            None if !self.integrated => shell,
            None => return false,
        };
        match crate::procinfo::cwd_of(asked) {
            Some(dir) if self.cwd.as_deref() != Some(dir.as_str()) => {
                self.cwd = Some(dir);
                true
            }
            _ => false,
        }
    }

    fn at_prompt(&self) -> bool {
        self.integrated && matches!(self.state, ShellState::Prompt) && !self.mode().contains(TermMode::ALT_SCREEN)
    }

    /// The first time something is typed at a prompt, the cursor's line
    /// still holds only the prompt: remember it, so what follows it on the
    /// screen can be told apart later.
    fn note_typing(&mut self) {
        if self.prompt_text.is_none() && self.display_offset() == 0 {
            self.prompt_text = Some(self.line_to_cursor());
        }
    }

    /// Call before passing a key to the shell, to keep track of the line
    /// being typed at the prompt.
    pub fn note_key(&mut self, key: &KeyEvent) {
        if !self.at_prompt() || self.awaiting_prompt {
            self.typeahead = true;
            return;
        }
        self.note_typing();
        let ctrl = key.modifiers == KeyModifiers::CONTROL;
        let plain = !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        let Some((text, cursor)) = &mut self.typed else {
            // Ctrl+C leaves an empty line whatever was on it; Enter sends it off.
            match key.code {
                KeyCode::Char('c') if ctrl => self.typed = Some((String::new(), 0)),
                KeyCode::Enter => self.awaiting_prompt = true,
                _ => {}
            }
            return;
        };
        let before = |text: &str, at: usize| text[..at].chars().next_back().map(char::len_utf8).unwrap_or(0);
        let after = |text: &str, at: usize| text[at..].chars().next().map(char::len_utf8).unwrap_or(0);
        let at_end = *cursor == text.len();
        match key.code {
            KeyCode::Char(c) if plain => {
                text.insert(*cursor, c);
                *cursor += c.len_utf8();
            }
            KeyCode::Backspace if plain => {
                let n = before(text, *cursor);
                text.drain(*cursor - n..*cursor);
                *cursor -= n;
            }
            KeyCode::Delete if plain => {
                let n = after(text, *cursor);
                text.drain(*cursor..*cursor + n);
            }
            KeyCode::Left if key.modifiers.is_empty() => *cursor -= before(text, *cursor),
            KeyCode::Home if key.modifiers.is_empty() => *cursor = 0,
            KeyCode::Char('a') if ctrl => *cursor = 0,
            // At the end of the line these accept a suggestion in shells
            // that offer them, which changes the line in a way not known here.
            KeyCode::Right if key.modifiers.is_empty() && !at_end => *cursor += after(text, *cursor),
            KeyCode::End if key.modifiers.is_empty() && !at_end => *cursor = text.len(),
            KeyCode::Char('e') if ctrl && !at_end => *cursor = text.len(),
            // Leaves an empty line, whatever was on it.
            KeyCode::Char('c') if ctrl => self.typed = Some((String::new(), 0)),
            KeyCode::Enter => {
                self.typed = None;
                self.awaiting_prompt = true;
            }
            _ => self.typed = None,
        }
    }

    /// Call before passing pasted text to the shell.
    pub fn note_paste(&mut self, text: &str) {
        if !self.at_prompt() || self.awaiting_prompt {
            self.typeahead = true;
            return;
        }
        self.note_typing();
        match &mut self.typed {
            Some((typed, cursor)) if !text.contains(char::is_control) => {
                typed.insert_str(*cursor, text);
                *cursor += text.len();
            }
            _ => self.typed = None,
        }
    }

    /// The keys that make the shell's line editor erase the line being
    /// typed: Ctrl+U erases back to the start of the line, preceded by
    /// Ctrl+E (to the end of the line) when the cursor is known to be
    /// somewhere in the middle.
    pub fn erase_line_keys(&self) -> Vec<u8> {
        match &self.typed {
            Some((text, cursor)) if *cursor < text.len() => vec![0x05, 0x15],
            _ => vec![0x15],
        }
    }

    /// The line at the prompt was erased on the user's behalf.
    pub fn line_abandoned(&mut self) {
        self.typed = Some((String::new(), 0));
        self.awaiting_prompt = false;
    }

    /// Is there anything on the cursor's row after the cursor, or does the
    /// row run on to the next?
    fn text_follows_cursor(&self) -> bool {
        let grid = self.term.grid();
        let cursor = grid.cursor.point;
        let row = &grid[cursor.line];
        let last = grid.last_column();
        let from = if grid.cursor.input_needs_wrap { last.0 + 1 } else { cursor.column.0 };
        (from..=last.0).any(|col| !matches!(row[Column(col)].c, ' ' | '\0')) || row[last].flags.contains(Flags::WRAPLINE)
    }

    /// What the user has typed at the prompt. A line that was only typed,
    /// corrected and moved about in is known exactly from the keys. One the
    /// shell had a hand in (history, completion) is read from the screen,
    /// from the end of the prompt to the cursor, and only when the cursor is
    /// at the end of what is on its row: text after it could be more of the
    /// line, a suggestion or a right-hand prompt, and there is no telling
    /// which. `None` whenever the line cannot be told for certain: no shell
    /// integration, a program running, a prompt that has changed since they
    /// began typing, a continuation line.
    pub fn typed_line(&self) -> Option<String> {
        if !self.at_prompt() || self.awaiting_prompt {
            return None;
        }
        if let Some((typed, _)) = &self.typed {
            return Some(typed.clone());
        }
        if self.display_offset() != 0 || self.text_follows_cursor() {
            return None;
        }
        let line = self.line_to_cursor();
        line.strip_prefix(self.prompt_text.as_deref()?).map(|typed| typed.trim_end().to_string())
    }

    // ---- keeping the screen across a restart -----------------------------------

    /// The pane's scrollback and screen as text a terminal can replay,
    /// colors included. `None` while a full-screen program has the screen:
    /// what was there before it is out of reach until it ends.
    pub fn dump(&self) -> Option<String> {
        if self.mode().contains(TermMode::ALT_SCREEN) {
            return None;
        }
        // A prompt nothing has been typed at says nothing worth keeping.
        let idle = matches!(self.state, ShellState::Prompt) && self.typed.as_ref().is_some_and(|(line, _)| line.is_empty());
        Some(dump_term(&self.term, idle))
    }

    /// Put back what an earlier pane showed, before this one's shell has
    /// said anything, with a line under it saying where it came from.
    pub fn preload(&mut self, text: &str, note: &str) {
        self.parser.advance(&mut self.term, text.as_bytes());
        let start = if self.term.grid().cursor.point.column.0 > 0 { "\r\n" } else { "" };
        let line = format!("\x1b[0m{start}\x1b[2m── {note} ──\x1b[0m\r\n");
        self.parser.advance(&mut self.term, line.as_bytes());
        self.redraw();
    }

    // ---- text extraction ----------------------------------------------------

    /// The last `max_lines` lines of screen + scrollback as plain text.
    pub fn recent_text(&self, max_lines: usize) -> String {
        let grid = self.term.grid();
        let bottom = grid.bottommost_line();
        let top = grid.topmost_line();
        let start_line = Line((bottom.0 - max_lines as i32 + 1).max(top.0));
        let start = Point::new(start_line, Column(0));
        let end = Point::new(bottom, grid.last_column());
        let text = without_pictures(self.term.bounds_to_string(start, end));
        let trimmed: Vec<&str> = text.lines().map(|l| l.trim_end()).collect();
        trimmed.join("\n").trim_end().to_string()
    }
}

/// How a cell is drawn, as the numbers of an SGR sequence (after a reset).
fn sgr_of(cell: &Cell) -> String {
    let mut out = String::new();
    for (flag, code) in [
        (Flags::BOLD, "1"),
        (Flags::DIM, "2"),
        (Flags::ITALIC, "3"),
        (Flags::ALL_UNDERLINES, "4"),
        (Flags::INVERSE, "7"),
        (Flags::HIDDEN, "8"),
        (Flags::STRIKEOUT, "9"),
    ] {
        if cell.flags.intersects(flag) {
            out.push(';');
            out.push_str(code);
        }
    }
    let mut color = |c: CellColor, base: u16| match c {
        CellColor::Named(n) if (n as usize) < 8 => out.push_str(&format!(";{}", base + n as u16)),
        CellColor::Named(n) if (n as usize) < 16 => out.push_str(&format!(";{}", base + 60 + n as u16 - 8)),
        CellColor::Named(NamedColor::Foreground | NamedColor::Background) | CellColor::Named(_) => {}
        CellColor::Indexed(i) => out.push_str(&format!(";{};5;{i}", base + 8)),
        CellColor::Spec(rgb) => out.push_str(&format!(";{};2;{};{};{}", base + 8, rgb.r, rgb.g, rgb.b)),
    };
    color(cell.fg, 30);
    color(cell.bg, 40);
    out
}

/// A terminal's scrollback and screen as text that draws the same again:
/// lines ending in CR LF, colors and styles as SGR sequences. A line that
/// was wrapped is written unbroken, so it wraps to fit wherever it is
/// replayed. Empty lines at the end are left out, and with `skip_cursor_line`
/// the line the cursor is on as well.
pub fn dump_term<T>(term: &Term<T>, skip_cursor_line: bool) -> String {
    let grid = term.grid();
    let cols = grid.columns();
    let top = grid.topmost_line().0;
    let mut last = grid.bottommost_line().0;
    while last >= top && grid[Line(last)].is_clear() {
        last -= 1;
    }
    if skip_cursor_line && last >= top && last == grid.cursor.point.line.0 {
        last -= 1;
    }
    let mut out = String::new();
    // The style in force, and the look of the cell that set it.
    let mut style = String::new();
    let mut look = None;
    const LOOKS: Flags = Flags::BOLD.union(Flags::DIM).union(Flags::ITALIC).union(Flags::ALL_UNDERLINES).union(Flags::INVERSE).union(Flags::HIDDEN).union(Flags::STRIKEOUT);
    for line in top..=last {
        let row = &grid[Line(line)];
        let wraps = row[Column(cols - 1)].flags.contains(Flags::WRAPLINE);
        // Up to the last cell that shows anything: a character, or a blank
        // with a background of its own.
        // (A picture's cells are saved as the blanks they would be without it.)
        let plain = |cell: &Cell| cell.c == PLACEHOLDER || (cell.bg == CellColor::Named(NamedColor::Background) && !cell.flags.contains(Flags::INVERSE));
        let mut len = row.line_length().0.min(cols);
        while len < cols && (len..cols).any(|col| !plain(&row[Column(col)])) {
            len = (len..cols).rev().find(|col| !plain(&row[Column(*col)])).map_or(len, |col| col + 1);
        }
        let blank = Cell::default();
        for col in 0..len {
            let cell = &row[Column(col)];
            let cell = if cell.c == PLACEHOLDER { &blank } else { cell };
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            // Most cells look like the one before: only a change is worked out.
            let this = Some((cell.fg, cell.bg, cell.flags & LOOKS));
            if this != look {
                look = this;
                let wanted = sgr_of(cell);
                if wanted != style {
                    out.push_str("\x1b[0");
                    out.push_str(&wanted);
                    out.push('m');
                    style = wanted;
                }
            }
            out.push(if cell.c == '\0' || cell.c == '\t' { ' ' } else { cell.c });
            if let Some(extra) = cell.zerowidth() {
                out.extend(extra.iter());
            }
        }
        if !wraps {
            if !style.is_empty() {
                out.push_str("\x1b[0m");
                style.clear();
                look = None;
            }
            out.push_str("\r\n");
        }
    }
    if !style.is_empty() {
        out.push_str("\x1b[0m");
    }
    out
}

/// A pattern that matches `text` as it is written.
fn escape_pattern(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        if matches!(c, '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$' | '#' | '&' | '-' | '~') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(cols: usize, lines: usize) -> Term<alacritty_terminal::event::VoidListener> {
        let config = TermConfig { scrolling_history: 100, ..TermConfig::default() };
        Term::new(config, &Dims { cols, lines }, alacritty_terminal::event::VoidListener)
    }

    fn screen<T>(t: &Term<T>) -> Vec<String> {
        let grid = t.grid();
        let (top, bottom) = (grid.topmost_line(), grid.bottommost_line());
        let text = t.bounds_to_string(Point::new(top, Column(0)), Point::new(bottom, grid.last_column()));
        text.lines().map(|l| l.trim_end().to_string()).collect()
    }

    #[test]
    fn a_dumped_screen_replays_the_same() {
        let mut a = term(20, 5);
        let mut parser: Processor = Processor::new();
        // Scrollback, colors, a wrapped line, a wide character, and a prompt.
        let input = "one\r\ntwo \x1b[1;31mred\x1b[0m and \x1b[38;2;1;2;3;48;5;17mdeep\x1b[0m\r\n\
                     a line that is longer than the twenty columns\r\n日本 wide\r\nfive\r\nsix\r\nseven\r\nprompt$ ";
        parser.advance(&mut a, input.as_bytes());
        let dumped = dump_term(&a, false);
        assert!(dumped.contains("\x1b[0;1;31mred\x1b[0m"), "{dumped:?}");
        assert!(dumped.contains("\x1b[0;38;2;1;2;3;48;5;17mdeep"), "{dumped:?}");
        assert!(dumped.contains("a line that is longer than the twenty columns\r\n"), "a wrapped line is written whole: {dumped:?}");
        assert!(dumped.ends_with("prompt$\r\n"), "{dumped:?}");

        // Replayed at the same size, every line is where it was.
        let mut b = term(20, 5);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut b, dumped.as_bytes());
        let (was, is) = (screen(&a), screen(&b));
        assert_eq!(was[..was.len() - 1], is[..was.len() - 1]);
        // In a wider terminal the long line is one line again.
        let mut c = term(60, 5);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut c, dumped.as_bytes());
        assert!(screen(&c).contains(&"a line that is longer than the twenty columns".to_string()));

        // Blanks with a background of their own are part of the picture.
        let mut d = term(20, 3);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut d, b"\x1b[44mblue   \x1b[0m\r\nnext");
        assert!(dump_term(&d, false).starts_with("\x1b[0;44mblue   \x1b[0m\r\n"), "{:?}", dump_term(&d, false));

        // The line the cursor is on can be left out, and so are empty ones below it.
        let short = dump_term(&a, true);
        assert!(short.ends_with("seven\r\n"), "{short:?}");
        assert_eq!(dump_term(&term(20, 5), false), "");
    }

    #[test]
    fn only_web_and_mail_links_are_opened() {
        for yes in ["https://example.com/a?b=c#d", "http://localhost:8080", "HTTPS://EXAMPLE.COM", "mailto:someone@example.com"] {
            assert!(openable(yes), "{yes}");
        }
        // A file can be a program; other schemes can be anything.
        for no in [
            "file:///Applications/Calculator.app",
            "file://host/share/run.command",
            "ftp://example.com/x",
            "javascript:alert(1)",
            "ssh://host",
            "-a Calculator",
            "https://example.com/\x1b[2J",
            "https://example.com/ --flag",
            "",
        ] {
            assert!(!openable(no), "{no}");
        }
    }

    #[test]
    fn search_text_is_taken_as_written() {
        assert_eq!(escape_pattern("a.b*c"), r"a\.b\*c");
        assert_eq!(escape_pattern("f(x) [1] $HOME"), r"f\(x\) \[1\] \$HOME");
        assert_eq!(escape_pattern(r"C:\dir"), r"C:\\dir");
        assert_eq!(escape_pattern("plain words"), "plain words");
        // And what comes out is a pattern the search accepts.
        for text in ["a.b*c", "f(x) [1] $HOME", "{}", "^-~&#|?+", "é ü 日本"] {
            assert!(RegexSearch::new(&escape_pattern(text)).is_ok(), "{text}");
        }
    }
}

fn random_hex() -> String {
    let mut bytes = [0u8; 12];
    let ok = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes)).is_ok();
    if !ok {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        bytes[..8].copy_from_slice(&(t as u64 ^ std::process::id() as u64).to_le_bytes());
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl Drop for Pane {
    fn drop(&mut self) {
        let _ = self.killer.kill();
    }
}
