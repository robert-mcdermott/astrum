//! Signing in to an MCP server on the network, as the protocol has it:
//! OAuth 2.1 with PKCE, the user's browser, and an address on this machine
//! for the browser to come back to.
//!
//! In turn: the server says where to sign in (its protected resource
//! metadata, found from what it said when it turned Astrum away); the
//! authorization server says how (its metadata); Astrum registers itself
//! with it if it lets clients do that and none was given; the browser is
//! sent to sign in, and comes back to `http://127.0.0.1:<port>/callback`
//! with a code; the code is exchanged for a token. The token, and the
//! means to renew it, are kept in the system's password store.
//!
//! Where the browser is on another machine than Astrum (Astrum used over
//! SSH), coming back to this machine cannot work: the address the browser
//! ends up at can then be pasted into Astrum instead.

use aws_lc_rs::digest;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

/// How long the browser is given to come back.
const SIGN_IN_WAIT: Duration = Duration::from_secs(300);
const ASK: Duration = Duration::from_secs(30);

/// A sign-in, as it is kept: the token, and what renewing it takes.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Grant {
    pub access: String,
    #[serde(default)]
    pub refresh: Option<String>,
    /// When the token runs out, in seconds since 1970, if the server said.
    #[serde(default)]
    pub expires: Option<u64>,
    pub token_endpoint: String,
    pub client_id: String,
    #[serde(default)]
    pub client_secret: Option<String>,
    /// The server the token is for.
    pub resource: String,
}

