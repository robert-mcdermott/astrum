//! The AWS plumbing Bedrock needs, written out by hand so no AWS SDK is
//! linked in: finding credentials, Signature Version 4 request signing, and
//! decoding the binary event-stream framing of streaming responses.

use crate::config::ProviderConfig;
use anyhow::{Context, Result, anyhow, bail};
use aws_lc_rs::{digest, hmac};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone)]
pub struct Credentials {
    pub access_key: String,
    pub secret_key: String,
    pub token: Option<String>,
}

#[derive(Clone)]
pub enum Auth {
    /// A Bedrock API key, sent as a bearer token.
    Bearer(String),
    SigV4(Credentials),
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn aws_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".aws")
}

fn credentials_file() -> PathBuf {
    env("AWS_SHARED_CREDENTIALS_FILE").map(PathBuf::from).unwrap_or_else(|| aws_dir().join("credentials"))
}

fn config_file() -> PathBuf {
    env("AWS_CONFIG_FILE").map(PathBuf::from).unwrap_or_else(|| aws_dir().join("config"))
}

/// Parse an AWS ini file into section -> key -> value.
fn parse_ini(text: &str) -> HashMap<String, HashMap<String, String>> {
    let mut out: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut section = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.trim().to_string();
            out.entry(section.clone()).or_default();
        } else if let Some((k, v)) = line.split_once('=') {
            out.entry(section.clone()).or_default().insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    out
}

fn profile_name(provider: &ProviderConfig) -> String {
    provider.profile.clone().or_else(|| env("AWS_PROFILE")).unwrap_or_else(|| "default".into())
}

/// The profile's section in `~/.aws/config` (named `[profile x]` there,
/// except for `[default]`).
fn config_section(profile: &str) -> HashMap<String, String> {
    let ini = std::fs::read_to_string(config_file()).map(|t| parse_ini(&t)).unwrap_or_default();
    ini.get(&format!("profile {profile}")).or_else(|| ini.get(profile)).cloned().unwrap_or_default()
}

pub fn region(provider: &ProviderConfig) -> String {
    provider
        .region
        .clone()
        .or_else(|| env("AWS_REGION"))
        .or_else(|| env("AWS_DEFAULT_REGION"))
        .or_else(|| config_section(&profile_name(provider)).get("region").cloned())
        .unwrap_or_else(|| "us-east-1".into())
}

/// Cheap check used to keep Bedrock out of the model picker for people who
/// have no AWS setup at all.
pub fn has_any_credentials(provider: &ProviderConfig) -> bool {
    provider.resolved_key().is_some()
        || (env("AWS_ACCESS_KEY_ID").is_some() && env("AWS_SECRET_ACCESS_KEY").is_some())
        || credentials_file().exists()
        || config_file().exists()
}

fn from_process_json(text: &str) -> Result<Credentials> {
    let v: Value = serde_json::from_str(text.trim()).context("credential helper did not print JSON")?;
    let get = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    Ok(Credentials {
        access_key: get("AccessKeyId").ok_or_else(|| anyhow!("credential helper output has no AccessKeyId"))?,
        secret_key: get("SecretAccessKey").ok_or_else(|| anyhow!("credential helper output has no SecretAccessKey"))?,
        token: get("SessionToken").filter(|t| !t.is_empty()),
    })
}

