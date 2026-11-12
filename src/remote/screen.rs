//! A terminal's screen, written out as what a program would have to print
//! to draw it: for showing a shell kept on another machine to a terminal
//! that has not been following it (this machine was restarted, or too
//! much was written while the connection was down to send it all).
//!
//! What is carried: the history and the screen with their colors and
//! styles, lines that ran on into the next, the full-screen program's
//! screen if one is running (and the shell's own beneath it), where the
//! cursor is, and what the program has asked of the terminal (the mouse,
//! pasting in brackets, the keys' other codes). Not carried: links,
//! pictures, the scrolling region, tab stops, the cursor's shape, the
//! color and style the next character would have been written in, a
//! line-drawing character set that was switched to, a saved cursor, and a
//! cursor waiting at the last column to wrap.

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor};

/// The colors and styles of a cell, as the numbers that select them.
fn style(cell: &Cell) -> Vec<u16> {
    let mut codes = Vec::new();
    let f = cell.flags;
    for (flag, code) in [
        (Flags::BOLD, 1),
        (Flags::DIM, 2),
        (Flags::ITALIC, 3),
        (Flags::UNDERLINE, 4),
        (Flags::INVERSE, 7),
        (Flags::HIDDEN, 8),
        (Flags::STRIKEOUT, 9),
        (Flags::DOUBLE_UNDERLINE, 21),
    ] {
        if f.contains(flag) {
            codes.push(code);
        }
    }
    // (The other underlines, curly and the rest, come out as a plain one.)
    if f.intersects(Flags::UNDERCURL | Flags::DOTTED_UNDERLINE | Flags::DASHED_UNDERLINE) {
        codes.push(4);
    }
    color(cell.fg, 30, &mut codes);
    color(cell.bg, 40, &mut codes);
    codes
}

fn color(c: Color, base: u16, codes: &mut Vec<u16>) {
    match c {
        Color::Named(name) => match name as usize {
            n @ 0..=7 => codes.push(base + n as u16),
            n @ 8..=15 => codes.push(base + 60 + (n as u16 - 8)),
            // The terminal's own foreground and background, and the dim
            // colors (which are the ordinary ones, drawn dim).
            _ => match name {
                NamedColor::DimBlack => codes.push(base),
                NamedColor::DimRed => codes.push(base + 1),
                NamedColor::DimGreen => codes.push(base + 2),
                NamedColor::DimYellow => codes.push(base + 3),
                NamedColor::DimBlue => codes.push(base + 4),
                NamedColor::DimMagenta => codes.push(base + 5),
                NamedColor::DimCyan => codes.push(base + 6),
                NamedColor::DimWhite => codes.push(base + 7),
                _ => {}
            },
        },
        Color::Indexed(i) => codes.extend([base + 8, 5, i as u16]),
        Color::Spec(rgb) => codes.extend([base + 8, 2, rgb.r as u16, rgb.g as u16, rgb.b as u16]),
    }
}

/// Is there nothing to see in this cell, wherever it is drawn?
fn empty(cell: &Cell) -> bool {
    (cell.c == ' ' || cell.c == '\t')
        && matches!(cell.bg, Color::Named(NamedColor::Background))
        && !cell.flags.intersects(Flags::INVERSE | Flags::UNDERLINE | Flags::DOUBLE_UNDERLINE | Flags::UNDERCURL | Flags::STRIKEOUT)
        && cell.zerowidth().is_none()
}

