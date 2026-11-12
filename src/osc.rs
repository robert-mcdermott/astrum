//! Shell-integration scanner.
//!
//! The terminal emulator ignores the semantic-prompt sequences (OSC 133) and
//! working-directory reports (OSC 7) that our shell hooks emit, so this
//! scanner watches the raw PTY stream alongside it. It reports the marks and,
//! between "output start" and "command end", captures the command's output
//! so the agent can read exactly what a command printed.

use std::collections::VecDeque;

const HEAD_MAX: usize = 96 * 1024;
const TAIL_MAX: usize = 96 * 1024;
const OSC_MAX: usize = 8 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub enum Mark {
    /// The shell is about to draw a prompt (OSC 133;A).
    PromptStart,
    /// A command line was accepted and its output begins (OSC 133;C).
    OutputStart { cmd: Option<String> },
    /// The command finished (OSC 133;D[;exit]).
    CmdEnd { exit: Option<i32>, output: Option<Capture> },
    /// Working directory changed (OSC 7).
    Cwd(String),
    /// A mark from a shell inside the pane's own (see `crate::inner`):
    /// how many shells in, counting from one, and the mark.
    Inner { level: usize, mark: Box<Mark> },
}

/// Bounded capture of a command's raw output: the first and last chunk are
/// kept, the middle is dropped once the limits are exceeded.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Capture {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    pub total: usize,
}

impl Capture {
    fn push(&mut self, bytes: &[u8]) {
        self.total += bytes.len();
        let room = HEAD_MAX.saturating_sub(self.head.len());
        let (a, b) = bytes.split_at(room.min(bytes.len()));
        self.head.extend_from_slice(a);
        if !b.is_empty() {
            let b = if b.len() > TAIL_MAX { &b[b.len() - TAIL_MAX..] } else { b };
            let overflow = (self.tail.len() + b.len()).saturating_sub(TAIL_MAX);
            self.tail.drain(..overflow);
            self.tail.extend(b);
        }
    }

    /// Add a line of our own to what the command printed.
    pub fn note(&mut self, text: &str) {
        self.push(text.as_bytes());
    }

    /// Plain-text rendering of what was captured so far.
    pub fn text(&self) -> String {
        let mut out = clean_output(&self.head);
        let kept = self.head.len() + self.tail.len();
        if self.total > kept {
            out.push_str(&format!("\n[... {} bytes of output omitted ...]\n", self.total - kept));
        }
        if !self.tail.is_empty() {
            let tail: Vec<u8> = self.tail.iter().copied().collect();
            out.push_str(&clean_output(&tail));
        }
        out
    }
}

#[derive(Clone, Copy, PartialEq)]
enum St {
    Ground,
    Esc,
    Osc,
    OscEsc,
}

pub struct Scanner {
    st: St,
    osc: Vec<u8>,
    pub capture: Option<Capture>,
    /// Marks must carry this per-pane secret (`k=<nonce>`), so output that
    /// merely contains mark sequences (say, `cat` of a crafted file) cannot
    /// forge command boundaries or exit codes.
    nonce: Option<String>,
    /// The secrets of the shells inside the pane's own, outermost first.
    inner: Vec<String>,
}

impl Default for Scanner {
    fn default() -> Self {
        Self { st: St::Ground, osc: Vec::new(), capture: None, nonce: None, inner: Vec::new() }
    }
}

impl Scanner {
    pub fn with_nonce(nonce: String) -> Self {
        Self { nonce: Some(nonce), ..Self::default() }
    }
}

impl Scanner {
    /// A shell inside the innermost one so far has its marks in place,
    /// carrying this secret: they count from now on, one shell further in.
    pub fn arrived(&mut self, secret: String) {
        self.inner.push(secret);
    }

    /// Only this many shells inside the pane's own are still there.
    pub fn keep_inner(&mut self, levels: usize) {
        self.inner.truncate(levels);
    }

    pub fn feed(&mut self, mut bytes: &[u8], marks: &mut Vec<Mark>) {
        while !bytes.is_empty() {
            match self.st {
                St::Ground => {
                    let n = bytes.iter().position(|&b| b == 0x1b).unwrap_or(bytes.len());
                    if n > 0 {
                        if let Some(c) = &mut self.capture {
                            c.push(&bytes[..n]);
                        }
                    }
                    if n < bytes.len() {
                        self.st = St::Esc;
                        bytes = &bytes[n + 1..];
                    } else {
                        bytes = &[];
                    }
                }
                St::Esc => {
                    let b = bytes[0];
                    bytes = &bytes[1..];
                    if b == b']' {
                        self.osc.clear();
                        self.st = St::Osc;
                    } else if b == 0x1b {
                        if let Some(c) = &mut self.capture {
                            c.push(&[0x1b]);
                        }
                    } else {
                        if let Some(c) = &mut self.capture {
                            c.push(&[0x1b, b]);
                        }
                        self.st = St::Ground;
                    }
                }
                St::Osc => {
                    let n = bytes.iter().position(|&b| b == 0x07 || b == 0x1b).unwrap_or(bytes.len());
                    if self.osc.len() + n <= OSC_MAX {
                        self.osc.extend_from_slice(&bytes[..n]);
                    } else {
                        // Oversized (e.g. an inline image); not one of ours.
                        self.osc.resize(OSC_MAX + 1, 0);
                    }
                    if n < bytes.len() {
                        let term = bytes[n];
                        bytes = &bytes[n + 1..];
                        if term == 0x07 {
                            self.finish(marks);
                        } else {
                            self.st = St::OscEsc;
                        }
                    } else {
                        bytes = &[];
                    }
                }
                St::OscEsc => {
                    let b = bytes[0];
                    if b == b'\\' {
                        bytes = &bytes[1..];
                        self.finish(marks);
                    } else {
                        // Malformed terminator: drop the OSC, reprocess as ESC.
                        self.st = St::Esc;
                    }
                }
            }
        }
    }

