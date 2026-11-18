//! A file pasted or dropped into a tab on another machine.
//!
//! A terminal that has a file dropped on it, or pasted into it, types the
//! file's path, as a paste; some write a pasted picture to a temporary
//! file and type that. In a tab on another machine such a path means
//! nothing. Astrum sees the paste before the shell there does: when it is
//! nothing but the path of a file on this machine, the file is sent over
//! the link and what is pasted is where it now is there.
//!
//! It asks first. A path can be meant as it is written (`/etc/hosts` is on
//! both machines), and text copied from a page can hold a path nobody
//! looked at: `~/.ssh/id_rsa`, sent without a word, would put a private
//! key on another machine. Only a picture in a temporary folder is sent
//! unasked, since its path can mean nothing else, and whatever the user
//! has said to send always to that machine.
//!
//! The agent has no part in it: a file goes because the user pasted it.

use super::{Ev, Menu, MenuAction, Overlay, Server};
use crate::remote::files::{self, MOST, PIECE};
use crate::remote::{Answer, Ask, Way};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::task::AbortHandle;

/// More than this is asked about, whatever was said before.
const ASK_ABOVE: u64 = 50 * 1024 * 1024;
/// The most files one paste is taken to name.
const MOST_FILES: usize = 20;
/// The most pieces on their way at a time.
const FLYING: usize = 4;

/// The words of a paste, as a shell would take them apart: split at
/// blanks, with `\ `, `'…'` and `"…"` read as terminals write them.
/// `None`: quotes that are not closed, which no terminal writes.
fn words(text: &str) -> Option<Vec<String>> {
    let mut all = Vec::new();
    let mut word = String::new();
    let mut begun = false;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if begun {
                    all.push(std::mem::take(&mut word));
                    begun = false;
                }
            }
            '\\' => {
                word.push(chars.next()?);
                begun = true;
            }
            '\'' => {
                begun = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        c => word.push(c),
                    }
                }
            }
            '"' => {
                begun = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => match chars.next()? {
                            c @ ('"' | '\\' | '$' | '`') => word.push(c),
                            c => {
                                word.push('\\');
                                word.push(c);
                            }
                        },
                        c => word.push(c),
                    }
                }
            }
            c => {
                word.push(c);
                begun = true;
            }
        }
    }
    if begun {
        all.push(word);
    }
    Some(all)
}

/// `%20` and the like, in a `file://` address.
fn unpercent(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The path a word names, if it names one from the top: `/a/b`, `~/b`,
/// `file:///a/b`.
fn path_of(word: &str) -> Option<PathBuf> {
    if let Some(rest) = word.strip_prefix("file://") {
        let rest = rest.strip_prefix("localhost").unwrap_or(rest);
        return rest.starts_with('/').then(|| unpercent(rest)).flatten().map(PathBuf::from);
    }
    if let Some(rest) = word.strip_prefix("~/") {
        return Some(crate::paths::home().join(rest));
    }
    word.starts_with('/').then(|| PathBuf::from(word))
}

fn is_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file())
}

/// The files a paste names, if it is nothing but the paths of files that
/// are on this machine: one as it is written, spaces and all, or several
/// the way terminals write them.
pub(super) fn files_in(text: &str) -> Option<Vec<PathBuf>> {
    let text = text.trim();
    if text.is_empty() || text.len() > 16 * 1024 {
        return None;
    }
    if !text.contains('\n') {
        if let Some(path) = path_of(text).filter(|p| is_file(p)) {
            return Some(vec![path]);
        }
    }
    let words = words(text)?;
    if words.is_empty() || words.len() > MOST_FILES {
        return None;
    }
    words.iter().map(|w| path_of(w).filter(|p| is_file(p))).collect()
}

/// A picture where things are put for a moment: the screenshot a terminal
/// wrote out so as to have a path to paste. Its path can mean nothing on
/// another machine, so nobody is asked.
fn passing_picture(path: &Path) -> bool {
    let picture = path.extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" | "tiff" | "tif" | "bmp"));
    let text = path.to_string_lossy();
    let temp = std::env::temp_dir();
    let passing = path.starts_with(&temp)
        || std::fs::canonicalize(&temp).is_ok_and(|t| path.starts_with(t))
        || ["/tmp/", "/private/tmp/", "/var/folders/", "/private/var/folders/", "/var/tmp/"].iter().any(|p| text.starts_with(p))
        || text.contains("/TemporaryItems/");
    picture && passing
}