async fn run_helper(program: &str, args: &[&str]) -> Result<Credentials> {
    let fut = tokio::process::Command::new(program).args(args).stdin(std::process::Stdio::null()).kill_on_drop(true).output();
    let out = tokio::time::timeout(Duration::from_secs(30), fut)
        .await
        .map_err(|_| anyhow!("`{program}` did not answer within 30s"))?
        .with_context(|| format!("could not run `{program}`"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("{}", err.lines().find(|l| !l.trim().is_empty()).unwrap_or("credential helper failed").trim().to_string());
    }
    from_process_json(&String::from_utf8_lossy(&out.stdout))
}

static CACHE: Mutex<Option<(String, Instant, Auth)>> = Mutex::new(None);
const CACHE_TTL: Duration = Duration::from_secs(600);

/// Find credentials the way AWS tools do: an API key, then environment keys,
/// then the profile (static keys, a `credential_process`, or single sign-on
/// and assumed roles through the AWS CLI when it is installed).
pub async fn resolve_auth(provider: &ProviderConfig) -> Result<Auth> {
    if let Some(key) = provider.resolved_key() {
        return Ok(Auth::Bearer(key));
    }
    if provider.profile.is_none() {
        if let (Some(access_key), Some(secret_key)) = (env("AWS_ACCESS_KEY_ID"), env("AWS_SECRET_ACCESS_KEY")) {
            return Ok(Auth::SigV4(Credentials { access_key, secret_key, token: env("AWS_SESSION_TOKEN") }));
        }
    }
    let profile = profile_name(provider);
    if let Some((name, at, auth)) = CACHE.lock().unwrap().as_ref() {
        if *name == profile && at.elapsed() < CACHE_TTL {
            return Ok(auth.clone());
        }
    }

    let shared = std::fs::read_to_string(credentials_file()).map(|t| parse_ini(&t)).unwrap_or_default();
    let creds = if let Some(c) = shared.get(&profile).and_then(|s| {
        Some(Credentials {
            access_key: s.get("aws_access_key_id")?.clone(),
            secret_key: s.get("aws_secret_access_key")?.clone(),
            token: s.get("aws_session_token").cloned().filter(|t| !t.is_empty()),
        })
    }) {
        c
    } else {
        let section = config_section(&profile);
        if let Some(cmd) = section.get("credential_process") {
            run_helper("/bin/sh", &["-c", cmd]).await.with_context(|| format!("credential_process for AWS profile `{profile}`"))?
        } else if section.keys().any(|k| k.starts_with("sso_") || k == "role_arn" || k == "source_profile") {
            // Single sign-on and assumed roles need token exchange; the AWS
            // CLI already knows how, and hands back temporary keys.
            run_helper("aws", &["configure", "export-credentials", "--profile", &profile, "--format", "process"])
                .await
                .with_context(|| {
                    format!("getting credentials for AWS profile `{profile}` through the AWS CLI (try `aws sso login --profile {profile}`)")
                })?
        } else {
            bail!(
                "no AWS credentials found for profile `{profile}`. Set AWS_BEARER_TOKEN_BEDROCK (a Bedrock API key), or AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY, or configure the profile in ~/.aws"
            );
        }
    };
    let auth = Auth::SigV4(creds);
    *CACHE.lock().unwrap() = Some((profile, Instant::now(), auth.clone()));
    Ok(auth)
}

/// Forget cached credentials (after the service rejects them).
pub fn forget_cached() {
    *CACHE.lock().unwrap() = None;
}

// ---- Signature Version 4 ------------------------------------------------------

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_hex(data: &[u8]) -> String {
    hex(digest::digest(&digest::SHA256, data).as_ref())
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), data).as_ref().to_vec()
}

/// Percent-encode everything except the RFC 3986 unreserved characters.
pub fn uri_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn signing_key(secret: &str, date: &str, region: &str, service: &str) -> Vec<u8> {
    let k = hmac_sha256(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let k = hmac_sha256(&k, region.as_bytes());
    let k = hmac_sha256(&k, service.as_bytes());
    hmac_sha256(&k, b"aws4_request")
}

/// `20150830T123600Z` for the given time.
fn amz_date(now: SystemTime) -> String {
    let secs = now.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as libc::time_t;
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::gmtime_r(&secs, &mut tm);
        format!(
            "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }
}

pub struct SignInput<'a> {
    pub method: &'a str,
    pub host: &'a str,
    /// The path exactly as it will be sent (already percent-encoded).
    pub path: &'a str,
    pub query: &'a [(&'a str, &'a str)],
    /// Extra headers to sign, lowercase names (e.g. content-type).
    pub headers: &'a [(&'a str, &'a str)],
    pub body: &'a [u8],
    pub region: &'a str,
    pub service: &'a str,
}

/// Returns the headers to add to the request: `x-amz-date`,
/// `authorization`, and `x-amz-security-token` for temporary credentials.
pub fn sign(creds: &Credentials, input: &SignInput, now: SystemTime) -> Vec<(String, String)> {
    let stamp = amz_date(now);
    let date = &stamp[..8];

    let mut headers: Vec<(String, String)> = input.headers.iter().map(|(k, v)| (k.to_lowercase(), v.trim().to_string())).collect();
    headers.push(("host".into(), input.host.to_string()));
    headers.push(("x-amz-date".into(), stamp.clone()));
    if let Some(token) = &creds.token {
        headers.push(("x-amz-security-token".into(), token.clone()));
    }
    headers.sort();
    let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed_headers = headers.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(";");

    // Every service except S3 encodes the path a second time when signing.
    let canonical_path = if input.path.is_empty() {
        "/".to_string()
    } else {
        input.path.split('/').map(uri_encode).collect::<Vec<_>>().join("/")
    };
    let mut query: Vec<(String, String)> = input.query.iter().map(|(k, v)| (uri_encode(k), uri_encode(v))).collect();
    query.sort();
    let canonical_query = query.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&");

    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        input.method,
        canonical_path,
        canonical_query,
        canonical_headers,
        signed_headers,
        sha256_hex(input.body)
    );
    let scope = format!("{date}/{}/{}/aws4_request", input.region, input.service);
    let to_sign = format!("AWS4-HMAC-SHA256\n{stamp}\n{scope}\n{}", sha256_hex(canonical_request.as_bytes()));
    let signature = hex(&hmac_sha256(&signing_key(&creds.secret_key, date, input.region, input.service), to_sign.as_bytes()));

    let mut out = vec![
        ("x-amz-date".to_string(), stamp),
        (
            "authorization".to_string(),
            format!(
                "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
                creds.access_key
            ),
        ),
    ];
    if let Some(token) = &creds.token {
        out.push(("x-amz-security-token".to_string(), token.clone()));
    }
    out
}