impl Grant {
    /// As it is kept in the password store (one line).
    pub fn to_stored(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// From the password store: `None` for something else kept there (a
    /// token given with `/mcp key`).
    pub fn from_stored(text: &str) -> Option<Grant> {
        text.starts_with('{').then(|| serde_json::from_str(text).ok()).flatten()
    }

    /// Will it have run out within a minute?
    pub fn stale(&self) -> bool {
        self.expires.is_some_and(|at| at <= now() + 60)
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// What a server asks for when it turns a caller away: a parameter of its
/// `WWW-Authenticate` header (`resource_metadata`, `scope`).
pub fn asked(www_authenticate: &str, key: &str) -> Option<String> {
    let lower = www_authenticate.to_ascii_lowercase();
    let mut from = 0;
    while let Some(at) = lower[from..].find(key) {
        let start = from + at;
        from = start + key.len();
        // (A whole parameter name: `scope` is not the end of `myscope`.)
        if start > 0 && lower.as_bytes()[start - 1].is_ascii_alphanumeric() {
            continue;
        }
        let rest = www_authenticate[from..].trim_start();
        let Some(rest) = rest.strip_prefix('=') else { continue };
        let rest = rest.trim_start();
        return Some(match rest.strip_prefix('"') {
            Some(quoted) => quoted.split('"').next().unwrap_or("").to_string(),
            None => rest.split([',', ' ']).next().unwrap_or("").to_string(),
        });
    }
    None
}

/// Where a well-known document of `base` is: at the root of its host, with
/// its own path after (RFC 8414's way); and, for OpenID, also after its path.
fn well_known(base: &Url, name: &str) -> Vec<Url> {
    let path = base.path().trim_end_matches('/');
    let mut root = base.clone();
    root.set_query(None);
    root.set_fragment(None);
    let mut found = Vec::new();
    let mut at = |p: String| {
        let mut u = root.clone();
        u.set_path(&p);
        if !found.contains(&u) {
            found.push(u);
        }
    };
    if !path.is_empty() {
        at(format!("/.well-known/{name}{path}"));
        if name == "openid-configuration" {
            at(format!("{path}/.well-known/{name}"));
        }
    }
    at(format!("/.well-known/{name}"));
    found
}

async fn get_json(http: &reqwest::Client, url: &Url) -> Option<Value> {
    let r = http.get(url.clone()).header("Accept", "application/json").header("MCP-Protocol-Version", super::PROTOCOL).timeout(ASK).send().await.ok()?;
    if !r.status().is_success() {
        return None;
    }
    r.json::<Value>().await.ok().filter(Value::is_object)
}

/// How to sign in to an authorization server.
#[derive(Debug, Clone, PartialEq)]
pub struct Endpoints {
    pub authorize: String,
    pub token: String,
    pub register: Option<String>,
    /// Scopes the server lists, if it lists any.
    pub scopes: Vec<String>,
}

/// Find out from the server where and how to sign in.
pub async fn discover(http: &reqwest::Client, server: &str, www_authenticate: Option<&str>) -> Result<Endpoints, String> {
    let server_url = Url::parse(server).map_err(|e| format!("its address is not one: {e}"))?;
    // The protected resource metadata: where it says, or where it is kept.
    let mut tried: Vec<Url> = www_authenticate.and_then(|h| asked(h, "resource_metadata")).and_then(|u| Url::parse(&u).ok()).into_iter().collect();
    tried.extend(well_known(&server_url, "oauth-protected-resource"));
    let mut resource = None;
    for url in &tried {
        if let Some(found) = get_json(http, url).await {
            resource = Some(found);
            break;
        }
    }
    let mut scopes: Vec<String> = resource.as_ref().and_then(|r| r.get("scopes_supported")).and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
    // Its authorization server; or, from before servers said, itself.
    let issuer = resource.as_ref().and_then(|r| r.get("authorization_servers")).and_then(Value::as_array).and_then(|a| a.first()).and_then(Value::as_str).map(str::to_string);
    let issuer = match issuer {
        Some(issuer) => Url::parse(&issuer).map_err(|e| format!("it names an authorization server that is not an address: {e}"))?,
        None => {
            let mut origin = server_url.clone();
            origin.set_path("/");
            origin.set_query(None);
            origin
        }
    };
    if issuer.scheme() != "https" && !super::http::is_this_machine(issuer.as_str()) {
        return Err(format!("its authorization server, {issuer}, is not reached over https"));
    }
    let mut meta = None;
    for url in well_known(&issuer, "oauth-authorization-server").into_iter().chain(well_known(&issuer, "openid-configuration")) {
        if let Some(found) = get_json(http, &url).await {
            meta = Some(found);
            break;
        }
    }
    let at = |key: &str, default: &str| -> String {
        meta.as_ref().and_then(|m| m.get(key)).and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| {
            let mut u = issuer.clone();
            u.set_path(default);
            u.to_string()
        })
    };
    if meta.is_none() && resource.is_none() {
        return Err("it does not say how to sign in to it (it has no OAuth metadata). If it takes a token, store one with `/mcp key`".into());
    }
    if let Some(methods) = meta.as_ref().and_then(|m| m.get("code_challenge_methods_supported")).and_then(Value::as_array) {
        if !methods.iter().any(|m| m == "S256") {
            return Err("its authorization server does not offer PKCE with S256, which signing in safely needs".into());
        }
    }
    if scopes.is_empty() {
        scopes = meta.as_ref().and_then(|m| m.get("scopes_supported")).and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
    }
    let register = meta.as_ref().and_then(|m| m.get("registration_endpoint")).and_then(Value::as_str).map(str::to_string).or_else(|| meta.is_none().then(|| at("registration_endpoint", "/register")));
    Ok(Endpoints { authorize: at("authorization_endpoint", "/authorize"), token: at("token_endpoint", "/token"), register, scopes })
}

fn random(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    use std::io::Read;
    let ok = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut buf)).is_ok();
    if !ok {
        // (Not a secret worth the name, but unique: the sign-in is then still checked.)
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (t >> ((i % 16) * 8)) as u8 ^ (std::process::id() as u8).wrapping_add(i as u8);
        }
    }
    URL_SAFE_NO_PAD.encode(buf)
}

fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(digest::digest(&digest::SHA256, verifier.as_bytes()).as_ref())
}

