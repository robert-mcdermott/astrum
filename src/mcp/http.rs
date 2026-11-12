//! A server on the network, reached the way the protocol calls Streamable
//! HTTP: each message is sent in a request of its own, and the answer
//! comes back as the response, either a JSON message or a stream of events
//! (server-sent events) with the answer among them. A server that keeps a
//! session for its caller says so in a header, which is sent back with
//! every request after.
//!
//! What a server may send unasked, outside an answer, is not listened for:
//! the tools are listed again whenever the server is reached.
//!
//! Redirects are not followed: what goes with a request (the headers, a
//! token, the session) goes to the address it was meant for and nowhere
//! else, and never from https to http.

use super::{Client, Cmd, Event, Failure, Log, OnEvent, Renew, from_server, notify_msg, request_msg};
use futures_util::StreamExt;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;

/// A message larger than this is not one.
const MAX_MESSAGE: usize = 64 * 1024 * 1024;
/// How much of what a server says when it refuses is read.
const MAX_REFUSAL: usize = 16 * 1024;
/// How long a message that is not a request (a notification, or an answer
/// to something the server asked) may take to be delivered.
const DELIVER: Duration = Duration::from_secs(30);

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
    /// What is sent as `Authorization: Bearer`; renewed, where a sign-in
    /// can be, when the server turns it away.
    token: Mutex<Option<String>>,
    renew: Option<Renew>,
    session: Mutex<Session>,
    on_event: OnEvent,
}

impl Line {
    fn token(&self) -> Option<String> {
        self.token.lock().ok().and_then(|t| t.clone())
    }
}

/// A client for MCP servers on the network and for signing in to them. It
/// follows no redirects (see above).
pub fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("astrum/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("could not set up the connection: {e}"))
}

