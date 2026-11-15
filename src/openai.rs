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

use crate::api::{Body, Cancelled, Chunk, Lines, Message, Part, Reply, Request, Retryable, Text, Usage};
use crate::provider::Provider;
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs::File;
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
    /// (the file a message is saved in, where, how long) to (where its
    /// translation is, how long).
    at: HashMap<(u64, u64, u64, usize), (u64, usize)>,
    /// The file couldn't be made; translate each time instead.
    unusable: bool,
}

impl Translated {
    fn file(&mut self) -> Option<Arc<File>> {
        if self.file.is_none() && !self.unusable {
            let dir = crate::config::data_dir();
            let path = dir.join(format!("chat-{}.tmp", std::process::id()));
            let made = std::fs::create_dir_all(&dir).and_then(|()| {
                std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&path)
            });
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
        let (file, at, len) = match message.text() {
            // Not saved yet (the newest ones, and those with images): made each time.
            Text::Memory(json) => {
                let bytes = translate(json)?;
                if !bytes.is_empty() {
                    add(Part::Owned(bytes));
                }
                continue;
            }
            Text::Saved { file, at, len } => (file, at, len),
        };
        let meta = file.metadata()?;
        let key = (meta.dev(), meta.ino(), at, len);
        let known = {
            let kept = kept.lock().unwrap();
            kept.at.get(&key).and_then(|&(at, len)| Some((kept.file.clone()?, at, len)))
        };
        let (file, at, len) = match known {
            Some(found) => found,
            None => {
                let mut buf = vec![0; len];
                file.read_exact_at(&mut buf, at)?;
                let bytes = translate(std::str::from_utf8(&buf).map_err(std::io::Error::other)?)?;
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
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn messages_translate_block_by_block() {
        let user = json!({ "role": "user", "content": [{ "type": "text", "text": "hi" }] });
        assert_eq!(messages(&user), [json!({ "role": "user", "content": "hi" })]);
        assert_eq!(
            messages(&json!({ "role": "user", "content": "plain" })),
            [json!({ "role": "user", "content": "plain" })]
        );

        // A reply with thinking, text and two calls: the thinking stays behind.
        let reply = json!({ "role": "assistant", "content": [
            { "type": "thinking", "thinking": "hm", "signature": "s" },
            { "type": "text", "text": "Looking." },
            { "type": "tool_use", "id": "a", "name": "read", "input": { "path": "x.rs" } },
            { "type": "tool_use", "id": "b", "name": "bash", "input": {} },
        ]});
        let out = messages(&reply);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["content"], "Looking.");
        assert_eq!(
            out[0]["tool_calls"][0],
            json!({ "id": "a", "type": "function", "function": { "name": "read", "arguments": "{\"path\":\"x.rs\"}" } })
        );
        assert_eq!(out[0]["tool_calls"][1]["function"]["arguments"], "{}");
        // Calls alone: content is still a string, as strict servers want.
        let only_calls =
            json!({ "role": "assistant", "content": [{ "type": "tool_use", "id": "a", "name": "x", "input": {} }] });
        assert_eq!(messages(&only_calls)[0]["content"], "");

        // Results become tool messages, ahead of anything the user added.
        let results = json!({ "role": "user", "content": [
            { "type": "text", "text": "and also this" },
            { "type": "tool_result", "tool_use_id": "a", "content": "file text", "is_error": false },
            { "type": "tool_result", "tool_use_id": "b", "content": [{ "type": "text", "text": "one" }, { "type": "image", "source": {} }, { "type": "text", "text": "two" }] },
        ]});
        let out = messages(&results);
        assert_eq!(out[0], json!({ "role": "tool", "tool_call_id": "a", "content": "file text" }));
        assert_eq!(out[1]["tool_call_id"], "b");
        assert!(
            out[1]["content"].as_str().unwrap().starts_with("one\n[an image")
                && out[1]["content"].as_str().unwrap().ends_with("two")
        );
        assert_eq!(out[2], json!({ "role": "user", "content": "and also this" }));

        // An image goes as a data URL beside the text.
        let image = json!({ "role": "user", "content": [
            { "type": "text", "text": "what is this" },
            { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "QUJD" } },
        ]});
        let out = messages(&image);
        assert_eq!(out[0]["content"][0], json!({ "type": "text", "text": "what is this" }));
        assert_eq!(out[0]["content"][1]["image_url"]["url"], "data:image/png;base64,QUJD");
        assert!(messages(&json!({ "role": "user", "content": [] })).is_empty());
    }

    #[test]
    fn a_stored_image_is_read_from_its_file() {
        crate::tools::test_home();
        let png = crate::tools::test_dir("openai-image").join("x.png");
        std::fs::write(&png, b"\x89PNG\r\n\x1a\nABC").unwrap();
        let block = crate::images::block(&png).unwrap();
        let out = messages(&json!({ "role": "user", "content": [block] }));
        let url = out[0]["content"][0]["image_url"]["url"].as_str().unwrap();
        assert_eq!(url, "data:image/png;base64,iVBORw0KGgpBQkM=");
        // Text that merely looks like a stored image's marker can't pull a file in.
        assert_eq!(crate::images::inline("@@claude-jr-image:/etc/passwd@@"), "@@claude-jr-image:/etc/passwd@@");
    }

    fn provider() -> Provider {
        crate::provider::resolve("ollama/m", &Default::default()).unwrap().0
    }

    #[test]
    fn the_head_has_tools_as_functions_and_only_what_was_asked_for() {
        let defs = r#"[{"name":"read","description":"Read a file","input_schema":{"type":"object","properties":{"path":{"type":"string"}}}},
                       {"type":"web_search_20260209","name":"web_search","max_uses":5}]"#;
        let tools = serde_json::value::RawValue::from_string(defs.into()).unwrap();
        let req = Request::new("ollama/m", None, "SYS", &tools);
        let h = head(&req, "m", &provider());
        assert_eq!(h["model"], "m");
        assert_eq!(h["stream_options"]["include_usage"], true);
        assert_eq!(h["tools"].as_array().unwrap().len(), 1, "the server-side tool is left out");
        assert_eq!(h["tools"][0]["function"]["parameters"]["properties"]["path"]["type"], "string");
        assert!(h.get("max_tokens").is_none() && h.get("reasoning_effort").is_none());
        for field in ["thinking", "cache_control", "system", "context_management"] {
            assert!(h.get(field).is_none(), "{field}");
        }

        let none = serde_json::value::RawValue::from_string("[]".into()).unwrap();
        let req = Request::new("ollama/m", Some("xhigh"), "SYS", &none);
        let h = head(&req, "m", &Provider { max_tokens: Some(4096), ..provider() });
        assert!(h.get("tools").is_none(), "no empty tool list");
        assert_eq!((h["max_tokens"].as_u64(), h["reasoning_effort"].as_str()), (Some(4096), Some("high")));
    }

