//! Shells that live on another machine.
//!
//! A remote tab's shells are not run by `ssh` from here: Astrum's own
//! program on the other machine keeps them (`hold`), so they and whatever
//! runs in them outlast the connection. This machine reaches that program
//! over the user's own `ssh` (`reach`), draws the shells as ordinary panes,
//! and finds them again after the connection has dropped. The agent stays
//! here, with the model and the keys; its commands are typed into the
//! shell there, and its file work is carried out there by that program.
//!
//! What goes over the connection is in this file: frames, each a message
//! (JSON) or a run of a shell's bytes.

pub mod copy;
pub mod hold;
pub mod reach;
mod screen;

use crate::agent::ToolCall;
use crate::procinfo::Job;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{self, Read};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};

/// The number of the language the two sides speak. Two versions of Astrum
/// with the same number work together, whatever else differs between
/// them: a message may gain a field (one that is not known is ignored, one
/// that is missing has a default), but a change the other side could not
/// make sense of takes a new number.
pub const LINK: u32 = 1;

/// What a line begins with when it is one side's word to the other, before
/// the frames begin. (A login may print anything first.)
pub const WORD: &str = "ASTRUM-LINK";

/// A message.
const MSG: u8 = 0;
/// What a shell wrote (downwards) or is to be sent (upwards): its key,
/// then the bytes.
const BYTES: u8 = 1;
/// A shell's screen as it now stands: its key, then what draws it.
const SCREEN: u8 = 2;

const MAX_FRAME: usize = 64 * 1024 * 1024;

/// From this machine to the program keeping the shells.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "snake_case")]
pub enum Up {
    /// Who is calling: `owner` names this machine's session, and only its
    /// own shells are shown to it.
    Hello { link: u32, version: String, owner: String },
    /// Start a shell, to go by `key`.
    Open {
        key: u64,
        cols: u16,
        rows: u16,
        px: (u16, u16),
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        session: String,
        #[serde(default)]
        scrollback: usize,
    },
    /// Find a shell again. `had`: how much of what it has written this
    /// side already has, if it has kept anything.
    Attach {
        key: u64,
        #[serde(default)]
        had: Option<u64>,
        cols: u16,
        rows: u16,
        px: (u16, u16),
    },
    Resize { key: u64, cols: u16, rows: u16, px: (u16, u16) },
    /// End a shell.
    Close { key: u64 },
    /// Something to be done there and answered (`Down::Answer`).
    Ask { n: u64, ask: Ask },
    Ping,
}

/// Work the agent has carried out on the other machine.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "snake_case")]
pub enum Ask {
    /// A tool that reads files, or one that changes them with no review.
    Tool { call: ToolCall, cwd: String },
    /// What a change to a file would be, without making it.
    Plan { call: ToolCall, cwd: String },
    /// Make a planned change, unless the file is no longer as it was.
    Commit { path: String, old: Option<String>, content: String },
    /// A command, beside the shell rather than in it.
    Run { command: String, cwd: String, timeout: u64, note: String },
    /// The project's own instructions for the agent, found from `cwd`.
    Rules { cwd: String },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "snake_case")]
pub enum Answer {
    /// What a tool or a command came back with, and whether it failed.
    Text { text: String, failed: bool },
    Plan { path: String, old: Option<String>, new: String, done: String, existed: bool },
    Rules {
        #[serde(default)]
        text: String,
        #[serde(default)]
        sources: Vec<String>,
    },
}

/// From the program keeping the shells to this machine.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "snake_case")]
pub enum Down {
    Hello {
        link: u32,
        version: String,
        there: Machine,
        /// The caller's shells that are running.
        shells: Vec<u64>,
    },
    /// A shell is up: just started, or found again. What it writes follows
    /// from `at`. `screen`: its screen as it stands comes first, because
    /// what was had cannot simply be continued. `replay`: this much of
    /// what follows was written while nobody was looking.
    Shell {
        key: u64,
        nonce: String,
        shell: String,
        #[serde(default)]
        cwd: Option<String>,
        at: u64,
        #[serde(default)]
        screen: bool,
        #[serde(default)]
        replay: u64,
        /// With a screen: whether the shell is at its prompt, if that is known.
        #[serde(default)]
        at_prompt: Option<bool>,
    },
    /// There is no such shell (it has ended, or the machine was restarted),
    /// or one could not be started, and why.
    Gone {
        key: u64,
        #[serde(default)]
        why: Option<String>,
    },
    Exited { key: u64 },
    /// What a shell is doing, said when it changes.
    Doing {
        key: u64,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        job: Option<Job>,
    },
    Answer { n: u64, answer: Answer },
    Pong,
}