    fn finish(&mut self, marks: &mut Vec<Mark>) {
        self.st = St::Ground;
        if self.osc.len() > OSC_MAX {
            return;
        }
        let Ok(s) = std::str::from_utf8(&self.osc) else { return };
        if let Some(rest) = s.strip_prefix("133;") {
            let mut parts = rest.splitn(2, ';');
            let kind = parts.next().unwrap_or("");
            let mut arg = parts.next();
            // Which shell the mark is from: the pane's own, or one inside it.
            let mut level = 0;
            if let Some(nonce) = &self.nonce {
                // Expect `k=<nonce>` as the first parameter.
                let Some(a) = arg else { return };
                let (first, remainder) = match a.split_once(';') {
                    Some((f, r)) => (f, Some(r)),
                    None => (a, None),
                };
                let Some(given) = first.strip_prefix("k=") else { return };
                if given == nonce {
                    level = 0;
                } else if let Some(at) = self.inner.iter().position(|secret| secret == given) {
                    level = at + 1;
                } else {
                    return;
                }
                arg = remainder;
            }
            let mark = match kind {
                "A" => Mark::PromptStart,
                "C" => {
                    let cmd = arg.and_then(|a| a.strip_prefix("cmd=")).map(|c| c.trim().to_string());
                    self.capture = Some(Capture::default());
                    Mark::OutputStart { cmd }
                }
                "D" => {
                    let exit = arg.and_then(|a| a.split(';').next()).and_then(|a| a.trim().parse().ok());
                    Mark::CmdEnd { exit, output: self.capture.take() }
                }
                _ => return,
            };
            marks.push(if level == 0 { mark } else { Mark::Inner { level, mark: Box::new(mark) } });
        } else if let Some(url) = s.strip_prefix("7;") {
            // (It says nothing of which shell it is from: the innermost one.)
            if let Some(path) = parse_file_url(url) {
                let mark = Mark::Cwd(path);
                let level = self.inner.len();
                marks.push(if level == 0 { mark } else { Mark::Inner { level, mark: Box::new(mark) } });
            }
        }
    }
}