/// A size as people say it.
pub(super) fn size_said(bytes: u64) -> String {
    match bytes {
        0..1024 => format!("{bytes} bytes"),
        1024..1_048_576 => format!("{} KB", bytes.div_ceil(1024)),
        1_048_576..1_073_741_824 => format!("{:.1} MB", bytes as f64 / 1_048_576.0),
        _ => format!("{:.1} GB", bytes as f64 / 1_073_741_824.0),
    }
}

/// A path as a shell takes it for one word. (A file's name there has
/// nothing in it to mind; the folders above it might.)
fn for_a_shell(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        if !(c.is_alphanumeric() || "/._+@=,-:%~".contains(c)) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn name_of(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into())
}

/// A name short enough for a menu or the status bar.
fn short(name: &str, most: usize) -> String {
    if name.chars().count() <= most {
        return name.to_string();
    }
    let keep = most.saturating_sub(1);
    let (head, tail) = (keep / 2, keep - keep / 2);
    let chars: Vec<char> = name.chars().collect();
    format!("{}…{}", chars[..head].iter().collect::<String>(), chars[chars.len() - tail..].iter().collect::<String>())
}

/// A paste that names files of this machine, in a pane on another.
pub struct Pasted {
    /// What was pasted, to paste after all if that is what is wanted.
    text: String,
    files: Vec<(PathBuf, u64)>,
    host: String,
    pane: u64,
}

impl Pasted {
    fn total(&self) -> u64 {
        self.files.iter().map(|(_, size)| size).sum()
    }

    /// `screenshot.png`, or `3 files`.
    fn what(&self, most: usize) -> String {
        match self.files.as_slice() {
            [(path, _)] => short(&name_of(path), most),
            many => format!("{} files", many.len()),
        }
    }
}

/// Files on their way to another machine.
pub struct Sending {
    pub id: u64,
    pub host: String,
    pane: u64,
    /// `screenshot.png`, or `3 files`.
    pub what: String,
    pub total: u64,
    pub sent: Arc<AtomicU64>,
    /// The paste ended in a blank, as a dropped file's does.
    blank_after: bool,
    /// The files begun (the mark of each, and the name this sending of
    /// it goes by), to say they are not wanted if it is stopped.
    begun: Arc<Mutex<Vec<(String, String)>>>,
    task: AbortHandle,
}

impl Sending {
    pub fn percent(&self) -> u64 {
        if self.total == 0 { 100 } else { (self.sent.load(Ordering::Relaxed).min(self.total) * 100) / self.total }
    }
}

