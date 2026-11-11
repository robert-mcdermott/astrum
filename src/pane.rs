//! A pane: one shell running on a PTY plus the terminal emulation state that
//! mirrors its screen.

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
use alacritty_terminal::vte::ansi::{Color as CellColor, NamedColor, Processor, Rgb};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::Path;
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
    /// The shell's process.
    pid: Option<i32>,
    /// There has been output since the directory was last looked up.
    pub cwd_stale: bool,
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

impl Pane {
    pub fn spawn(o: SpawnOpts, tx: mpsc::Sender<Ev>) -> anyhow::Result<Pane> {
        let (cols, rows) = (o.cols.max(2), o.rows.max(1));
        let pair = native_pty_system().openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;

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
            pid,
            cwd_stale: false,
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
        let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
    }

    /// Feed PTY output through the emulator and the shell-integration scanner.
    pub fn feed(&mut self, data: &[u8], out: &mut Vec<PaneEvent>) {
        self.last_output = Instant::now();
        self.cwd_stale = true;
        self.parser.advance(&mut self.term, data);

        let mut marks = Vec::new();
        self.scanner.feed(data, &mut marks);
        for mark in marks {
            self.integrated = true;
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
                            cwd: self.cwd.clone(),
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
                Mark::Cwd(path) => self.cwd = Some(path),
            }
        }

        while let Ok(ev) = self.term_events.try_recv() {
            match ev {
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
                    let size = WindowSize {
                        num_lines: self.rows,
                        num_cols: self.cols,
                        cell_width: 8,
                        cell_height: 16,
                    };
                    self.write(fmt(size).into_bytes());
                }
                TermEvent::Bell => out.push(PaneEvent::Bell),
                _ => {}
            }
        }
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
        self.term.selection_to_string().filter(|s| !s.is_empty())
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
        let moved = self.term.vi_mode_cursor.scroll(&self.term, lines);
        self.term.vi_goto_point(moved.point);
        self.term.scroll_display(Scroll::Delta(lines));
        let point = self.term.vi_mode_cursor.point;
        self.term.scroll_to_point(point);
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
        let origin = find.origin;
        let Some(regex) = find.regex.as_mut() else {
            find.total = 0;
            find.index = 0;
            find.visible.clear();
            return;
        };
        // (A match that begins right where the search began counts.)
        if let Some(m) = self.term.search_next(regex, origin, Direction::Left, Direction::Left, None) {
            self.show_match(*m.start());
        }
        self.find_count();
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
        if let Some(m) = self.term.search_next(regex, origin, direction, Direction::Left, None) {
            find.origin = *m.start();
            self.show_match(*m.start());
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
        let text = self.term.bounds_to_string(start, end);
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
        let len = row.line_length().0.min(cols);
        for col in 0..len {
            let cell = &row[Column(col)];
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

        // The line the cursor is on can be left out, and so are empty ones below it.
        let short = dump_term(&a, true);
        assert!(short.ends_with("seven\r\n"), "{short:?}");
        assert_eq!(dump_term(&term(20, 5), false), "");
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
