//! Amazon Bedrock through the Converse API, which presents every Bedrock
//! model (Anthropic, Amazon, Meta, Mistral, ...) behind one request shape
//! with streaming and tool use.

use super::aws::{self, Auth, EventStream, SignInput};
use super::llm::{Request, STREAM_IDLE_TIMEOUT, error_detail};
use super::{Completion, Msg, Stop, StreamEv, ToolCall, Usage};
use crate::config::{ProviderConfig, ProviderKind};
use anyhow::{Context, Result, anyhow, bail};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::time::{Duration, SystemTime};

const SERVICE: &str = "bedrock";

/// Split `https://host/base` into (host, base path without trailing slash).
fn split_url(url: &str) -> (String, String) {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    match rest.split_once('/') {
        Some((host, path)) => (host.to_string(), format!("/{}", path.trim_end_matches('/')).trim_end_matches('/').to_string()),
        None => (rest.to_string(), String::new()),
    }
}

fn runtime_base(provider: &ProviderConfig, region: &str) -> String {
    provider
        .base_url
        .clone()
        .map(|u| u.trim_end_matches('/').to_string())
        .unwrap_or_else(|| format!("https://bedrock-runtime.{region}.amazonaws.com"))
}

/// Build an authenticated request for either Bedrock endpoint.
async fn request(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    method: reqwest::Method,
    base: &str,
    path: &str,
    query: &[(&str, &str)],
    body: Option<Vec<u8>>,
) -> Result<reqwest::RequestBuilder> {
    let region = aws::region(provider);
    let (host, base_path) = split_url(base);
    let full_path = format!("{base_path}{path}");
    let mut url = format!("{}{}", base.trim_end_matches('/'), path);
    if !query.is_empty() {
        let q: Vec<String> = query.iter().map(|(k, v)| format!("{}={}", aws::uri_encode(k), aws::uri_encode(v))).collect();
        url.push('?');
        url.push_str(&q.join("&"));
    }
    let mut rb = client.request(method.clone(), url);
    let payload = body.unwrap_or_default();
    let has_body = !payload.is_empty();
    if has_body {
        rb = rb.header("content-type", "application/json");
    }
    match aws::resolve_auth(provider).await? {
        Auth::Bearer(key) => rb = rb.bearer_auth(key),
        Auth::SigV4(creds) => {
            let signed_headers: &[(&str, &str)] = if has_body { &[("content-type", "application/json")] } else { &[] };
            let input = SignInput {
                method: method.as_str(),
                host: &host,
                path: &full_path,
                query,
                headers: signed_headers,
                body: &payload,
                region: &region,
                service: SERVICE,
            };
            for (name, value) in aws::sign(&creds, &input, SystemTime::now()) {
                rb = rb.header(name, value);
            }
        }
    }
    if has_body {
        rb = rb.body(payload);
    }
    Ok(rb)
}

async fn check(resp: reqwest::Response) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let kind = resp.headers().get("x-amzn-errortype").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let body = resp.text().await.unwrap_or_default();
    let detail = error_detail(&body);
    if status.as_u16() == 403 {
        // Expired or revoked temporary keys: look them up afresh next time.
        aws::forget_cached();
    }
    let hint = match status.as_u16() {
        403 if detail.to_lowercase().contains("expired") => " (credentials expired; sign in again, e.g. `aws sso login`)",
        403 => " (check that these AWS credentials may use Bedrock and that the model is enabled in this region)",
        404 => " (check the model id and region)",
        429 => " (throttled; try again shortly)",
        _ => "",
    };
    let kind = kind.split(':').next().unwrap_or("").to_string();
    bail!("Bedrock HTTP {}{hint}: {}{}", status.as_u16(), if kind.is_empty() { String::new() } else { format!("{kind}: ") }, detail.trim())
}

// ---- model listing ------------------------------------------------------------