/// The machine the shells are on, as it describes itself.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct Machine {
    pub user: String,
    pub host: String,
    /// `uname -sr`, more or less.
    pub os: String,
    pub home: String,
    /// The shell new panes get.
    pub shell: String,
    /// Whether the shells outlast the last logout: `Some(false)` where the
    /// system ends a user's programs then and has not been told otherwise.
    #[serde(default)]
    pub outlasts_logout: Option<bool>,
}

/// One frame, either way.
#[derive(Debug)]
pub enum Frame<M> {
    Msg(M),
    Bytes(u64, Vec<u8>),
    Screen(u64, Vec<u8>),
}

fn keyed(key: u64, bytes: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(8 + bytes.len());
    body.extend_from_slice(&key.to_le_bytes());
    body.extend_from_slice(bytes);
    body
}

/// A frame as it goes over the connection.
pub fn pack<M: Serialize>(frame: &Frame<M>) -> Vec<u8> {
    let (tag, body) = match frame {
        Frame::Msg(msg) => (MSG, serde_json::to_vec(msg).unwrap_or_default()),
        Frame::Bytes(key, bytes) => (BYTES, keyed(*key, bytes)),
        Frame::Screen(key, bytes) => (SCREEN, keyed(*key, bytes)),
    };
    let mut out = Vec::with_capacity(5 + body.len());
    out.push(tag);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

fn unpack<M: for<'a> Deserialize<'a>>(tag: u8, body: Vec<u8>) -> io::Result<Option<Frame<M>>> {
    let keyed = |body: &[u8]| -> io::Result<(u64, Vec<u8>)> {
        let key = body.get(..8).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "a frame too short"))?;
        Ok((u64::from_le_bytes(key.try_into().unwrap_or_default()), body[8..].to_vec()))
    };
    Ok(match tag {
        // A message this version does not know is one a newer version
        // added: it is passed over.
        MSG => serde_json::from_slice(&body).ok().map(Frame::Msg),
        BYTES => Some(keyed(&body).map(|(key, bytes)| Frame::Bytes(key, bytes))?),
        SCREEN => Some(keyed(&body).map(|(key, bytes)| Frame::Screen(key, bytes))?),
        _ => None,
    })
}

/// The next frame, waited for. (`None` in place of one this version does
/// not understand.)
pub fn read_frame<M: for<'a> Deserialize<'a>>(r: &mut impl Read) -> io::Result<Option<Frame<M>>> {
    let mut head = [0u8; 5];
    r.read_exact(&mut head)?;
    let len = u32::from_le_bytes([head[1], head[2], head[3], head[4]]) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "a frame too large"));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    unpack(head[0], body)
}

pub async fn read_frame_async<M: for<'a> Deserialize<'a>>(r: &mut (impl tokio::io::AsyncRead + Unpin)) -> io::Result<Option<Frame<M>>> {
    use tokio::io::AsyncReadExt;
    let mut head = [0u8; 5];
    r.read_exact(&mut head).await?;
    let len = u32::from_le_bytes([head[1], head[2], head[3], head[4]]) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "a frame too large"));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;
    unpack(head[0], body)
}

#[cfg(test)]
pub fn write_frame<M: Serialize>(w: &mut impl std::io::Write, frame: &Frame<M>) -> io::Result<()> {
    w.write_all(&pack(frame))?;
    w.flush()
}

/// The way from here to the program keeping a machine's shells, for as
/// long as the connection it was made for stands. Cheap to copy: panes
/// and the agent's work each hold one.
#[derive(Clone)]
pub struct Way {
    tx: mpsc::UnboundedSender<Frame<Up>>,
    asked: Arc<Asked>,
}

#[derive(Default)]
struct Asked {
    /// The connection this was the way over is gone.
    gone: std::sync::atomic::AtomicBool,
    next: AtomicU64,
    waiting: Mutex<HashMap<u64, oneshot::Sender<Answer>>>,
}

