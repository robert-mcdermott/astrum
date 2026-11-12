//! Tool definitions offered to the model, and the tools that run inside the
//! agent task (file access and the subprocess fallback for commands).

use super::ToolCall;
use super::search;
use crate::osc;
use crate::paths;
use serde_json::{Value, json};
use std::borrow::Cow;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;

pub struct ToolSpec {
    pub name: Cow<'static, str>,
    pub description: Cow<'static, str>,
    pub schema: Value,
}

/// The tools that act on this machine's files, whatever the shell in the
/// pane has led to.
pub fn is_local(name: &str) -> bool {
    matches!(name, "read_file" | "find_files" | "search_files" | "write_file" | "edit_file")
}

pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: Cow::Borrowed("run_command"),
            description: Cow::Borrowed("Run a shell command in the user's terminal session. It runs in their real interactive shell, so the working directory, environment and shell state persist between calls and the user watches it happen. Returns the exit code and output. If the command is still running when the timeout passes you get the output so far; use command_status to keep waiting or send_input to interact with it."),
            schema: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "The exact command line to run."},
                    "explanation": {"type": "string", "description": "One short sentence telling the user why you are running it."},
                    "timeout_secs": {"type": "integer", "description": "How long to wait for it to finish (default 60, max 600)."}
                },
                "required": ["command"]
            }),
        },
        ToolSpec {
            name: Cow::Borrowed("command_status"),
            description: Cow::Borrowed("Check on a command started with run_command that had not finished. Waits up to wait_secs for it to complete and returns its latest output and whether it is done."),
            schema: json!({
                "type": "object",
                "properties": {
                    "wait_secs": {"type": "integer", "description": "Seconds to wait for completion (default 15, max 300)."}
                }
            }),
        },
        ToolSpec {
            name: Cow::Borrowed("send_input"),
            description: Cow::Borrowed("Type into the program currently running in the terminal, e.g. to answer a prompt, quit a pager, or interrupt a stuck command. Never use it for passwords or secrets; ask the user to type those themselves."),
            schema: json!({
                "type": "object",
                "properties": {
                    "text": {"type": "string", "description": "Literal text to type (may be empty)."},
                    "key": {"type": "string", "enum": ["enter", "ctrl-c", "ctrl-d", "ctrl-z", "esc", "tab", "up", "down", "space"], "description": "A key to press after the text."}
                }
            }),
        },
        ToolSpec {
            name: Cow::Borrowed("read_terminal"),
            description: Cow::Borrowed("Read the most recent lines visible in the user's terminal (screen plus scrollback). Use it to see what the user is looking at, e.g. an error from a command they ran themselves."),
            schema: json!({
                "type": "object",
                "properties": {
                    "lines": {"type": "integer", "description": "How many lines from the bottom (default 80, max 1000)."}
                }
            }),
        },
        ToolSpec {
            name: Cow::Borrowed("read_file"),
            description: Cow::Borrowed("Read a text file. Paths may be absolute, relative to the terminal's working directory, or start with ~. Output lines are numbered."),
            schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "start_line": {"type": "integer", "description": "First line to return (1-based, default 1)."},
                    "max_lines": {"type": "integer", "description": "Maximum number of lines (default 400)."}
                },
                "required": ["path"]
            }),
        },
        ToolSpec {
            name: Cow::Borrowed("find_files"),
            description: Cow::Borrowed("Find files by name. Returns the paths under a directory that match a glob pattern, most recently changed first. `*` and `?` stay within one name, `**` crosses directories, and a pattern without a slash matches the file name at any depth (`*.rs` finds every Rust file). Version-control, dependency and build directories (.git, node_modules, target, .venv and the like) are skipped unless the search starts inside one. Needs no approval; prefer it to running find."),
            schema: json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Glob pattern, e.g. `*.toml`, `src/**/*.rs`, `**/test_*.{py,js}`."},
                    "path": {"type": "string", "description": "Directory to search (default: the terminal's working directory)."},
                    "max_results": {"type": "integer", "description": "Most paths to return (default 200)."}
                },
                "required": ["pattern"]
            }),
        },
        ToolSpec {
            name: Cow::Borrowed("search_files"),
            description: Cow::Borrowed("Search the text of files with a regular expression. Returns matching lines as `path:line: text`. Searches one file, or every text file under a directory, skipping version-control, dependency and build directories, binary files and files over 2 MB. Needs no approval; prefer it to running grep."),
            schema: json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Regular expression to look for in each line."},
                    "path": {"type": "string", "description": "File or directory to search (default: the terminal's working directory)."},
                    "glob": {"type": "string", "description": "Only search files matching this glob, e.g. `*.py`."},
                    "ignore_case": {"type": "boolean"},
                    "context": {"type": "integer", "description": "Lines to show before and after each match (default 0, max 5)."},
                    "max_results": {"type": "integer", "description": "Most matching lines to return (default 100)."}
                },
                "required": ["pattern"]
            }),
        },
        ToolSpec {
            name: Cow::Borrowed("write_file"),
            description: Cow::Borrowed("Create a file or replace its entire contents. Parent directories are created. Prefer edit_file for changing part of an existing file."),
            schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string", "description": "The complete new file contents."}
                },
                "required": ["path", "content"]
            }),
        },
        ToolSpec {
            name: Cow::Borrowed("edit_file"),
            description: Cow::Borrowed("Replace an exact piece of text in a file. old_text must match the file exactly (including whitespace) and be unique unless replace_all is true."),
            schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "old_text": {"type": "string"},
                    "new_text": {"type": "string"},
                    "replace_all": {"type": "boolean"}
                },
                "required": ["path", "old_text", "new_text"]
            }),
        },
    ]
}