impl Drop for Sending {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// What the other machine said to something asked of it about a file.
async fn asked(way: &Way, ask: Ask) -> Result<String, String> {
    match way.ask(ask).await {
        Ok(Answer::Text { text, failed: false }) => Ok(text),
        Ok(Answer::Text { text, .. }) => Err(text),
        Ok(_) => Err("The other machine answered something else.".into()),
        Err(_) => Err("the connection was lost on the way".into()),
    }
}

/// Send one file; where it is there.
async fn send(way: &Way, path: &Path, size: u64, sent: &AtomicU64, begun: &Mutex<Vec<(String, String)>>, moved: &(impl Fn() + Sync)) -> Result<String, String> {
    use base64::Engine;
    use futures_util::StreamExt;
    use tokio::io::AsyncReadExt;
    let name = name_of(path);
    let unread = |e: std::io::Error| format!("{name} could not be read: {e}");
    let sum = files::sum_of(path.to_path_buf()).await.map_err(unread)?;
    // It may be there from before: then there is nothing to send.
    let there = asked(way, Ask::Have { sum: sum.clone(), name: name.clone(), size }).await?;
    if !there.is_empty() {
        sent.fetch_add(size, Ordering::Relaxed);
        moved();
        return Ok(there);
    }
    // (A name of this sending's own, that no other has.)
    let put = {
        use std::hash::{BuildHasher, Hasher};
        format!("{:016x}", std::collections::hash_map::RandomState::new().build_hasher().finish())
    };
    if let Ok(mut begun) = begun.lock() {
        begun.push((sum.clone(), put.clone()));
    }
    let mut file = tokio::fs::File::open(path).await.map_err(unread)?;
    let mut flying = futures_util::stream::FuturesUnordered::new();
    let mut at = 0u64;
    loop {
        // A piece is filled before it goes: the last is the only short one.
        let mut piece = vec![0u8; PIECE];
        let mut filled = 0;
        while filled < PIECE {
            match file.read(&mut piece[filled..]).await.map_err(unread)? {
                0 => break,
                n => filled += n,
            }
        }
        if filled == 0 {
            break;
        }
        let ask = Ask::Piece { sum: sum.clone(), put: put.clone(), size, at, bytes: base64::engine::general_purpose::STANDARD.encode(&piece[..filled]) };
        flying.push(async move { asked(way, ask).await.map(|_| filled as u64) });
        at += filled as u64;
        if flying.len() >= FLYING {
            if let Some(done) = flying.next().await {
                sent.fetch_add(done?, Ordering::Relaxed);
                moved();
            }
        }
    }
    while let Some(done) = flying.next().await {
        sent.fetch_add(done?, Ordering::Relaxed);
        moved();
    }
    if at != size {
        let _ = asked(way, Ask::Forget { sum, put }).await;
        return Err(format!("{name} changed while it was being sent"));
    }
    asked(way, Ask::Keep { sum, put, name, size }).await
}

/// The picture on the clipboard, written to a file: its path, or why
/// there is none. `ASTRUM_PICTURE_TOOL` names a program that writes it
/// (a PNG) to its output, or `none`.
async fn clipboard_picture() -> Result<PathBuf, String> {
    use std::process::Stdio;
    use tokio::process::Command;
    const NONE: &str = "There is no picture on the clipboard.";
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let to = std::env::temp_dir().join(format!("clipboard-{secs}-{}.png", NEXT.fetch_add(1, Ordering::Relaxed)));
    let chosen = std::env::var("ASTRUM_PICTURE_TOOL").ok().filter(|t| !t.is_empty());
    // (Each: a program and its arguments, and whether it writes the file
    // itself or the picture to its output.)
    let ways: Vec<(String, Vec<String>, bool)> = match chosen.as_deref() {
        Some("none") => return Err("Reading pictures from the clipboard is turned off (ASTRUM_PICTURE_TOOL).".into()),
        Some(tool) => vec![(tool.to_string(), vec![], false)],
        None if cfg!(target_os = "macos") => {
            let script = ["on run argv", "set png to (the clipboard as «class PNGf»)", "set f to (open for access POSIX file (item 1 of argv) with write permission)", "set eof f to 0", "write png to f", "close access f", "end run"];
            let mut args: Vec<String> = script.iter().flat_map(|line| ["-e".to_string(), line.to_string()]).collect();
            args.push(to.to_string_lossy().into_owned());
            vec![("osascript".into(), args, true)]
        }
        None => vec![
            ("wl-paste".into(), vec!["--type".into(), "image/png".into()], false),
            ("xclip".into(), vec!["-selection".into(), "clipboard".into(), "-t".into(), "image/png".into(), "-o".into()], false),
        ],
    };
    let mut found_a_tool = false;
    for (tool, args, writes_it) in ways {
        let run = Command::new(&tool).args(&args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true).output();
        let Ok(Ok(out)) = tokio::time::timeout(Duration::from_secs(10), run).await else { continue };
        found_a_tool = true;
        if !writes_it && out.status.success() {
            let _ = tokio::fs::write(&to, &out.stdout).await;
        }
        // (A PNG, and not a word of complaint written where one was to be.)
        let png = tokio::fs::read(&to).await.is_ok_and(|bytes| bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        if png {
            return Ok(to);
        }
        let _ = tokio::fs::remove_file(&to).await;
    }
    Err(if found_a_tool { NONE.into() } else { "No program was found that reads a picture from the clipboard (wl-paste or xclip).".to_string() })
}

impl Server {
    fn sends_always(&self, host: &str) -> bool {
        self.cfg.state.send_files.iter().flatten().any(|h| h == host)
    }

    /// A paste into the shell the keyboard is in: if that shell is on
    /// another machine and the paste names files of this one, it is taken
    /// from here (`true`), and nothing is pasted yet.
    pub(super) fn file_paste(&mut self, text: &str) -> bool {
        let Some(pane) = self.tab().map(|t| t.panes.focused()) else { return false };
        let Some(host) = pane.there().map(|t| t.host.clone()) else { return false };
        let pane = pane.id;
        let Some(paths) = files_in(text) else { return false };
        let name = self.far(&host).map(|f| f.name()).unwrap_or_else(|| host.clone());
        let Some(far) = self.far(&host).filter(|f| f.way.is_some()) else { return false };
        if !far.takes_files {
            self.toast(format!("The Astrum on {name} is from before files could be sent to it, so the path was pasted as it is. Update Astrum there to have the file sent."));
            return false;
        }
        let files: Vec<(PathBuf, u64)> = paths.into_iter().map(|p| (std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0), p)).map(|(size, p)| (p, size)).collect();
        if let Some((path, _)) = files.iter().find(|(_, size)| *size > MOST) {
            self.toast(format!("{} is larger than {}, which is more than is sent this way: the path was pasted as it is.", short(&name_of(path), 40), size_said(MOST)));
            return false;
        }
        let pasted = Pasted { text: text.to_string(), files, host, pane };
        let unasked = self.sends_always(&pasted.host) || pasted.files.iter().all(|(p, _)| passing_picture(p));
        if unasked && pasted.total() <= ASK_ABOVE {
            self.send_files(pasted);
        } else {
            self.ask_files(pasted);
        }
        true
    }

    /// Ask what is to be done with a paste that names files, where the
    /// pointer is if it is over the pane, and in the pane's corner if not.
    fn ask_files(&mut self, pasted: Pasted) {
        let name = self.far(&pasted.host).map(|f| f.name()).unwrap_or_else(|| pasted.host.clone());
        let rect = self.focused_rect();
        let (col, row) = self.ui.pointer;
        let over = col >= rect.x && col < rect.x + rect.width && row >= rect.y && row < rect.y + rect.height;
        let at = if over { (col, row) } else { (rect.x + 2, rect.y + 1) };
        let mut items = vec![(format!("Send {} ({}) to {name}, and paste where it is there", pasted.what(36), size_said(pasted.total())), MenuAction::FilesSend)];
        if !self.sends_always(&pasted.host) {
            items.push((format!("Always send files pasted on {name}"), MenuAction::FilesAlways));
        }
        items.push(("Paste the text as it is".to_string(), MenuAction::FilesAsText));
        self.pasted = Some(pasted);
        self.ui.overlay = Some(Overlay::Menu(Menu { at, items, selected: 0 }));
        self.dirty = true;
    }

    /// What was chosen of a paste that names files.
    pub(super) fn files_menu_action(&mut self, action: &MenuAction) {
        match action {
            MenuAction::FilesSend | MenuAction::FilesAlways => {
                let Some(pasted) = self.pasted.take() else { return };
                if matches!(action, MenuAction::FilesAlways) && !self.sends_always(&pasted.host) {
                    self.cfg.state.send_files.get_or_insert_default().push(pasted.host.clone());
                    self.cfg.save_state();
                }
                self.send_files(pasted);
            }
            MenuAction::FilesAsText => {
                if let Some(pasted) = self.pasted.take() {
                    self.paste_into(pasted.pane, &pasted.text);
                }
            }
            // (Of the machine the pane in front is on, as the menu said.)
            MenuAction::FilesAskAgain => {
                let Some(host) = self.tab().and_then(|t| t.panes.focused().there()).map(|t| t.host.clone()) else { return };
                if let Some(always) = self.cfg.state.send_files.as_mut() {
                    always.retain(|h| *h != host);
                    self.cfg.save_state();
                }
                let name = self.far(&host).map(|f| f.name()).unwrap_or_else(|| host.clone());
                self.toast(format!("Files pasted on {name} are asked about again before they are sent"));
            }
            MenuAction::FilesStop(id) => self.stop_sending(*id),
            MenuAction::PastePicture => self.paste_picture(),
            _ => {}
        }
    }

    /// The item of the main menu that takes "always" back, where it was said.
    pub(super) fn files_menu_item(&self) -> Option<(String, MenuAction)> {
        let host = self.tab()?.panes.focused().there()?.host.clone();
        let name = self.far(&host)?.name();
        self.sends_always(&host).then(|| (format!("Ask before sending files to {name}"), MenuAction::FilesAskAgain))
    }

    /// Paste into a pane by its number, as a paste made there would be.
    fn paste_into(&mut self, pane: u64, text: &str) -> bool {
        let Some(pane) = self.find_pane(pane).and_then(|idx| self.tabs[idx].panes.get_mut(pane)) else { return false };
        pane.leave_copy();
        pane.scroll_to_bottom();
        pane.note_paste(text);
        let bytes = crate::input::encode_paste(text, pane.mode());
        pane.write(bytes);
        self.dirty = true;
        true
    }

    fn send_files(&mut self, pasted: Pasted) {
        let Some(way) = self.far(&pasted.host).and_then(|f| f.way.clone()) else {
            let name = self.far(&pasted.host).map(|f| f.name()).unwrap_or_else(|| pasted.host.clone());
            return self.toast(format!("{name} is out of reach just now: nothing was sent, and nothing pasted"));
        };
        let id = self.next_sending;
        self.next_sending += 1;
        let (sent, begun) = (Arc::new(AtomicU64::new(0)), Arc::new(Mutex::new(Vec::new())));
        let (counted, marks, tx, files) = (sent.clone(), begun.clone(), self.tx.clone(), pasted.files.clone());
        let task = tokio::spawn(async move {
            // (The window is drawn again as it goes, a few times a second.)
            let last = Mutex::new(Instant::now());
            let moved = || {
                if let Ok(mut last) = last.lock() {
                    if last.elapsed() > Duration::from_millis(200) {
                        *last = Instant::now();
                        let _ = tx.try_send(Ev::FilesMoved);
                    }
                }
            };
            let mut there = Vec::new();
            let mut result = Ok(());
            for (path, size) in &files {
                match send(&way, path, *size, &counted, &marks, &moved).await {
                    Ok(path) => there.push(path),
                    Err(why) => {
                        result = Err(why);
                        break;
                    }
                }
            }
            let _ = tx.send(Ev::FilesSent { id, result: result.map(|()| there) }).await;
        });
        self.sending.push(Sending {
            id,
            what: pasted.what(28),
            total: pasted.total(),
            blank_after: pasted.text.ends_with(char::is_whitespace),
            host: pasted.host,
            pane: pasted.pane,
            sent,
            begun,
            task: task.abort_handle(),
        });
        self.dirty = true;
    }

    /// Files have arrived, or have not: where they are is pasted into the
    /// pane the paste was made in.
    pub(super) fn on_files_sent(&mut self, id: u64, result: Result<Vec<String>, String>) {
        let Some(at) = self.sending.iter().position(|s| s.id == id) else { return };
        let sending = self.sending.remove(at);
        let name = self.far(&sending.host).map(|f| f.name()).unwrap_or_else(|| sending.host.clone());
        self.dirty = true;
        match result {
            Ok(paths) => {
                let mut text = paths.iter().map(|p| for_a_shell(p)).collect::<Vec<_>>().join(" ");
                if sending.blank_after {
                    text.push(' ');
                }
                if !self.paste_into(sending.pane, &text) {
                    self.toast(format!("{} is on {name}, at {}", sending.what, text.trim_end()));
                }
            }
            Err(why) => self.toast(format!("{} was not sent to {name}: {why}. Nothing was pasted.", sending.what)),
        }
    }

    pub(super) fn stop_sending(&mut self, id: u64) {
        let Some(at) = self.sending.iter().position(|s| s.id == id) else { return };
        let sending = self.sending.remove(at);
        // What came of it there is not wanted.
        if let Some(way) = self.far(&sending.host).and_then(|f| f.way.clone()) {
            let begun = sending.begun.lock().map(|b| b.clone()).unwrap_or_default();
            sending.task.abort();
            tokio::spawn(async move {
                // (And once more, after the pieces that were on their way
                // have landed and made it anew.)
                for wait in [0, 3] {
                    tokio::time::sleep(Duration::from_secs(wait)).await;
                    for (sum, put) in &begun {
                        let _ = way.ask(Ask::Forget { sum: sum.clone(), put: put.clone() }).await;
                    }
                }
            });
        }
        self.toast(format!("Stopped sending {}. Nothing was pasted.", sending.what));
    }

    /// Nothing is sent to a machine nothing is shown from any more.
    pub(super) fn drop_unused_sending(&mut self) {
        let fars: Vec<&str> = self.fars.iter().map(|f| f.host.as_str()).collect();
        self.sending.retain(|s| fars.contains(&s.host.as_str()));
    }

    /// The menu of what is being sent: each to stop.
    pub(super) fn open_sending_menu(&mut self, col: u16) {
        let items: Vec<(String, MenuAction)> = self.sending.iter().map(|s| (format!("Stop sending {} ({} of {})", s.what, size_said(s.sent.load(Ordering::Relaxed).min(s.total)), size_said(s.total)), MenuAction::FilesStop(s.id))).collect();
        if items.is_empty() {
            return;
        }
        let at = (col, self.rows.saturating_sub(items.len() as u16 + 4));
        self.ui.overlay = Some(Overlay::Menu(Menu { at, items, selected: 0 }));
        self.dirty = true;
    }

    /// The picture on the clipboard, for a terminal that pastes nothing
    /// when a picture is what there is: written to a file, and that
    /// file's path pasted (on another machine, after it was sent there).
    fn paste_picture(&mut self) {
        let Some(pane) = self.tab().map(|t| t.panes.focused().id) else { return };
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = clipboard_picture().await;
            let _ = tx.send(Ev::Picture { pane, result }).await;
        });
    }

    pub(super) fn on_picture(&mut self, pane: u64, result: Result<PathBuf, String>) {
        let path = match result {
            Ok(path) => path,
            Err(why) => return self.toast(why),
        };
        let text = for_a_shell(&path.to_string_lossy());
        let host = self.find_pane(pane).and_then(|idx| self.tabs[idx].panes.get(pane)).and_then(|p| p.there()).map(|t| t.host.clone());
        let Some(host) = host else {
            self.paste_into(pane, &text);
            return;
        };
        let name = self.far(&host).map(|f| f.name()).unwrap_or_else(|| host.clone());
        if !self.far(&host).is_some_and(|f| f.takes_files) {
            return self.toast(format!("The Astrum on {name} is from before files could be sent to it. Update Astrum there to paste a picture."));
        }
        // (Asked for by name: nobody is asked again.)
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        self.send_files(Pasted { text, files: vec![(path, size)], host, pane });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("astrum-paste-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // (As a terminal would write it: without the links on the way.)
        std::fs::canonicalize(&dir).unwrap()
    }

    #[test]
    fn a_paste_is_taken_apart_as_a_shell_would() {
        let words = |text: &str| words(text).unwrap();
        assert_eq!(words("/a/b /c/d"), ["/a/b", "/c/d"]);
        assert_eq!(words("/a/My\\ File.png "), ["/a/My File.png"]);
        assert_eq!(words("'/a/My File.png' '/b/it'\\''s.txt'"), ["/a/My File.png", "/b/it's.txt"]);
        assert_eq!(words("\"/a/My File.png\"\n\"/b/a \\\"b\\\".txt\""), ["/a/My File.png", "/b/a \"b\".txt"]);
        assert_eq!(words("/a/b\\(1\\).png"), ["/a/b(1).png"]);
        assert_eq!(words("''"), [""]);
        assert!(words("   ").is_empty());
        // Quotes left open are not something a terminal writes.
        assert_eq!(super::words("'/a/b"), None);
        assert_eq!(super::words("\"/a/b"), None);
        assert_eq!(super::words("/a/b\\"), None);
    }

    #[test]
    fn the_path_a_word_names() {
        assert_eq!(path_of("/a/b.png"), Some("/a/b.png".into()));
        assert_eq!(path_of("file:///a/My%20File.png"), Some("/a/My File.png".into()));
        assert_eq!(path_of("file://localhost/a/b"), Some("/a/b".into()));
        assert_eq!(path_of("~/b.png"), Some(crate::paths::home().join("b.png")));
        for none in ["b.png", "./b.png", "../b", "file://elsewhere/a/b", "file:///a/%zz", "https://example.com/a.png", ""] {
            assert_eq!(path_of(none), None, "{none}");
        }
    }

    #[test]
    fn a_paste_that_names_files_and_one_that_does_not() {
        let dir = scratch("names");
        let (plain, spaced) = (dir.join("shot.png"), dir.join("My File (1).png"));
        std::fs::write(&plain, b"a").unwrap();
        std::fs::write(&spaced, b"b").unwrap();
        let (p, s) = (plain.to_string_lossy().into_owned(), spaced.to_string_lossy().into_owned());
        // One file, each way a terminal writes it, and as it is written.
        assert_eq!(files_in(&p), Some(vec![plain.clone()]));
        assert_eq!(files_in(&format!("{p} ")), Some(vec![plain.clone()]));
        assert_eq!(files_in(&s), Some(vec![spaced.clone()]));
        assert_eq!(files_in(&s.replace(' ', "\\ ").replace('(', "\\(").replace(')', "\\)")), Some(vec![spaced.clone()]));
        assert_eq!(files_in(&format!("'{s}'")), Some(vec![spaced.clone()]));
        assert_eq!(files_in(&format!("file://{}", s.replace(' ', "%20"))), Some(vec![spaced.clone()]));
        // Several, with blanks or lines between.
        assert_eq!(files_in(&format!("{p} '{s}'")), Some(vec![plain.clone(), spaced.clone()]));
        assert_eq!(files_in(&format!("{p}\n'{s}'\n")), Some(vec![plain.clone(), spaced.clone()]));
        // Not files, or not only files: pasted as it is.
        let dir_text = dir.to_string_lossy().into_owned();
        for not in [dir_text.as_str(), &format!("{p}.gone"), &format!("cat {p}"), &format!("{p} and more"), "shot.png", "hello", "", "/", &format!("{p} {dir_text}")] {
            assert_eq!(files_in(not), None, "{not}");
        }
        assert_eq!(files_in(&vec![p.as_str(); MOST_FILES + 1].join(" ")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_a_picture_in_a_passing_place_is_sent_unasked() {
        assert!(passing_picture(Path::new("/var/folders/lc/abc/T/clipboard-2026-11-17-210341-CF485711.png")));
        assert!(passing_picture(Path::new("/private/var/folders/lc/abc/T/TemporaryItems/NSIRD_screencaptureui_x/Screenshot.PNG")));
        assert!(passing_picture(Path::new("/tmp/clip-1.jpeg")));
        assert!(passing_picture(&std::env::temp_dir().join("shot.webp")));
        // A picture that is kept somewhere, and what is no picture.
        assert!(!passing_picture(&crate::paths::home().join("Desktop/Screenshot.png")));
        assert!(!passing_picture(Path::new("/tmp/id_rsa")));
        assert!(!passing_picture(Path::new("/tmp/notes.txt")));
        assert!(!passing_picture(Path::new("/etc/hosts")));
    }

    #[test]
    fn sizes_and_names_as_they_are_said() {
        assert_eq!(size_said(0), "0 bytes");
        assert_eq!(size_said(1023), "1023 bytes");
        assert_eq!(size_said(1024), "1 KB");
        assert_eq!(size_said(217_000), "212 KB");
        assert_eq!(size_said(5 * 1_048_576 + 524_288), "5.5 MB");
        assert_eq!(size_said(2 * 1_073_741_824), "2.0 GB");
        assert_eq!(short("shot.png", 20), "shot.png");
        assert_eq!(short("Screenshot 2026-11-17 at 9.03.41 PM.png", 20), "Screensho….41 PM.png");
        assert_eq!(short("Screenshot 2026-11-17 at 9.03.41 PM.png", 20).chars().count(), 20);
        assert_eq!(for_a_shell("/home/ada/.cache/astrum/received/0f1e2d3c4b/My_File.png"), "/home/ada/.cache/astrum/received/0f1e2d3c4b/My_File.png");
        assert_eq!(for_a_shell("/home/ada b/x$y.png"), "/home/ada\\ b/x\\$y.png");
    }
}
