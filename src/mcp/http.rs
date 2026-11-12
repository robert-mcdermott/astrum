//! A server on the network, reached the way the protocol calls Streamable
//! HTTP: each message is sent in a request of its own, and the answer
//! comes back as the response, either a JSON message or a stream of events
//! (server-sent events) with the answer among them. A server that keeps a
//! session for its caller says so in a header, which is sent back with
//! every request after.
//!
//! What a server may send unasked, outside an answer, is not listened for:
//! the tools are listed again whenever the server is reached.

use super::{Client, Cmd, Event, Failure, OnEvent, from_server, notify_msg, request_msg};
use futures_util::StreamExt;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;

/// A message larger than this is not one.
const MAX_MESSAGE: usize = 64 * 1024 * 1024;

#[derive(Default)]
struct Session {
    /// The server's name for this session, once it has given one.
    id: Option<String>,
    /// The version of the protocol both sides agreed on.
    protocol: Option<String>,
}

struct Line {
    http: reqwest::Client,
    url: String,
    headers: Vec<(String, String)>,
    token: Option<String>,
    session: Mutex<Session>,
    on_event: OnEvent,
}

pub fn start(url: &str, headers: &[(String, String)], token: Option<String>, on_event: OnEvent) -> Result<Client, Failure> {
    let http = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .user_agent(concat!("astrum/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| Failure::Broken(format!("could not set up the connection: {e}")))?;
    let line = Arc::new(Line { http, url: url.to_string(), headers: headers.to_vec(), token, session: Mutex::default(), on_event });
    let (client, mut cmds) = Client::new();
    let gone = client.gone.clone();
    tokio::spawn(async move {
        let mut working: HashMap<u64, tokio::task::JoinHandle<()>> = HashMap::new();
        while let Some(cmd) = cmds.recv().await {
            working.retain(|_, task| !task.is_finished());
            match cmd {
                Cmd::Request { id, method, params, reply } => {
                    let line = line.clone();
                    working.insert(id, tokio::spawn(async move { ask(&line, id, &method, &params, reply).await }));
                }
                Cmd::Notify { method, params } => {
                    let line = line.clone();
                    tokio::spawn(async move {
                        let _ = post(&line, &notify_msg(&method, &params)).await;
                    });
                }
                // Given up on: the request is let go, and the server told (by the Notify that follows).
                Cmd::Forget { id } => {
                    if let Some(task) = working.remove(&id) {
                        task.abort();
                    }
                }
                Cmd::Agreed(protocol) => {
                    if let Ok(mut s) = line.session.lock() {
                        s.protocol = Some(protocol);
                    }
                }
                Cmd::Close => break,
            }
        }
        gone.store(true, std::sync::atomic::Ordering::Relaxed);
        for (_, task) in working.drain() {
            task.abort();
        }
        // A session the server keeps is ended, so that it need not wait to find out.
        let id = line.session.lock().ok().and_then(|s| s.id.clone());
        if id.is_some() {
            let mut request = line.http.delete(&line.url).timeout(Duration::from_secs(5));
            request = with_headers(&line, request);
            let _ = request.send().await;
        }
    });
    Ok(client)
}

/// The version both sides have agreed on, to be sent with every request after.
pub fn agreed(client: &Client, protocol: &str) {
    let _ = client.tx.send(Cmd::Agreed(protocol.to_string()));
}

fn with_headers(line: &Line, mut request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    for (name, value) in &line.headers {
        request = request.header(name.as_str(), value.as_str());
    }
    if let Some(token) = &line.token {
        request = request.bearer_auth(token);
    }
    if let Ok(s) = line.session.lock() {
        if let Some(id) = &s.id {
            request = request.header("Mcp-Session-Id", id.as_str());
        }
        if let Some(protocol) = &s.protocol {
            request = request.header("MCP-Protocol-Version", protocol.as_str());
        }
    }
    request
}

/// Send a message; the response, once its headers are in.
async fn post(line: &Line, msg: &Value) -> Result<reqwest::Response, Failure> {
    let request = line
        .http
        .post(&line.url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .body(msg.to_string());
    let response = with_headers(line, request).send().await.map_err(|e| Failure::Broken(format!("it could not be reached: {}", plain(&e))))?;
    if let Some(id) = response.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()) {
        if let Ok(mut s) = line.session.lock() {
            s.id = Some(id.to_string());
        }
    }
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        let asks = response.headers().get("www-authenticate").and_then(|v| v.to_str().ok()).map(str::to_string);
        return Err(Failure::Unauthorized(asks));
    }
    if status == reqwest::StatusCode::NOT_FOUND && line.session.lock().is_ok_and(|s| s.id.is_some()) {
        (line.on_event)(Event::Ended("the server no longer knows this session".into()));
        return Err(Failure::Broken("the server no longer knows this session".into()));
    }
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        let body: String = body.trim().chars().take(200).collect();
        return Err(Failure::Broken(format!("it answered {status}{}", if body.is_empty() { String::new() } else { format!(": {body}") })));
    }
    Ok(response)
}

