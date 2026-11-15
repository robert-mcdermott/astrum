//! What a pane says about itself: that it needs you, is at work, is done,
//! or has failed. Astrum's own agent is not the only thing that can need
//! you; any program in any pane can, and so can a command that has been
//! running a while.
//!
//! What is understood of what programs print (the scanner in `crate::osc`
//! hands these over):
//!
//! - a desktop notification, which Astrum would otherwise ignore: OSC 9
//!   (`ESC ] 9 ; text BEL`, but not its numbered forms, such as `9;4`
//!   progress and `9;9` directories), OSC 777 (`ESC ] 777 ; notify ; title
//!   ; body BEL`) and kitty's OSC 99 (metadata, then the text, perhaps in
//!   pieces)
//! - the program-status sequence, `ESC ] 7501 ; key=value:key=value ESC \`,
//!   with `state` (`idle`, `working`, `done`, `blocked`, `error` or
//!   `clear`), `kind` (with `blocked`: `permission`, `question` or `auth`),
//!   `progress` (0 to 100), `msg` and `title` (base64), `app`, and `id` for
//!   one of several tasks in the pane; and `ESC ] 7501 ; ? ESC \`, which
//!   asks what is understood
//!
//! Everything a program says here is only text to show: control characters
//! and the characters that reorder or hide text are taken out, and its
//! length is capped, before it is drawn, sent on, or reaches the agent.

use base64::Engine;
use std::time::Instant;

/// The answer to `ESC ] 7501 ; ? ESC \`: what is understood.
pub const UNDERSTOOD: &str = "\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth\x1b\\";

/// The longest message and title a program can give, in bytes once decoded.
const MSG_MAX: usize = 2048;
const TITLE_MAX: usize = 192;
/// The longest name a program can give itself, in characters.
const APP_MAX: usize = 32;
/// A task's name: at most this many parts (`deploy/eu-west`), and this long.
const ID_PARTS: usize = 8;
const ID_MAX: usize = 128;
/// The longest notification text kept (title and body together).
const NOTICE_MAX: usize = 1024;
/// How many of kitty's notifications can be arriving in pieces at once.
const KITTY_OPEN: usize = 4;
/// How many things a pane can be saying at once (tasks and news).
const KEPT: usize = 16;

/// What a program said, as the scanner found it.
#[derive(Debug, Clone, PartialEq)]
pub enum Said {
    /// It asked for a desktop notification.
    Notice { title: String, body: String },
    /// It told its state.
    Status(Status),
    /// It asked what is understood of the program-status sequence.
    Asks,
}

/// A program's state, as the program-status sequence tells it.
#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    /// Which of the pane's tasks it is about: empty for the pane itself.
    pub id: String,
    pub state: State,
    pub kind: Option<String>,
    pub progress: Option<u8>,
    pub msg: String,
    pub title: String,
    pub app: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum State {
    Idle,
    Working,
    Done,
    Blocked,
    Error,
    Clear,
}

/// One of kitty's notifications, still arriving.
#[derive(Debug, Default)]
pub struct KittyPart {
    id: String,
    title: String,
    body: String,
}

/// Read an operating-system command (what came between `ESC ]` and its
/// end), if it is one of those understood here.
pub fn parse(osc: &str, kitty: &mut Vec<KittyPart>) -> Option<Said> {
    if let Some(rest) = osc.strip_prefix("7501;") {
        return status(rest);
    }
    if let Some(rest) = osc.strip_prefix("777;notify;") {
        let (title, body) = rest.split_once(';').unwrap_or((rest, ""));
        return notice(title, body);
    }
    if let Some(rest) = osc.strip_prefix("99;") {
        return kitty_part(rest, kitty);
    }
    if let Some(text) = osc.strip_prefix("9;") {
        // The numbered forms are something else: progress (`9;4;…`), the
        // directory (`9;9;…`) and others of the same family.
        let numbered = text.split(';').next().is_some_and(|first| !first.is_empty() && first.bytes().all(|b| b.is_ascii_digit()));
        if numbered {
            return None;
        }
        return notice("", text);
    }
    None
}

