//! A client for MCP servers: programs and services that offer the agent
//! tools of their own (the Model Context Protocol).
//!
//! A server is reached in one of two ways: a program run on this machine
//! and talked to over its standard input and output (`stdio`), or an
//! address on the network that is sent each message in a request of its
//! own (`http`). Either way the conversation is JSON-RPC: Astrum says who
//! it is (`initialize`), asks for the tools (`tools/list`), and calls them
//! (`tools/call`). Astrum asks nothing else of a server and offers it
//! nothing: no sampling, no roots, no questions to the user.
//!
//! Each connection is a task of its own; a `Client` is a cheap handle to
//! it, held by the session and by any turn of the agent that is using one
//! of its tools.

pub mod http;
pub mod oauth;
mod stdio;

use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

/// The version of the protocol Astrum asks for. A server may answer with
/// another it prefers; any of `SPOKEN` will do, since all Astrum uses
/// (tools) is the same in each.
pub const PROTOCOL: &str = "2025-06-18";
const SPOKEN: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];

/// How long a server has to say who it is. (A program fetched on its first
/// run, as `npx` and `uvx` do, may take a while.)
pub const STARTUP: Duration = Duration::from_secs(60);
/// How many tools are taken from one server at most.
const MAX_TOOLS: usize = 500;
/// How many lines of what a program writes to its error output are kept,
/// to say why it failed.
const LOG_LINES: usize = 40;

/// A tool, as the server describes it.
#[derive(Clone, Debug, PartialEq)]
pub struct Tool {
    /// The server's own name for it.
    pub name: String,
    pub description: String,
    /// The JSON schema of its arguments: an object.
    pub schema: Value,
}

/// What a server says of itself when it is reached.
#[derive(Clone, Debug, Default)]
pub struct Info {
    pub name: String,
    pub version: String,
    /// What the server would like the model to know of it.
    pub instructions: Option<String>,
    pub protocol: String,
}

/// How to reach a server.
#[derive(Clone, Debug, PartialEq)]
pub enum Spec {
    Stdio {
        program: String,
        args: Vec<String>,
        /// The whole environment the program gets.
        env: Vec<(String, String)>,
        cwd: Option<String>,
    },
    Http {
        url: String,
        headers: Vec<(String, String)>,
        /// A token to send as `Authorization: Bearer`.
        token: Option<String>,
    },
}

/// Why a server could not be used.
#[derive(Clone, Debug, PartialEq)]
pub enum Failure {
    /// Something went wrong, and this says what.
    Broken(String),
    /// It is on the network and wants to know who is asking: a token, or
    /// signing in. What it said, if anything.
    Unauthorized(Option<String>),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Broken(why) => f.write_str(why),
            Failure::Unauthorized(Some(why)) => write!(f, "it asks to be signed in to ({why})"),
            Failure::Unauthorized(None) => f.write_str("it asks to be signed in to"),
        }
    }
}

/// Something a server did on its own, for the session to hear of.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Its tools are not the ones they were.
    ToolsChanged,
    /// The connection is over: the program ended, or the server went away.
    Ended(String),
}

/// What a server's errors, and the end of a connection, are told with.
pub type OnEvent = Arc<dyn Fn(Event) + Send + Sync>;

enum Cmd {
    Request { id: u64, method: String, params: Value, reply: oneshot::Sender<Result<Value, Failure>> },
    Notify { method: String, params: Value },
    /// The one waiting for an answer to this has stopped waiting.
    Forget { id: u64 },
    Close,
}

/// A connection to a server.
#[derive(Clone)]
pub struct Client {
    tx: mpsc::UnboundedSender<Cmd>,
    next: Arc<AtomicU64>,
    gone: Arc<AtomicBool>,
    /// The last of what the program wrote to its error output.
    pub log: Arc<Mutex<VecDeque<String>>>,
}

/// Waiting for an answer: if the wait is given up (a timeout, or the turn
/// that asked is cancelled), the server is told to stop working on it.
struct Waiting<'a> {
    client: &'a Client,
    id: u64,
    answered: bool,
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        if !self.answered {
            let _ = self.client.tx.send(Cmd::Forget { id: self.id });
            let params = json!({"requestId": self.id, "reason": "Astrum stopped waiting"});
            let _ = self.client.tx.send(Cmd::Notify { method: "notifications/cancelled".into(), params });
        }
    }
}