impl Way {
    /// A way, and the frames sent down it as they come.
    pub fn new() -> (Way, mpsc::UnboundedReceiver<Frame<Up>>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Way { tx, asked: Arc::default() }, rx)
    }

    pub fn send(&self, msg: Up) {
        let _ = self.tx.send(Frame::Msg(msg));
    }

    pub fn input(&self, key: u64, bytes: Vec<u8>) {
        let _ = self.tx.send(Frame::Bytes(key, bytes));
    }

    /// Have something done there; its answer, or why there is none.
    pub async fn ask(&self, ask: Ask) -> Result<Answer, String> {
        const LOST: &str = "The connection to the other machine was lost before this was answered. Nothing says whether it was done: look before doing it again.";
        let n = self.asked.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        // (Under the lock `lost` takes: nobody is left waiting who began
        // to wait as the connection went.)
        match self.asked.waiting.lock() {
            Ok(mut waiting) if !self.asked.gone.load(Ordering::Relaxed) => {
                waiting.insert(n, tx);
            }
            _ => return Err(LOST.into()),
        }
        if self.tx.send(Frame::Msg(Up::Ask { n, ask })).is_err() {
            self.lost();
            return Err(LOST.into());
        }
        rx.await.map_err(|_| LOST.to_string())
    }

    /// An answer has come.
    pub fn answered(&self, n: u64, answer: Answer) {
        let waiting = self.asked.waiting.lock().ok().and_then(|mut w| w.remove(&n));
        if let Some(tx) = waiting {
            let _ = tx.send(answer);
        }
    }

    /// The connection is gone: nobody waiting will be answered.
    pub fn lost(&self) {
        if let Ok(mut waiting) = self.asked.waiting.lock() {
            self.asked.gone.store(true, Ordering::Relaxed);
            waiting.clear();
        }
    }

}

/// The name a machine's kind of system has in a release (`astrum-linux-arm64`),
/// from what `uname -s` and `uname -m` say there. `None`: no build for it.
pub fn asset(system: &str, machine: &str) -> Option<String> {
    let os = match system {
        "Linux" => "linux",
        "Darwin" => "darwin",
        _ => return None,
    };
    let arch = match machine {
        "x86_64" | "amd64" => "amd64",
        "aarch64" | "arm64" => "arm64",
        _ => return None,
    };
    // (The only build for a Mac is for Apple's own processors.)
    if (os, arch) == ("darwin", "amd64") {
        return None;
    }
    Some(format!("astrum-{os}-{arch}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_come_back_as_they_went() {
        let mut wire = Vec::new();
        write_frame(&mut wire, &Frame::Msg(Up::Close { key: 7 })).unwrap();
        write_frame(&mut wire, &Frame::<Up>::Bytes(u64::MAX, b"ls\r".to_vec())).unwrap();
        write_frame(&mut wire, &Frame::<Up>::Screen(3, Vec::new())).unwrap();
        let mut r = &wire[..];
        assert!(matches!(read_frame::<Up>(&mut r).unwrap(), Some(Frame::Msg(Up::Close { key: 7 }))));
        assert!(matches!(read_frame::<Up>(&mut r).unwrap(), Some(Frame::Bytes(u64::MAX, b)) if b == b"ls\r"));
        assert!(matches!(read_frame::<Up>(&mut r).unwrap(), Some(Frame::Screen(3, b)) if b.is_empty()));
        assert!(read_frame::<Up>(&mut r).is_err());
    }

    #[test]
    fn what_a_newer_version_added_is_passed_over() {
        // A message of a kind not known, and a known one with a field more.
        let mut wire = Vec::new();
        for body in [r#"{"later":{"x":1}}"#, r#"{"close":{"key":4,"gently":true}}"#] {
            wire.push(MSG);
            wire.extend_from_slice(&(body.len() as u32).to_le_bytes());
            wire.extend_from_slice(body.as_bytes());
        }
        // And a kind of frame not known.
        wire.extend_from_slice(&[9, 1, 0, 0, 0, 0]);
        let mut r = &wire[..];
        assert!(read_frame::<Up>(&mut r).unwrap().is_none());
        assert!(matches!(read_frame::<Up>(&mut r).unwrap(), Some(Frame::Msg(Up::Close { key: 4 }))));
        assert!(read_frame::<Up>(&mut r).unwrap().is_none());
    }

    #[test]
    fn a_frame_cut_short_is_an_error_not_a_panic() {
        let mut r = &[BYTES, 3, 0, 0, 0, 1, 2, 3][..];
        assert!(read_frame::<Up>(&mut r).is_err());
    }

    #[test]
    fn the_builds_there_are() {
        assert_eq!(asset("Linux", "x86_64").as_deref(), Some("astrum-linux-amd64"));
        assert_eq!(asset("Linux", "aarch64").as_deref(), Some("astrum-linux-arm64"));
        assert_eq!(asset("Darwin", "arm64").as_deref(), Some("astrum-darwin-arm64"));
        assert_eq!(asset("Darwin", "x86_64"), None);
        assert_eq!(asset("FreeBSD", "amd64"), None);
        assert_eq!(asset("Linux", "riscv64"), None);
    }
}