fn form(pairs: &[(&str, &str)]) -> String {
    let mut u = Url::parse("http://x/").unwrap_or_else(|_| unreachable!());
    u.query_pairs_mut().extend_pairs(pairs.iter().filter(|(_, v)| !v.is_empty()));
    u.query().unwrap_or("").to_string()
}

async fn token_request(http: &reqwest::Client, endpoint: &str, pairs: &[(&str, &str)]) -> Result<Value, String> {
    let r = http
        .post(endpoint)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .body(form(pairs))
        .timeout(ASK)
        .send()
        .await
        .map_err(|e| format!("the authorization server could not be reached: {e}"))?;
    let status = r.status();
    let body: Value = r.json().await.unwrap_or_default();
    if !status.is_success() {
        let why = body.get("error_description").or_else(|| body.get("error")).and_then(Value::as_str).unwrap_or("no reason given");
        return Err(format!("the authorization server refused ({status}): {why}"));
    }
    if body.get("access_token").and_then(Value::as_str).is_none_or(str::is_empty) {
        return Err("the authorization server gave no token".into());
    }
    Ok(body)
}

fn grant_from(body: &Value, before: Option<&Grant>, token_endpoint: &str, client_id: &str, client_secret: Option<&str>, resource: &str) -> Grant {
    let s = |k: &str| body.get(k).and_then(Value::as_str).map(str::to_string);
    Grant {
        access: s("access_token").unwrap_or_default(),
        // (A server that does not hand out a new one keeps the old one good.)
        refresh: s("refresh_token").or_else(|| before.and_then(|b| b.refresh.clone())),
        expires: body.get("expires_in").and_then(Value::as_u64).map(|secs| now() + secs),
        token_endpoint: token_endpoint.to_string(),
        client_id: client_id.to_string(),
        client_secret: client_secret.map(str::to_string),
        resource: resource.to_string(),
    }
}

/// Renew a sign-in that has run out, or is about to.
pub async fn refresh(http: &reqwest::Client, grant: &Grant) -> Result<Grant, String> {
    let Some(refresh) = &grant.refresh else { return Err("there is nothing to renew it with: sign in again".into()) };
    let secret = grant.client_secret.as_deref().unwrap_or("");
    let body = token_request(http, &grant.token_endpoint, &[("grant_type", "refresh_token"), ("refresh_token", refresh), ("client_id", &grant.client_id), ("client_secret", secret), ("resource", &grant.resource)]).await?;
    Ok(grant_from(&body, Some(grant), &grant.token_endpoint, &grant.client_id, grant.client_secret.as_deref(), &grant.resource))
}

/// What a sign-in is told to do, and where it is.
pub enum Progress {
    /// Send the browser here.
    Open(String),
}

/// What to ask for, from the server's word, the config file and the
/// server's list (in that order).
fn scope_for(www_authenticate: Option<&str>, configured: Option<&str>, listed: &[String]) -> String {
    www_authenticate.and_then(|h| asked(h, "scope")).or_else(|| configured.map(str::to_string)).unwrap_or_else(|| listed.join(" "))
}

