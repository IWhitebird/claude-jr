// Anthropic Messages API: one streaming request, parsed from SSE into content blocks.

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::sleep;
use std::time::Duration;

const VERSION: &str = "2026-10-21";
const BETA_FALLBACK: &str = "server-side-fallback-2026-07-01";
const BETA_UPDATES: &str = "thinking-display-updates-2026-08-18";
const BETA_COMPACT: &str = "compact-2026-01-12";
const MAX_ATTEMPTS: u32 = 5;

/// What a model supports beyond the basic Messages API.
#[derive(Clone, Copy)]
pub struct Caps {
    /// Server-side refusal fallback (`fallbacks: "default"`).
    pub fallback: bool,
    /// Progress notes between tool calls as visible thinking text.
    pub updates: bool,
    /// Server-side compaction near the context limit.
    pub compact: bool,
}

impl Caps {
    pub fn of(model: &str) -> Caps {
        let (fallback, updates, compact) = match model {
            "claude-opus-5-5" | "claude-sonnet-5-5" | "claude-fable-5-1" => (true, true, true),
            "claude-opus-5" => (true, false, true),
            "claude-fable-5" => (false, true, true),
            "claude-haiku-5-5" | "claude-sonnet-5" | "claude-opus-4-8" | "claude-opus-4-7" | "claude-opus-4-6"
            | "claude-sonnet-4-6" => (false, false, true),
            _ => (false, false, false),
        };
        Caps { fallback, updates, compact }
    }

    fn betas(self) -> String {
        let all = [(self.fallback, BETA_FALLBACK), (self.updates, BETA_UPDATES), (self.compact, BETA_COMPACT)];
        all.iter().filter(|(on, _)| *on).map(|(_, b)| *b).collect::<Vec<_>>().join(",")
    }
}

/// Dollars per million tokens: input, output, cache read. Cache writes are 1.25x input.
fn price(model: &str) -> Option<(f64, f64, f64)> {
    Some(match model {
        "claude-opus-5-5" => (4.0, 20.0, 0.20),
        "claude-sonnet-5-5" | "claude-sonnet-5" => (2.0, 10.0, 0.20),
        "claude-haiku-5-5" => (0.10, 0.50, 0.01),
        "claude-fable-5-1" => (10.0, 50.0, 0.25),
        "claude-fable-5" => (10.0, 50.0, 1.0),
        "claude-opus-5" | "claude-opus-4-8" | "claude-opus-4-7" | "claude-opus-4-6" => (5.0, 25.0, 0.50),
        "claude-sonnet-4-6" => (3.0, 15.0, 0.30),
        "claude-haiku-4-5" => (1.0, 5.0, 0.10),
        _ => return None,
    })
}

pub fn cost(model: &str, u: &Usage) -> Option<f64> {
    let (i, o, r) = price(model)?;
    let tokens = u.input_tokens as f64 * i
        + u.cache_creation_input_tokens as f64 * i * 1.25
        + u.cache_read_input_tokens as f64 * r
        + u.output_tokens as f64 * o;
    Some(tokens / 1e6)
}

/// Everything in a request except the messages, which are spliced in from
/// their stored JSON text when the body is sent.
#[derive(Serialize)]
pub struct Request<'a> {
    pub model: &'a str,
    pub max_tokens: u32,
    pub system: &'a str,
    pub tools: &'a RawValue,
    pub stream: bool,
    pub cache_control: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallbacks: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_management: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_config: Option<Value>,
}

impl<'a> Request<'a> {
    pub fn new(model: &'a str, effort: Option<&str>, system: &'a str, tools: &'a RawValue) -> Self {
        let caps = Caps::of(model);
        Request {
            model,
            max_tokens: 64_000,
            system,
            tools,
            stream: true,
            cache_control: json!({ "type": "ephemeral" }),
            thinking: caps.updates.then(|| json!({ "type": "adaptive", "display": "updates" })),
            fallbacks: caps.fallback.then_some("default"),
            context_management: caps.compact.then(|| json!({ "edits": [{ "type": "compact_20260112" }] })),
            output_config: effort.map(|e| json!({ "effort": e })),
        }
    }
}

#[derive(Deserialize, Default, Debug, Clone, Copy)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
}

impl Usage {
    /// Tokens in the context window after this reply.
    pub fn context(&self) -> u64 {
        self.input_tokens + self.cache_creation_input_tokens + self.cache_read_input_tokens + self.output_tokens
    }
}

pub struct Reply {
    pub content: Vec<Value>,
    pub stop_reason: String,
    pub usage: Usage,
}

