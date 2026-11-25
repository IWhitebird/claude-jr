// The Messages format: the request (everything but the messages) and the
// reply, parsed from the event stream into content blocks. Blocks are kept
// exactly as sent (thinking signatures, compaction blocks, unknown types),
// because they must be passed back unchanged.

use super::models::Caps;
use super::stream::Lines;
use super::{Chunk, Reply, Retryable, Usage};
use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::{Value, json};
use std::io::BufRead;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

pub(super) const VERSION: &str = "2026-10-21";

/// Everything in a request except the messages, which are spliced in from
/// their stored JSON text when the body is sent.
#[derive(Serialize, Clone)]
pub struct Request<'a> {
    /// Empty where the address names the model (Vertex).
    #[serde(skip_serializing_if = "str::is_empty")]
    pub model: &'a str,
    /// Where the version goes in the body and not in a header (Vertex).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anthropic_version: Option<&'static str>,
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
    /// Remote MCP servers for the API's connector.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<&'a RawValue>,
}

impl<'a> Request<'a> {
    pub fn new(model: &'a str, effort: Option<&str>, system: &'a str, tools: &'a RawValue) -> Self {
        let caps = Caps::of(model);
        Request {
            model,
            anthropic_version: None,
            max_tokens: 64_000,
            system,
            tools,
            stream: true,
            cache_control: json!({ "type": "ephemeral" }),
            thinking: caps.updates.then(|| json!({ "type": "adaptive", "display": "updates" })),
            fallbacks: caps.fallback.then_some("default"),
            context_management: caps.compact.then(|| json!({ "edits": [{ "type": "compact_20260112" }] })),
            output_config: effort.map(|e| json!({ "effort": e })),
            mcp_servers: None,
        }
    }
}

pub(super) fn read_sse(
    r: impl BufRead,
    cancel: &AtomicBool,
    silence: Duration,
    on: &mut impl FnMut(Chunk),
) -> Result<Reply> {
    let emitted = std::cell::Cell::new(false);
    let mut on = |c: Chunk| {
        emitted.set(true);
        on(c)
    };
    let mut content: Vec<Value> = Vec::new();
    let mut json_buf = String::new();
    let mut stop_reason = String::new();
    let mut usage = Usage::default();

    let mut lines = Lines::new(r);
    while let Some(data) = lines.next(cancel, silence, emitted.get())? {
        let mut ev: Value = serde_json::from_str(&data)?;

        match ev["type"].as_str().unwrap_or("") {
            "message_start" => {
                usage = Usage::deserialize(&ev["message"]["usage"]).unwrap_or_default();
            }
            "content_block_start" => {
                let block = ev["content_block"].take();
                if matches!(block["type"].as_str(), Some("tool_use" | "mcp_tool_use" | "server_tool_use")) {
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
                if let Some(block) = content.last_mut() {
                    if !json_buf.is_empty() {
                        block["input"] = serde_json::from_str(&json_buf)
                            .unwrap_or_else(|e| json!({ "_invalid_json": e.to_string() }));
                        json_buf.clear();
                    }
                    let kind = block["type"].as_str().unwrap_or("");
                    if kind == "server_tool_use" || kind == "mcp_tool_use" || kind.ends_with("_tool_result") {
                        on(Chunk::ServerTool(block));
                    }
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
            "message_stop" => {
                // A model now and then sends a text block with nothing in
                // it (before a tool call, say). Sent back, the API refuses
                // the whole conversation, from then on.
                content.retain(|b| b["type"] != "text" || b["text"].as_str().is_some_and(|t| !t.trim().is_empty()));
                return Ok(Reply { content, stop_reason, usage });
            }
            "error" => {
                let msg = format!("API error: {}", ev["error"]["message"].as_str().unwrap_or("stream error"));
                return Err(if content.is_empty() { Retryable(msg).into() } else { anyhow!(msg) });
            }
            _ => {}
        }
    }
    if content.is_empty() {
        return Err(Retryable("the stream ended before any reply".into()).into());
    }
    bail!("the stream ended early")
}

/// Applies a delta to its block: strings are appended, anything else replaces.
/// This covers text, thinking, signatures and delta types added later.
pub(super) fn merge(block: &mut Value, delta: serde_json::Map<String, Value>) {
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
