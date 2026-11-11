//! Streaming chat clients for the supported provider kinds.
//!
//! Both speak plain HTTPS + server-sent events through `reqwest`; no vendor
//! SDKs are linked in.

use super::tools::ToolSpec;
use super::{Completion, Msg, Stop, StreamEv, ToolCall, Usage};
use crate::config::{ProviderConfig, ProviderKind};
use anyhow::{Context, Result, anyhow, bail};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::time::Duration;

const ANTHROPIC_VERSION: &str = "2026-10-21";
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
/// Models whose safety classifiers can decline a request; for these we opt in
/// to server-side fallback so a decline is re-run on another model.
const FALLBACK_MODELS: &[&str] =
    &["claude-fable-5-2", "claude-fable-5-1", "claude-opus-5-5", "claude-opus-5", "claude-sonnet-5-5"];
const ANTHROPIC_DEFAULT_MODELS: &[&str] =
    &["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5", "claude-fable-5-2"];
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

pub struct Request<'a> {
    pub provider: &'a ProviderConfig,
    pub model: &'a str,
    pub system: &'a str,
    pub messages: &'a [Msg],
    pub tools: &'a [ToolSpec],
    pub max_tokens: Option<u32>,
}

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .user_agent(concat!("astrum/", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_default()
}

pub async fn complete(
    client: &reqwest::Client,
    req: &Request<'_>,
    on: &mut (dyn FnMut(StreamEv) + Send),
) -> Result<Completion> {
    match req.provider.kind {
        ProviderKind::Openai => openai_complete(client, req, on).await,
        ProviderKind::Anthropic => anthropic_complete(client, req, on).await,
        ProviderKind::Bedrock => bail!("Amazon Bedrock support is not implemented yet (see ROADMAP.md, phase 2)"),
    }
}

/// Model ids the provider offers, for the `/model` picker.
pub async fn list_models(client: &reqwest::Client, provider: &ProviderConfig) -> Result<Vec<String>> {
    let mut models = provider.models.clone();
    let fetched: Result<Vec<String>> = match provider.kind {
        ProviderKind::Openai => {
            let base = base_url(provider, "https://api.openai.com/v1");
            let mut rb = client.get(format!("{base}/models")).timeout(Duration::from_secs(8));
            if let Some(key) = provider.resolved_key() {
                rb = rb.bearer_auth(key);
            }
            ids_from(rb.send().await)
                .await
        }
        ProviderKind::Anthropic => match provider.resolved_key() {
            None => Ok(ANTHROPIC_DEFAULT_MODELS.iter().map(|s| s.to_string()).collect()),
            Some(key) => {
                let base = base_url(provider, "https://api.anthropic.com");
                let rb = client
                    .get(format!("{base}/v1/models?limit=100"))
                    .timeout(Duration::from_secs(8))
                    .header("x-api-key", key)
                    .header("anthropic-version", ANTHROPIC_VERSION);
                ids_from(rb.send().await).await
            }
        },
        ProviderKind::Bedrock => Ok(Vec::new()),
    };
    match fetched {
        Ok(list) => {
            for m in list {
                if !models.contains(&m) {
                    models.push(m);
                }
            }
            Ok(models)
        }
        Err(e) if models.is_empty() => Err(e),
        Err(_) => Ok(models),
    }
}

async fn ids_from(resp: reqwest::Result<reqwest::Response>) -> Result<Vec<String>> {
    let resp = check(resp?).await?;
    let body: Value = resp.json().await?;
    let list = body.get("data").and_then(Value::as_array).cloned().unwrap_or_default();
    Ok(list.iter().filter_map(|m| m.get("id").and_then(Value::as_str).map(str::to_string)).collect())
}

fn base_url(provider: &ProviderConfig, default: &str) -> String {
    provider.base_url.as_deref().unwrap_or(default).trim_end_matches('/').to_string()
}

/// Turn a non-2xx response into a readable error.
async fn check(resp: reqwest::Response) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp.text().await.unwrap_or_default();
    let detail = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|v| {
            let e = v.get("error")?;
            e.get("message").and_then(Value::as_str).or_else(|| e.as_str()).map(str::to_string)
        })
        .unwrap_or_else(|| body.chars().take(400).collect());
    let hint = match status.as_u16() {
        401 | 403 => " (check the provider's API key)",
        404 => " (check the model name and base_url)",
        429 => " (rate limited; try again shortly)",
        _ => "",
    };
    bail!("HTTP {}{hint}: {}", status.as_u16(), detail.trim())
}