/// Names of required string arguments per tool, checked before anything runs
/// (tool input is streamed unvalidated, so it can be cut off or malformed).
/// Is the call one that can be made, with what it needs? `mcp`: the tools
/// of MCP servers on offer, with the schemas of their arguments.
pub fn validate(call: &ToolCall, mcp: &std::collections::HashMap<String, Value>) -> Result<(), String> {
    if let Some(raw) = call.args.get("__invalid_json").and_then(Value::as_str) {
        let shown: String = raw.chars().take(300).collect();
        return Err(format!("The arguments were not valid JSON, so nothing was run. Send the call again with valid JSON. Received: {shown}"));
    }
    if let Some(schema) = mcp.get(&call.name) {
        if !call.args.is_object() && !call.args.is_null() {
            return Err("The arguments must be a JSON object; nothing was sent to the server.".into());
        }
        let missing: Vec<&str> = schema.get("required").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).filter(|key| call.args.get(*key).is_none_or(Value::is_null)).collect();
        if !missing.is_empty() {
            return Err(format!("Missing required argument{} {}; nothing was sent to the server.", if missing.len() == 1 { "" } else { "s" }, missing.iter().map(|k| format!("`{k}`")).collect::<Vec<_>>().join(", ")));
        }
        return Ok(());
    }
    let required: &[&str] = match call.name.as_str() {
        "run_command" => &["command"],
        "read_file" => &["path"],
        "find_files" | "search_files" => &["pattern"],
        "write_file" => &["path", "content"],
        "edit_file" => &["path", "old_text", "new_text"],
        "command_status" | "send_input" | "read_terminal" => &[],
        other => return Err(format!("Unknown tool `{other}`.")),
    };
    for key in required {
        if call.str_arg(key).is_none() {
            return Err(format!("Missing required string argument `{key}`; nothing was run."));
        }
    }
    if call.name == "run_command" && call.str_arg("command").is_some_and(|c| c.trim().is_empty()) {
        return Err("The command is empty.".into());
    }
    Ok(())
}

const MAX_READ_BYTES: u64 = 1024 * 1024;

/// Run a file tool. Returns (result text, is_error).
pub async fn run_local(call: &ToolCall, cwd: &str) -> (String, bool) {
    let result = match call.name.as_str() {
        "read_file" => read_file(call, cwd).await,
        "find_files" | "search_files" => search(call, cwd).await,
        "write_file" | "edit_file" => change_file(call, cwd).await,
        other => Err(format!("Tool `{other}` cannot run here.")),
    };
    match result {
        Ok(text) => (text, false),
        Err(text) => (text, true),
    }
}

async fn read_file(call: &ToolCall, cwd: &str) -> Result<String, String> {
    let path = paths::resolve(call.str_arg("path").unwrap_or_default(), cwd);
    let meta = tokio::fs::metadata(&path).await.map_err(|e| format!("{}: {e}", path.display()))?;
    if meta.is_dir() {
        return Err(format!("{} is a directory; list it with run_command (ls).", path.display()));
    }
    if meta.len() > MAX_READ_BYTES * 8 {
        return Err(format!(
            "{} is {} bytes, too large to read whole. Use run_command with head, tail, grep or sed -n to view part of it.",
            path.display(),
            meta.len()
        ));
    }
    let bytes = tokio::fs::read(&path).await.map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes.iter().take(8192).any(|&b| b == 0) {
        return Err(format!("{} looks like a binary file.", path.display()));
    }
    let text = String::from_utf8_lossy(&bytes);
    let start = call.u64_arg("start_line").unwrap_or(1).max(1) as usize;
    let max = call.u64_arg("max_lines").unwrap_or(400).clamp(1, 5000) as usize;
    let total = text.lines().count();
    let mut out = String::new();
    let mut shown = 0;
    for (i, line) in text.lines().enumerate().skip(start - 1).take(max) {
        let clipped: String = line.chars().take(2000).collect();
        out.push_str(&format!("{:>5}  {}\n", i + 1, clipped));
        shown += 1;
    }
    if total == 0 {
        return Ok(format!("{} is empty.", path.display()));
    }
    if start - 1 + shown < total {
        out.push_str(&format!("[showing lines {}-{} of {}; call again with start_line to read more]\n", start, start - 1 + shown, total));
    }
    Ok(out)
}

