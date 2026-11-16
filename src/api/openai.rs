// The Chat Completions format, for providers that speak it (see provider.rs).
//
// Conversations are kept as Anthropic messages. On the way out each one is
// turned into Chat Completions messages: once, for a saved message, with the
// result kept on disk and read from there by every later request, so a long
// history costs neither memory nor repeated work. On the way back the
// stream is turned into the same reply and the same chunks the rest of the
// program gets from Anthropic's API.
//
//   Anthropic                            Chat Completions
//   system (a request field)             {"role":"system"}, first
//   user: text, image blocks             {"role":"user"}: a string or text/image_url parts
//   user: tool_result blocks             {"role":"tool"} each, before any user text
//   assistant: text                      {"role":"assistant","content"}
//   assistant: tool_use blocks           tool_calls, with the input as a JSON string
//   assistant: thinking                  left out (it can't be sent back)

use crate::api::provider::Provider;
use crate::api::{Body, Cancelled, Chunk, Lines, Message, Part, Reply, Request, Retryable, Text, Usage};
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::BufRead;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Everything in the request but the messages.
pub fn head(req: &Request, model: &str, provider: &Provider) -> Value {
    let mut head = json!({
        "model": model,
        "stream": true,
        // Without this most servers don't say how many tokens were used.
        "stream_options": { "include_usage": true },
    });
    let tools = tools(req.tools.get());
    if !tools.is_empty() {
        head["tools"] = json!(tools);
    }
    if let Some(max) = provider.max_tokens {
        head["max_tokens"] = json!(max);
    }
    // Only when the user chose one: not every model takes it.
    if let Some(effort) = req.output_config.as_ref().and_then(|c| c["effort"].as_str()) {
        head["reasoning_effort"] = json!(if matches!(effort, "low" | "medium") { effort } else { "high" });
    }
    head
}

/// Our tool definitions as functions. Tools that run on Anthropic's side
/// (web search) have no schema of ours and are left out.
fn tools(defs: &str) -> Vec<Value> {
    let defs: Vec<Value> = serde_json::from_str(defs).unwrap_or_default();
    defs.iter()
        .filter(|d| d.get("input_schema").is_some())
        .map(|d| {
            json!({
                "type": "function",
                "function": { "name": d["name"], "description": d["description"], "parameters": d["input_schema"] },
            })
        })
        .collect()
}