impl Client {
    fn new() -> (Client, mpsc::UnboundedReceiver<Cmd>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let client = Client { tx, next: Arc::new(AtomicU64::new(1)), gone: Arc::new(AtomicBool::new(false)), log: Arc::default() };
        (client, rx)
    }

    /// Ask the server something and wait for its answer.
    pub async fn request(&self, method: &str, params: Value, within: Duration) -> Result<Value, Failure> {
        if self.gone.load(Ordering::Relaxed) {
            return Err(Failure::Broken("the server is no longer connected".into()));
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (reply, answer) = oneshot::channel();
        if self.tx.send(Cmd::Request { id, method: method.into(), params, reply }).is_err() {
            return Err(Failure::Broken("the server is no longer connected".into()));
        }
        let mut waiting = Waiting { client: self, id, answered: false };
        let got = tokio::time::timeout(within, answer).await;
        waiting.answered = got.is_ok();
        match got {
            Err(_) => Err(Failure::Broken(match within.as_millis() {
                ms @ 0..1000 => format!("it did not answer within {ms} ms"),
                _ => format!("it did not answer within {}s", within.as_secs()),
            })),
            Ok(Err(_)) => Err(Failure::Broken("the server stopped before it answered".into())),
            Ok(Ok(answer)) => answer,
        }
    }

    pub fn notify(&self, method: &str, params: Value) {
        let _ = self.tx.send(Cmd::Notify { method: method.into(), params });
    }

    /// End the connection (and the program, if there is one).
    pub fn close(&self) {
        self.gone.store(true, Ordering::Relaxed);
        let _ = self.tx.send(Cmd::Close);
    }

    pub fn is_gone(&self) -> bool {
        self.gone.load(Ordering::Relaxed)
    }

    /// All the server's tools.
    pub async fn tools(&self) -> Result<Vec<Tool>, Failure> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        // (A server that hands out the same page for ever is not followed for ever.)
        for _ in 0..50 {
            let params = match &cursor {
                Some(c) => json!({"cursor": c}),
                None => json!({}),
            };
            let page = self.request("tools/list", params, STARTUP).await?;
            for tool in page.get("tools").and_then(Value::as_array).into_iter().flatten() {
                if let Some(tool) = tool_from(tool) {
                    tools.push(tool);
                }
            }
            cursor = page.get("nextCursor").and_then(Value::as_str).filter(|c| !c.is_empty()).map(str::to_string);
            if cursor.is_none() || tools.len() >= MAX_TOOLS {
                break;
            }
        }
        tools.truncate(MAX_TOOLS);
        Ok(tools)
    }

    /// Call a tool; what it returned, as text for the model, and whether
    /// it says it failed.
    pub async fn call(&self, tool: &str, args: Value, within: Duration) -> (String, bool) {
        let args = if args.is_object() { args } else { json!({}) };
        match self.request("tools/call", json!({"name": tool, "arguments": args}), within).await {
            Ok(result) => result_text(&result),
            Err(why) => (format!("The tool could not be used: {why}."), true),
        }
    }
}

/// A tool from what `tools/list` says of it.
fn tool_from(v: &Value) -> Option<Tool> {
    let name = v.get("name")?.as_str()?.trim();
    if name.is_empty() || name.len() > 200 {
        return None;
    }
    let description = v.get("description").and_then(Value::as_str).or_else(|| v.get("title").and_then(Value::as_str)).unwrap_or("").trim();
    let mut schema = v.get("inputSchema").cloned().filter(Value::is_object).unwrap_or_else(|| json!({"type": "object"}));
    if let Some(map) = schema.as_object_mut() {
        // (Some providers refuse a schema that names the dialect it is in.)
        map.remove("$schema");
        map.insert("type".into(), json!("object"));
        map.entry("properties").or_insert_with(|| json!({}));
    }
    let description: String = description.chars().take(4000).collect();
    Some(Tool { name: name.to_string(), description, schema })
}