/// The lines of the grid that is showing, from `first` down, as text
/// that draws them from the top of an empty screen of the same size.
fn lines<T: EventListener>(term: &Term<T>, first: i32, out: &mut Vec<u8>) {
    let grid = term.grid();
    let (cols, rows) = (grid.columns(), grid.screen_lines() as i32);
    let mut worn: Vec<u16> = Vec::new();
    let mut utf8 = [0u8; 4];
    let mut ran_on_before = false;
    for line in first..rows {
        let row = &grid[Line(line)];
        let runs_on = cols > 0 && row[Column(cols - 1)].flags.contains(Flags::WRAPLINE);
        // A line that runs on into the next is written to its very end,
        // so that the terminal wraps it as it was; any other, as far as
        // there is something to see.
        let end = if runs_on { cols } else { (0..cols).rev().find(|c| !empty(&row[Column(*c)])).map_or(0, |c| c + 1) };
        for c in 0..end {
            let cell = &row[Column(c)];
            // The second half of a wide character, and the gap left where
            // one did not fit: the character itself makes both.
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            let wears = style(cell);
            if wears != worn {
                out.extend_from_slice(b"\x1b[0");
                for code in &wears {
                    out.extend_from_slice(format!(";{code}").as_bytes());
                }
                out.push(b'm');
                worn = wears;
            }
            let ch = if cell.c == '\t' || cell.c.is_control() { ' ' } else { cell.c };
            out.extend_from_slice(ch.encode_utf8(&mut utf8).as_bytes());
            for extra in cell.zerowidth().into_iter().flatten() {
                out.extend_from_slice(extra.encode_utf8(&mut utf8).as_bytes());
            }
        }
        // A line begun by the one before running on may have been scrolled
        // into view by a character with a background of its own, and a
        // new line takes that color all the way along: what is past the
        // end of this one is put back as it was.
        if ran_on_before && end < cols {
            if !worn.is_empty() {
                out.extend_from_slice(b"\x1b[0m");
                worn.clear();
            }
            // (And one that was emptied afterwards still has to be begun:
            // it is a character arriving that makes the line before run on.)
            if end == 0 {
                out.push(b' ');
            }
            out.extend_from_slice(b"\x1b[K");
        }
        ran_on_before = runs_on;
        if !runs_on && line + 1 < rows {
            // (Colors are put down first: a new line scrolled into view
            // takes the background being worn.)
            if !worn.is_empty() {
                out.extend_from_slice(b"\x1b[0m");
                worn.clear();
            }
            out.extend_from_slice(b"\r\n");
        }
    }
    if !worn.is_empty() {
        out.extend_from_slice(b"\x1b[0m");
    }
}

fn cursor<T: EventListener>(term: &Term<T>, out: &mut Vec<u8>) {
    let at = term.grid().cursor.point;
    out.extend_from_slice(format!("\x1b[{};{}H", at.line.0.max(0) + 1, at.column.0 + 1).as_bytes());
}