/// The search tools read many files, so they run off the async threads.
async fn search(call: &ToolCall, cwd: &str) -> Result<String, String> {
    let target = paths::resolve(call.str_arg("path").unwrap_or("."), cwd);
    let call = call.clone();
    let work = tokio::task::spawn_blocking(move || {
        let pattern = call.str_arg("pattern").unwrap_or_default();
        if call.name == "find_files" {
            let max = call.u64_arg("max_results").unwrap_or(200).clamp(1, 1000) as usize;
            search::find_files(&target, pattern, max)
        } else {
            let query = search::Grep {
                pattern,
                glob: call.str_arg("glob").filter(|g| !g.trim().is_empty()),
                ignore_case: call.bool_arg("ignore_case"),
                context: call.u64_arg("context").unwrap_or(0).min(5) as usize,
                max: call.u64_arg("max_results").unwrap_or(100).clamp(1, 1000) as usize,
            };
            search::search_files(&target, &query)
        }
    });
    work.await.unwrap_or_else(|e| Err(format!("the search stopped unexpectedly: {e}")))
}

/// A change to a file, worked out but not yet made.
pub struct Change {
    pub path: std::path::PathBuf,
    /// There is a file there now.
    pub existed: bool,
    /// What the file holds now; `None` when it does not exist yet (or is
    /// not text, in which case it is replaced whole).
    pub old: Option<String>,
    pub new: String,
    /// What to tell the model once the whole change has been made.
    pub done: String,
}

/// Work out what a `write_file` or `edit_file` call would do, without
/// doing it.
pub async fn plan_change(call: &ToolCall, cwd: &str) -> Result<Change, String> {
    let path = paths::resolve(call.str_arg("path").unwrap_or_default(), cwd);
    if tokio::fs::metadata(&path).await.is_ok_and(|m| m.is_dir()) {
        return Err(format!("{} is a directory.", path.display()));
    }
    if call.name == "write_file" {
        let new = call.str_arg("content").unwrap_or_default().to_string();
        let existed = path.exists();
        let old = tokio::fs::read_to_string(&path).await.ok();
        let done = format!("{} {} ({} lines, {} bytes).", if existed { "Replaced" } else { "Created" }, path.display(), new.lines().count(), new.len());
        return Ok(Change { path, existed, old, new, done });
    }
    let old = call.str_arg("old_text").unwrap_or_default();
    let new = call.str_arg("new_text").unwrap_or_default();
    if old.is_empty() {
        return Err("old_text is empty; use write_file to create a file.".into());
    }
    let text = tokio::fs::read_to_string(&path).await.map_err(|e| format!("{}: {e}", path.display()))?;
    let count = text.matches(old).count();
    if count == 0 {
        return Err(format!(
            "old_text was not found in {}. Read the file again and copy the text exactly, including indentation.",
            path.display()
        ));
    }
    let replace_all = call.bool_arg("replace_all");
    if count > 1 && !replace_all {
        return Err(format!(
            "old_text matches {count} places in {}. Include more surrounding lines to make it unique, or set replace_all.",
            path.display()
        ));
    }
    let updated = if replace_all { text.replace(old, new) } else { text.replacen(old, new, 1) };
    let made = if replace_all { count } else { 1 };
    let done = format!("Edited {} ({made} replacement{}).", path.display(), if made > 1 { "s" } else { "" });
    Ok(Change { path, existed: true, old: Some(text), new: updated, done })
}

/// Write `content` as the new contents of the planned change's file,
/// unless the file has changed since the plan was made.
pub async fn commit_change(change: &Change, content: &str) -> Result<(), String> {
    let path = &change.path;
    let now = tokio::fs::read_to_string(path).await.ok();
    if change.old.is_some() && now != change.old {
        return Err(format!("{} changed on disk while this was waiting for approval. Read it again before changing it.", path.display()));
    }
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    tokio::fs::write(path, content).await.map_err(|e| format!("{}: {e}", path.display()))
}

// ---- the same, wherever the tab is ----------------------------------------------
//
// A tab on another machine has its files there. `at` is the way to that
// machine's Astrum (see `crate::remote`), which does the work with the
// functions above and sends back what they return; `None` is this machine.