fn notice(title: &str, body: &str) -> Option<Said> {
    let (title, body) = (clean(title, NOTICE_MAX), clean(body, NOTICE_MAX));
    (!title.is_empty() || !body.is_empty()).then_some(Said::Notice { title, body })
}

/// A piece of one of kitty's notifications: `metadata ; text`. The
/// metadata says which notification (`i`), whether the text is the title
/// or the body (`p`), whether it is base64 (`e=1`), and whether more is to
/// come (`d=0`).
fn kitty_part(rest: &str, kitty: &mut Vec<KittyPart>) -> Option<Said> {
    let (meta, text) = rest.split_once(';').unwrap_or((rest, ""));
    let (mut id, mut done, mut part, mut encoded) = ("", true, "title", false);
    for pair in meta.split(':') {
        match pair.split_once('=') {
            Some(("i", v)) => id = v,
            Some(("d", v)) => done = v != "0",
            Some(("p", v)) => part = v,
            Some(("e", v)) => encoded = v == "1",
            _ => {}
        }
    }
    // Only words are shown: not icons, buttons, or questions about what
    // is understood.
    if !matches!(part, "title" | "body") {
        return None;
    }
    let text = if encoded { decode(text, NOTICE_MAX)? } else { text.to_string() };
    let at = match kitty.iter().position(|k| k.id == id) {
        Some(at) => at,
        None => {
            if kitty.len() >= KITTY_OPEN {
                kitty.remove(0);
            }
            kitty.push(KittyPart { id: id.to_string(), ..KittyPart::default() });
            kitty.len() - 1
        }
    };
    let open = &mut kitty[at];
    let into = if part == "body" { &mut open.body } else { &mut open.title };
    if into.len() + text.len() <= NOTICE_MAX * 4 {
        into.push_str(&text);
    }
    if !done {
        return None;
    }
    let whole = kitty.remove(at);
    notice(&whole.title, &whole.body)
}

fn status(rest: &str) -> Option<Said> {
    if rest == "?" {
        return Some(Said::Asks);
    }
    let mut state = None;
    let mut status = Status { id: String::new(), state: State::Idle, kind: None, progress: None, msg: String::new(), title: String::new(), app: String::new() };
    for pair in rest.split(':') {
        let Some((key, value)) = pair.split_once('=') else { continue };
        match key {
            "state" => {
                state = Some(match value {
                    "idle" => State::Idle,
                    "working" => State::Working,
                    "done" => State::Done,
                    "blocked" => State::Blocked,
                    "error" => State::Error,
                    "clear" => State::Clear,
                    _ => return None,
                })
            }
            "kind" if matches!(value, "permission" | "question" | "auth") => status.kind = Some(value.to_string()),
            "progress" => status.progress = value.parse::<u32>().ok().map(|p| p.min(100) as u8),
            "msg" => status.msg = clean(&decode(value, MSG_MAX).unwrap_or_default(), MSG_MAX),
            "title" => status.title = clean(&decode(value, TITLE_MAX).unwrap_or_default(), TITLE_MAX),
            "app" => status.app = clean(value, APP_MAX),
            "id" => status.id = task(value),
            _ => {}
        }
    }
    status.state = state?;
    Some(Said::Status(status))
}

/// A task's name, kept to its limits.
fn task(value: &str) -> String {
    let parts: Vec<String> = value.split('/').take(ID_PARTS).map(|p| clean(p, ID_MAX)).filter(|p| !p.is_empty()).collect();
    clean(&parts.join("/"), ID_MAX)
}