pub async fn list_models(client: &reqwest::Client, provider: &ProviderConfig) -> Result<Vec<String>> {
    if !aws::has_any_credentials(provider) {
        return Ok(Vec::new());
    }
    let region = aws::region(provider);
    let base = format!("https://bedrock.{region}.amazonaws.com");
    let get = |path: &'static str, query: &'static [(&'static str, &'static str)]| {
        let base = base.clone();
        async move {
            let rb = request(client, provider, reqwest::Method::GET, &base, path, query, None).await?;
            let resp = check(rb.timeout(Duration::from_secs(12)).send().await?).await?;
            Ok::<Value, anyhow::Error>(resp.json().await?)
        }
    };
    let (profiles, models) = tokio::join!(
        get("/inference-profiles", &[("maxResults", "1000"), ("type", "SYSTEM_DEFINED")]),
        get("/foundation-models", &[("byOutputModality", "TEXT")]),
    );
    let mut out: Vec<String> = Vec::new();
    // Cross-region inference profiles first: newer models are only callable through them.
    if let Ok(v) = &profiles {
        for p in v["inferenceProfileSummaries"].as_array().into_iter().flatten() {
            if p["status"].as_str().is_none_or(|s| s == "ACTIVE") {
                if let Some(id) = p["inferenceProfileId"].as_str() {
                    out.push(id.to_string());
                }
            }
        }
    }
    if let Ok(v) = &models {
        for m in v["modelSummaries"].as_array().into_iter().flatten() {
            let on_demand = m["inferenceTypesSupported"].as_array().is_some_and(|a| a.iter().any(|t| t == "ON_DEMAND"));
            let streams = m["responseStreamingSupported"].as_bool().unwrap_or(false);
            let active = m["modelLifecycle"]["status"].as_str().is_none_or(|s| s == "ACTIVE");
            if let Some(id) = m["modelId"].as_str().filter(|_| on_demand && streams && active) {
                if !out.iter().any(|o| o == id) {
                    out.push(id.to_string());
                }
            }
        }
    }
    out.sort_by(|a, b| sort_key(a).cmp(&sort_key(b)));
    match (profiles, models) {
        (Err(e), Err(_)) => Err(e),
        _ => Ok(out),
    }
}

/// Orders the picker by vendor and model name, so the regional variants of
/// one model (`us.…`, `global.…`, and the plain id) sit next to each other.
/// Bedrock returns them in no useful order.
fn sort_key(id: &str) -> (&str, &str) {
    const GEOGRAPHIES: [&str; 8] = ["us", "eu", "apac", "global", "us-gov", "jp", "au", "ca"];
    match id.split_once('.') {
        Some((prefix, rest)) if GEOGRAPHIES.contains(&prefix) => (rest, prefix),
        _ => (id, ""),
    }
}

// ---- conversation -------------------------------------------------------------

fn to_messages(messages: &[Msg]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    // Converse requires roles to alternate, so consecutive blocks for the
    // same role (several tool results, or a cancelled turn followed by a new
    // prompt) are merged into one message.
    let mut push = |role: &str, blocks: Vec<Value>| {
        if blocks.is_empty() {
            return;
        }
        match out.last_mut() {
            Some(last) if last["role"] == role => last["content"].as_array_mut().unwrap().extend(blocks),
            _ => out.push(json!({"role": role, "content": blocks})),
        }
    };
    for m in messages {
        match m {
            Msg::User(text) => push("user", vec![json!({"text": text})]),
            Msg::Assistant { text, calls, native } => {
                let blocks = match native {
                    // Replayed unchanged: reasoning blocks carry a signature
                    // that the service checks on later requests.
                    Some((ProviderKind::Bedrock, Value::Array(blocks))) => blocks.clone(),
                    _ => {
                        let mut blocks = Vec::new();
                        if !text.trim().is_empty() {
                            blocks.push(json!({"text": text}));
                        }
                        for c in calls {
                            blocks.push(json!({"toolUse": {"toolUseId": c.id, "name": c.name, "input": c.args}}));
                        }
                        blocks
                    }
                };
                push("assistant", blocks);
            }
            Msg::ToolResult { id, content, is_error, .. } => {
                let text = if content.is_empty() { "(no output)" } else { content.as_str() };
                push(
                    "user",
                    vec![json!({"toolResult": {
                        "toolUseId": id,
                        "content": [{"text": text}],
                        "status": if *is_error { "error" } else { "success" }
                    }})],
                );
            }
        }
    }
    out
}

fn request_body(req: &Request<'_>) -> Value {
    let mut body = json!({
        "messages": to_messages(req.messages),
        "system": [{"text": req.system}],
    });
    if !req.tools.is_empty() {
        body["toolConfig"] = json!({
            "tools": req.tools.iter().map(|t| json!({
                "toolSpec": {"name": t.name, "description": t.description, "inputSchema": {"json": t.schema}}
            })).collect::<Vec<_>>()
        });
    }
    // Output limits differ a lot between Bedrock models; unless the user set
    // one, each model's own default applies.
    if let Some(max) = req.max_tokens {
        body["inferenceConfig"] = json!({"maxTokens": max});
    }
    body
}

