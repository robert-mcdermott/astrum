//! This machine's side: reaching the program that keeps the shells, over
//! the user's own `ssh`.
//!
//! `ssh` is run as the user would run it, with whatever they typed for the
//! machine, so their keys, their jump hosts and their config all apply.
//! It is given a terminal of its own to ask its questions on (a password,
//! a new host's fingerprint, a second factor), which the pane shows and
//! takes the answers for; what passes between the two Astrums goes
//! through pipes beside it, where nothing is in the way of a byte.
//!
//! One line is run on the other machine. It says whether Astrum is there
//! and what kind of system it is, waits to be told what to do (go on, or
//! take a copy of Astrum first), and then becomes `astrum __link`.

use super::{Down, Frame, Up, WORD, Way, pack, read_frame};
use crate::server::Ev;
use anyhow::{Context, Result};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc as std_mpsc;
use tokio::sync::mpsc;

/// What is run on the other machine. One line, with nothing in it that a
/// login shell other than `sh` would read differently inside single
/// quotes (no quote of that kind, no backslash, no `!`).
///
/// It answers `ASTRUM-LINK <n|none> <system> <machine> [version]`: the
/// number of the language the Astrum there speaks (if there is one, and it
/// knows of this at all), then what `uname` says. Then it reads one line:
/// `go` has it become that Astrum's link; `put <bytes>` has it take that
/// many bytes as the new `~/.local/bin/astrum` (tried before it replaces
/// anything) and become that one's.
pub const SCRIPT: &str = concat!(
    r#"a="$HOME/.local/bin/astrum"; [ -x "$a" ] || a=$(command -v astrum 2>/dev/null); "#,
    r#"p=; v=; if [ -n "$a" ]; then p=$("$a" __link --speaks 2>/dev/null); v=$("$a" --version 2>/dev/null); fi; "#,
    r#"echo "ASTRUM-LINK ${p:-none} $(uname -s) $(uname -m) $v"; "#,
    r#"read -r w n || exit 0; "#,
    r#"case "$w" in "#,
    r#"go) exec "$a" __link;; "#,
    r#"put) d="$HOME/.local/bin"; t="$d/.astrum-new-$$"; "#,
    r#"mkdir -p "$d" && head -c "$n" >"$t" && chmod 755 "$t" && [ -n "$("$t" __link --speaks 2>/dev/null)" ] && mv -f "$t" "$d/astrum" && exec "$d/astrum" __link; "#,
    r#"rm -f "$t"; echo "ASTRUM-LINK failed";; "#,
    r#"esac"#,
);

/// What an attempt to reach a machine tells the server, as it happens.
pub enum Told {
    /// What `ssh` wrote on its terminal: a question, a warning, why it failed.
    Tty(Vec<u8>),
    /// The other machine has answered.
    Found { link: Option<u32>, system: String, machine: String, version: Option<String> },
    /// The build to send has been found, or why it could not be.
    Build(Result<super::copy::Build, String>),
    /// The copy sent could not be put in place, or would not run there.
    Failed,
    /// The keeper is on the line: frames follow.
    Ready,
    Frame(Frame<Down>),
    /// `ssh` has ended.
    Closed,
}

/// An `ssh` to one machine, from when it is started until it ends.
pub struct Reach {
    pub id: u64,
    child: Option<Child>,
    /// Its terminal: this end is where what it writes is read and what is
    /// typed to it goes.
    tty: std::fs::File,
    /// What is to go to the other side.
    over: std_mpsc::Sender<Vec<u8>>,
}

fn winsize(cols: u16, rows: u16) -> libc::winsize {
    libc::winsize { ws_row: rows.max(1), ws_col: cols.max(2), ws_xpixel: 0, ws_ypixel: 0 }
}