// ---- event-stream decoding ----------------------------------------------------

/// One message of an `application/vnd.amazon.eventstream` response.
#[derive(Debug, PartialEq)]
pub struct Frame {
    pub headers: HashMap<String, String>,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn header(&self, name: &str) -> &str {
        self.headers.get(name).map(String::as_str).unwrap_or("")
    }
}

const MAX_FRAME: usize = 24 * 1024 * 1024;

/// Incremental decoder: feed it bytes as they arrive, get whole frames back.
#[derive(Default)]
pub struct EventStream {
    buf: Vec<u8>,
}

impl EventStream {
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<Frame>> {
        self.buf.extend_from_slice(chunk);
        let mut frames = Vec::new();
        loop {
            if self.buf.len() < 12 {
                break;
            }
            let be = |i: usize| u32::from_be_bytes([self.buf[i], self.buf[i + 1], self.buf[i + 2], self.buf[i + 3]]) as usize;
            let (total, header_len) = (be(0), be(4));
            if total < 16 || total > MAX_FRAME || header_len > total - 16 {
                bail!("malformed event stream from the service");
            }
            if self.buf.len() < total {
                break;
            }
            let headers = parse_headers(&self.buf[12..12 + header_len])?;
            let payload = self.buf[12 + header_len..total - 4].to_vec();
            frames.push(Frame { headers, payload });
            self.buf.drain(..total);
        }
        Ok(frames)
    }
}