/// Sign in. `told` hears where to send the browser; `pasted` brings the
/// address the browser ended at, if it is pasted in.
pub async fn sign_in(
    http: &reqwest::Client,
    server: &str,
    www_authenticate: Option<&str>,
    client_id: Option<&str>,
    scope: Option<&str>,
    told: mpsc::Sender<Progress>,
    mut pasted: mpsc::Receiver<String>,
) -> Result<Grant, String> {
    let endpoints = discover(http, server, www_authenticate).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.map_err(|e| format!("no address on this machine for the browser to come back to: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect = format!("http://127.0.0.1:{port}/callback");

    // Who is asking: a client the config file names, or one registered now.
    let (client_id, client_secret) = match client_id {
        Some(id) => (id.to_string(), None),
        None => {
            let Some(register) = &endpoints.register else {
                return Err("its authorization server does not let clients register themselves: give the client id it has given you as `client_id` for this server".into());
            };
            let asked = json!({
                "client_name": "Astrum",
                "redirect_uris": [redirect],
                "grant_types": ["authorization_code", "refresh_token"],
                "response_types": ["code"],
                "token_endpoint_auth_method": "none",
            });
            let r = http.post(register).json(&asked).timeout(ASK).send().await.map_err(|e| format!("registering with the authorization server failed: {e}"))?;
            let status = r.status();
            let body: Value = r.json().await.unwrap_or_default();
            let id = body.get("client_id").and_then(Value::as_str).filter(|_| status.is_success());
            let Some(id) = id else {
                let why = body.get("error_description").or_else(|| body.get("error")).and_then(Value::as_str).unwrap_or("no reason given");
                return Err(format!("the authorization server would not register Astrum ({status}): {why}"));
            };
            (id.to_string(), body.get("client_secret").and_then(Value::as_str).map(str::to_string))
        }
    };

    let verifier = random(48);
    let state = random(18);
    let scope = scope_for(www_authenticate, scope, &endpoints.scopes);
    let mut go = Url::parse(&endpoints.authorize).map_err(|e| format!("its sign-in address is not one: {e}"))?;
    {
        let mut q = go.query_pairs_mut();
        q.append_pair("response_type", "code").append_pair("client_id", &client_id).append_pair("redirect_uri", &redirect);
        q.append_pair("code_challenge", &challenge(&verifier)).append_pair("code_challenge_method", "S256").append_pair("state", &state);
        q.append_pair("resource", server);
        if !scope.is_empty() {
            q.append_pair("scope", &scope);
        }
    }
    let _ = told.send(Progress::Open(go.to_string())).await;

    // The browser comes back here, or what it ended at is pasted in.
    let came_back = async {
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let Ok((mut stream, _)) = accepted else { continue };
                    let mut buf = vec![0u8; 16 * 1024];
                    let n = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut buf)).await.ok().and_then(Result::ok).unwrap_or(0);
                    let request = String::from_utf8_lossy(&buf[..n]).to_string();
                    let target = request.lines().next().and_then(|l| l.split_whitespace().nth(1)).unwrap_or("").to_string();
                    let Ok(at) = Url::parse(&format!("http://127.0.0.1{target}")) else { continue };
                    if at.path() != "/callback" {
                        let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                        continue;
                    }
                    let answer = returned(&at, &state);
                    let page = match &answer {
                        Ok(_) => "<!doctype html><meta charset=utf-8><title>Astrum</title><p>Astrum is signed in. You can close this tab.</p>".to_string(),
                        Err(why) => format!("<!doctype html><meta charset=utf-8><title>Astrum</title><p>Astrum was not signed in: {}</p>", why.replace('<', "&lt;")),
                    };
                    let reply = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}", page.len());
                    let _ = stream.write_all(reply.as_bytes()).await;
                    let _ = stream.shutdown().await;
                    return answer;
                }
                Some(text) = pasted.recv() => {
                    match Url::parse(text.trim()) {
                        Ok(at) => return returned(&at, &state),
                        Err(_) => return Err("what was pasted is not an address".into()),
                    }
                }
            }
        }
    };
    let code = tokio::time::timeout(SIGN_IN_WAIT, came_back).await.map_err(|_| "the browser did not come back within five minutes".to_string())??;
    let secret = client_secret.as_deref().unwrap_or("");
    let body = token_request(
        http,
        &endpoints.token,
        &[("grant_type", "authorization_code"), ("code", &code), ("redirect_uri", &redirect), ("client_id", &client_id), ("code_verifier", &verifier), ("client_secret", secret), ("resource", server)],
    )
    .await?;
    Ok(grant_from(&body, None, &endpoints.token, &client_id, client_secret.as_deref(), server))
}

