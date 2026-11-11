//! Conversations on disk, so they outlive the server that held them.
//!
//! Each conversation is one file in the data directory: a small header line
//! (enough to list it) followed by a line with the messages and the
//! transcript. Listing reads only the headers.

use super::session::{Item, Totals};
use crate::agent::Msg;
use crate::paths;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Meta {
    pub version: u32,
    pub id: String,
    pub title: String,
    /// Seconds since the Unix epoch.
    pub created: u64,
    pub updated: u64,
    /// Where the shell was when the conversation was last active.
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub session: String,
    #[serde(default)]
    pub model: String,
    /// Number of user prompts, for the list.
    #[serde(default)]
    pub prompts: usize,
    #[serde(default)]
    pub totals: Totals,
}

#[derive(Deserialize)]
pub struct Body {
    pub history: Vec<Msg>,
    pub items: Vec<Item>,
}

#[derive(Serialize)]
struct BodyRef<'a> {
    history: &'a [Msg],
    items: &'a [Item],
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn dir() -> PathBuf {
    paths::data_dir().join("conversations")
}

fn path_of(id: &str) -> PathBuf {
    dir().join(format!("{id}.jsonl"))
}

/// Ids are also file names; anything else is refused.
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// A new id that sorts by creation time and reads as a date.
pub fn new_id() -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let stamp = unsafe {
        let secs = now.as_secs() as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&secs, &mut tm);
        format!("{:04}{:02}{:02}-{:02}{:02}{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min, tm.tm_sec)
    };
    // Two conversations can start in the same second (two tabs, two sessions).
    format!("{stamp}-{:05x}", (now.subsec_nanos() ^ std::process::id().rotate_left(12)) & 0xfffff)
}

/// Write the conversation, replacing any earlier version in one step so a
/// crash mid-write never leaves half a file.
pub fn save(meta: &Meta, history: &[Msg], items: &[Item]) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    if !valid_id(&meta.id) {
        bail!("invalid conversation id");
    }
    let dir = dir();
    if !dir.exists() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let mut data = serde_json::to_vec(meta)?;
    data.push(b'\n');
    serde_json::to_writer(&mut data, &BodyRef { history, items })?;
    data.push(b'\n');
    let path = path_of(&meta.id);
    let tmp = dir.join(format!("{}.tmp", meta.id));
    // Conversations can contain command output; keep them private.
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    file.write_all(&data)?;
    drop(file);
    std::fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn read_meta(path: &std::path::Path) -> Option<Meta> {
    let mut line = String::new();
    BufReader::new(std::fs::File::open(path).ok()?).read_line(&mut line).ok()?;
    serde_json::from_str(&line).ok()
}

/// Every saved conversation, most recently active first.
pub fn list() -> Vec<Meta> {
    let Ok(entries) = std::fs::read_dir(dir()) else { return Vec::new() };
    let mut all: Vec<Meta> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|e| read_meta(&e.path()))
        .collect();
    all.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| b.id.cmp(&a.id)));
    all
}

pub fn load(id: &str) -> Result<(Meta, Body)> {
    if !valid_id(id) {
        bail!("invalid conversation id");
    }
    let path = path_of(id);
    let file = std::fs::File::open(&path).with_context(|| format!("opening {}", path.display()))?;
    let mut lines = BufReader::new(file).lines();
    let header = lines.next().context("the file is empty")??;
    let meta: Meta = serde_json::from_str(&header).context("unreadable header")?;
    if meta.version > VERSION {
        bail!("it was saved by a newer version of Astrum");
    }
    let body = lines.next().context("the file has no messages")??;
    let body: Body = serde_json::from_str(&body).context("unreadable messages")?;
    Ok((meta, body))
}

pub fn delete(id: &str) -> Result<()> {
    if !valid_id(id) {
        bail!("invalid conversation id");
    }
    std::fs::remove_file(path_of(id))?;
    Ok(())
}

pub fn new_meta(id: &str, created: u64) -> Meta {
    Meta {
        version: VERSION,
        id: id.to_string(),
        title: String::new(),
        created,
        updated: now(),
        cwd: None,
        session: String::new(),
        model: String::new(),
        prompts: 0,
        totals: Totals::default(),
    }
}

/// "just now", "5m ago", "3h ago", "2d ago".
pub fn ago(then: u64, now: u64) -> String {
    let secs = now.saturating_sub(then);
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86_399 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::ToolCall;
    use crate::server::session::{ToolItem, ToolState};

    #[test]
    fn ids_are_safe_file_names() {
        let id = new_id();
        assert!(valid_id(&id), "{id}");
        assert!(!valid_id("../escape") && !valid_id("") && !valid_id("a/b"));
    }

    #[test]
    fn ages_read_naturally() {
        assert_eq!(ago(100, 130), "just now");
        assert_eq!(ago(0, 60 * 5), "5m ago");
        assert_eq!(ago(0, 3600 * 3 + 5), "3h ago");
        assert_eq!(ago(0, 86_400 * 2), "2d ago");
        assert_eq!(ago(50, 10), "just now");
    }

    #[test]
    fn a_conversation_survives_the_round_trip() {
        let history = vec![
            Msg::User("hi".into()),
            Msg::Assistant {
                text: "running".into(),
                calls: vec![ToolCall { id: "c1".into(), name: "run_command".into(), args: serde_json::json!({"command": "ls"}) }],
                native: Some((crate::config::ProviderKind::Anthropic, serde_json::json!([{"type": "thinking", "thinking": "hm", "signature": "s"}]))),
            },
            Msg::ToolResult { id: "c1".into(), name: "run_command".into(), content: "exit code: 0".into(), is_error: false },
        ];
        let items = vec![
            Item::User("hi".into()),
            Item::Tool(ToolItem { id: "c1".into(), name: "run_command".into(), target: "ls".into(), state: ToolState::Done, exit: Some(0), summary: String::new() }),
            Item::Assistant("done".into()),
        ];
        let text = serde_json::to_string(&BodyRef { history: &history, items: &items }).unwrap();
        let back: Body = serde_json::from_str(&text).unwrap();
        assert_eq!(back.history.len(), 3);
        assert!(matches!(&back.history[1], Msg::Assistant { calls, native: Some((crate::config::ProviderKind::Anthropic, _)), .. } if calls[0].id == "c1"));
        assert!(matches!(&back.items[1], Item::Tool(t) if t.state == ToolState::Done && t.exit == Some(0)));
        // One document per line: the header can be read without the rest.
        assert!(!text.contains('\n'));
    }
}