/// The text of a tool result or a similar block list.
fn plain(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .map(|b| match b["type"].as_str() {
                Some("text") => b["text"].as_str().unwrap_or("").to_string(),
                Some("image") => "[an image, which this model's API can't take in a tool result]".to_string(),
                _ => String::new(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// One Anthropic message as the Chat Completions messages it becomes.
pub fn messages(msg: &Value) -> Vec<Value> {
    let blocks = match &msg["content"] {
        Value::Array(blocks) => blocks.clone(),
        Value::String(text) => vec![json!({ "type": "text", "text": text })],
        _ => Vec::new(),
    };
    if msg["role"] == "assistant" {
        let text: Vec<&str> =
            blocks.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect();
        let calls: Vec<Value> = blocks
            .iter()
            .filter(|b| b["type"] == "tool_use")
            .map(|b| {
                json!({
                    "id": b["id"],
                    "type": "function",
                    "function": { "name": b["name"], "arguments": b["input"].to_string() },
                })
            })
            .collect();
        let mut out = json!({ "role": "assistant", "content": text.join("\n") });
        if !calls.is_empty() {
            out["tool_calls"] = json!(calls);
        }
        return vec![out];
    }

    // Tool results first: they have to follow the calls they answer directly.
    let mut out: Vec<Value> = blocks
        .iter()
        .filter(|b| b["type"] == "tool_result")
        .map(|b| json!({ "role": "tool", "tool_call_id": b["tool_use_id"], "content": plain(&b["content"]) }))
        .collect();
    let parts: Vec<Value> = blocks
        .iter()
        .filter_map(|b| match b["type"].as_str()? {
            "text" => Some(json!({ "type": "text", "text": b["text"] })),
            "image" => {
                let (kind, data) = (b["source"]["media_type"].as_str()?, b["source"]["data"].as_str()?);
                let url = format!("data:{kind};base64,{}", crate::images::inline(data));
                Some(json!({ "type": "image_url", "image_url": { "url": url } }))
            }
            _ => None,
        })
        .collect();
    match parts.as_slice() {
        [] => {}
        // Text alone goes as a plain string, which every server takes.
        [only] if only["type"] == "text" => out.push(json!({ "role": "user", "content": only["text"] })),
        _ => out.push(json!({ "role": "user", "content": parts })),
    }
    out
}

/// One Anthropic message (as JSON text) as Chat Completions messages,
/// joined by commas; empty if it becomes none.
fn translate(json: &str) -> std::io::Result<Vec<u8>> {
    let message: Value = serde_json::from_str(json).map_err(std::io::Error::other)?;
    let out: Vec<String> = messages(&message).iter().map(Value::to_string).collect();
    Ok(out.join(",").into_bytes())
}

/// Saved messages in the Chat Completions format, each translated once and
/// kept in a file of its own, so that a request reads them straight from
/// there: asking again costs no more than it does in Anthropic's format, and
/// memory holds only where each one is.
#[derive(Default)]
pub struct Translated {
    /// Where the translations are kept. It has no name on disk and goes when
    /// the program does.
    file: Option<Arc<File>>,
    end: u64,
    /// (the file a message is saved in, where, how long), or for a message
    /// in memory (no file, a hash of its text, its length), to (where its
    /// translation is, how long).
    at: HashMap<(u64, u64, u64, usize), (u64, usize)>,
    /// The file couldn't be made; translate each time instead.
    unusable: bool,
}

impl Translated {
    fn file(&mut self) -> Option<Arc<File>> {
        if self.file.is_none() && !self.unusable {
            // A name nothing else has, made fresh: a second one in this
            // process must not open (and empty) the first one's file.
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = crate::config::data_dir();
            let path = dir.join(format!("chat-{}-{n}.tmp", std::process::id()));
            let made = std::fs::create_dir_all(&dir)
                .and_then(|()| std::fs::OpenOptions::new().read(true).write(true).create_new(true).open(&path));
            let _ = std::fs::remove_file(&path);
            match made {
                Ok(f) => self.file = Some(Arc::new(f)),
                Err(_) => self.unusable = true,
            }
        }
        self.file.clone()
    }

    /// Keeps `bytes` as the translation under `key` and says where it is.
    fn keep(&mut self, key: (u64, u64, u64, usize), bytes: &[u8]) -> Option<(Arc<File>, u64, usize)> {
        let file = self.file()?;
        file.write_all_at(bytes, self.end).ok()?;
        let at = self.end;
        self.end += bytes.len() as u64;
        self.at.insert(key, (at, bytes.len()));
        Some((file, at, bytes.len()))
    }
}

/// The request body: `{"messages":[` system, each message in the Chat
/// Completions format, `],` and the rest of the request.
pub fn body<'a, M: Message>(
    head: &'a [u8],
    system: &str,
    source: &'a [M],
    kept: &Mutex<Translated>,
) -> std::io::Result<Body<'a>> {
    let mut parts = vec![Part::Bytes(b"{\"messages\":[")];
    let mut any = false;
    let mut add = |part: Part<'a>| {
        if std::mem::replace(&mut any, true) {
            parts.push(Part::Bytes(b","));
        }
        parts.push(part);
    };
    if !system.is_empty() {
        add(Part::Owned(json!({ "role": "system", "content": system }).to_string().into_bytes()));
    }
    for message in source {
        // What the message is known by: where it is saved, or, while it is
        // only in memory (a subagent's, or the newest ones), what it says.
        let key = match message.text() {
            Text::Memory(json) => {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                json.hash(&mut hasher);
                (u64::MAX, hasher.finish(), 0, json.len())
            }
            Text::Saved { file, at, len } => {
                let meta = file.metadata()?;
                (meta.dev(), meta.ino(), at, len)
            }
        };
        let known = {
            let kept = kept.lock().unwrap();
            kept.at.get(&key).and_then(|&(at, len)| Some((kept.file.clone()?, at, len)))
        };
        let (file, at, len) = match known {
            Some(found) => found,
            None => {
                let bytes = match message.text() {
                    Text::Memory(json) => translate(json)?,
                    Text::Saved { file, at, len } => {
                        let mut buf = vec![0; len];
                        file.read_exact_at(&mut buf, at)?;
                        translate(std::str::from_utf8(&buf).map_err(std::io::Error::other)?)?
                    }
                };
                match kept.lock().unwrap().keep(key, &bytes) {
                    Some(found) => found,
                    None => {
                        if !bytes.is_empty() {
                            add(Part::Owned(bytes));
                        }
                        continue;
                    }
                }
            }
        };
        if len > 0 {
            add(Part::Kept { file, at, left: len });
        }
    }
    parts.push(Part::Bytes(b"],"));
    parts.push(Part::Bytes(&head[1..])); // the request object without its opening brace
    Ok(Body::from_parts(parts))
}

/// A tool call as it arrives: its id, its name and its input so far.
#[derive(Default)]
struct Call {
    id: String,
    name: String,
    input: String,
}

/// Reads a streamed reply.
pub fn read(r: impl BufRead, cancel: &AtomicBool, silence: Duration, on: &mut impl FnMut(Chunk)) -> Result<Reply> {
    let mut emitted = false;
    let (mut text, mut calls, mut finish): (String, Vec<Call>, Option<String>) = Default::default();
    let mut usage = Usage::default();
    // Whether text or thinking has been shown and its block not yet ended.
    let mut open = false;

    let mut lines = Lines::new(r);
    while let Some(data) = lines.next(cancel, silence, emitted)? {
        if data == "[DONE]" {
            break;
        }
        let ev: Value = serde_json::from_str(&data)?;
        if !ev["error"].is_null() {
            let msg = format!("API error: {}", ev["error"]["message"].as_str().unwrap_or("stream error"));
            return Err(if emitted { anyhow!(msg) } else { Retryable(msg).into() });
        }
        if let Some(u) = ev["usage"].as_object() {
            let count = |v: Option<&Value>| v.and_then(Value::as_u64).unwrap_or(0);
            let cached = count(u.get("prompt_tokens_details").and_then(|d| d.get("cached_tokens")));
            usage.cache_read_input_tokens = cached;
            usage.input_tokens = count(u.get("prompt_tokens")).saturating_sub(cached);
            usage.output_tokens = count(u.get("completion_tokens"));
        }
        let choice = &ev["choices"][0];
        if let Some(reason) = choice["finish_reason"].as_str() {
            finish = Some(reason.to_string());
        }
        let delta = &choice["delta"];
        // Models that think aloud send it under one of two names.
        if let Some(thought) = delta["reasoning_content"].as_str().or(delta["reasoning"].as_str())
            && !thought.is_empty()
        {
            (emitted, open) = (true, true);
            on(Chunk::Thinking(thought));
        }
        if let Some(piece) = delta["content"].as_str()
            && !piece.is_empty()
        {
            (emitted, open) = (true, true);
            text.push_str(piece);
            on(Chunk::Text(piece));
        }
        for (i, call) in delta["tool_calls"].as_array().into_iter().flatten().enumerate() {
            emitted = true;
            if std::mem::take(&mut open) {
                on(Chunk::BlockEnd);
            }
            // Which call this piece belongs to; servers that send whole calls may leave it out.
            let at = call["index"].as_u64().map_or(calls.len().saturating_sub(1).max(i), |n| n as usize);
            while calls.len() <= at {
                calls.push(Call::default());
            }
            if let Some(id) = call["id"].as_str().filter(|id| !id.is_empty()) {
                calls[at].id = id.to_string();
            }
            if let Some(name) = call["function"]["name"].as_str().filter(|n| !n.is_empty()) {
                if calls[at].name.is_empty() {
                    on(Chunk::ToolStart(name));
                }
                calls[at].name = name.to_string();
            }
            if let Some(input) = call["function"]["arguments"].as_str() {
                calls[at].input.push_str(input);
                on(Chunk::ToolInput(input.len()));
            }
        }
    }
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(Cancelled.into());
    }
    if finish.is_none() {
        if !emitted {
            return Err(Retryable("the stream ended before any reply".into()).into());
        }
        bail!("the stream ended early")
    }
    if open || !calls.is_empty() {
        on(Chunk::BlockEnd);
    }

    let mut content = Vec::new();
    if !text.trim().is_empty() {
        content.push(json!({ "type": "text", "text": text }));
    }
    for (n, call) in calls.iter().enumerate().filter(|(_, c)| !c.name.is_empty()) {
        let input = if call.input.trim().is_empty() { "{}" } else { call.input.as_str() };
        content.push(json!({
            "type": "tool_use",
            // Some local servers give calls no id; the result must still name one.
            "id": if call.id.is_empty() { format!("call_{n}") } else { call.id.clone() },
            "name": call.name,
            "input": serde_json::from_str::<Value>(input).unwrap_or_else(|e| json!({ "_invalid_json": e.to_string() })),
        }));
    }
    let stop_reason = match finish.as_deref() {
        _ if content.iter().any(|b| b["type"] == "tool_use") => "tool_use",
        Some("length") => "max_tokens",
        Some("content_filter") => "refusal",
        _ => "end_turn",
    };
    Ok(Reply { content, stop_reason: stop_reason.to_string(), usage })
}

#[cfg(test)]
#[path = "openai_tests.rs"]
mod tests;