fn parse_file_url(url: &str) -> Option<String> {
    let rest = url.strip_prefix("file://")?;
    let path = &rest[rest.find('/')?..];
    Some(percent_decode(path))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            if let Some(v) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Turn raw terminal output into plain text: escape sequences are removed and
/// carriage returns / backspaces are applied the way a terminal would, so
/// progress bars collapse to their final state.
pub fn clean_output(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let mut out = String::with_capacity(text.len());
    let mut line: Vec<char> = Vec::new();
    let mut col = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    // CSI: parameters/intermediates until a final byte @..~
                    for c in chars.by_ref() {
                        if ('\x40'..='\x7e').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') | Some('P') | Some('^') | Some('_') => {
                    chars.next();
                    // String sequence: until BEL or ST.
                    while let Some(c) = chars.next() {
                        if c == '\x07' {
                            break;
                        }
                        if c == '\x1b' {
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                Some('(') | Some(')') | Some('*') | Some('+') | Some('#') | Some('%') => {
                    chars.next();
                    chars.next();
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            },
            '\n' => {
                out.extend(line.drain(..));
                while out.ends_with(' ') {
                    out.pop();
                }
                out.push('\n');
                col = 0;
            }
            '\r' => col = 0,
            '\x08' => col = col.saturating_sub(1),
            '\t' => {
                put(&mut line, &mut col, '\t');
            }
            c if c.is_control() => {}
            c => put(&mut line, &mut col, c),
        }
    }
    out.extend(line.drain(..));
    out
}

fn put(line: &mut Vec<char>, col: &mut usize, c: char) {
    if *col < line.len() {
        line[*col] = c;
    } else {
        while line.len() < *col {
            line.push(' ');
        }
        line.push(c);
    }
    *col += 1;
}

/// Keep the first `head` and last `tail` lines of a long text.
pub fn truncate_lines(text: &str, head: usize, tail: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= head + tail + 1 {
        return text.trim_end().to_string();
    }
    let mut out = lines[..head].join("\n");
    out.push_str(&format!("\n[... {} lines omitted ...]\n", lines.len() - head - tail));
    out.push_str(&lines[lines.len() - tail..].join("\n"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(chunks: &[&[u8]]) -> Vec<Mark> {
        let mut s = Scanner::default();
        let mut marks = Vec::new();
        for c in chunks {
            s.feed(c, &mut marks);
        }
        marks
    }

    #[test]
    fn marks_and_capture() {
        let m = run(&[b"\x1b]133;A\x07$ ls\r\n\x1b]133;C;cmd=ls -la\x07a.txt\r\nb.txt\r\n\x1b]133;D;0\x07\x1b]133;A\x07"]);
        assert_eq!(m[0], Mark::PromptStart);
        assert_eq!(m[1], Mark::OutputStart { cmd: Some("ls -la".into()) });
        match &m[2] {
            Mark::CmdEnd { exit, output } => {
                assert_eq!(*exit, Some(0));
                assert_eq!(output.as_ref().unwrap().text(), "a.txt\nb.txt\n");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(m[3], Mark::PromptStart);
    }

    #[test]
    fn split_across_chunks() {
        let m = run(&[b"\x1b]13", b"3;C\x1b", b"\\hel", b"lo\x1b]133;D;", b"12\x07"]);
        match &m[1] {
            Mark::CmdEnd { exit, output } => {
                assert_eq!(*exit, Some(12));
                assert_eq!(output.as_ref().unwrap().text(), "hello");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn marks_from_a_shell_inside_say_how_far_in_it_is() {
        let mut s = Scanner::with_nonce("own".into());
        let mut marks = Vec::new();
        // A secret nobody has vouched for means nothing.
        s.feed(b"\x1b]133;A;k=far\x07", &mut marks);
        assert!(marks.is_empty());
        // Once the shell with it is there, its marks are marks, one shell in.
        s.arrived("far".into());
        s.feed(b"\x1b]7;file://web/srv\x07\x1b]133;A;k=far\x07\x1b]133;C;k=far;cmd=ls\x07out\x1b]133;D;k=far;2\x07", &mut marks);
        assert!(matches!(&marks[0], Mark::Inner { level: 1, mark } if **mark == Mark::Cwd("/srv".into())));
        assert!(matches!(&marks[1], Mark::Inner { level: 1, mark } if **mark == Mark::PromptStart));
        assert!(matches!(&marks[3], Mark::Inner { level: 1, mark } if matches!(&**mark, Mark::CmdEnd { exit: Some(2), output: Some(c) } if c.text() == "out")));
        // The pane's own shell is still itself, and when the one inside has gone, so are its marks.
        marks.clear();
        s.feed(b"\x1b]133;D;k=own;0\x07", &mut marks);
        assert!(matches!(&marks[0], Mark::CmdEnd { exit: Some(0), .. }));
        s.keep_inner(0);
        marks.clear();
        s.feed(b"\x1b]133;A;k=far\x07\x1b]7;file://here/home\x07", &mut marks);
        assert_eq!(marks, [Mark::Cwd("/home".into())]);
    }

    #[test]
    fn nonce_rejects_forged_marks() {
        let mut s = Scanner::with_nonce("s3cret".into());
        let mut marks = Vec::new();
        s.feed(b"\x1b]133;A\x07\x1b]133;D;0\x07\x1b]133;A;k=wrong\x07", &mut marks);
        assert!(marks.is_empty());
        s.feed(b"\x1b]133;A;k=s3cret\x07\x1b]133;C;k=s3cret;cmd=echo a;b\x07x\x1b]133;D;k=s3cret;3\x07", &mut marks);
        assert_eq!(marks[0], Mark::PromptStart);
        assert_eq!(marks[1], Mark::OutputStart { cmd: Some("echo a;b".into()) });
        assert!(matches!(&marks[2], Mark::CmdEnd { exit: Some(3), .. }));
    }

    #[test]
    fn cwd() {
        let m = run(&[b"\x1b]7;file://host/Users/me/My%20Code\x07"]);
        assert_eq!(m, vec![Mark::Cwd("/Users/me/My Code".into())]);
    }

    #[test]
    fn cleaning() {
        assert_eq!(clean_output(b"\x1b[1;32mok\x1b[0m\r\n"), "ok\n");
        assert_eq!(clean_output(b"10%\r50%\r100%\n"), "100%\n");
        assert_eq!(clean_output(b"abc\x08\x08xy\n"), "axy\n");
        assert_eq!(clean_output(b"\x1b]0;title\x07text"), "text");
    }

    #[test]
    fn capture_is_bounded() {
        let mut c = Capture::default();
        let chunk = vec![b'x'; 50_000];
        for _ in 0..10 {
            c.push(&chunk);
        }
        assert_eq!(c.total, 500_000);
        assert!(c.head.len() <= HEAD_MAX && c.tail.len() <= TAIL_MAX);
        assert!(c.text().contains("omitted"));
    }
}