/// Base64 to text, the bytes past `max` dropped.
fn decode(value: &str, max: usize) -> Option<String> {
    let engines = [base64::engine::general_purpose::STANDARD, base64::engine::general_purpose::STANDARD_NO_PAD];
    let mut bytes = engines.iter().find_map(|e| e.decode(value.trim()).ok())?;
    bytes.truncate(max);
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Text from a program, made safe to show: on one line, without control
/// characters or those that reorder or hide what is around them, and no
/// longer than `max` (in bytes, cut at a character).
pub fn clean(text: &str, max: usize) -> String {
    let mut out = String::new();
    for c in text.chars() {
        let c = if c.is_whitespace() { ' ' } else { c };
        if c.is_control() || hidden(c) || (c == ' ' && (out.is_empty() || out.ends_with(' '))) {
            continue;
        }
        if out.len() + c.len_utf8() > max {
            break;
        }
        out.push(c);
    }
    out.truncate(out.trim_end().len());
    out
}

/// Characters that change the order text is shown in, or are not shown.
fn hidden(c: char) -> bool {
    matches!(c, '\u{061C}' | '\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}')
}

// ---- what a pane is saying ----------------------------------------------------

/// How much a pane needs you, least first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Need {
    Working,
    Done,
    Failed,
    NeedsYou,
}

/// Who said it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum From {
    /// Shell integration: a command that ran a while has ended.
    Shell,
    /// The bell.
    Bell,
    /// A program's desktop notification.
    Notice,
    /// A program's own state (the program-status sequence).
    Program,
}

/// One thing a pane is saying.
#[derive(Debug, Clone, PartialEq)]
pub struct Signal {
    pub from: From,
    /// The task it is about, for a program with several; otherwise empty.
    pub id: String,
    pub need: Need,
    /// What kind of answer is wanted, for a program waiting on you.
    pub kind: Option<String>,
    /// The command, the notification, or the program's message.
    pub text: String,
    pub title: String,
    pub app: String,
    pub progress: Option<u8>,
    pub exit: Option<i32>,
    pub at: Instant,
    /// It came while nobody could see the pane, and has not been seen since.
    pub news: bool,
}

impl Signal {
    pub fn new(from: From, need: Need, text: String) -> Self {
        Signal { from, id: String::new(), need, kind: None, text, title: String::new(), app: String::new(), progress: None, exit: None, at: Instant::now(), news: false }
    }

    /// A state the program holds until it says otherwise (at work, or
    /// waiting for you): seeing it does not make it go away.
    pub fn lasting(&self) -> bool {
        self.from == From::Program && matches!(self.need, Need::Working | Need::NeedsYou)
    }
}

/// What a pane is saying, and whether you want to hear it.
#[derive(Debug, Default)]
pub struct Signals {
    pub list: Vec<Signal>,
    /// "Don't tell me about this pane."
    pub quiet: bool,
    /// When a notification about it last went out.
    pub told: Option<Instant>,
}

impl Signals {
    /// Say it, in place of what was said before from the same source about
    /// the same task.
    pub fn set(&mut self, signal: Signal) {
        self.list.retain(|s| !(s.from == signal.from && s.id == signal.id));
        if self.list.len() >= KEPT {
            self.list.remove(0);
        }
        self.list.push(signal);
    }

    /// A program's task has nothing more to say.
    pub fn clear_task(&mut self, id: &str) {
        self.list.retain(|s| !(s.from == From::Program && s.id == id));
    }

    /// The program that was speaking has ended (the shell is back at its
    /// prompt): what it held is over. What it told meanwhile is still news.
    pub fn program_ended(&mut self) {
        self.list.retain(|s| !s.lasting());
    }

    /// Someone can see the pane: what was news is not any longer. Returns
    /// whether anything changed.
    pub fn look(&mut self) -> bool {
        let before = self.list.len();
        let mut changed = false;
        self.list.retain(|s| s.lasting());
        for s in self.list.iter_mut().filter(|s| s.news) {
            s.news = false;
            changed = true;
        }
        changed || self.list.len() != before
    }