impl Reach {
    /// Start `ssh` to `host` (what the user typed: a name, or anything
    /// else `ssh` would take before a command). Everything that comes of
    /// it arrives as `Ev::Remote` with this `id`.
    pub fn start(id: u64, host: &str, size: (u16, u16), tx: mpsc::Sender<Ev>) -> Result<Reach> {
        let (mut master, mut slave) = (0, 0);
        let size = winsize(size.0, size.1);
        if unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), &size as *const _ as *mut _) } != 0 {
            return Err(std::io::Error::last_os_error()).context("opening a terminal for ssh");
        }
        for fd in [master, slave] {
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        }
        let (tty, slave) = unsafe { (std::fs::File::from_raw_fd(master), std::fs::File::from_raw_fd(slave)) };

        let program = std::env::var("ASTRUM_SSH").ok().filter(|p| !p.is_empty()).unwrap_or_else(|| "ssh".into());
        let mut cmd = Command::new(&program);
        // No terminal on the other side: what passes is not for one.
        cmd.arg("-T");
        cmd.args(host.split_whitespace());
        cmd.arg(format!("sh -c '{SCRIPT}'"));
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped());
        let slave_fd = slave.as_raw_fd();
        unsafe {
            cmd.pre_exec(move || {
                // A session of its own, with that terminal as the one it
                // asks its questions on (`/dev/tty`) and writes its
                // complaints to.
                if libc::setsid() == -1 || libc::ioctl(slave_fd, libc::TIOCSCTTY as _, 0) == -1 || libc::dup2(slave_fd, 2) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = cmd.spawn().with_context(|| format!("starting {program}"))?;
        drop(slave);
        let (stdin, stdout) = (child.stdin.take().context("no way in to ssh")?, child.stdout.take().context("no way out of ssh")?);

        let mut from_tty = tty.try_clone()?;
        let tty_tx = tx.clone();
        std::thread::Builder::new().name(format!("reach-tty-{id}")).spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n @ 1..) = from_tty.read(&mut buf) {
                if tty_tx.blocking_send(Ev::Remote { reach: id, told: Told::Tty(buf[..n].to_vec()) }).is_err() {
                    break;
                }
            }
        })?;

        std::thread::Builder::new().name(format!("reach-read-{id}")).spawn(move || {
            let tell = |told| tx.blocking_send(Ev::Remote { reach: id, told }).is_ok();
            listen(BufReader::with_capacity(256 * 1024, stdout), &tell);
            tell(Told::Closed);
        })?;

        let (over, rx) = std_mpsc::channel::<Vec<u8>>();
        std::thread::Builder::new().name(format!("reach-write-{id}")).spawn(move || {
            let mut stdin = stdin;
            while let Ok(bytes) = rx.recv() {
                if stdin.write_all(&bytes).is_err() || stdin.flush().is_err() {
                    break;
                }
            }
        })?;

        Ok(Reach { id, child: Some(child), tty, over })
    }

    /// Something typed in answer to `ssh`.
    pub fn typed(&mut self, bytes: &[u8]) {
        let _ = self.tty.write_all(bytes);
    }

    /// Tell the line run on the other machine what to do next.
    pub fn go(&self) {
        let _ = self.over.send(b"go\n".to_vec());
    }

    /// Have it take `build` as the Astrum there.
    pub fn put(&self, build: Vec<u8>) {
        let _ = self.over.send(format!("put {}\n", build.len()).into_bytes());
        let _ = self.over.send(build);
    }

    /// The way to the keeper, once it is on the line.
    pub fn way(&self) -> Way {
        let (way, mut rx) = Way::new();
        let over = self.over.clone();
        tokio::spawn(async move {
            while let Some(frame) = rx.recv().await {
                if over.send(pack::<Up>(&frame)).is_err() {
                    break;
                }
            }
        });
        way
    }
}

impl Drop for Reach {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else { return };
        // Nothing more will be sent from here. What has been (a shell to
        // be closed, say) is still on its way: `ssh` is given a moment to
        // pass it on and end by itself, as it does once its input closes.
        self.over = std_mpsc::channel().0;
        std::thread::spawn(move || {
            let until = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while std::time::Instant::now() < until && child.try_wait().ok().flatten().is_none() {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            if child.try_wait().ok().flatten().is_none() {
                // It leads a session of its own: whatever it started (a
                // jump host's `ssh`, say) goes with it.
                unsafe { libc::kill(-(child.id() as i32), libc::SIGHUP) };
                let _ = child.kill();
            }
            let _ = child.wait();
        });
    }
}

