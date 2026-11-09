// Anthropic Messages API: one streaming request, parsed from SSE into content blocks.

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::sleep;
use std::time::Duration;

const VERSION: &str = "2026-10-21";
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
const MAX_ATTEMPTS: u32 = 5;

#[derive(Serialize)]
pub struct Request<'a> {
    pub model: &'a str,
    pub max_tokens: u32,
    pub system: &'a str,
    pub messages: &'a [Value],
    #[serde(skip_serializing_if = "<[Value]>::is_empty")]
    pub tools: &'a [Value],
    pub stream: bool,
    pub cache_control: CacheControl,
    pub fallbacks: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_config: Option<OutputConfig<'a>>,
}

#[derive(Serialize)]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub kind: &'static str,
}

#[derive(Serialize)]
pub struct OutputConfig<'a> {
    pub effort: &'a str,
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

pub struct Reply {
    pub content: Vec<Value>,
    pub stop_reason: String,
    pub usage: Usage,
}

/// Things worth showing while a reply streams in.
pub enum Chunk<'a> {
    Text(&'a str),
    ToolStart(&'a str),
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

    pub fn stream(
        &self,
        req: &Request,
        cancel: &AtomicBool,
        mut on: impl FnMut(Chunk),
    ) -> Result<Reply> {
        let body = serde_json::to_vec(req)?;
        let mut attempt = 0;
        loop {
            attempt += 1;
            let res = self
                .http
                .post(&self.url)
                .header("x-api-key", &self.key)
                .header("anthropic-version", VERSION)
                .header("anthropic-beta", FALLBACK_BETA)
                .header("content-type", "application/json")
                .send(&body[..]);

            let mut res = match res {
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

            let retry_after = res
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());
            let text = res.body_mut().read_to_string().unwrap_or_default();
            let retryable = status == 408 || status == 409 || status == 429 || status >= 500;
            if retryable && attempt < MAX_ATTEMPTS {
                wait(backoff(attempt, retry_after), cancel)?;
                continue;
            }
            bail!("API error {status}: {}", error_message(&text));
        }
    }
}

fn backoff(attempt: u32, retry_after: Option<u64>) -> Duration {
    match retry_after {
        Some(s) => Duration::from_secs(s.min(60)),
        None => Duration::from_millis(500 * 2u64.pow(attempt - 1)).min(Duration::from_secs(16)),
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

/// Builds the assistant content blocks from SSE events. Thinking blocks and their
/// signatures are kept exactly as sent, because they must be passed back unchanged.
fn read_sse(r: impl BufRead, cancel: &AtomicBool, on: &mut impl FnMut(Chunk)) -> Result<Reply> {
    let mut content: Vec<Value> = Vec::new();
    let mut partial: Vec<String> = Vec::new();
    let mut stop_reason = String::new();
    let mut usage = Usage::default();

    for line in r.lines() {
        if cancel.load(Ordering::Relaxed) {
            return Err(Cancelled.into());
        }
        let line = line?;
        let Some(data) = line.strip_prefix("data:") else { continue };
        let ev: Value = serde_json::from_str(data.trim_start())?;

        match ev["type"].as_str().unwrap_or("") {
            "message_start" => {
                if let Ok(u) = serde_json::from_value(ev["message"]["usage"].clone()) {
                    usage = u;
                }
            }
            "content_block_start" => {
                let block = ev["content_block"].clone();
                if block["type"] == "tool_use" {
                    on(Chunk::ToolStart(block["name"].as_str().unwrap_or("")));
                }
                content.push(block);
                partial.push(String::new());
            }
            "content_block_delta" => {
                let i = ev["index"].as_u64().unwrap_or(0) as usize;
                let (Some(block), Some(json_buf)) = (content.get_mut(i), partial.get_mut(i)) else {
                    continue;
                };
                let d = &ev["delta"];
                match d["type"].as_str().unwrap_or("") {
                    "text_delta" => {
                        let t = d["text"].as_str().unwrap_or("");
                        append(block, "text", t);
                        on(Chunk::Text(t));
                    }
                    "thinking_delta" => append(block, "thinking", d["thinking"].as_str().unwrap_or("")),
                    "signature_delta" => block["signature"] = d["signature"].clone(),
                    "input_json_delta" => json_buf.push_str(d["partial_json"].as_str().unwrap_or("")),
                    _ => {}
                }
            }
            "content_block_stop" => {
                let i = ev["index"].as_u64().unwrap_or(0) as usize;
                if let (Some(block), Some(buf)) = (content.get_mut(i), partial.get_mut(i)) {
                    if !buf.is_empty() {
                        block["input"] = serde_json::from_str(buf).unwrap_or_else(
                            |e| json!({ "_invalid_json": e.to_string() }),
                        );
                        *buf = String::new();
                    }
                }
            }
            "message_delta" => {
                if let Some(s) = ev["delta"]["stop_reason"].as_str() {
                    stop_reason = s.to_string();
                }
                if let Some(n) = ev["usage"]["output_tokens"].as_u64() {
                    usage.output_tokens = n;
                }
            }
            "message_stop" => {
                return Ok(Reply { content, stop_reason, usage });
            }
            "error" => {
                let msg = ev["error"]["message"].as_str().unwrap_or("stream error");
                return Err(anyhow!("API error: {msg}"));
            }
            _ => {}
        }
    }
    bail!("stream ended early")
}

fn append(block: &mut Value, key: &str, s: &str) {
    match block.get_mut(key) {
        Some(Value::String(cur)) => cur.push_str(s),
        _ => block[key] = Value::String(s.to_string()),
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
data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":""}}
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
event: ping
data: {"type":"ping"}
data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":7}}
data: {"type":"message_stop"}
"#;
        let mut text = String::new();
        let mut tools = Vec::new();
        let reply = read_sse(sse.as_bytes(), &AtomicBool::new(false), &mut |c| match c {
            Chunk::Text(t) => text.push_str(t),
            Chunk::ToolStart(n) => tools.push(n.to_string()),
        })
        .unwrap();

        assert_eq!(text, "Hello");
        assert_eq!(tools, ["read"]);
        assert_eq!(reply.stop_reason, "tool_use");
        assert_eq!(reply.usage.input_tokens, 10);
        assert_eq!(reply.usage.cache_read_input_tokens, 4);
        assert_eq!(reply.usage.output_tokens, 7);
        assert_eq!(reply.content[0]["signature"], "sig");
        assert_eq!(reply.content[1]["text"], "Hello");
        assert_eq!(reply.content[2]["input"]["path"], "a.rs");
    }
}