    /// The most pressing thing it says.
    pub fn top(&self) -> Option<&Signal> {
        self.list.iter().max_by_key(|s| (s.need, s.at))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(osc: &str) -> Option<Said> {
        parse(osc, &mut Vec::new())
    }

    fn b64(s: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(s)
    }

    #[test]
    fn desktop_notifications_of_each_kind() {
        assert_eq!(one("9;Build finished"), Some(Said::Notice { title: String::new(), body: "Build finished".into() }));
        assert_eq!(one("777;notify;Tests;3 failed"), Some(Said::Notice { title: "Tests".into(), body: "3 failed".into() }));
        assert_eq!(one("99;;Hello there"), Some(Said::Notice { title: "Hello there".into(), body: String::new() }));
        // Not notifications: progress, a directory, and other numbered forms.
        assert_eq!(one("9;4;1;50"), None);
        assert_eq!(one("9;9;/home/me"), None);
        assert_eq!(one("9;12"), None);
        // Nor other commands of the same numbers, nor an empty one.
        assert_eq!(one("777;preexec"), None);
        assert_eq!(one("9;"), None);
        assert_eq!(one("0;a title"), None);
    }

    #[test]
    fn kitty_notifications_in_pieces_and_in_base64() {
        let mut open = Vec::new();
        assert_eq!(parse("99;i=1:d=0;Deploy", &mut open), None);
        assert_eq!(parse("99;i=2:d=0:p=title;Other", &mut open), None);
        assert_eq!(parse(&format!("99;i=1:p=body:e=1;{}", b64("is done")), &mut open), Some(Said::Notice { title: "Deploy".into(), body: "is done".into() }));
        assert_eq!(open.len(), 1, "the other is still open");
        // An icon or a question is not shown.
        assert_eq!(parse("99;i=3:p=icon;abc", &mut open), None);
        assert_eq!(parse("99;i=3:p=?;", &mut open), None);
    }

    #[test]
    fn the_program_status_sequence() {
        let said = one(&format!("7501;state=blocked:kind=permission:app=coder:id=deploy/eu-west:progress=140:msg={}:title={}", b64("Allow a write?"), b64("Waiting")));
        let Some(Said::Status(s)) = said else { panic!("{said:?}") };
        assert_eq!((s.state, s.kind.as_deref(), s.app.as_str(), s.id.as_str(), s.progress), (State::Blocked, Some("permission"), "coder", "deploy/eu-west", Some(100)));
        assert_eq!((s.msg.as_str(), s.title.as_str()), ("Allow a write?", "Waiting"));
        assert_eq!(one("7501;?"), Some(Said::Asks));
        // A state that is not one of them, or none, is not taken.
        assert_eq!(one("7501;state=sleeping"), None);
        assert_eq!(one("7501;progress=3"), None);
        // An unknown kind is left out; the rest stands.
        assert!(matches!(one("7501;state=blocked:kind=coffee"), Some(Said::Status(Status { kind: None, state: State::Blocked, .. }))));
    }

    #[test]
    fn what_programs_say_is_made_safe_and_kept_short() {
        assert_eq!(clean("a\x1b[31mred\x07\r\nline\u{202E}, evil\u{200B}", 100), "a[31mred line, evil");
        assert_eq!(clean("  spaced \t out  ", 100), "spaced out");
        assert_eq!(clean("ééé", 5), "éé");
        let long = b64(&"x".repeat(5000));
        let Some(Said::Status(s)) = one(&format!("7501;state=done:msg={long}:app={}", "y".repeat(80))) else { panic!() };
        assert_eq!((s.msg.len(), s.app.len()), (MSG_MAX, APP_MAX));
        assert_eq!(task("a/b/c/d/e/f/g/h/i/j"), "a/b/c/d/e/f/g/h");
    }

    #[test]
    fn what_a_pane_says_is_kept_until_seen_or_over() {
        let mut s = Signals::default();
        let mut working = Signal::new(From::Program, Need::Working, "building".into());
        working.id = "build".into();
        s.set(working);
        s.set(Signal { news: true, ..Signal::new(From::Bell, Need::NeedsYou, String::new()) });
        assert_eq!(s.top().map(|t| t.from), Some(From::Bell));
        // Seeing the pane ends the news, not what the program holds.
        assert!(s.look());
        assert_eq!(s.list.len(), 1);
        assert!(!s.look());
        s.program_ended();
        assert!(s.list.is_empty());
    }
}