pub fn start(url: &str, headers: &[(String, String)], token: Option<String>, renew: Option<Renew>, on_event: OnEvent, log: Log) -> Result<Client, Failure> {
    let http = client().map_err(Failure::Broken)?;
    let line = Arc::new(Line { http, url: url.to_string(), headers: headers.to_vec(), token: Mutex::new(token), renew, session: Mutex::default(), on_event });
    let (client, mut cmds) = Client::new(log);
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
                        let _ = post(&line, &notify_msg(&method, &params), Some(DELIVER)).await;
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
    if let Some(token) = line.token() {
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

/// Send a message; the response, once its headers are in. `within`, how
/// long the whole of it may take (a request's answer is waited for by
/// whoever asked, for as long as they are willing to).
async fn post(line: &Line, msg: &Value, within: Option<Duration>) -> Result<reqwest::Response, Failure> {
    let mut request = line
        .http
        .post(&line.url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .body(msg.to_string());
    if let Some(within) = within {
        request = request.timeout(within);
    }
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
    if status.is_redirection() {
        let to = response.headers().get("location").and_then(|v| v.to_str().ok()).unwrap_or("");
        let to: String = to.chars().take(200).collect();
        return Err(Failure::Broken(format!("it answered {status}, sending Astrum to `{to}`; redirects are not followed, so if that is where the server is, give that address as its `url`")));
    }
    if !status.is_success() {
        // (Only the start of what it says, and only for a while: it is shown in a line.)
        let body = tokio::time::timeout(Duration::from_secs(10), read_start(response, MAX_REFUSAL)).await.unwrap_or_default();
        let body: String = String::from_utf8_lossy(&body).trim().chars().take(200).collect();
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
    let msg = request_msg(id, method, params);
    let used = line.token();
    let mut sent = post(line, &msg, None).await;
    // Turned away: a sign-in that can be renewed is, and the request sent
    // again (once) with the new token.
    if let (Err(Failure::Unauthorized(_)), Some(renew)) = (&sent, &line.renew) {
        if let Some(fresh) = renew(used).await {
            if let Ok(mut token) = line.token.lock() {
                *token = Some(fresh);
            }
            sent = post(line, &msg, None).await;
        }
    }
    let response = match sent {
        Ok(response) => response,
        Err(why) => {
            if let Failure::Unauthorized(asks) = &why {
                (line.on_event)(Event::Unauthorized(asks.clone()));
            }
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
    } else if let Some(body) = read_limited(response, MAX_MESSAGE).await {
        take(line, &body, &mut pending).await;
    }
    if let Some(waiting) = pending.remove(&id) {
        let _ = waiting.send(Err(Failure::Broken("its answer did not come".into())));
    }
}

/// A response's body, if it is no larger than `limit`: reading stops as
/// soon as it is.
pub(super) async fn read_limited(response: reqwest::Response, limit: usize) -> Option<Vec<u8>> {
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        body.extend_from_slice(&chunk.ok()?);
        if body.len() > limit {
            return None;
        }
    }
    Some(body)
}

/// The start of a response's body, at most `limit` bytes of it.
async fn read_start(response: reqwest::Response, limit: usize) -> Vec<u8> {
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(Ok(chunk)) = stream.next().await {
        body.extend_from_slice(&chunk);
        if body.len() >= limit {
            body.truncate(limit);
            break;
        }
    }
    body
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
            let _ = post(line, &answer, Some(DELIVER)).await;
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

/// Is this an address on this machine, where a connection is not seen by
/// anyone else? (The host is read as the connection reads it, which is
/// not always as it looks: `http://a.example\@localhost/` is `a.example`.)
pub fn is_this_machine(url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(url) else { return false };
    let host = url.host_str().unwrap_or("");
    matches!(host, "localhost" | "127.0.0.1" | "[::1]") || host.ends_with(".localhost")
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
        for url in ["http://example.com/mcp", "http://localhost.example.com/", "http://127.0.0.2/", "http://evil.com/?h=localhost", "http://localhost@evil.com/", "http://evil.example\\@localhost/mcp", "not an address"] {
            assert!(!is_this_machine(url), "{url}");
        }
        // Written another way, it is still this machine.
        assert!(is_this_machine("http://LOCALHOST:3000/") && is_this_machine("http://0x7f.1/mcp"));
    }

    /// A server that answers one request with `reply` (and then keeps the
    /// connection open, saying nothing more, when `hold`); what it was sent.
    fn answer_once(reply: Vec<u8>, hold: bool) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        let heard = seen.clone();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            let Ok((mut stream, _)) = listener.accept() else { return };
            let mut buf = [0u8; 8192];
            let n = stream.read(&mut buf).unwrap_or(0);
            heard.lock().unwrap().push(String::from_utf8_lossy(&buf[..n]).to_string());
            let _ = stream.write_all(&reply);
            if hold {
                std::thread::sleep(Duration::from_secs(30));
            }
        });
        (format!("http://127.0.0.1:{port}/mcp"), seen)
    }

    fn quiet() -> OnEvent {
        Arc::new(|_| {})
    }

    #[tokio::test]
    async fn a_redirect_is_not_followed() {
        let (elsewhere, followed) = answer_once(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec(), false);
        let reply = format!("HTTP/1.1 307 Temporary Redirect\r\nLocation: {elsewhere}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        let (url, _) = answer_once(reply.into_bytes(), false);
        let headers = vec![("X-Api-Key".to_string(), "secret-header".to_string())];
        let client = start(&url, &headers, Some("tok-123".into()), None, quiet(), Log::default()).unwrap();
        let why = client.request("initialize", serde_json::json!({}), Duration::from_secs(10)).await.unwrap_err().to_string();
        assert!(why.contains("307") && why.contains("redirects are not followed"), "{why}");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(followed.lock().unwrap().is_empty(), "the address it was sent to was not visited");
        client.close();
    }

    #[tokio::test]
    async fn a_refusal_is_read_only_so_far() {
        // It says it will send a great deal, sends more than is read, and then nothing.
        let mut reply = b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 100000000\r\n\r\n".to_vec();
        reply.extend(std::iter::repeat_n(b'x', 64 * 1024));
        let (url, _) = answer_once(reply, true);
        let client = start(&url, &[], None, None, quiet(), Log::default()).unwrap();
        let started = std::time::Instant::now();
        let why = client.request("initialize", serde_json::json!({}), Duration::from_secs(20)).await.unwrap_err().to_string();
        assert!(why.starts_with("it answered 500 Internal Server Error: xxxx") && why.len() < 300, "{why}");
        assert!(started.elapsed() < Duration::from_secs(5), "it did not wait for the rest");
        client.close();
    }

    #[tokio::test]
    async fn a_token_turned_away_is_renewed_and_the_request_sent_again() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}/mcp", listener.local_addr().unwrap().port());
        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        let heard = seen.clone();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut buf = [0u8; 8192];
                let n = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).to_string();
                let fresh = request.to_lowercase().contains("authorization: bearer fresh-token");
                heard.lock().unwrap().push(if fresh { "fresh" } else { "old" }.to_string());
                let msg: Value = request.split_once("\r\n\r\n").and_then(|(_, b)| serde_json::from_str(b).ok()).unwrap_or_default();
                let body = serde_json::json!({"jsonrpc": "2.0", "id": msg["id"], "result": {"ok": true}}).to_string();
                let reply = if fresh {
                    format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
                } else {
                    "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Bearer\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                };
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        let asked: Arc<Mutex<Vec<Option<String>>>> = Arc::default();
        let told = asked.clone();
        let renew: Renew = Arc::new(move |refused| {
            told.lock().unwrap().push(refused);
            Box::pin(async { Some("fresh-token".to_string()) })
        });
        let events: Arc<Mutex<Vec<Event>>> = Arc::default();
        let heard_events = events.clone();
        let on: OnEvent = Arc::new(move |e| heard_events.lock().unwrap().push(e));
        let client = start(&url, &[], Some("old-token".into()), Some(renew), on, Log::default()).unwrap();
        assert_eq!(client.request("tools/list", serde_json::json!({}), Duration::from_secs(10)).await, Ok(serde_json::json!({"ok": true})));
        assert_eq!(*asked.lock().unwrap(), [Some("old-token".to_string())], "renewed once, told which token was turned away");
        assert_eq!(*seen.lock().unwrap(), ["old", "fresh"]);
        // From then on the new token is sent.
        client.request("tools/list", serde_json::json!({}), Duration::from_secs(10)).await.unwrap();
        assert_eq!(asked.lock().unwrap().len(), 1);
        assert!(events.lock().unwrap().is_empty());
        client.close();

        // Where there is nothing to renew it with, the session hears of it.
        let events: Arc<Mutex<Vec<Event>>> = Arc::default();
        let heard_events = events.clone();
        let on: OnEvent = Arc::new(move |e| heard_events.lock().unwrap().push(e));
        let client = start(&url, &[], Some("old-token".into()), None, on, Log::default()).unwrap();
        let why = client.request("tools/call", serde_json::json!({}), Duration::from_secs(10)).await.unwrap_err();
        assert!(matches!(why, Failure::Unauthorized(_)), "{why:?}");
        assert_eq!(*events.lock().unwrap(), [Event::Unauthorized(Some("Bearer".into()))]);
        client.close();
    }
}