/// What a connection error comes down to, without the chain of causes
/// that repeat the address.
fn plain(e: &reqwest::Error) -> String {
    let mut text = e.to_string();
    let mut source = std::error::Error::source(e);
    while let Some(cause) = source {
        text = cause.to_string();
        source = cause.source();
    }
    text
}

/// Ask, and pass the answer on to whoever is waiting for it.
async fn ask(line: &Line, id: u64, method: &str, params: &Value, reply: oneshot::Sender<Result<Value, Failure>>) {
    let mut pending = HashMap::new();
    pending.insert(id, reply);
    let response = match post(line, &request_msg(id, method, params)).await {
        Ok(response) => response,
        Err(why) => {
            if let Some(waiting) = pending.remove(&id) {
                let _ = waiting.send(Err(why));
            }
            return;
        }
    };
    let events = response.headers().get("content-type").and_then(|v| v.to_str().ok()).is_some_and(|t| t.starts_with("text/event-stream"));
    if events {
        let mut stream = response.bytes_stream();
        let mut parser = Events::default();
        'reading: while let Some(chunk) = stream.next().await {
            let Ok(chunk) = chunk else { break };
            for data in parser.feed(&chunk) {
                if take(line, &data, &mut pending).await || !pending.contains_key(&id) {
                    break 'reading;
                }
            }
            if parser.too_large() {
                break;
            }
        }
    } else {
        match read_limited(response).await {
            Some(body) => {
                take(line, &body, &mut pending).await;
            }
            None => {}
        }
    }
    if let Some(waiting) = pending.remove(&id) {
        let _ = waiting.send(Err(Failure::Broken("its answer did not come".into())));
    }
}

async fn read_limited(response: reqwest::Response) -> Option<Vec<u8>> {
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        body.extend_from_slice(&chunk.ok()?);
        if body.len() > MAX_MESSAGE {
            return None;
        }
    }
    Some(body)
}

/// A message (or a batch of them) from the server. True once nothing more
/// is wanted from this response.
async fn take(line: &Line, data: &[u8], pending: &mut HashMap<u64, oneshot::Sender<Result<Value, Failure>>>) -> bool {
    let Ok(msg) = serde_json::from_slice::<Value>(data) else { return false };
    let messages = match msg {
        Value::Array(batch) => batch,
        one => vec![one],
    };
    for msg in messages {
        if let Some(answer) = from_server(&msg, pending, &line.on_event) {
            // (What it asked of Astrum is answered in a request of its own.)
            let _ = post(line, &answer).await;
        }
    }
    pending.is_empty()
}

/// Server-sent events, taken apart: the data of each, as it completes.
#[derive(Default)]
struct Events {
    buf: Vec<u8>,
    data: Vec<u8>,
    has_data: bool,
}

impl Events {
    fn feed(&mut self, chunk: &[u8]) -> Vec<Vec<u8>> {
        self.buf.extend_from_slice(chunk);
        let mut done = Vec::new();
        while let Some(at) = self.buf.iter().position(|b| *b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=at).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.is_empty() {
                if self.has_data {
                    done.push(std::mem::take(&mut self.data));
                    self.has_data = false;
                }
                continue;
            }
            if let Some(rest) = line.strip_prefix(b"data:") {
                if self.has_data {
                    self.data.push(b'\n');
                }
                self.data.extend_from_slice(rest.strip_prefix(b" ").unwrap_or(rest));
                self.has_data = true;
            }
            // (`event:`, `id:`, `retry:` and comments say nothing Astrum needs.)
        }
        done
    }

    fn too_large(&self) -> bool {
        self.buf.len() + self.data.len() > MAX_MESSAGE
    }
}

/// Is this an address on this machine, where a connection is not seen by anyone else?
pub fn is_this_machine(url: &str) -> bool {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit_once('@').map(|(_, h)| h).unwrap_or(host);
    let host = if host.starts_with('[') { host.split(']').next().unwrap_or("").trim_start_matches('[') } else { host.split(':').next().unwrap_or("") };
    matches!(host, "localhost" | "127.0.0.1" | "::1") || host.ends_with(".localhost")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_taken_apart_however_they_arrive() {
        let mut e = Events::default();
        assert!(e.feed(b"event: message\r\nid: 1\r\ndata: {\"a\":").is_empty());
        assert_eq!(e.feed(b"1}\r\n\r\n: a comment\n\ndata: x\ndata:y\n\n"), vec![b"{\"a\":1}".to_vec(), b"x\ny".to_vec()]);
        // An event with no data is none at all.
        assert!(e.feed(b"event: ping\n\n").is_empty());
    }

    #[test]
    fn which_addresses_are_this_machine() {
        for url in ["http://localhost:3000/mcp", "http://127.0.0.1/mcp", "http://[::1]:8080/", "http://me@localhost/x", "http://dev.localhost/mcp"] {
            assert!(is_this_machine(url), "{url}");
        }
        for url in ["http://example.com/mcp", "http://localhost.example.com/", "http://127.0.0.2/", "http://evil.com/?h=localhost", "http://localhost@evil.com/"] {
            assert!(!is_this_machine(url), "{url}");
        }
    }
}