/// What a tool returned, as text for the model, and whether it failed.
pub fn result_text(result: &Value) -> (String, bool) {
    let failed = result.get("isError").and_then(Value::as_bool).unwrap_or(false);
    let mut parts: Vec<String> = Vec::new();
    let s = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).unwrap_or("").to_string();
    for item in result.get("content").and_then(Value::as_array).into_iter().flatten() {
        match item.get("type").and_then(Value::as_str) {
            Some("text") => parts.push(s(item, "text")),
            Some(kind @ ("image" | "audio")) => {
                parts.push(format!("[the tool returned {} ({}); it is not shown to you]", if kind == "image" { "a picture" } else { "a recording" }, s(item, "mimeType")))
            }
            Some("resource") => {
                let r = item.get("resource").cloned().unwrap_or_default();
                match r.get("text").and_then(Value::as_str) {
                    Some(text) => parts.push(format!("[{}]\n{text}", s(&r, "uri"))),
                    None => parts.push(format!("[the tool returned the contents of {} ({}) as data; it is not shown to you]", s(&r, "uri"), s(&r, "mimeType"))),
                }
            }
            Some("resource_link") => {
                let name = s(item, "name");
                parts.push(format!("[a link: {}{}]", s(item, "uri"), if name.is_empty() { String::new() } else { format!(" ({name})") }))
            }
            _ => parts.push(item.to_string().chars().take(2000).collect()),
        }
    }
    if parts.is_empty() {
        if let Some(structured) = result.get("structuredContent").filter(|v| !v.is_null()) {
            parts.push(serde_json::to_string_pretty(structured).unwrap_or_default());
        }
    }
    if parts.is_empty() {
        parts.push("(the tool returned nothing)".into());
    }
    (parts.join("\n"), failed)
}

/// Reach a server: start the program or open the connection, say who is
/// asking, and fetch its tools.
pub async fn connect(spec: &Spec, on_event: OnEvent) -> Result<(Client, Info, Vec<Tool>), Failure> {
    let client = match spec {
        Spec::Stdio { program, args, env, cwd } => stdio::start(program, args, env, cwd.as_deref(), on_event)?,
        Spec::Http { url, headers, token } => http::start(url, headers, token.clone(), on_event)?,
    };
    let hello = json!({
        "protocolVersion": PROTOCOL,
        "capabilities": {},
        "clientInfo": {"name": "astrum", "title": "Astrum", "version": env!("CARGO_PKG_VERSION")},
    });
    let answer = match client.request("initialize", hello, STARTUP).await {
        Ok(answer) => answer,
        Err(why) => {
            let why = with_log(why, &client);
            client.close();
            return Err(why);
        }
    };
    let protocol = answer.get("protocolVersion").and_then(Value::as_str).unwrap_or("").to_string();
    if !SPOKEN.contains(&protocol.as_str()) {
        client.close();
        return Err(Failure::Broken(format!("it speaks version `{protocol}` of the protocol, which Astrum does not know")));
    }
    let server = answer.get("serverInfo").cloned().unwrap_or_default();
    let instructions = answer.get("instructions").and_then(Value::as_str).map(|t| t.chars().take(4000).collect::<String>()).filter(|t: &String| !t.trim().is_empty());
    let info = Info {
        name: server.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
        version: server.get("version").and_then(Value::as_str).unwrap_or("").to_string(),
        instructions,
        protocol: protocol.clone(),
    };
    if let Spec::Http { .. } = spec {
        http::agreed(&client, &protocol);
    }
    client.notify("notifications/initialized", json!({}));
    // A server without tools has nothing for Astrum.
    let offers_tools = answer.get("capabilities").and_then(|c| c.get("tools")).is_some();
    let tools = if offers_tools {
        match client.tools().await {
            Ok(tools) => tools,
            Err(why) => {
                client.close();
                return Err(why);
            }
        }
    } else {
        Vec::new()
    };
    Ok((client, info, tools))
}

/// A failure, with the last thing the program said on its way out.
fn with_log(why: Failure, client: &Client) -> Failure {
    match why {
        Failure::Broken(why) => {
            let last: Vec<String> = client.log.lock().map(|l| l.iter().rev().take(3).rev().cloned().collect()).unwrap_or_default();
            if last.is_empty() { Failure::Broken(why) } else { Failure::Broken(format!("{why}; it said: {}", last.join(" / "))) }
        }
        other => other,
    }
}

fn keep_log(log: &Mutex<VecDeque<String>>, line: &str) {
    let line = line.trim_end();
    if line.is_empty() {
        return;
    }
    if let Ok(mut log) = log.lock() {
        log.push_back(line.chars().take(300).collect());
        while log.len() > LOG_LINES {
            log.pop_front();
        }
    }
}