/// Incremental server-sent-events decoder: feeds each `data:` payload to `f`.
#[derive(Default)]
struct Sse {
    buf: Vec<u8>,
}

impl Sse {
    fn push(&mut self, chunk: &[u8], f: &mut dyn FnMut(&str) -> Result<()>) -> Result<()> {
        self.buf.extend_from_slice(chunk);
        let mut start = 0;
        while let Some(pos) = self.buf[start..].iter().position(|&b| b == b'\n') {
            let line = &self.buf[start..start + pos];
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if let Some(data) = line.strip_prefix(b"data:") {
                let text = String::from_utf8_lossy(data);
                let text = text.trim();
                if !text.is_empty() {
                    f(text)?;
                }
            }
            start += pos + 1;
        }
        self.buf.drain(..start);
        Ok(())
    }
}

async fn pump(resp: reqwest::Response, f: &mut (dyn FnMut(&str) -> Result<()> + Send)) -> Result<()> {
    let mut stream = resp.bytes_stream();
    let mut sse = Sse::default();
    loop {
        let next = tokio::time::timeout(STREAM_IDLE_TIMEOUT, stream.next())
            .await
            .map_err(|_| anyhow!("the model stopped responding (no data for 5 minutes)"))?;
        match next {
            Some(chunk) => sse.push(&chunk.context("connection lost while streaming")?, f)?,
            None => return Ok(()),
        }
    }
}

// ---- OpenAI-compatible chat completions ---------------------------------

fn openai_messages(system: &str, messages: &[Msg]) -> Vec<Value> {
    let mut out = vec![json!({"role": "system", "content": system})];
    for m in messages {
        match m {
            Msg::User(text) => out.push(json!({"role": "user", "content": text})),
            Msg::Assistant { text, calls, .. } => {
                let mut msg = json!({"role": "assistant", "content": text});
                if !calls.is_empty() {
                    msg["tool_calls"] = calls
                        .iter()
                        .map(|c| {
                            json!({
                                "id": c.id,
                                "type": "function",
                                "function": {"name": c.name, "arguments": c.args.to_string()}
                            })
                        })
                        .collect();
                }
                out.push(msg);
            }
            Msg::ToolResult { id, content, .. } => {
                out.push(json!({"role": "tool", "tool_call_id": id, "content": content}));
            }
        }
    }
    out
}