/// One line of the other side's, if it is its word to us.
fn word(line: &[u8]) -> Option<Told> {
    let text = String::from_utf8_lossy(line);
    let rest = text.trim().strip_prefix(WORD)?;
    // (The word and no more than the word: not something that begins with it.)
    if !rest.starts_with(' ') {
        return None;
    }
    let words: Vec<&str> = rest.split_whitespace().collect();
    match words.as_slice() {
        ["ready"] => Some(Told::Ready),
        ["failed", ..] => Some(Told::Failed),
        [link, system, machine, version @ ..] => {
            // (`astrum 0.5.0`, as `--version` prints it.)
            let version = version.last().map(|v| v.to_string());
            Some(Told::Found { link: link.parse().ok(), system: system.to_string(), machine: machine.to_string(), version })
        }
        _ => None,
    }
}

/// Follow what the other side says until it stops. A login may print
/// anything before our line runs: whatever is not our word is passed over.
fn listen(mut from: impl BufRead, tell: &dyn Fn(Told) -> bool) {
    let mut line = Vec::new();
    loop {
        line.clear();
        // (A line is never long. Something that is, is not a line.)
        match from.by_ref().take(64 * 1024).read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        match word(&line) {
            Some(Told::Ready) => {
                if !tell(Told::Ready) {
                    return;
                }
                break;
            }
            Some(told) => {
                if !tell(told) {
                    return;
                }
            }
            None => {}
        }
    }
    loop {
        match read_frame::<Down>(&mut from) {
            Ok(Some(frame)) => {
                if !tell(Told::Frame(frame)) {
                    return;
                }
            }
            Ok(None) => {}
            Err(_) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn the_line_reads_the_same_to_any_login_shell() {
        for bad in ['\'', '\\', '!', '\n', '`'] {
            assert!(!SCRIPT.contains(bad), "{bad:?} is in the line");
        }
        // (And it is one argument to `ssh`, which the far side splits again.)
        assert!(SCRIPT.len() < 1000, "{}", SCRIPT.len());
    }

    fn heard(bytes: &[u8]) -> Vec<String> {
        let all = RefCell::new(Vec::new());
        listen(bytes, &|told| {
            all.borrow_mut().push(match told {
                Told::Found { link, system, machine, version } => format!("found {link:?} {system} {machine} {version:?}"),
                Told::Ready => "ready".into(),
                Told::Failed => "failed".into(),
                Told::Frame(Frame::Msg(msg)) => format!("{msg:?}"),
                Told::Frame(Frame::Bytes(key, bytes)) => format!("bytes {key} {}", String::from_utf8_lossy(&bytes)),
                _ => "?".into(),
            });
            true
        });
        all.into_inner()
    }

    #[test]
    fn what_a_login_prints_first_is_passed_over() {
        let mut wire = b"Last login: yesterday\r\nWelcome to the machine\nASTRUM-LINK 1 Linux x86_64 astrum 0.5.0\n".to_vec();
        wire.extend_from_slice(b"ASTRUM-LINK ready\n");
        wire.extend_from_slice(&pack(&Frame::Msg(Down::Pong)));
        wire.extend_from_slice(&pack(&Frame::<Down>::Bytes(5, b"hello".to_vec())));
        assert_eq!(heard(&wire), ["found Some(1) Linux x86_64 Some(\"0.5.0\")", "ready", "Pong", "bytes 5 hello"]);
    }

    #[test]
    fn a_machine_without_astrum_and_a_copy_that_failed() {
        assert_eq!(heard(b"ASTRUM-LINK none Linux aarch64 \nASTRUM-LINK failed\n"), ["found None Linux aarch64 None", "failed"]);
        // An older Astrum there, which knows nothing of this.
        assert_eq!(heard(b"ASTRUM-LINK none Darwin arm64 astrum 0.4.0\n"), ["found None Darwin arm64 Some(\"0.4.0\")"]);
    }

    #[test]
    fn nothing_that_is_not_our_word_is_taken_for_it() {
        assert!(heard(b"echo ASTRUM-LINK ready\nNOT-ASTRUM-LINK ready\nASTRUM-LINK\nASTRUM-LINK one\nASTRUM-LINKS are not ours\nASTRUM-LINKready\n").is_empty());
    }
}