/// Answer what a server asks of Astrum, and pass on what it says unasked.
/// `None` when nothing is to be sent back.
fn from_server(msg: &Value, pending: &mut HashMap<u64, oneshot::Sender<Result<Value, Failure>>>, on_event: &OnEvent) -> Option<Value> {
    let method = msg.get("method").and_then(Value::as_str);
    let id = msg.get("id").filter(|id| !id.is_null());
    match (method, id) {
        // An answer to something Astrum asked.
        (None, Some(id)) => {
            let waiting = id.as_u64().and_then(|n| pending.remove(&n))?;
            let answer = match (msg.get("result"), msg.get("error")) {
                (_, Some(error)) => Err(Failure::Broken(rpc_error(error))),
                (Some(result), None) => Ok(result.clone()),
                (None, None) => Err(Failure::Broken("its answer had neither a result nor an error".into())),
            };
            let _ = waiting.send(answer);
            None
        }
        // It asks something of Astrum. Only whether it is still there is
        // answered: nothing else was offered.
        (Some(method), Some(id)) => Some(if method == "ping" {
            json!({"jsonrpc": "2.0", "id": id, "result": {}})
        } else {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("Astrum does not offer `{method}`")}})
        }),
        (Some("notifications/tools/list_changed"), None) => {
            on_event(Event::ToolsChanged);
            None
        }
        _ => None,
    }
}

fn rpc_error(error: &Value) -> String {
    let message = error.get("message").and_then(Value::as_str).unwrap_or("an error with no message");
    match error.get("code").and_then(Value::as_i64) {
        Some(code) => format!("{message} (error {code})"),
        None => message.to_string(),
    }
}