    #[test]
    fn the_body_is_one_valid_request_of_exactly_the_length_promised() {
        let head = serde_json::to_vec(&json!({ "model": "m", "stream": true })).unwrap();
        let history = [
            r#"{"role":"user","content":[{"type":"text","text":"go \u00e9"}]}"#,
            r#"{"role":"assistant","content":[{"type":"thinking","thinking":"x"},{"type":"tool_use","id":"t1","name":"bash","input":{"command":"ls"}}]}"#,
            r#"{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"a\nb"}]}"#,
            r#"{"role":"user","content":[]}"#,
            r#"{"role":"assistant","content":[{"type":"text","text":"done"}]}"#,
        ];
        let kept = Mutex::new(Translated::default());
        for (system, count) in [("SYS", 5), ("", 4)] {
            let mut body = body(&head, system, &history, &kept).unwrap();
            let mut out = Vec::new();
            // Read through a tiny buffer, as a slow connection would.
            let mut buf = [0u8; 7];
            loop {
                let n = body.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                out.extend_from_slice(&buf[..n]);
            }
            assert_eq!(out.len(), body.len, "system {system:?}");
            let v: Value = serde_json::from_slice(&out).unwrap();
            let roles: Vec<&str> =
                v["messages"].as_array().unwrap().iter().map(|m| m["role"].as_str().unwrap()).collect();
            assert_eq!(roles.len(), count);
            assert_eq!(&roles[roles.len() - 4..], ["user", "assistant", "tool", "assistant"]);
            assert_eq!((v["model"].as_str(), v["stream"].as_bool()), (Some("m"), Some(true)));
        }
        // No messages at all is still a valid request.
        let mut empty = Vec::new();
        body(&head, "", &[] as &[&str], &kept).unwrap().read_to_end(&mut empty).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&empty).unwrap()["messages"], json!([]));
        // A message that isn't JSON is an error, not a broken request.
        assert!(body(&head, "", &["not json"], &kept).is_err());
    }

    #[test]
    fn saved_messages_are_translated_once_and_then_read_from_disk() {
        crate::tools::test_home();
        let dir = crate::tools::test_dir("openai-kept");
        let lines = [
            r#"{"role":"user","content":[{"type":"text","text":"first"}]}"#,
            r#"{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"bash","input":{"command":"ls"}}]}"#,
            r#"{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"out"}]}"#,
            r#"{"role":"user","content":[]}"#,
        ];
        let path = dir.join("session.jsonl");
        std::fs::write(&path, lines.join("\n")).unwrap();
        let file = File::open(&path).unwrap();
        let mut at = 0;
        let mut saved: Vec<Text> = Vec::new();
        for line in lines {
            saved.push(Text::Saved { file: &file, at, len: line.len() });
            at += line.len() as u64 + 1;
        }
        let head = serde_json::to_vec(&json!({ "model": "m" })).unwrap();
        let kept = Mutex::new(Translated::default());
        let read = |source: &[Text]| {
            let mut out = Vec::new();
            let mut b = body(&head, "", source, &kept).unwrap();
            b.read_to_end(&mut out).unwrap();
            assert_eq!(out.len(), b.len);
            serde_json::from_slice::<Value>(&out).unwrap()
        };
        let first = read(&saved);
        let roles: Vec<&str> =
            first["messages"].as_array().unwrap().iter().map(|m| m["role"].as_str().unwrap()).collect();
        assert_eq!(roles, ["user", "assistant", "tool"], "the empty message adds nothing, and no stray comma");
        assert_eq!(kept.lock().unwrap().at.len(), 4, "each saved message is kept, the empty one too");
        let size = kept.lock().unwrap().end;

        // Asked again, with a newer unsaved message after them: nothing is translated twice.
        let mut more = saved.clone();
        more.push(Text::Memory(r#"{"role":"assistant","content":[{"type":"text","text":"newest"}]}"#));
        let second = read(&more);
        assert_eq!(second["messages"].as_array().unwrap()[..3], first["messages"].as_array().unwrap()[..]);
        assert_eq!(second["messages"][3]["content"], "newest");
        // (One lock at a time: two in one expression would wait on each other.)
        let entries = kept.lock().unwrap().at.len();
        assert_eq!((entries, kept.lock().unwrap().end), (4, size));

        // With nowhere to keep them it still works, translating each time.
        let nowhere = Mutex::new(Translated { unusable: true, ..Translated::default() });
        let mut out = Vec::new();
        body(&head, "", &saved, &nowhere).unwrap().read_to_end(&mut out).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&out).unwrap(), first);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn parse(stream: &str) -> (Result<Reply>, Vec<String>) {
        let mut seen = Vec::new();
        let reply = read(stream.as_bytes(), &AtomicBool::new(false), Duration::from_secs(5), &mut |c| {
            seen.push(match c {
                Chunk::Text(t) => format!("text:{t}"),
                Chunk::Thinking(t) => format!("think:{t}"),
                Chunk::ToolStart(n) => format!("tool:{n}"),
                Chunk::ToolInput(n) => format!("input:{n}"),
                Chunk::BlockEnd => "end".into(),
                Chunk::ServerTool(_) => "server".into(),
            })
        });
        (reply, seen)
    }

    fn lines(chunks: &[Value]) -> String {
        chunks.iter().map(|c| format!("data: {c}\n\n")).collect::<String>() + "data: [DONE]\n\n"
    }

    fn delta(d: Value, finish: Option<&str>) -> Value {
        json!({ "choices": [{ "index": 0, "delta": d, "finish_reason": finish }] })
    }

    #[test]
    fn a_reply_with_thinking_text_and_two_calls_is_read() {
        let stream = lines(&[
            delta(json!({ "role": "assistant", "content": "" }), None),
            delta(json!({ "reasoning_content": "let me see" }), None),
            delta(json!({ "content": "I'll " }), None),
            delta(json!({ "content": "check." }), None),
            delta(
                json!({ "tool_calls": [{ "index": 0, "id": "c1", "type": "function", "function": { "name": "read", "arguments": "" } }] }),
                None,
            ),
            delta(json!({ "tool_calls": [{ "index": 0, "function": { "arguments": "{\"path\":" } }] }), None),
            delta(
                json!({ "tool_calls": [{ "index": 1, "id": "c2", "function": { "name": "bash", "arguments": "{\"command\":\"ls\"}" } }] }),
                None,
            ),
            delta(json!({ "tool_calls": [{ "index": 0, "function": { "arguments": "\"a.rs\"}" } }] }), None),
            delta(json!({}), Some("tool_calls")),
            json!({ "choices": [], "usage": { "prompt_tokens": 120, "completion_tokens": 30, "prompt_tokens_details": { "cached_tokens": 100 } } }),
        ]);
        let (reply, seen) = parse(&stream);
        let reply = reply.unwrap();
        assert_eq!(reply.stop_reason, "tool_use");
        assert_eq!(reply.content[0], json!({ "type": "text", "text": "I'll check." }));
        assert_eq!(
            reply.content[1],
            json!({ "type": "tool_use", "id": "c1", "name": "read", "input": { "path": "a.rs" } })
        );
        assert_eq!(reply.content[2]["input"]["command"], "ls");
        assert_eq!(reply.content.len(), 3, "thinking isn't kept: it can't be sent back");
        assert_eq!(
            (reply.usage.input_tokens, reply.usage.cache_read_input_tokens, reply.usage.output_tokens),
            (20, 100, 30)
        );
        assert_eq!(seen[..4], ["think:let me see", "text:I'll ", "text:check.", "end"]);
        assert!(seen.contains(&"tool:read".to_string()) && seen.contains(&"tool:bash".to_string()));
        assert_eq!(seen.last().unwrap(), "end");
    }

    #[test]
    fn odd_servers_are_understood() {
        // Whole calls with no index and no id; "reasoning" for thinking; no usage; no [DONE].
        let stream = format!(
            "data: {}\n\ndata: {}\n\ndata: {}\n\n",
            delta(json!({ "reasoning": "hm" }), None),
            delta(
                json!({ "tool_calls": [{ "function": { "name": "bash", "arguments": "{\"command\":\"pwd\"}" } }, { "function": { "name": "read", "arguments": "" } }] }),
                None
            ),
            delta(json!({}), Some("stop")),
        );
        let (reply, _) = parse(&stream);
        let reply = reply.unwrap();
        assert_eq!(reply.stop_reason, "tool_use", "calls were made, whatever the server calls the ending");
        assert_eq!(
            (reply.content[0]["id"].as_str(), reply.content[0]["input"]["command"].as_str()),
            (Some("call_0"), Some("pwd"))
        );
        assert_eq!((reply.content[1]["id"].as_str(), &reply.content[1]["input"]), (Some("call_1"), &json!({})));

        // Input that isn't JSON goes back to the model to try again.
        let stream = lines(&[delta(
            json!({ "tool_calls": [{ "index": 0, "id": "x", "function": { "name": "bash", "arguments": "{oops" } }] }),
            Some("tool_calls"),
        )]);
        assert!(parse(&stream).0.unwrap().content[0]["input"]["_invalid_json"].is_string());

        // Plain text, cut off by the length limit, and a refusal.
        for (finish, want) in [("stop", "end_turn"), ("length", "max_tokens"), ("content_filter", "refusal")] {
            let stream = lines(&[delta(json!({ "content": "Hello" }), None), delta(json!({}), Some(finish))]);
            let (reply, seen) = parse(&stream);
            assert_eq!(reply.unwrap().stop_reason, want);
            assert_eq!(seen, ["text:Hello", "end"]);
        }
    }

    #[test]
    fn broken_streams_are_errors_and_retried_only_before_anything_was_shown() {
        let retryable = |stream: &str| parse(stream).0.err().is_some_and(|e| e.is::<Retryable>());
        assert!(retryable(""), "nothing at all");
        assert!(retryable("data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n\n"));
        assert!(retryable("data: {\"error\":{\"message\":\"overloaded\"}}\n\n"));
        // Once text has been shown, asking again would show it twice.
        let shown = format!("data: {}\n\n", delta(json!({ "content": "Hel" }), None));
        let (reply, _) = parse(&shown);
        let err = reply.err().unwrap();
        assert!(!err.is::<Retryable>() && err.to_string().contains("ended early"), "{err}");
        let (reply, _) = parse(&format!("{shown}data: {{\"error\":{{\"message\":\"boom\"}}}}\n\n"));
        assert_eq!(reply.err().unwrap().to_string(), "API error: boom");
        assert!(parse("data: not json\n\n").0.is_err());
    }
}