fn parse_headers(mut b: &[u8]) -> Result<HashMap<String, String>> {
    let bad = || anyhow!("malformed event stream header");
    let mut out = HashMap::new();
    while !b.is_empty() {
        let name_len = b[0] as usize;
        if b.len() < 1 + name_len + 1 {
            return Err(bad());
        }
        let name = String::from_utf8_lossy(&b[1..1 + name_len]).into_owned();
        let kind = b[1 + name_len];
        b = &b[2 + name_len..];
        // Value sizes by type: booleans carry none; strings and byte arrays
        // are length-prefixed; the rest are fixed width.
        let fixed = match kind {
            0 | 1 => Some(0),
            2 => Some(1),
            3 => Some(2),
            4 => Some(4),
            5 | 8 => Some(8),
            9 => Some(16),
            6 | 7 => None,
            _ => return Err(bad()),
        };
        match fixed {
            Some(n) => {
                if b.len() < n {
                    return Err(bad());
                }
                b = &b[n..];
            }
            None => {
                if b.len() < 2 {
                    return Err(bad());
                }
                let n = u16::from_be_bytes([b[0], b[1]]) as usize;
                if b.len() < 2 + n {
                    return Err(bad());
                }
                if kind == 7 {
                    out.insert(name, String::from_utf8_lossy(&b[2..2 + n]).into_owned());
                }
                b = &b[2 + n..];
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// Build a frame the way the service does (checksums left as zero; the
    /// decoder does not verify them, TLS already protects the stream).
    pub fn encode_frame(headers: &[(&str, &str)], payload: &[u8]) -> Vec<u8> {
        let mut h = Vec::new();
        for (k, v) in headers {
            h.push(k.len() as u8);
            h.extend_from_slice(k.as_bytes());
            h.push(7);
            h.extend_from_slice(&(v.len() as u16).to_be_bytes());
            h.extend_from_slice(v.as_bytes());
        }
        let total = 12 + h.len() + payload.len() + 4;
        let mut out = Vec::new();
        out.extend_from_slice(&(total as u32).to_be_bytes());
        out.extend_from_slice(&(h.len() as u32).to_be_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&h);
        out.extend_from_slice(payload);
        out.extend_from_slice(&[0; 4]);
        out
    }

    #[test]
    fn signing_key_matches_the_published_example() {
        // The key-derivation example from the AWS Signature Version 4 documentation.
        let key = signing_key("wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY", "20120215", "us-east-1", "iam");
        assert_eq!(hex(&key), "f4780e2d9f65fa895f9c67b32ce1baf0b0d8a43505a000a1a9e090d414db404d");
    }

    #[test]
    fn signature_matches_the_reference_test_suite() {
        // "get-vanilla" from the AWS SigV4 test suite: GET / at 2015-08-30 12:36:00 UTC.
        let creds = Credentials {
            access_key: "AKIDEXAMPLE".into(),
            secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            token: None,
        };
        let input = SignInput {
            method: "GET",
            host: "example.amazonaws.com",
            path: "/",
            query: &[],
            headers: &[],
            body: b"",
            region: "us-east-1",
            service: "service",
        };
        let when = UNIX_EPOCH + Duration::from_secs(1_440_938_160);
        let headers = sign(&creds, &input, when);
        assert_eq!(headers[0], ("x-amz-date".to_string(), "20150830T123600Z".to_string()));
        assert_eq!(
            headers[1].1,
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, SignedHeaders=host;x-amz-date, Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
    }

    #[test]
    fn model_ids_are_encoded_twice_in_the_signed_path() {
        assert_eq!(uri_encode("us.anthropic.claude-x-v1:0"), "us.anthropic.claude-x-v1%3A0");
        // Sent as %3A, signed as %253A.
        let sent = format!("/model/{}/converse-stream", uri_encode("a.b-v1:0"));
        let canonical: String = sent.split('/').map(uri_encode).collect::<Vec<_>>().join("/");
        assert_eq!(canonical, "/model/a.b-v1%253A0/converse-stream");
    }

    #[test]
    fn temporary_credentials_sign_and_send_the_token() {
        let creds = Credentials { access_key: "AK".into(), secret_key: "SK".into(), token: Some("TOKEN".into()) };
        let input = SignInput {
            method: "POST",
            host: "bedrock-runtime.us-west-2.amazonaws.com",
            path: "/model/m/converse-stream",
            query: &[],
            headers: &[("content-type", "application/json")],
            body: b"{}",
            region: "us-west-2",
            service: "bedrock",
        };
        let headers = sign(&creds, &input, UNIX_EPOCH + Duration::from_secs(1_700_000_000));
        assert!(headers[1].1.contains("SignedHeaders=content-type;host;x-amz-date;x-amz-security-token"));
        assert!(headers[1].1.contains("/us-west-2/bedrock/aws4_request"));
        assert_eq!(headers[2], ("x-amz-security-token".to_string(), "TOKEN".to_string()));
    }

    #[test]
    fn ini_profiles() {
        let ini = parse_ini("[default]\nregion = us-east-1\n\n# comment\n[profile work]\nregion=eu-west-1\nsso_session = corp\n");
        assert_eq!(ini["default"]["region"], "us-east-1");
        assert_eq!(ini["profile work"]["sso_session"], "corp");
    }

    #[test]
    fn helper_output() {
        let c = from_process_json(r#"{"Version":1,"AccessKeyId":"A","SecretAccessKey":"S","SessionToken":"T","Expiration":"2030-01-01T00:00:00Z"}"#).unwrap();
        assert_eq!((c.access_key.as_str(), c.secret_key.as_str(), c.token.as_deref()), ("A", "S", Some("T")));
        assert!(from_process_json("not json").is_err());
    }

    #[test]
    fn frames_split_across_chunks() {
        let a = encode_frame(&[(":message-type", "event"), (":event-type", "contentBlockDelta")], br#"{"delta":{"text":"hi"}}"#);
        let b = encode_frame(&[(":message-type", "exception"), (":exception-type", "throttlingException")], br#"{"message":"slow down"}"#);
        let mut all = a.clone();
        all.extend_from_slice(&b);
        let mut es = EventStream::default();
        let mut frames = Vec::new();
        for chunk in all.chunks(7) {
            frames.extend(es.push(chunk).unwrap());
        }
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].header(":event-type"), "contentBlockDelta");
        assert_eq!(frames[0].payload, br#"{"delta":{"text":"hi"}}"#);
        assert_eq!(frames[1].header(":exception-type"), "throttlingException");
        assert!(EventStream::default().push(&[0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0]).is_err());
    }
}