fn request_msg(id: u64, method: &str, params: &Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn notify_msg(method: &str, params: &Value) -> Value {
    json!({"jsonrpc": "2.0", "method": method, "params": params})
}

// ---- names ---------------------------------------------------------------------

/// The name the model knows a server's tool by: the server's name and the
/// tool's, as providers allow names to be written (letters, digits, `_`
/// and `-`, at most 64 of them), and different from every name in `taken`.
pub fn model_name(server: &str, tool: &str, taken: &HashSet<String>) -> String {
    let clean = |s: &str| -> String { s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect() };
    let mut name = format!("{}__{}", clean(server), clean(tool));
    if name.len() > 64 {
        // Cut, with a mark of what was cut so that two long names stay two.
        let mark = format!("{:08x}", fnv(&name));
        name = format!("{}_{mark}", &name[..55]);
    }
    if !taken.contains(&name) {
        return name;
    }
    (2..).map(|n| format!("{}_{n}", &name[..name.len().min(60)])).find(|candidate| !taken.contains(candidate)).unwrap_or(name)
}

fn fnv(s: &str) -> u32 {
    s.bytes().fold(0x811c_9dc5u32, |h, b| (h ^ b as u32).wrapping_mul(0x0100_0193))
}

/// Does `pattern` (where `*` stands for anything) cover `name`?
pub fn matches(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == name;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !name.starts_with(first) || !name[first.len()..].ends_with(last) || name.len() < first.len() + last.len() {
        return false;
    }
    let mut rest = &name[first.len()..name.len() - last.len()];
    for part in &parts[1..parts.len() - 1] {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    true
}

/// A server's name as the config file may give it: it is part of the
/// names the model knows the tools by, and of a name in the password store.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 32 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_model_knows_tools_by() {
        let none = HashSet::new();
        assert_eq!(model_name("files", "read_text_file", &none), "files__read_text_file");
        assert_eq!(model_name("gh", "issues.list/all", &none), "gh__issues_list_all");
        let long = model_name("server", &"t".repeat(100), &none);
        assert!(long.len() <= 64 && long.starts_with("server__ttt"), "{long}");
        assert_ne!(long, model_name("server", &format!("{}u", "t".repeat(99)), &none), "two long names stay two");
        let taken: HashSet<String> = ["a__b".to_string(), "a__b_2".to_string()].into();
        assert_eq!(model_name("a", "b", &taken), "a__b_3");
        for name in [long, model_name("x", "ü ñ", &none)] {
            assert!(name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') && name.len() <= 64, "{name}");
        }
    }

    #[test]
    fn patterns_of_names() {
        assert!(matches("read_file", "read_file") && !matches("read_file", "read_files"));
        assert!(matches("*", "anything") && matches("get_*", "get_issue") && !matches("get_*", "list_issues"));
        assert!(matches("*_issue", "get_issue") && matches("a*b*c", "a-x-b-y-c") && !matches("a*b*c", "a-c-b"));
        assert!(!matches("ab*ba", "aba"), "the two ends may not overlap");
    }

    #[test]
    fn what_a_tool_returns_becomes_text() {
        let r = json!({"content": [
            {"type": "text", "text": "first"},
            {"type": "image", "data": "AAAA", "mimeType": "image/png"},
            {"type": "resource", "resource": {"uri": "file:///a.txt", "text": "inside"}},
            {"type": "resource", "resource": {"uri": "file:///b.bin", "mimeType": "application/octet-stream", "blob": "AA=="}},
            {"type": "resource_link", "uri": "https://x/y", "name": "y"},
        ]});
        let (text, failed) = result_text(&r);
        assert!(!failed);
        assert_eq!(text.lines().next(), Some("first"));
        assert!(text.contains("a picture (image/png)") && text.contains("[file:///a.txt]\ninside") && text.contains("file:///b.bin") && text.contains("[a link: https://x/y (y)]"), "{text}");
        assert!(!text.contains("AAAA"), "data is not passed on");
        let (text, failed) = result_text(&json!({"content": [], "structuredContent": {"n": 3}, "isError": true}));
        assert!(failed && text.contains("\"n\": 3"));
        assert_eq!(result_text(&json!({})).0, "(the tool returned nothing)");
    }

    #[test]
    fn tools_are_taken_as_providers_will_have_them() {
        let t = tool_from(&json!({"name": "q", "inputSchema": {"$schema": "https://json-schema.org/draft/2020-12/schema", "properties": {"a": {"type": "string"}}}})).unwrap();
        assert_eq!(t.schema, json!({"type": "object", "properties": {"a": {"type": "string"}}}));
        assert_eq!(tool_from(&json!({"name": "bare"})).unwrap().schema, json!({"type": "object", "properties": {}}));
        assert!(tool_from(&json!({"name": ""})).is_none() && tool_from(&json!({"description": "no name"})).is_none());
        assert_eq!(tool_from(&json!({"name": "t", "title": "A title"})).unwrap().description, "A title");
    }

    #[test]
    fn what_a_server_says_is_answered_or_passed_on() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let heard = seen.clone();
        let on: OnEvent = Arc::new(move |e| heard.lock().unwrap().push(e));
        let mut pending = HashMap::new();
        let (tx, mut rx) = oneshot::channel();
        pending.insert(7, tx);
        assert_eq!(from_server(&json!({"jsonrpc": "2.0", "id": 7, "result": {"ok": 1}}), &mut pending, &on), None);
        assert_eq!(rx.try_recv().unwrap().unwrap(), json!({"ok": 1}));
        let (tx, mut rx) = oneshot::channel();
        pending.insert(8, tx);
        from_server(&json!({"jsonrpc": "2.0", "id": 8, "error": {"code": -32602, "message": "bad"}}), &mut pending, &on);
        assert_eq!(rx.try_recv().unwrap(), Err(Failure::Broken("bad (error -32602)".into())));
        // An answer to nothing that was asked is passed over.
        assert_eq!(from_server(&json!({"jsonrpc": "2.0", "id": 99, "result": {}}), &mut pending, &on), None);
        assert_eq!(from_server(&json!({"jsonrpc": "2.0", "id": "s1", "method": "ping"}), &mut pending, &on), Some(json!({"jsonrpc": "2.0", "id": "s1", "result": {}})));
        let refused = from_server(&json!({"jsonrpc": "2.0", "id": 3, "method": "sampling/createMessage", "params": {}}), &mut pending, &on).unwrap();
        assert_eq!(refused["error"]["code"], json!(-32601));
        from_server(&json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}), &mut pending, &on);
        from_server(&json!({"jsonrpc": "2.0", "method": "notifications/message", "params": {}}), &mut pending, &on);
        assert_eq!(*seen.lock().unwrap(), [Event::ToolsChanged]);
    }

    // ---- a program over its input and output -------------------------------

    const STAND_IN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/mcp/stand-in.sh");

    fn stand_in(name: &str, more: &[(&str, &str)]) -> (Spec, std::path::PathBuf) {
        let log = std::env::temp_dir().join(format!("astrum-mcp-{}-{name}.log", std::process::id()));
        let _ = std::fs::remove_file(&log);
        let _ = std::fs::remove_file(log.with_extension("log.grown"));
        let mut env = vec![("PATH".to_string(), std::env::var("PATH").unwrap_or_default()), ("MCP_LOG".to_string(), log.display().to_string()), ("MCP_NAME".to_string(), name.to_string())];
        env.extend(more.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        (Spec::Stdio { program: "sh".into(), args: vec![STAND_IN.into()], env, cwd: None }, log)
    }

    fn events() -> (OnEvent, Arc<Mutex<Vec<Event>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let heard = seen.clone();
        (Arc::new(move |e| heard.lock().unwrap().push(e)), seen)
    }

    async fn until(what: &str, mut ok: impl FnMut() -> bool) {
        for _ in 0..200 {
            if ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("timed out waiting for {what}");
    }

    #[tokio::test]
    async fn a_program_is_reached_and_its_tools_are_used() {
        let (spec, log) = stand_in("first", &[]);
        let (on, seen) = events();
        let (client, info, tools) = connect(&spec, on).await.unwrap();
        assert_eq!((info.name.as_str(), info.version.as_str(), info.protocol.as_str()), ("first", "1.0", "2025-06-18"));
        assert_eq!(info.instructions.as_deref(), Some("Stand-in instructions: be brief."));
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["echo", "boom", "slow", "grow", "quit"]);
        assert_eq!(tools[0].schema["required"], json!(["words"]));
        assert_eq!(client.call("echo", json!({"words": "over the pipe"}), Duration::from_secs(5)).await, ("echo: over the pipe".into(), false));
        assert_eq!(client.call("boom", json!({}), Duration::from_secs(5)).await, ("it went wrong".into(), true));
        // Who it was told it is talking to, and that it was told it is ready.
        let said = std::fs::read_to_string(&log).unwrap();
        assert!(said.lines().next().unwrap().contains(r#""clientInfo":{"name":"astrum","title":"Astrum""#), "{said}");
        assert!(said.contains(r#""method":"notifications/initialized""#), "{said}");
        // What it wrote to its error output is its log.
        assert!(client.log.lock().unwrap().iter().any(|l| l == "first: starting"));

        // Its tools change: it says so.
        client.call("grow", json!({}), Duration::from_secs(5)).await;
        until("the change to be heard", || seen.lock().unwrap().contains(&Event::ToolsChanged)).await;
        assert_eq!(client.tools().await.unwrap().len(), 6);

        // Closed: the program is asked to stop, and does.
        client.close();
        until("the program to end", || client.log.lock().unwrap().iter().any(|l| l == "first: input closed")).await;
        assert!(client.is_gone());
        let (text, failed) = client.call("echo", json!({"words": "x"}), Duration::from_secs(5)).await;
        assert!(failed && text.contains("no longer connected"), "{text}");
        assert!(seen.lock().unwrap().iter().all(|e| !matches!(e, Event::Ended(_))), "closing it is not its ending");
    }

    #[tokio::test]
    async fn a_tool_that_takes_too_long_is_given_up_and_the_server_told() {
        let (spec, log) = stand_in("slow", &[]);
        let (on, _) = events();
        let (client, _, _) = connect(&spec, on).await.unwrap();
        let (text, failed) = client.call("slow", json!({}), Duration::from_millis(300)).await;
        assert!(failed && text.contains("did not answer within 300 ms"), "{text}");
        until("the cancellation to reach it", || std::fs::read_to_string(&log).unwrap_or_default().contains(r#""method":"notifications/cancelled""#)).await;
        // Its late answer is passed over, and the next call is answered.
        assert_eq!(client.call("echo", json!({"words": "after"}), Duration::from_secs(10)).await.0, "echo: after");
        client.close();
    }

    #[tokio::test]
    async fn a_program_that_ends_or_will_not_start_says_why() {
        let (spec, _) = stand_in("quitter", &[]);
        let (on, seen) = events();
        let (client, _, _) = connect(&spec, on).await.unwrap();
        let (text, failed) = client.call("quit", json!({}), Duration::from_secs(5)).await;
        assert!(failed && text.contains("stopped before it answered"), "{text}");
        until("its end to be heard", || seen.lock().unwrap().iter().any(|e| matches!(e, Event::Ended(_)))).await;
        let ended = seen.lock().unwrap().clone();
        assert!(matches!(&ended[0], Event::Ended(why) if why.contains("exit 0") && why.contains("asked to quit")), "{ended:?}");

        let (spec, _) = stand_in("broken", &[("MCP_BROKEN", "1")]);
        let (on, _) = events();
        let why = connect(&spec, on).await.err().unwrap().to_string();
        assert!(why.contains("stopped before it answered") && why.contains("no configuration found"), "{why}");

        let (on, _) = events();
        let missing = Spec::Stdio { program: "astrum-no-such-program".into(), args: vec![], env: vec![("PATH".into(), "/usr/bin:/bin".into())], cwd: None };
        assert_eq!(connect(&missing, on).await.err(), Some(Failure::Broken("`astrum-no-such-program` was not found".into())));
    }
}