/// Things worth showing while a reply streams in.
pub enum Chunk<'a> {
    Text(&'a str),
    Thinking(&'a str),
    ToolStart(&'a str),
    /// More of a tool call's input arrived (this many bytes).
    ToolInput(usize),
    BlockEnd,
}

#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

pub struct Client {
    http: ureq::Agent,
    url: String,
    key: String,
}

impl Client {
    pub fn new(key: String, base: Option<String>) -> Self {
        let http = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(30)))
            .build()
            .into();
        let base = base.unwrap_or_else(|| "https://api.anthropic.com".into());
        Client { http, url: format!("{}/v1/messages", base.trim_end_matches('/')), key }
    }

    /// Sends a request and streams the reply. `messages` are JSON texts.
    pub fn stream<M: AsRef<str>>(
        &self,
        req: &Request,
        messages: &[M],
        cancel: &AtomicBool,
        mut on: impl FnMut(Chunk),
    ) -> Result<Reply> {
        let head = serde_json::to_vec(req)?;
        let betas = Caps::of(req.model).betas();
        let mut attempt = 0;
        loop {
            attempt += 1;
            let mut call = self
                .http
                .post(&self.url)
                .header("x-api-key", &self.key)
                .header("anthropic-version", VERSION)
                .header("content-type", "application/json");
            if !betas.is_empty() {
                call = call.header("anthropic-beta", &betas);
            }

            let mut body = Body::new(&head, messages);
            call = call.header("content-length", body.len.to_string());
            let mut res = match call.send(ureq::SendBody::from_reader(&mut body)) {
                Ok(r) => r,
                Err(_) if attempt < MAX_ATTEMPTS => {
                    wait(backoff(attempt, None), cancel)?;
                    continue;
                }
                Err(e) => return Err(e.into()),
            };

            let status = res.status().as_u16();
            if status == 200 {
                let reader = BufReader::new(res.body_mut().as_reader());
                return read_sse(reader, cancel, &mut on);
            }

            let retry_after =
                res.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok());
            let text = res.body_mut().read_to_string().unwrap_or_default();
            let retryable = matches!(status, 408 | 409 | 429) || status >= 500;
            if retryable && attempt < MAX_ATTEMPTS {
                wait(backoff(attempt, retry_after), cancel)?;
                continue;
            }
            bail!("API error {status}: {}", error_message(&text));
        }
    }
}

/// The request body as a list of byte slices read in order, so the history is
/// sent from where it's stored instead of being copied into one buffer:
/// `{"messages":[` m1 `,` m2 ... `],` + the rest of the request.
struct Body<'a> {
    parts: Vec<&'a [u8]>,
    len: usize,
    part: usize,
    offset: usize,
}

impl<'a> Body<'a> {
    fn new<M: AsRef<str>>(head: &'a [u8], messages: &'a [M]) -> Self {
        let mut parts = Vec::with_capacity(messages.len() * 2 + 2);
        parts.push(&b"{\"messages\":["[..]);
        for (i, m) in messages.iter().enumerate() {
            if i > 0 {
                parts.push(b",");
            }
            parts.push(m.as_ref().as_bytes());
        }
        parts.push(b"],");
        parts.push(&head[1..]); // the request object without its opening brace
        let len = parts.iter().map(|p| p.len()).sum();
        Body { parts, len, part: 0, offset: 0 }
    }
}

impl std::io::Read for Body<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        while let Some(p) = self.parts.get(self.part) {
            let rest = &p[self.offset..];
            if rest.is_empty() {
                self.part += 1;
                self.offset = 0;
                continue;
            }
            let n = rest.len().min(buf.len());
            buf[..n].copy_from_slice(&rest[..n]);
            self.offset += n;
            return Ok(n);
        }
        Ok(0)
    }
}

fn backoff(attempt: u32, retry_after: Option<u64>) -> Duration {
    match retry_after {
        Some(s) => Duration::from_secs(s.min(60)),
        None => Duration::from_millis(500 << (attempt - 1)).min(Duration::from_secs(16)),
    }
}

fn wait(d: Duration, cancel: &AtomicBool) -> Result<()> {
    let step = Duration::from_millis(100);
    let mut left = d;
    while !left.is_zero() {
        if cancel.load(Ordering::Relaxed) {
            return Err(Cancelled.into());
        }
        let s = left.min(step);
        sleep(s);
        left -= s;
    }
    Ok(())
}

fn error_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(String::from))
        .unwrap_or_else(|| body.chars().take(500).collect())
}