/// Folds stream events into the final message.
#[derive(Default)]
struct Assembler {
    blocks: Vec<Value>,
    partial: Vec<String>,
    text: String,
    usage: Usage,
    stop: Option<String>,
}

impl Assembler {
    fn block(&mut self, idx: usize) -> &mut Value {
        while self.blocks.len() <= idx {
            self.blocks.push(Value::Null);
            self.partial.push(String::new());
        }
        &mut self.blocks[idx]
    }

    fn event(&mut self, kind: &str, v: &Value, on: &mut (dyn FnMut(StreamEv) + Send)) {
        let idx = v["contentBlockIndex"].as_u64().unwrap_or(0) as usize;
        match kind {
            "contentBlockStart" => {
                if let Some(tool) = v["start"].get("toolUse") {
                    *self.block(idx) = json!({"toolUse": {"toolUseId": tool["toolUseId"], "name": tool["name"], "input": {}}});
                }
            }
            "contentBlockDelta" => {
                let delta = &v["delta"];
                if let Some(piece) = delta["text"].as_str() {
                    // Text blocks have no start event; the first delta opens them.
                    let block = self.block(idx);
                    if block.is_null() {
                        *block = json!({"text": ""});
                    }
                    let cur = block["text"].as_str().unwrap_or("").to_string();
                    block["text"] = json!(cur + piece);
                    self.text.push_str(piece);
                    on(StreamEv::Text(piece.to_string()));
                } else if let Some(piece) = delta["toolUse"]["input"].as_str() {
                    self.block(idx);
                    self.partial[idx].push_str(piece);
                } else if let Some(r) = delta.get("reasoningContent") {
                    let block = self.block(idx);
                    if let Some(data) = r["redactedContent"].as_str() {
                        *block = json!({"reasoningContent": {"redactedContent": data}});
                        return;
                    }
                    if block.is_null() {
                        *block = json!({"reasoningContent": {"reasoningText": {"text": ""}}});
                    }
                    let rt = &mut block["reasoningContent"]["reasoningText"];
                    if let Some(piece) = r["text"].as_str() {
                        let cur = rt["text"].as_str().unwrap_or("").to_string();
                        rt["text"] = json!(cur + piece);
                        if !piece.is_empty() {
                            on(StreamEv::Thinking(piece.to_string()));
                        }
                    }
                    if let Some(sig) = r["signature"].as_str() {
                        let cur = rt["signature"].as_str().unwrap_or("").to_string();
                        rt["signature"] = json!(cur + sig);
                    }
                }
            }
            "contentBlockStop" => {
                self.block(idx);
                let raw = std::mem::take(&mut self.partial[idx]);
                if let Some(tool) = self.blocks[idx].get_mut("toolUse") {
                    tool["input"] = if raw.trim().is_empty() {
                        json!({})
                    } else {
                        serde_json::from_str(&raw).unwrap_or_else(|_| json!({"__invalid_json": raw}))
                    };
                }
            }
            "messageStop" => self.stop = v["stopReason"].as_str().map(str::to_string),
            "metadata" => {
                self.usage.input = v["usage"]["inputTokens"].as_u64().unwrap_or(0)
                    + v["usage"]["cacheReadInputTokens"].as_u64().unwrap_or(0)
                    + v["usage"]["cacheWriteInputTokens"].as_u64().unwrap_or(0);
                self.usage.output = v["usage"]["outputTokens"].as_u64().unwrap_or(0);
            }
            _ => {}
        }
    }