/// What draws this terminal as it now stands, on a new one of its size.
pub fn of<T: EventListener>(term: &mut Term<T>, title: Option<&str>) -> Vec<u8> {
    let mut out = Vec::new();
    let top = |term: &Term<T>| -(term.grid().history_size() as i32);
    if term.mode().contains(TermMode::ALT_SCREEN) {
        // The full-screen program's screen is showing; the shell's own,
        // with the history, is the other one. It can only be looked at by
        // changing over to it, and changing back empties this one: so
        // this one is taken down first and put back afterwards.
        // (From the top left: a screen that is changed over to starts with
        // the cursor wherever the other one's was.)
        let mut front = b"\x1b[H".to_vec();
        lines(term, 0, &mut front);
        cursor(term, &mut front);
        term.swap_alt();
        lines(term, top(term), &mut out);
        cursor(term, &mut out);
        term.swap_alt();
        Processor::<alacritty_terminal::vte::ansi::StdSyncHandler>::new().advance(term, &front);
        out.extend_from_slice(b"\x1b[?1049h");
        out.extend_from_slice(&front);
    } else {
        lines(term, top(term), &mut out);
        cursor(term, &mut out);
    }

    let mode = *term.mode();
    let asked: [(TermMode, bool, &str); 14] = [
        (TermMode::SHOW_CURSOR, false, "\x1b[?25l"),
        (TermMode::APP_CURSOR, true, "\x1b[?1h"),
        (TermMode::APP_KEYPAD, true, "\x1b="),
        (TermMode::BRACKETED_PASTE, true, "\x1b[?2004h"),
        (TermMode::MOUSE_REPORT_CLICK, true, "\x1b[?1000h"),
        (TermMode::MOUSE_DRAG, true, "\x1b[?1002h"),
        (TermMode::MOUSE_MOTION, true, "\x1b[?1003h"),
        (TermMode::UTF8_MOUSE, true, "\x1b[?1005h"),
        (TermMode::SGR_MOUSE, true, "\x1b[?1006h"),
        (TermMode::FOCUS_IN_OUT, true, "\x1b[?1004h"),
        (TermMode::ALTERNATE_SCROLL, false, "\x1b[?1007l"),
        (TermMode::LINE_WRAP, false, "\x1b[?7l"),
        (TermMode::INSERT, true, "\x1b[4h"),
        (TermMode::LINE_FEED_NEW_LINE, true, "\x1b[20h"),
    ];
    for (flag, when_set, says) in asked {
        if mode.contains(flag) == when_set {
            out.extend_from_slice(says.as_bytes());
        }
    }
    let keys = [
        (TermMode::DISAMBIGUATE_ESC_CODES, 1),
        (TermMode::REPORT_EVENT_TYPES, 2),
        (TermMode::REPORT_ALTERNATE_KEYS, 4),
        (TermMode::REPORT_ALL_KEYS_AS_ESC, 8),
        (TermMode::REPORT_ASSOCIATED_TEXT, 16),
    ]
    .iter()
    .filter(|(flag, _)| mode.contains(*flag))
    .map(|(_, bit)| bit)
    .sum::<u8>();
    if keys != 0 {
        out.extend_from_slice(format!("\x1b[>{keys}u").as_bytes());
    }
    if let Some(title) = title.filter(|t| !t.chars().any(char::is_control)) {
        out.extend_from_slice(format!("\x1b]0;{title}\x07").as_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::Config;

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

    fn term(cols: usize, rows: usize) -> Term<VoidListener> {
        Term::new(Config { scrolling_history: 200, kitty_keyboard: true, ..Config::default() }, &Size(cols, rows), VoidListener)
    }

    fn write(term: &mut Term<VoidListener>, bytes: &[u8]) {
        Processor::<alacritty_terminal::vte::ansi::StdSyncHandler>::new().advance(term, bytes);
    }

    /// Everything there is to see of a terminal, to hold beside another.
    fn seen(term: &Term<VoidListener>) -> Vec<String> {
        let grid = term.grid();
        let mut all = Vec::new();
        for line in -(grid.history_size() as i32)..grid.screen_lines() as i32 {
            let mut text = String::new();
            for c in 0..grid.columns() {
                let cell = &grid[Line(line)][Column(c)];
                let mut flags = cell.flags;
                // (Which underline it was is not carried.)
                if flags.intersects(Flags::UNDERCURL | Flags::DOTTED_UNDERLINE | Flags::DASHED_UNDERLINE) {
                    flags.remove(Flags::UNDERCURL | Flags::DOTTED_UNDERLINE | Flags::DASHED_UNDERLINE);
                    flags.insert(Flags::UNDERLINE);
                }
                let ch = if cell.c == '\t' { ' ' } else { cell.c };
                if empty(cell) {
                    text.push_str(if flags.contains(Flags::WRAPLINE) { "·↩ " } else { "· " });
                } else {
                    text.push_str(&format!("{ch}{:?}/{:?}/{:?}/{:?} ", cell.zerowidth(), cell.fg, cell.bg, flags));
                }
            }
            all.push(format!("{line:>4} {text}"));
        }
        let at = grid.cursor.point;
        all.push(format!("cursor {},{}  mode {:?}", at.line.0, at.column.0, term.mode()));
        all
    }

    /// Draw `bytes` on one terminal, take its screen, draw that on
    /// another: the two are to look the same.
    fn carried(cols: usize, rows: usize, bytes: &[u8]) -> Term<VoidListener> {
        let mut a = term(cols, rows);
        write(&mut a, bytes);
        let before = seen(&a);
        let screen = of(&mut a, None);
        assert_eq!(seen(&a), before, "taking the screen changed the terminal it was taken from");
        let mut b = term(cols, rows);
        write(&mut b, &screen);
        let (was, is) = (seen(&a), seen(&b));
        for (was, is) in was.iter().zip(&is) {
            assert_eq!(was, is, "\nthe screen:\n{}", String::from_utf8_lossy(&screen).escape_debug());
        }
        assert_eq!(was.len(), is.len());
        b
    }

    #[test]
    fn text_with_its_colors_and_styles() {
        carried(40, 6, b"plain \x1b[1;31mbold red\x1b[0m \x1b[3;4;38;5;208mfancy\x1b[0m \x1b[48;2;10;20;30mon a color\x1b[0m\r\n\x1b[7mturned\x1b[27m \x1b[2mdim\x1b[22m \x1b[9mstruck\x1b[0m \x1b[94;103mbright\x1b[0m\r\n\x1b[4:3mcurly\x1b[0m\x1b[21mtwice\x1b[0m");
    }

    #[test]
    fn the_history_comes_too() {
        let mut bytes = Vec::new();
        for i in 0..60 {
            bytes.extend_from_slice(format!("\x1b[3{}mline {i}\x1b[0m\r\n", i % 8).as_bytes());
        }
        bytes.extend_from_slice(b"$ ");
        let b = carried(30, 8, &bytes);
        assert_eq!(b.grid().history_size(), 53);
    }

    #[test]
    fn a_line_that_ran_on_still_runs_on() {
        let long = "word ".repeat(30);
        let b = carried(20, 10, format!("{long}\r\nshort\r\n{long}").as_bytes());
        // Made wider, the lines join up again: they were not cut into pieces.
        let mut b = b;
        b.resize(Size(150, 10));
        let first: String = (0..150).map(|c| b.grid()[Line(0)][Column(c)].c).collect();
        assert_eq!(first.trim_end(), long.trim_end());
    }

    #[test]
    fn wide_characters_and_ones_that_did_not_fit() {
        carried(11, 5, "日本語のテキスト、wide at the edge: 12345678日\r\ne\u{301}a\u{308} joined".as_bytes());
    }

    #[test]
    fn a_colored_background_to_the_end_of_the_line() {
        carried(20, 5, b"\x1b[44mblue\x1b[K\x1b[0m\r\nnext\r\n\x1b[41m\x1b[2K\x1b[0m\r\nafter");
    }

    #[test]
    fn blank_lines_and_a_cursor_in_the_middle() {
        carried(20, 8, b"top\r\n\r\n\r\nlower\x1b[2;7Hx\x1b[2;4H");
    }

    #[test]
    fn a_full_screen_program_and_the_shell_beneath_it() {
        let mut bytes = Vec::new();
        for i in 0..20 {
            bytes.extend_from_slice(format!("before {i}\r\n").as_bytes());
        }
        bytes.extend_from_slice(b"$ vi notes\r\n\x1b[?1049h\x1b[H\x1b[2J\x1b[1;7m notes \x1b[0m\r\n~\r\n~\x1b[?1h\x1b=\x1b[?2004h\x1b[?1000h\x1b[?1006h\x1b[?25l\x1b[2;1H");
        let mut b = carried(30, 8, &bytes);
        assert!(b.mode().contains(TermMode::ALT_SCREEN | TermMode::APP_CURSOR | TermMode::BRACKETED_PASTE | TermMode::SGR_MOUSE));
        assert!(!b.mode().contains(TermMode::SHOW_CURSOR));
        // The program leaves: the shell's screen and history are there.
        write(&mut b, b"\x1b[?1049l");
        let row: String = (0..12).map(|c| b.grid()[Line(6)][Column(c)].c).collect();
        assert_eq!(row.trim_end(), "$ vi notes");
        assert_eq!(b.grid().history_size(), 14);
    }

    #[test]
    fn a_full_screen_program_started_from_the_middle_of_a_line() {
        // (As `git commit` does: a hint with no end of line, then the editor.)
        let b = carried(40, 8, b"$ git commit\r\nhint: Waiting for your editor... \x1b[?1049h\x1b[H\x1b[2J\x1b[7m COMMIT_EDITMSG \x1b[0m\r\n# Please enter the message\r\n~\x1b[1;1H");
        let top: String = (0..16).map(|c| b.grid()[Line(0)][Column(c)].c).collect();
        assert_eq!(top, " COMMIT_EDITMSG ");
    }

    #[test]
    fn what_the_keys_were_asked_to_send() {
        let b = carried(20, 4, b"\x1b[>5u\x1b[?1004h\x1b[?7l");
        assert!(b.mode().contains(TermMode::DISAMBIGUATE_ESC_CODES | TermMode::REPORT_ALTERNATE_KEYS | TermMode::FOCUS_IN_OUT));
        assert!(!b.mode().contains(TermMode::LINE_WRAP));
    }

    #[test]
    fn anything_at_all() {
        // A long run of whatever a program might write, the same each time.
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        for _ in 0..40 {
            let mut bytes = Vec::new();
            for _ in 0..next(400) + 20 {
                match next(12) {
                    0 => bytes.extend_from_slice(b"\r\n"),
                    1 => bytes.extend_from_slice(format!("\x1b[{};{}H", next(12) + 1, next(30) + 1).as_bytes()),
                    2 => bytes.extend_from_slice(format!("\x1b[{}m", [0, 1, 3, 4, 7, 31, 42, 93, 104][next(9) as usize]).as_bytes()),
                    3 => bytes.extend_from_slice(format!("\x1b[38;5;{}m", next(256)).as_bytes()),
                    4 => bytes.extend_from_slice("漢字".as_bytes()),
                    5 => bytes.extend_from_slice(b"\x1b[K"),
                    6 => bytes.extend_from_slice(b"\t"),
                    _ => bytes.extend_from_slice(&b"the quick brown fox jumps"[..next(25) as usize + 1]),
                }
            }
            carried(30, 12, &bytes);
        }
    }
}