/// The code from the address the browser came back to, if it is the
/// answer to this sign-in.
fn returned(at: &Url, state: &str) -> Result<String, String> {
    let get = |k: &str| at.query_pairs().find(|(key, _)| key == k).map(|(_, v)| v.into_owned());
    if let Some(error) = get("error") {
        let why = get("error_description").unwrap_or(error);
        return Err(format!("the sign-in was refused: {why}"));
    }
    if get("state").as_deref() != Some(state) {
        return Err("that is not the answer to this sign-in".into());
    }
    get("code").filter(|c| !c.is_empty()).ok_or_else(|| "the address has no code in it".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_a_server_asks_for_is_read_from_its_header() {
        let h = r#"Bearer error="invalid_token", resource_metadata="https://mcp.example.com/.well-known/oauth-protected-resource/mcp", scope="files:read files:write""#;
        assert_eq!(asked(h, "resource_metadata").as_deref(), Some("https://mcp.example.com/.well-known/oauth-protected-resource/mcp"));
        assert_eq!(asked(h, "scope").as_deref(), Some("files:read files:write"));
        assert_eq!(asked("Bearer realm=x, scope=one", "scope").as_deref(), Some("one"));
        assert_eq!(asked("Bearer myscope=\"x\"", "scope"), None);
        assert_eq!(asked("Bearer", "scope"), None);
    }

    #[test]
    fn where_well_known_documents_are() {
        let shown = |u: &str, name: &str| well_known(&Url::parse(u).unwrap(), name).iter().map(|u| u.to_string()).collect::<Vec<_>>();
        assert_eq!(shown("https://a.example/mcp", "oauth-protected-resource"), ["https://a.example/.well-known/oauth-protected-resource/mcp", "https://a.example/.well-known/oauth-protected-resource"]);
        assert_eq!(shown("https://auth.example/tenant1", "openid-configuration"), [
            "https://auth.example/.well-known/openid-configuration/tenant1",
            "https://auth.example/tenant1/.well-known/openid-configuration",
            "https://auth.example/.well-known/openid-configuration"
        ]);
        assert_eq!(shown("https://auth.example/", "oauth-authorization-server"), ["https://auth.example/.well-known/oauth-authorization-server"]);
    }

    #[test]
    fn pkce_as_the_standard_has_it() {
        // RFC 7636, appendix B.
        assert_eq!(challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        let v = random(48);
        assert!(v.len() >= 43 && v.len() <= 128 && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'), "{v}");
        assert_ne!(random(18), random(18));
    }

    #[test]
    fn only_the_answer_to_this_sign_in_is_taken() {
        let at = |q: &str| Url::parse(&format!("http://127.0.0.1:5000/callback?{q}")).unwrap();
        assert_eq!(returned(&at("code=abc&state=s1"), "s1"), Ok("abc".into()));
        assert!(returned(&at("code=abc&state=other"), "s1").unwrap_err().contains("not the answer"));
        assert!(returned(&at("code=abc"), "s1").is_err());
        assert!(returned(&at("error=access_denied&error_description=The+user+said+no&state=s1"), "s1").unwrap_err().contains("The user said no"));
    }

    #[test]
    fn a_grant_is_kept_as_one_line_and_told_from_a_plain_token() {
        let g = Grant { access: "a".into(), refresh: Some("r".into()), expires: Some(now() + 3600), token_endpoint: "https://x/token".into(), client_id: "c".into(), client_secret: None, resource: "https://x/mcp".into() };
        let kept = g.to_stored();
        assert!(!kept.contains('\n'));
        assert_eq!(Grant::from_stored(&kept), Some(g.clone()));
        assert_eq!(Grant::from_stored("ghp_plain_token"), None);
        assert!(!g.stale());
        assert!(Grant { expires: Some(now() + 10), ..g.clone() }.stale());
        assert!(!Grant { expires: None, ..g }.stale());
    }

    #[test]
    fn the_scope_asked_for() {
        assert_eq!(scope_for(Some("Bearer scope=\"a b\""), Some("c"), &["d".into()]), "a b");
        assert_eq!(scope_for(None, Some("c"), &["d".into()]), "c");
        assert_eq!(scope_for(None, None, &["d".into(), "e".into()]), "d e");
        assert_eq!(scope_for(None, None, &[]), "");
    }
}