    fn finish(mut self) -> Completion {
        self.blocks.retain(|b| !b.is_null());
        let calls: Vec<ToolCall> = self
            .blocks
            .iter()
            .filter_map(|b| b.get("toolUse"))
            .map(|t| ToolCall {
                id: t["toolUseId"].as_str().unwrap_or_default().to_string(),
                name: t["name"].as_str().unwrap_or_default().to_string(),
                args: t["input"].clone(),
            })
            .collect();
        let stop = match self.stop.as_deref() {
            Some("tool_use") => Stop::ToolUse,
            Some("max_tokens") => Stop::MaxTokens,
            Some(r @ ("guardrail_intervened" | "content_filtered")) => Stop::Refusal(Some(r.to_string())),
            Some("end_turn") | Some("stop_sequence") | None => {
                if calls.is_empty() {
                    Stop::End
                } else {
                    Stop::ToolUse
                }
            }
            Some(other) => Stop::Other(other.to_string()),
        };
        Completion { text: self.text, calls, native: Some((ProviderKind::Bedrock, Value::Array(self.blocks))), stop, usage: self.usage }
    }
}

pub async fn complete(client: &reqwest::Client, req: &Request<'_>, on: &mut (dyn FnMut(StreamEv) + Send)) -> Result<Completion> {
    let region = aws::region(req.provider);
    let base = runtime_base(req.provider, &region);
    let path = format!("/model/{}/converse-stream", aws::uri_encode(req.model));
    let body = serde_json::to_vec(&request_body(req))?;
    let rb = request(client, req.provider, reqwest::Method::POST, &base, &path, &[], Some(body)).await?;
    let resp = check(rb.send().await.with_context(|| format!("could not reach {base}"))?).await?;

    let mut stream = resp.bytes_stream();
    let mut decoder = EventStream::default();
    let mut asm = Assembler::default();
    loop {
        let next = tokio::time::timeout(STREAM_IDLE_TIMEOUT, stream.next())
            .await
            .map_err(|_| anyhow!("the model stopped responding (no data for 5 minutes)"))?;
        let Some(chunk) = next else { break };
        for frame in decoder.push(&chunk.context("connection lost while streaming")?)? {
            let payload: Value = serde_json::from_slice(&frame.payload).unwrap_or(Value::Null);
            match frame.header(":message-type") {
                "event" => asm.event(frame.header(":event-type"), &payload, on),
                "exception" | "error" => {
                    let name = frame.header(":exception-type");
                    let name = if name.is_empty() { frame.header(":error-code") } else { name };
                    let msg = payload["message"].as_str().or_else(|| payload["Message"].as_str()).unwrap_or("the service reported an error");
                    bail!("Bedrock {name}: {msg}");
                }
                _ => {}
            }
        }
    }
    Ok(asm.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::aws::tests::encode_frame;

    #[test]
    fn regional_variants_of_a_model_sort_together() {
        let mut ids = vec![
            "us.meta.llama3-3-70b-instruct-v1:0",
            "global.anthropic.claude-haiku-5-5",
            "us.anthropic.claude-3-haiku-20240307-v1:0",
            "anthropic.claude-haiku-5-5",
            "us.anthropic.claude-haiku-5-5",
        ];
        ids.sort_by(|a, b| sort_key(a).cmp(&sort_key(b)));
        assert_eq!(
            ids,
            [
                "us.anthropic.claude-3-haiku-20240307-v1:0",
                "anthropic.claude-haiku-5-5",
                "global.anthropic.claude-haiku-5-5",
                "us.anthropic.claude-haiku-5-5",
                "us.meta.llama3-3-70b-instruct-v1:0",
            ]
        );
    }

    fn run(events: &[(&str, Value)]) -> (Completion, String) {
        let mut decoder = EventStream::default();
        let mut asm = Assembler::default();
        let mut streamed = String::new();
        let mut on = |ev: StreamEv| {
            if let StreamEv::Text(t) = ev {
                streamed.push_str(&t);
            }
        };
        let mut wire = Vec::new();
        for (kind, payload) in events {
            wire.extend(encode_frame(&[(":message-type", "event"), (":event-type", kind)], payload.to_string().as_bytes()));
        }
        // Deliver in awkward pieces, as a network would.
        for chunk in wire.chunks(11) {
            for frame in decoder.push(chunk).unwrap() {
                let v: Value = serde_json::from_slice(&frame.payload).unwrap();
                asm.event(frame.header(":event-type"), &v, &mut on);
            }
        }
        (asm.finish(), streamed)
    }

    #[test]
    fn text_then_tool_use() {
        let (c, streamed) = run(&[
            ("messageStart", json!({"role": "assistant"})),
            ("contentBlockDelta", json!({"contentBlockIndex": 0, "delta": {"text": "Let me "}})),
            ("contentBlockDelta", json!({"contentBlockIndex": 0, "delta": {"text": "check."}})),
            ("contentBlockStop", json!({"contentBlockIndex": 0})),
            ("contentBlockStart", json!({"contentBlockIndex": 1, "start": {"toolUse": {"toolUseId": "tu_1", "name": "run_command"}}})),
            ("contentBlockDelta", json!({"contentBlockIndex": 1, "delta": {"toolUse": {"input": "{\"command\":"}}})),
            ("contentBlockDelta", json!({"contentBlockIndex": 1, "delta": {"toolUse": {"input": "\"df -h\"}"}}})),
            ("contentBlockStop", json!({"contentBlockIndex": 1})),
            ("messageStop", json!({"stopReason": "tool_use"})),
            ("metadata", json!({"usage": {"inputTokens": 120, "outputTokens": 30, "totalTokens": 150}})),
        ]);
        assert_eq!(streamed, "Let me check.");
        assert_eq!(c.text, "Let me check.");
        assert_eq!(c.stop, Stop::ToolUse);
        assert_eq!(c.calls.len(), 1);
        assert_eq!(c.calls[0].id, "tu_1");
        assert_eq!(c.calls[0].args, json!({"command": "df -h"}));
        assert_eq!((c.usage.input, c.usage.output), (120, 30));
    }

    #[test]
    fn reasoning_round_trips_with_its_signature() {
        let (c, _) = run(&[
            ("contentBlockDelta", json!({"contentBlockIndex": 0, "delta": {"reasoningContent": {"text": "Thinking "}}})),
            ("contentBlockDelta", json!({"contentBlockIndex": 0, "delta": {"reasoningContent": {"text": "hard."}}})),
            ("contentBlockDelta", json!({"contentBlockIndex": 0, "delta": {"reasoningContent": {"signature": "SIG"}}})),
            ("contentBlockStop", json!({"contentBlockIndex": 0})),
            ("contentBlockDelta", json!({"contentBlockIndex": 1, "delta": {"text": "Done."}})),
            ("contentBlockStop", json!({"contentBlockIndex": 1})),
            ("messageStop", json!({"stopReason": "end_turn"})),
        ]);
        assert_eq!(c.stop, Stop::End);
        let msg = Msg::Assistant { text: c.text.clone(), calls: c.calls.clone(), native: c.native.clone() };
        let sent = to_messages(&[Msg::User("hi".into()), msg]);
        assert_eq!(
            sent[1]["content"][0],
            json!({"reasoningContent": {"reasoningText": {"text": "Thinking hard.", "signature": "SIG"}}})
        );
        assert_eq!(sent[1]["content"][1], json!({"text": "Done."}));
    }

    #[test]
    fn roles_always_alternate() {
        let call = |id: &str| ToolCall { id: id.into(), name: "run_command".into(), args: json!({"command": "ls"}) };
        let msgs = vec![
            Msg::User("first".into()),
            Msg::Assistant { text: String::new(), calls: vec![call("a"), call("b")], native: None },
            Msg::ToolResult { id: "a".into(), name: "run_command".into(), content: "ok".into(), is_error: false },
            Msg::ToolResult { id: "b".into(), name: "run_command".into(), content: String::new(), is_error: true },
            // A cancelled turn is followed directly by the next prompt.
            Msg::User("second".into()),
        ];
        let sent = to_messages(&msgs);
        let roles: Vec<&str> = sent.iter().map(|m| m["role"].as_str().unwrap()).collect();
        assert_eq!(roles, vec!["user", "assistant", "user"]);
        assert_eq!(sent[1]["content"][0]["toolUse"]["toolUseId"], "a");
        let last = sent[2]["content"].as_array().unwrap();
        assert_eq!(last.len(), 3);
        assert_eq!(last[0]["toolResult"]["status"], "success");
        assert_eq!(last[1]["toolResult"]["status"], "error");
        assert_eq!(last[1]["toolResult"]["content"][0]["text"], "(no output)");
        assert_eq!(last[2]["text"], "second");
    }

    #[test]
    fn urls() {
        assert_eq!(split_url("https://bedrock-runtime.us-east-1.amazonaws.com"), ("bedrock-runtime.us-east-1.amazonaws.com".into(), String::new()));
        assert_eq!(split_url("https://proxy.example.com/aws/"), ("proxy.example.com".into(), "/aws".into()));
    }
}