/// Builds the assistant content blocks from SSE events. Blocks are kept exactly
/// as sent (thinking signatures, compaction blocks, unknown types), because they
/// must be passed back unchanged.
fn read_sse(r: impl BufRead, cancel: &AtomicBool, on: &mut impl FnMut(Chunk)) -> Result<Reply> {
    let mut content: Vec<Value> = Vec::new();
    let mut json_buf = String::new();
    let mut stop_reason = String::new();
    let mut usage = Usage::default();

    for line in r.lines() {
        if cancel.load(Ordering::Relaxed) {
            return Err(Cancelled.into());
        }
        let line = line?;
        let Some(data) = line.strip_prefix("data:") else { continue };
        let mut ev: Value = serde_json::from_str(data.trim_start())?;

        match ev["type"].as_str().unwrap_or("") {
            "message_start" => {
                usage = Usage::deserialize(&ev["message"]["usage"]).unwrap_or_default();
            }
            "content_block_start" => {
                let block = ev["content_block"].take();
                if block["type"] == "tool_use" {
                    on(Chunk::ToolStart(block["name"].as_str().unwrap_or("")));
                }
                content.push(block);
                json_buf.clear();
            }
            "content_block_delta" => {
                let Some(block) = content.last_mut() else { continue };
                let Value::Object(delta) = ev["delta"].take() else { continue };
                match delta.get("type").and_then(Value::as_str).unwrap_or("") {
                    "input_json_delta" => {
                        let part = delta["partial_json"].as_str().unwrap_or("");
                        json_buf.push_str(part);
                        on(Chunk::ToolInput(part.len()));
                        continue;
                    }
                    "text_delta" => on(Chunk::Text(delta["text"].as_str().unwrap_or(""))),
                    "thinking_delta" => on(Chunk::Thinking(delta["thinking"].as_str().unwrap_or(""))),
                    _ => {}
                }
                merge(block, delta);
            }
            "content_block_stop" => {
                if let Some(block) = content.last_mut()
                    && !json_buf.is_empty()
                {
                    block["input"] =
                        serde_json::from_str(&json_buf).unwrap_or_else(|e| json!({ "_invalid_json": e.to_string() }));
                    json_buf.clear();
                }
                on(Chunk::BlockEnd);
            }
            "message_delta" => {
                if let Some(s) = ev["delta"]["stop_reason"].as_str() {
                    stop_reason = s.to_string();
                }
                if let Some(n) = ev["usage"]["output_tokens"].as_u64() {
                    usage.output_tokens = n;
                }
            }
            "message_stop" => return Ok(Reply { content, stop_reason, usage }),
            "error" => {
                let msg = ev["error"]["message"].as_str().unwrap_or("stream error");
                return Err(anyhow!("API error: {msg}"));
            }
            _ => {}
        }
    }
    bail!("stream ended early")
}

/// Applies a delta to its block: strings are appended, anything else replaces.
/// This covers text, thinking, signatures and delta types added later.
fn merge(block: &mut Value, delta: serde_json::Map<String, Value>) {
    for (k, v) in delta {
        if k == "type" {
            continue;
        }
        match (block.get_mut(&k), v) {
            (Some(Value::String(cur)), Value::String(s)) => cur.push_str(&s),
            (_, v) => block[k] = v,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_text_thinking_and_tool_use() {
        let sse = r#"event: message_start
data: {"type":"message_start","message":{"usage":{"input_tokens":10,"cache_read_input_tokens":4}}}

data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}
data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Checking a.rs"}}
data: {"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig"}}
data: {"type":"content_block_stop","index":0}
data: {"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}
data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Hel"}}
data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"lo"}}
data: {"type":"content_block_stop","index":1}
data: {"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"t1","name":"read","input":{}}}
data: {"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}
data: {"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"\"a.rs\"}"}}
data: {"type":"content_block_stop","index":2}
data: {"type":"content_block_start","index":3,"content_block":{"type":"compaction","content":""}}
data: {"type":"content_block_delta","index":3,"delta":{"type":"compaction_delta","content":"sum"}}
data: {"type":"content_block_delta","index":3,"delta":{"type":"compaction_delta","content":"mary"}}
data: {"type":"content_block_stop","index":3}
event: ping
data: {"type":"ping"}
data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":7}}
data: {"type":"message_stop"}
"#;
        let (mut text, mut thinking, mut tools, mut ends) = (String::new(), String::new(), Vec::new(), 0);
        let reply = read_sse(sse.as_bytes(), &AtomicBool::new(false), &mut |c| match c {
            Chunk::Text(t) => text.push_str(t),
            Chunk::Thinking(t) => thinking.push_str(t),
            Chunk::ToolStart(n) => tools.push(n.to_string()),
            Chunk::ToolInput(_) => {}
            Chunk::BlockEnd => ends += 1,
        })
        .unwrap();

        assert_eq!((text.as_str(), thinking.as_str(), ends), ("Hello", "Checking a.rs", 4));
        assert_eq!(tools, ["read"]);
        assert_eq!(reply.stop_reason, "tool_use");
        assert_eq!(reply.usage.context(), 21);
        assert_eq!(reply.content[0]["signature"], "sig");
        assert_eq!(reply.content[1]["text"], "Hello");
        assert_eq!(reply.content[2]["input"]["path"], "a.rs");
        assert_eq!(reply.content[3]["content"], "summary");
    }

    #[test]
    fn body_splices_messages() {
        let head = br#"{"model":"m","stream":true}"#;
        let msgs = [r#"{"role":"user","content":"hi"}"#, r#"{"role":"assistant","content":"yo"}"#];
        let mut body = Body::new(head, &msgs);
        let mut out = String::new();
        std::io::Read::read_to_string(&mut body, &mut out).unwrap();
        assert_eq!(out.len(), body.len);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["messages"][1]["content"], "yo");
        assert_eq!(v["model"], "m");
    }

    #[test]
    fn caps_and_cost() {
        assert_eq!(Caps::of("claude-opus-5-5").betas(), [BETA_FALLBACK, BETA_UPDATES, BETA_COMPACT].join(","));
        assert_eq!(Caps::of("something-else").betas(), "");
        let u = Usage { input_tokens: 1_000_000, output_tokens: 1_000_000, ..Default::default() };
        assert_eq!(cost("claude-opus-5-5", &u), Some(24.0));
    }
}