async fn openai_complete(
    client: &reqwest::Client,
    req: &Request<'_>,
    on: &mut (dyn FnMut(StreamEv) + Send),
) -> Result<Completion> {
    let base = base_url(req.provider, "https://api.openai.com/v1");
    let mut body = json!({
        "model": req.model,
        "messages": openai_messages(req.system, req.messages),
        "stream": true,
        "stream_options": {"include_usage": true},
    });
    if !req.tools.is_empty() {
        body["tools"] = req
            .tools
            .iter()
            .map(|t| json!({"type": "function", "function": {"name": t.name, "description": t.description, "parameters": t.schema}}))
            .collect();
    }
    if let Some(max) = req.max_tokens {
        let key = if base.contains("openai.com") { "max_completion_tokens" } else { "max_tokens" };
        body[key] = json!(max);
    }
    let mut rb = client.post(format!("{base}/chat/completions")).json(&body);
    if let Some(key) = req.provider.resolved_key() {
        rb = rb.bearer_auth(key);
    }
    let resp = check(rb.send().await.with_context(|| format!("could not reach {base}"))?).await?;

    let mut text = String::new();
    let mut usage = Usage::default();
    let mut finish: Option<String> = None;
    // (id, name, argument fragments) by tool-call index.
    let mut calls: Vec<(String, String, String)> = Vec::new();

    pump(resp, &mut |data| {
        if data == "[DONE]" {
            return Ok(());
        }
        let v: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => return Ok(()),
        };
        if let Some(err) = v.get("error") {
            let msg = err.get("message").and_then(Value::as_str).unwrap_or("provider error");
            bail!("{msg}");
        }
        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
            usage.input = u.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(usage.input);
            usage.output = u.get("completion_tokens").and_then(Value::as_u64).unwrap_or(usage.output);
        }
        let Some(choice) = v.get("choices").and_then(|c| c.get(0)) else { return Ok(()) };
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            finish = Some(reason.to_string());
        }
        let Some(delta) = choice.get("delta") else { return Ok(()) };
        for key in ["reasoning_content", "reasoning"] {
            if let Some(r) = delta.get(key).and_then(Value::as_str).filter(|r| !r.is_empty()) {
                on(StreamEv::Thinking(r.to_string()));
            }
        }
        if let Some(c) = delta.get("content").and_then(Value::as_str).filter(|c| !c.is_empty()) {
            text.push_str(c);
            on(StreamEv::Text(c.to_string()));
        }
        if let Some(tcs) = delta.get("tool_calls").and_then(Value::as_array) {
            for tc in tcs {
                let idx = tc.get("index").and_then(Value::as_u64).unwrap_or(calls.len() as u64) as usize;
                while calls.len() <= idx {
                    calls.push(Default::default());
                }
                let slot = &mut calls[idx];
                if let Some(id) = tc.get("id").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                    slot.0 = id.to_string();
                }
                if let Some(f) = tc.get("function") {
                    if let Some(name) = f.get("name").and_then(Value::as_str) {
                        slot.1.push_str(name);
                    }
                    match f.get("arguments") {
                        Some(Value::String(s)) => slot.2.push_str(s),
                        Some(obj @ Value::Object(_)) => slot.2 = obj.to_string(),
                        _ => {}
                    }
                }
            }
        }
        Ok(())
    })
    .await?;

    let mut parsed = Vec::new();
    for (i, (id, name, args)) in calls.into_iter().enumerate() {
        if name.is_empty() {
            continue;
        }
        let args = if args.trim().is_empty() {
            json!({})
        } else {
            // Keep unparseable arguments so the tool layer can report them back.
            serde_json::from_str(&args).unwrap_or_else(|_| json!({"__invalid_json": args}))
        };
        let id = if id.is_empty() { format!("call_{i}") } else { id };
        parsed.push(ToolCall { id, name, args });
    }
    let stop = match finish.as_deref() {
        Some("length") => Stop::MaxTokens,
        Some("content_filter") => Stop::Refusal(None),
        _ if !parsed.is_empty() => Stop::ToolUse,
        Some("stop") | None => Stop::End,
        Some(other) => Stop::Other(other.to_string()),
    };
    Ok(Completion { text, calls: parsed, native: None, stop, usage })
}

// ---- Anthropic Messages API -----------------------------------------------

fn anthropic_messages(messages: &[Msg]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for m in messages {
        match m {
            Msg::User(text) => out.push(json!({"role": "user", "content": [{"type": "text", "text": text}]})),
            Msg::Assistant { text, calls, native } => {
                let content = match native {
                    Some((ProviderKind::Anthropic, blocks)) => blocks.clone(),
                    _ => {
                        let mut blocks = Vec::new();
                        if !text.is_empty() {
                            blocks.push(json!({"type": "text", "text": text}));
                        }
                        for c in calls {
                            blocks.push(json!({"type": "tool_use", "id": c.id, "name": c.name, "input": c.args}));
                        }
                        Value::Array(blocks)
                    }
                };
                out.push(json!({"role": "assistant", "content": content}));
            }
            Msg::ToolResult { id, content, is_error, .. } => {
                let block = json!({"type": "tool_result", "tool_use_id": id, "content": content, "is_error": is_error});
                // All results for one assistant turn go in a single user message.
                let appended = out.last_mut().is_some_and(|last| {
                    let is_results = last["role"] == "user"
                        && last["content"].as_array().is_some_and(|a| a.iter().all(|b| b["type"] == "tool_result"));
                    if is_results {
                        last["content"].as_array_mut().unwrap().push(block.clone());
                    }
                    is_results
                });
                if !appended {
                    out.push(json!({"role": "user", "content": [block]}));
                }
            }
        }
    }
    out
}