use crate::remote::{Answer, Ask, Way};

/// What the other machine said, as a tool's result. (An answer of the
/// wrong kind can only come from a version that has changed its mind
/// about what is asked.)
fn said(answer: Result<Answer, String>) -> (String, bool) {
    match answer {
        Ok(Answer::Text { text, failed }) => (text, failed),
        Ok(_) => ("The other machine's Astrum answered something else than was asked. Nothing can be said about what it did.".into(), true),
        Err(lost) => (lost, true),
    }
}

pub async fn run_at(at: &Option<Way>, call: &ToolCall, cwd: &str) -> (String, bool) {
    match at {
        None => run_local(call, cwd).await,
        Some(way) => said(way.ask(Ask::Tool { call: call.clone(), cwd: cwd.to_string() }).await),
    }
}

pub async fn plan_at(at: &Option<Way>, call: &ToolCall, cwd: &str) -> Result<Change, String> {
    match at {
        None => plan_change(call, cwd).await,
        Some(way) => match way.ask(Ask::Plan { call: call.clone(), cwd: cwd.to_string() }).await {
            Ok(Answer::Plan { path, old, new, done, existed }) => Ok(Change { path: path.into(), existed, old, new, done }),
            other => Err(said(other).0),
        },
    }
}

pub async fn commit_at(at: &Option<Way>, change: &Change, content: &str) -> Result<(), String> {
    match at {
        None => commit_change(change, content).await,
        Some(way) => {
            let ask = Ask::Commit { path: change.path.to_string_lossy().into_owned(), old: change.old.clone(), content: content.to_string() };
            match said(way.ask(ask).await) {
                (_, false) => Ok(()),
                (why, true) => Err(why),
            }
        }
    }
}

pub async fn subprocess_at(at: &Option<Way>, command: &str, cwd: &str, timeout: u64, note: &str) -> (String, bool) {
    match at {
        None => run_subprocess(command, cwd, timeout, note).await,
        Some(way) => said(way.ask(Ask::Run { command: command.to_string(), cwd: cwd.to_string(), timeout, note: note.to_string() }).await),
    }
}

/// Make a file change in one go (no review).
async fn change_file(call: &ToolCall, cwd: &str) -> Result<String, String> {
    let change = plan_change(call, cwd).await?;
    commit_change(&change, &change.new).await?;
    Ok(change.done)
}

/// Run a command outside the terminal, capturing its output.
pub async fn run_subprocess(command: &str, cwd: &str, timeout: u64, note: &str) -> (String, bool) {
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into());
    let mut cmd = tokio::process::Command::new(&shell);
    cmd.arg("-c")
        .arg(command)
        .current_dir(if std::path::Path::new(cwd).is_dir() { cwd } else { "/" })
        .env("PAGER", "cat")
        .env("GIT_PAGER", "cat")
        .env("TERM", "dumb")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return (format!("could not start {shell}: {e}"), true),
    };
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let work = async {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let (_, _, status) = tokio::join!(stdout.read_to_end(&mut out), stderr.read_to_end(&mut err), child.wait());
        (out, err, status)
    };
    match tokio::time::timeout(Duration::from_secs(timeout), work).await {
        Err(_) => (format!("{note}The command did not finish within {timeout}s and was stopped."), true),
        Ok((out, err, status)) => {
            let code = status.ok().and_then(|s| s.code());
            let mut text = osc::clean_output(&out);
            let err_text = osc::clean_output(&err);
            if !err_text.trim().is_empty() {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(&err_text);
            }
            (format_command_result(note, code, None, &text), code != Some(0))
        }
    }
}

/// The text a finished command is reported to the model as.
pub fn format_command_result(note: &str, exit: Option<i32>, cwd: Option<&str>, output: &str) -> String {
    let mut s = String::from(note);
    match exit {
        Some(code) => s.push_str(&format!("exit code: {code}\n")),
        None => s.push_str("exit code: unknown\n"),
    }
    if let Some(cwd) = cwd {
        s.push_str(&format!("cwd: {cwd}\n"));
    }
    let body = clip_for_model(output);
    if body.trim().is_empty() {
        s.push_str("(no output)");
    } else {
        s.push_str("output:\n");
        s.push_str(&body);
    }
    s
}

/// Bound what a tool result costs in context: keep the head and tail.
pub fn clip_for_model(text: &str) -> String {
    let clipped: Vec<String> = text
        .lines()
        .map(|l| if l.chars().count() > 1500 { l.chars().take(1500).collect::<String>() + " [line clipped]" } else { l.to_string() })
        .collect();
    osc::truncate_lines(&clipped.join("\n"), 150, 150)
}