/// Models from before adaptive thinking existed take no `thinking` field here.
fn supports_adaptive_thinking(model: &str) -> bool {
    !(model.contains("-4-5") || model.contains("-4-1") || model.contains("claude-3"))
}

async fn anthropic_complete(
    client: &reqwest::Client,
    req: &Request<'_>,
    on: &mut (dyn FnMut(StreamEv) + Send),
) -> Result<Completion> {
    let base = base_url(req.provider, "https://api.anthropic.com");
    let first_party = base.contains("api.anthropic.com");
    let mut body = json!({
        "model": req.model,
        "max_tokens": req.max_tokens.unwrap_or(32_000),
        "system": req.system,
        "messages": anthropic_messages(req.messages),
        "stream": true,
        // Cache the stable prefix (tools + system + earlier turns) between steps.
        "cache_control": {"type": "ephemeral"},
    });
    if !req.tools.is_empty() {
        body["tools"] = req
            .tools
            .iter()
            .map(|t| {
                let mut tool = json!({"name": t.name, "description": t.description, "input_schema": t.schema});
                if first_party {
                    tool["eager_input_streaming"] = json!(true);
                }
                tool
            })
            .collect();
    }
    if supports_adaptive_thinking(req.model) {
        body["thinking"] = json!({"type": "adaptive", "display": "summarized"});
    }
    let mut rb = client
        .post(format!("{base}/v1/messages"))
        .header("anthropic-version", ANTHROPIC_VERSION)
        .header("content-type", "application/json");
    if first_party && FALLBACK_MODELS.contains(&req.model) {
        body["fallbacks"] = json!("default");
        rb = rb.header("anthropic-beta", FALLBACK_BETA);
    }
    match req.provider.resolved_key() {
        Some(key) => rb = rb.header("x-api-key", key),
        None => bail!(
            "no API key for this Anthropic provider; set {} or add api_key_env to its config",
            req.provider.api_key_env.as_deref().unwrap_or("ANTHROPIC_API_KEY")
        ),
    }
    let resp = check(rb.json(&body).send().await.with_context(|| format!("could not reach {base}"))?).await?;

    let mut blocks: Vec<Value> = Vec::new();
    let mut partial: Vec<String> = Vec::new();
    let mut text = String::new();
    let mut usage = Usage::default();
    let mut stop_reason: Option<String> = None;
    let mut refusal_category: Option<String> = None;

    pump(resp, &mut |data| {
        let v: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => return Ok(()),
        };
        match v["type"].as_str().unwrap_or("") {
            "message_start" => {
                let u = &v["message"]["usage"];
                usage.input = u["input_tokens"].as_u64().unwrap_or(0)
                    + u["cache_read_input_tokens"].as_u64().unwrap_or(0)
                    + u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
            }
            "content_block_start" => {
                let idx = v["index"].as_u64().unwrap_or(blocks.len() as u64) as usize;
                while blocks.len() <= idx {
                    blocks.push(Value::Null);
                    partial.push(String::new());
                }
                blocks[idx] = v["content_block"].clone();
            }
            "content_block_delta" => {
                let idx = v["index"].as_u64().unwrap_or(0) as usize;
                let Some(block) = blocks.get_mut(idx) else { return Ok(()) };
                let delta = &v["delta"];
                let append = |block: &mut Value, field: &str, piece: &str| {
                    let cur = block[field].as_str().unwrap_or("").to_string();
                    block[field] = json!(cur + piece);
                };
                match delta["type"].as_str().unwrap_or("") {
                    "text_delta" => {
                        let piece = delta["text"].as_str().unwrap_or("");
                        append(block, "text", piece);
                        text.push_str(piece);
                        on(StreamEv::Text(piece.to_string()));
                    }
                    "thinking_delta" => {
                        let piece = delta["thinking"].as_str().unwrap_or("");
                        append(block, "thinking", piece);
                        if !piece.is_empty() {
                            on(StreamEv::Thinking(piece.to_string()));
                        }
                    }
                    "signature_delta" => {
                        let piece = delta["signature"].as_str().unwrap_or("");
                        append(block, "signature", piece);
                    }
                    "input_json_delta" => {
                        partial[idx].push_str(delta["partial_json"].as_str().unwrap_or(""));
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let idx = v["index"].as_u64().unwrap_or(0) as usize;
                if let Some(block) = blocks.get_mut(idx) {
                    if block["type"] == "tool_use" {
                        let raw = std::mem::take(&mut partial[idx]);
                        block["input"] = if raw.trim().is_empty() {
                            json!({})
                        } else {
                            serde_json::from_str(&raw).unwrap_or_else(|_| json!({"__invalid_json": raw}))
                        };
                    }
                }
            }
            "message_delta" => {
                if let Some(r) = v["delta"]["stop_reason"].as_str() {
                    stop_reason = Some(r.to_string());
                }
                if let Some(c) = v["delta"]["stop_details"]["category"].as_str() {
                    refusal_category = Some(c.to_string());
                }
                if let Some(o) = v["usage"]["output_tokens"].as_u64() {
                    usage.output = o;
                }
            }
            "error" => {
                let msg = v["error"]["message"].as_str().unwrap_or("provider error");
                bail!("{msg}");
            }
            _ => {}
        }
        Ok(())
    })
    .await?;

    blocks.retain(|b| !b.is_null());
    let calls: Vec<ToolCall> = blocks
        .iter()
        .filter(|b| b["type"] == "tool_use")
        .map(|b| ToolCall {
            id: b["id"].as_str().unwrap_or_default().to_string(),
            name: b["name"].as_str().unwrap_or_default().to_string(),
            args: b["input"].clone(),
        })
        .collect();
    let stop = match stop_reason.as_deref() {
        Some("tool_use") => Stop::ToolUse,
        Some("max_tokens") => Stop::MaxTokens,
        Some("refusal") => Stop::Refusal(refusal_category),
        Some("end_turn") | Some("stop_sequence") | None => Stop::End,
        Some(other) => Stop::Other(other.to_string()),
    };
    Ok(Completion { text, calls, native: Some((ProviderKind::Anthropic, Value::Array(blocks))), stop, usage })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_handles_split_lines() {
        let mut sse = Sse::default();
        let mut seen = Vec::new();
        let mut f = |d: &str| {
            seen.push(d.to_string());
            Ok(())
        };
        sse.push(b"event: x\r\ndata: {\"a\"", &mut f).unwrap();
        sse.push(b":1}\r\n\r\ndata: [DONE]\n", &mut f).unwrap();
        assert_eq!(seen, vec!["{\"a\":1}", "[DONE]"]);
    }

    #[test]
    fn tool_results_are_grouped_for_anthropic() {
        let msgs = vec![
            Msg::User("hi".into()),
            Msg::Assistant {
                text: String::new(),
                calls: vec![
                    ToolCall { id: "a".into(), name: "x".into(), args: json!({}) },
                    ToolCall { id: "b".into(), name: "x".into(), args: json!({}) },
                ],
                native: None,
            },
            Msg::ToolResult { id: "a".into(), name: "x".into(), content: "1".into(), is_error: false },
            Msg::ToolResult { id: "b".into(), name: "x".into(), content: "2".into(), is_error: true },
        ];
        let out = anthropic_messages(&msgs);
        assert_eq!(out.len(), 3);
        assert_eq!(out[2]["content"].as_array().unwrap().len(), 2);
        assert_eq!(out[1]["content"][0]["type"], "tool_use");
    }
}
