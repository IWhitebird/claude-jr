// Anthropic Messages API: one streaming request, parsed from SSE into content blocks.

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant};

const VERSION: &str = "2026-10-21";
const BETA_FALLBACK: &str = "server-side-fallback-2026-07-01";
const BETA_UPDATES: &str = "thinking-display-updates-2026-08-18";
const BETA_COMPACT: &str = "compact-2026-01-12";
const BETA_MCP: &str = "mcp-client-2025-11-20";
const MAX_ATTEMPTS: u32 = 5;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// One blocking read gives up after this. It also covers the wait for the
/// reply to begin, which the library can't pick up again after a timeout (the
/// request would be sent, and billed, a second time), so it has to be longer
/// than the model ever takes to start answering.
const READ_TIMEOUT: Duration = Duration::from_secs(60);
/// How long to keep trying when the API can't be reached at all (no network,
/// DNS failing, connection refused): long enough to ride out a Wi-Fi drop.
const OFFLINE_PATIENCE: Duration = Duration::from_secs(120);
/// A stream with no bytes at all (not even pings) for this long is dead.
const SILENCE_LIMIT: Duration = Duration::from_secs(120);

/// What a model supports beyond the basic Messages API.
#[derive(Clone, Copy)]
pub struct Caps {
    /// Server-side refusal fallback (`fallbacks: "default"`).
    pub fallback: bool,
    /// Progress notes between tool calls as visible thinking text.
    pub updates: bool,
    /// Server-side compaction near the context limit.
    pub compact: bool,
    /// Context window in tokens.
    pub window: u64,
    /// Anthropic's web search and fetch tools (the 2026-02-09 versions).
    pub web: bool,
}

impl Caps {
    pub fn of(model: &str) -> Caps {
        let (fallback, updates, compact) = match model {
            "claude-opus-5-5" | "claude-sonnet-5-5" | "claude-fable-5-2" | "claude-fable-5-1" => (true, true, true),
            "claude-opus-5" => (true, false, true),
            "claude-fable-5" => (false, true, true),
            "claude-haiku-5-5" | "claude-sonnet-5" | "claude-opus-4-8" | "claude-opus-4-7" | "claude-opus-4-6"
            | "claude-sonnet-4-6" => (false, false, true),
            // A newer version than this build knows: assume what its family
            // has, but leave out the refusal fallback, which is per model and
            // makes the API reject the request where it isn't supported.
            m if family(m) == Some("haiku") => (false, false, true),
            m if family(m).is_some() => (false, true, true),
            _ => (false, false, false),
        };
        let window = if model.starts_with("claude-haiku-4") { 200_000 } else { 1_000_000 };
        // Known models by name; newer Opus and Sonnet versions by family.
        let web = matches!(
            model,
            "claude-opus-5-5"
                | "claude-opus-5"
                | "claude-opus-4-8"
                | "claude-opus-4-7"
                | "claude-opus-4-6"
                | "claude-sonnet-5-5"
                | "claude-sonnet-5"
                | "claude-sonnet-4-6"
                | "claude-fable-5-2"
                | "claude-fable-5-1"
                | "claude-fable-5"
        ) || matches!(family(model), Some("opus" | "sonnet"));
        Caps { fallback, updates, compact, window, web }
    }

    fn betas(self) -> String {
        let all = [(self.fallback, BETA_FALLBACK), (self.updates, BETA_UPDATES), (self.compact, BETA_COMPACT)];
        all.iter().filter(|(on, _)| *on).map(|(_, b)| *b).collect::<Vec<_>>().join(",")
    }
}

/// Models /model offers, with what each is good for. Any other model id
/// works too with /model <id>.
pub const CATALOGUE: [(&str, &str); 7] = [
    ("claude-opus-5-5", "default; strong at long coding tasks"),
    ("claude-sonnet-5-5", "fast, half the price; everyday edits"),
    ("claude-haiku-5-5", "fastest and cheapest; small changes"),
    ("claude-fable-5-2", "most capable; hardest problems"),
    ("claude-opus-5", "previous Opus"),
    ("claude-sonnet-5", "previous Sonnet"),
    ("claude-opus-4-8", "older Opus"),
];

/// "$4/$20 · 1M context": dollars per million tokens in and out.
pub fn describe(model: &str) -> String {
    let window = match Caps::of(model).window {
        w if w >= 1_000_000 => format!("{}M", w / 1_000_000),
        w => format!("{}k", w / 1000),
    };
    match price(model) {
        Some((i, o, _)) => format!("${i}/${o} · {window} context"),
        None => format!("{window} context"),
    }
}

/// "opus", "sonnet", "haiku" or "fable" for a Claude 4.6-or-later model id
/// (Mythos counts as Fable), including versions newer than this build.
fn family(model: &str) -> Option<&'static str> {
    let rest = model.strip_prefix("claude-")?;
    let (name, version) = rest.split_once('-')?;
    let major: u32 = version.split('-').next()?.parse().ok()?;
    if major < 5 && !matches!(rest, "opus-4-6" | "opus-4-7" | "opus-4-8" | "sonnet-4-6") {
        return None;
    }
    Some(match name {
        "opus" => "opus",
        "sonnet" => "sonnet",
        "haiku" => "haiku",
        "fable" | "mythos" => "fable",
        _ => return None,
    })
}

/// Dollars per million tokens: input, output, cache read. Cache writes are 1.25x input.
/// Unknown newer versions are priced like the latest one this build knows
/// in their family: an estimate, until the table is updated.
fn price(model: &str) -> Option<(f64, f64, f64)> {
    Some(match model {
        "claude-opus-5-5" => (4.0, 20.0, 0.20),
        "claude-sonnet-5-5" => (2.0, 10.0, 0.10),
        "claude-sonnet-5" => (2.0, 10.0, 0.20),
        "claude-fable-5-2" => (8.0, 40.0, 0.20),
        "claude-haiku-5-5" => (0.10, 0.50, 0.01),
        "claude-fable-5-1" => (10.0, 50.0, 0.25),
        "claude-fable-5" => (10.0, 50.0, 1.0),
        "claude-opus-5" | "claude-opus-4-8" | "claude-opus-4-7" | "claude-opus-4-6" => (5.0, 25.0, 0.50),
        "claude-sonnet-4-6" => (3.0, 15.0, 0.30),
        "claude-haiku-4-5" => (1.0, 5.0, 0.10),
        m => {
            return price(match family(m)? {
                "opus" => "claude-opus-5-5",
                "sonnet" => "claude-sonnet-5-5",
                "haiku" => "claude-haiku-5-5",
                _ => "claude-fable-5-2",
            });
        }
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
    /// Remote MCP servers for the API's connector.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<&'a RawValue>,
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
            mcp_servers: None,
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
    /// A finished block from a tool that ran on Anthropic's side (web search
    /// and fetch, remote MCP): the call, then its result.
    ServerTool(&'a Value),
    BlockEnd,
}

/// The stream broke before any content arrived (an "overloaded" event, a
/// dropped connection): safe to send the request again.
#[derive(Debug)]
struct Retryable(String);

impl std::fmt::Display for Retryable {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Retryable {}

#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

pub struct Client {
    /// Replaced after Esc: shutting a connection down closes the whole agent.
    http: std::sync::Mutex<ureq::Agent>,
    read: Duration,
    url: String,
    /// Empty until the user logs in. Behind a lock so /login applies at once.
    key: std::sync::RwLock<String>,
    silence: Duration,
    offline: Duration,
}

impl Client {
    pub fn new(key: String, base: Option<String>) -> Self {
        Client::with_read_timeout(key, base, READ_TIMEOUT)
    }

    fn with_read_timeout(key: String, base: Option<String>, read: Duration) -> Self {
        let base = base.unwrap_or_else(|| "https://api.anthropic.com".into());
        Client {
            http: std::sync::Mutex::new(agent(read)),
            read,
            url: format!("{}/v1/messages", base.trim_end_matches('/')),
            key: std::sync::RwLock::new(key),
            silence: SILENCE_LIMIT,
            // CLAUDE_JR_OFFLINE_SECS: for scripts that would rather fail fast.
            offline: std::env::var("CLAUDE_JR_OFFLINE_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .map_or(OFFLINE_PATIENCE, Duration::from_secs),
        }
    }

    pub fn set_key(&self, key: String) {
        *self.key.write().unwrap() = key;
    }

    pub fn has_key(&self) -> bool {
        !self.key.read().unwrap().is_empty()
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
        let mut betas = Caps::of(req.model).betas();
        if req.mcp_servers.is_some() {
            betas = if betas.is_empty() { BETA_MCP.to_string() } else { format!("{betas},{BETA_MCP}") };
        }
        let key = self.key.read().unwrap().clone();
        if key.is_empty() {
            bail!("no API key yet: type /login to add one");
        }
        let http = self.http.lock().unwrap().clone();
        // Esc must stop a request at once, even while it waits for the reply
        // to begin, which no read timeout can pick up again: a watcher closes
        // the connection, and a fresh agent is put in place for the next one.
        let done = AtomicBool::new(false);
        let result = std::thread::scope(|s| {
            s.spawn(|| {
                while !done.load(Ordering::Relaxed) {
                    if cancel.load(Ordering::Relaxed) {
                        *self.http.lock().unwrap() = agent(self.read);
                        http.clone().shutdown();
                        return;
                    }
                    sleep(Duration::from_millis(50));
                }
            });
            let result = self.send(&http, &head, &betas, &key, messages, cancel, &mut on);
            done.store(true, Ordering::Relaxed);
            result
        });
        // Whatever the closed connection made the request fail with, it was Esc.
        if cancel.load(Ordering::Relaxed) && result.is_err() {
            return Err(Cancelled.into());
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn send<M: AsRef<str>>(
        &self,
        http: &ureq::Agent,
        head: &[u8],
        betas: &str,
        key: &str,
        messages: &[M],
        cancel: &AtomicBool,
        on: &mut impl FnMut(Chunk),
    ) -> Result<Reply> {
        let mut attempt = 0;
        // Since when the API hasn't been reachable, and how often that was tried.
        let mut offline: Option<(Instant, u32)> = None;
        loop {
            attempt += 1;
            let mut call = http
                .post(&self.url)
                .header("x-api-key", key)
                .header("anthropic-version", VERSION)
                .header("content-type", "application/json");
            if !betas.is_empty() {
                call = call.header("anthropic-beta", betas);
            }

            let mut body = Body::new(head, messages);
            call = call.header("content-length", body.len.to_string());
            let mut res = match call.send(ureq::SendBody::from_reader(&mut body)) {
                Ok(r) => r,
                // The request went out and no reply began in time: it may have
                // been worked on (and billed), so this counts as an attempt.
                Err(ureq::Error::Timeout(_)) if attempt < MAX_ATTEMPTS => {
                    wait(backoff(attempt, None), cancel)?;
                    continue;
                }
                Err(e @ ureq::Error::Timeout(_)) => return Err(e.into()),
                // The API couldn't be reached at all, so nothing was sent:
                // keep trying for a while without using up attempts.
                Err(e) => {
                    let (since, tries) = offline.get_or_insert((Instant::now(), 0));
                    if since.elapsed() >= self.offline {
                        bail!("can't reach the API after {}s: {e}", since.elapsed().as_secs());
                    }
                    *tries += 1;
                    wait(backoff(*tries, None).min(Duration::from_secs(4)), cancel)?;
                    attempt -= 1;
                    continue;
                }
            };
            offline = None;

            let status = res.status().as_u16();
            if status == 200 {
                let reader = BufReader::new(res.body_mut().as_reader());
                match read_sse(reader, cancel, self.silence, on) {
                    Err(e) if e.is::<Retryable>() && attempt < MAX_ATTEMPTS => {
                        wait(backoff(attempt, None), cancel)?;
                        continue;
                    }
                    result => return result,
                }
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

/// The request body, read in order without copying the history into one
/// buffer: `{"messages":[` m1 `,` m2 ... `],` + the rest of the request.
/// Stored images are encoded from disk as they're read.
struct Body<'a> {
    parts: Vec<Part<'a>>,
    len: usize,
    part: usize,
    offset: usize,
}

enum Part<'a> {
    Bytes(&'a [u8]),
    Image(crate::images::Encoder),
}

impl<'a> Body<'a> {
    fn new<M: AsRef<str>>(head: &'a [u8], messages: &'a [M]) -> Self {
        use crate::images::{Encoder, Piece};
        let mut parts = Vec::with_capacity(messages.len() * 2 + 2);
        parts.push(Part::Bytes(b"{\"messages\":["));
        for (i, m) in messages.iter().enumerate() {
            if i > 0 {
                parts.push(Part::Bytes(b","));
            }
            for piece in crate::images::pieces(m.as_ref()) {
                parts.push(match piece {
                    Piece::Text(t) => Part::Bytes(t),
                    Piece::Image(path) => Part::Image(Encoder::open(&path)),
                });
            }
        }
        parts.push(Part::Bytes(b"],"));
        parts.push(Part::Bytes(&head[1..])); // the request object without its opening brace
        let len = parts
            .iter()
            .map(|p| match p {
                Part::Bytes(b) => b.len(),
                Part::Image(e) => e.len,
            })
            .sum();
        Body { parts, len, part: 0, offset: 0 }
    }
}

impl std::io::Read for Body<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        while let Some(p) = self.parts.get_mut(self.part) {
            let n = match p {
                Part::Bytes(b) => {
                    let rest = &b[self.offset..];
                    let n = rest.len().min(buf.len());
                    buf[..n].copy_from_slice(&rest[..n]);
                    self.offset += n;
                    n
                }
                Part::Image(e) => e.read(buf)?,
            };
            if n > 0 {
                return Ok(n);
            }
            self.part += 1;
            self.offset = 0;
        }
        Ok(0)
    }
}

fn agent(read: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_per_read(Some(read))
        .build()
        .into()
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
fn read_sse(mut r: impl BufRead, cancel: &AtomicBool, silence: Duration, on: &mut impl FnMut(Chunk)) -> Result<Reply> {
    let mut emitted = false;
    let mut on = |c: Chunk| {
        emitted = true;
        on(c)
    };
    let mut content: Vec<Value> = Vec::new();
    let mut json_buf = String::new();
    let mut stop_reason = String::new();
    let mut usage = Usage::default();

    let mut raw = Vec::new();
    let mut last_byte = Instant::now();
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Cancelled.into());
        }
        // `raw` keeps a partly read line across read timeouts.
        let before = raw.len();
        match r.read_until(b'\n', &mut raw) {
            Ok(0) if raw.is_empty() => break,
            Ok(_) => {}
            Err(e) if timed_out(&e) => {
                if raw.len() != before {
                    last_byte = Instant::now();
                }
                if last_byte.elapsed() < silence {
                    continue;
                }
                let msg = format!("the stream went silent for {}s", silence.as_secs());
                return Err(if emitted { anyhow!(msg) } else { Retryable(msg).into() });
            }
            Err(e) => {
                let msg = format!("the connection broke: {e}");
                return Err(if emitted { anyhow!(msg) } else { Retryable(msg).into() });
            }
        }
        last_byte = Instant::now();
        if raw.last() != Some(&b'\n') {
            // end of stream in the middle of a line
            raw.clear();
            break;
        }
        let line = String::from_utf8_lossy(&raw).into_owned();
        raw.clear();
        let line = line.trim_end_matches(['\n', '\r']);
        let Some(data) = line.strip_prefix("data:") else { continue };
        let mut ev: Value = serde_json::from_str(data.trim_start())?;

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
            "message_stop" => return Ok(Reply { content, stop_reason, usage }),
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

/// A read that hit its time limit. ureq reports its own timeouts as
/// `ErrorKind::Other` wrapping `ureq::Error::Timeout`.
fn timed_out(e: &std::io::Error) -> bool {
    use std::io::ErrorKind::{TimedOut, WouldBlock};
    matches!(e.kind(), TimedOut | WouldBlock)
        || e.get_ref()
            .and_then(|i| i.downcast_ref::<ureq::Error>())
            .is_some_and(|u| matches!(u, ureq::Error::Timeout(_)))
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
        let reply = read_sse(sse.as_bytes(), &AtomicBool::new(false), SILENCE_LIMIT, &mut |c| match c {
            Chunk::Text(t) => text.push_str(t),
            Chunk::Thinking(t) => thinking.push_str(t),
            Chunk::ToolStart(n) => tools.push(n.to_string()),
            Chunk::ToolInput(_) | Chunk::ServerTool(_) => {}
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
    fn newer_versions_use_their_family() {
        let caps = Caps::of("claude-fable-5-3");
        assert!(caps.updates && caps.compact && !caps.fallback && !caps.web);
        assert_eq!(cost("claude-fable-5-3", &Usage { input_tokens: 1_000_000, ..Default::default() }), Some(8.0));
        assert!(Caps::of("claude-sonnet-5-7").web);
        assert!(!Caps::of("claude-haiku-5-6").updates);
        assert_eq!(cost("gpt-9", &Usage::default()), None);
        assert_eq!(family("claude-opus-4-5"), None);
    }

    #[test]
    fn errors_before_any_content_can_be_retried() {
        let overloaded = "data: {\"type\":\"error\",\"error\":{\"message\":\"Overloaded\"}}\n";
        let err = read_sse(overloaded.as_bytes(), &AtomicBool::new(false), SILENCE_LIMIT, &mut |_| {})
            .err()
            .expect("an error");
        assert!(err.is::<Retryable>());
        let late =
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n"
                .to_string() + overloaded;
        let err =
            read_sse(late.as_bytes(), &AtomicBool::new(false), SILENCE_LIMIT, &mut |_| {}).err().expect("an error");
        assert!(!err.is::<Retryable>(), "text was already shown");
    }

    #[test]
    fn caps_and_cost() {
        assert_eq!(Caps::of("claude-opus-5-5").betas(), [BETA_FALLBACK, BETA_UPDATES, BETA_COMPACT].join(","));
        assert_eq!(Caps::of("something-else").betas(), "");
        let u = Usage { input_tokens: 1_000_000, output_tokens: 1_000_000, ..Default::default() };
        assert_eq!(cost("claude-opus-5-5", &u), Some(24.0));
    }

    /// Serves one SSE event, then goes quiet with the connection open.
    fn stalled_server() -> (String, std::thread::JoinHandle<()>) {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut buf = [0u8; 8192];
            let _ = s.read(&mut buf);
            let ev = "data: {\"type\":\"message_start\",\"message\":{\"usage\":{}}}\n\n";
            let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n";
            s.write_all(format!("{head}{:x}\r\n{ev}\r\n", ev.len()).as_bytes()).unwrap();
            // hold the socket open until the client hangs up
            while s.read(&mut buf).map(|n| n > 0).unwrap_or(false) {}
        });
        (base, h)
    }

    /// Esc stops a request at once even while it waits for the reply to begin
    /// (no read timeout can be picked up again there), and the next request works.
    #[test]
    fn cancel_works_before_the_reply_begins() {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            // First request: read it, then say nothing until the client hangs up.
            let (mut s, _) = l.accept().unwrap();
            let mut buf = [0u8; 8192];
            let _ = s.read(&mut buf);
            while s.read(&mut buf).map(|n| n > 0).unwrap_or(false) {}
            // Second request: a normal reply.
            let (mut s, _) = l.accept().unwrap();
            let _ = s.read(&mut buf);
            let body = "data: {\"type\":\"message_start\",\"message\":{\"usage\":{}}}\n\n\
                data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
                data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"again\"}}\n\n\
                data: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
                data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n\
                data: {\"type\":\"message_stop\"}\n\n";
            let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n";
            let _ = s.write_all(format!("{head}content-length: {}\r\n\r\n{body}", body.len()).as_bytes());
        });
        // The normal client: a 60 s wait for the reply to begin.
        let client = Client::new("k".into(), Some(base));
        let tools = RawValue::from_string("[]".into()).unwrap();
        let req = Request::new("claude-opus-5-5", None, "", &tools);
        let cancel = AtomicBool::new(false);
        let started = Instant::now();
        let err = std::thread::scope(|sc| {
            sc.spawn(|| {
                sleep(Duration::from_millis(400));
                cancel.store(true, Ordering::Relaxed);
            });
            client.stream(&req, &[] as &[&str], &cancel, |_| {}).err().expect("an error")
        });
        assert!(err.is::<Cancelled>(), "{err}");
        assert!(started.elapsed() < Duration::from_secs(2), "took {:?}", started.elapsed());

        // The next request goes through on a fresh connection.
        let mut text = String::new();
        let reply = client.stream(&req, &[] as &[&str], &AtomicBool::new(false), |c| {
            if let Chunk::Text(t) = c {
                text.push_str(t);
            }
        });
        assert!(reply.is_ok(), "{:?}", reply.err());
        assert_eq!(text, "again");
        server.join().unwrap();
    }

    /// A network that is down for a while (longer than the few quick retries
    /// other failures get) is waited out, and nothing is sent until it is back.
    // 127.0.0.2 is loopback on Linux; elsewhere it may not exist.
    #[test]
    #[cfg_attr(not(target_os = "linux"), ignore)]
    fn a_network_outage_is_waited_out() {
        use std::io::{Read, Write};
        // A port with nothing behind it yet: connections are refused. On an
        // address no other test uses, so none can be given the port while it
        // is free and answer in the server's place.
        let l = std::net::TcpListener::bind("127.0.0.2:0").unwrap();
        let addr = l.local_addr().unwrap();
        drop(l);
        let server = std::thread::spawn(move || {
            sleep(Duration::from_millis(8500)); // longer than five quick retries take
            let l = std::net::TcpListener::bind(addr).unwrap();
            let (mut s, _) = l.accept().unwrap();
            let mut buf = [0u8; 8192];
            let _ = s.read(&mut buf);
            let body = "data: {\"type\":\"message_start\",\"message\":{\"usage\":{}}}\n\n\
                data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
                data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"back\"}}\n\n\
                data: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
                data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n\
                data: {\"type\":\"message_stop\"}\n\n";
            let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n";
            let _ = s.write_all(format!("{head}content-length: {}\r\n\r\n{body}", body.len()).as_bytes());
        });
        let mut client = Client::new("k".into(), Some(format!("http://{addr}")));
        client.offline = Duration::from_secs(60);
        let tools = RawValue::from_string("[]".into()).unwrap();
        let req = Request::new("claude-opus-5-5", None, "", &tools);
        let mut text = String::new();
        let started = Instant::now();
        let reply = client.stream(&req, &[] as &[&str], &AtomicBool::new(false), |c| {
            if let Chunk::Text(t) = c {
                text.push_str(t);
            }
        });
        assert!(reply.is_ok(), "{:?}", reply.err());
        assert_eq!(text, "back");
        assert!(started.elapsed() >= Duration::from_secs(8));
        server.join().unwrap();

        // With no patience left it gives up, and says how long it tried.
        let mut client = Client::new("k".into(), Some("http://127.0.0.1:9".into()));
        client.offline = Duration::from_millis(600);
        let err = client.stream(&req, &[] as &[&str], &AtomicBool::new(false), |_| {}).err().unwrap();
        assert!(err.to_string().starts_with("can't reach the API after"), "{err}");
    }

    /// The model can take a while to begin its reply. Giving up on that wait
    /// re-sends the request, which is slow and is billed again.
    #[test]
    fn a_slow_start_is_waited_for_not_sent_again() {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = requests.clone();
        std::thread::spawn(move || {
            for mut s in l.incoming().flatten() {
                seen.fetch_add(1, Ordering::Relaxed);
                let mut buf = [0u8; 8192];
                let _ = s.read(&mut buf);
                sleep(Duration::from_millis(3500));
                let body = "data: {\"type\":\"message_start\",\"message\":{\"usage\":{}}}\n\n\
                    data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
                    data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"late\"}}\n\n\
                    data: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
                    data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n\
                    data: {\"type\":\"message_stop\"}\n\n";
                let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n";
                let _ = s.write_all(format!("{head}content-length: {}\r\n\r\n{body}", body.len()).as_bytes());
            }
        });
        let client = Client::new("k".into(), Some(base));
        let tools = RawValue::from_string("[]".into()).unwrap();
        let req = Request::new("claude-opus-5-5", None, "", &tools);
        let mut text = String::new();
        let reply = client
            .stream(&req, &[] as &[&str], &AtomicBool::new(false), |c| {
                if let Chunk::Text(t) = c {
                    text.push_str(t);
                }
            })
            .expect("the reply arrives");
        assert_eq!(text, "late");
        assert_eq!(reply.stop_reason, "end_turn");
        assert_eq!(requests.load(Ordering::Relaxed), 1, "the request was sent once");
    }

    #[test]
    fn cancel_works_while_the_stream_stalls() {
        let (base, server) = stalled_server();
        let client = Client::with_read_timeout("k".into(), Some(base), Duration::from_secs(1));
        let tools = RawValue::from_string("[]".into()).unwrap();
        let req = Request::new("claude-opus-5-5", None, "", &tools);
        let cancel = AtomicBool::new(false);
        let start = Instant::now();
        let err = std::thread::scope(|sc| {
            sc.spawn(|| {
                sleep(Duration::from_millis(500));
                cancel.store(true, Ordering::Relaxed);
            });
            client.stream(&req, &[] as &[&str], &cancel, |_| {}).err().expect("an error")
        });
        assert!(err.is::<Cancelled>(), "{err}");
        assert!(start.elapsed() < Duration::from_secs(5), "took {:?}", start.elapsed());
        drop(client);
        server.join().unwrap();
    }

    #[test]
    fn a_silent_stream_times_out_as_retryable() {
        use std::io::Write;
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            s.write_all(b"data: {\"type\":\"ping\"}\n").unwrap();
            sleep(Duration::from_secs(3));
        });
        let sock = std::net::TcpStream::connect(addr).unwrap();
        sock.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        let start = Instant::now();
        let err = read_sse(BufReader::new(sock), &AtomicBool::new(false), Duration::from_millis(700), &mut |_| {})
            .err()
            .expect("an error");
        assert!(err.is::<Retryable>(), "{err}");
        assert!(start.elapsed() < Duration::from_secs(3));
        server.join().unwrap();
    }

    #[test]
    fn a_broken_stream_is_retryable_only_before_output() {
        struct Broken(&'static [u8]);
        impl std::io::Read for Broken {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.0.is_empty() {
                    return Err(std::io::ErrorKind::ConnectionReset.into());
                }
                let n = self.0.len().min(buf.len());
                buf[..n].copy_from_slice(&self.0[..n]);
                self.0 = &self.0[n..];
                Ok(n)
            }
        }
        let run = |data: &'static [u8]| {
            read_sse(BufReader::new(Broken(data)), &AtomicBool::new(false), SILENCE_LIMIT, &mut |_| {})
                .err()
                .expect("an error")
        };
        assert!(run(b"data: {\"type\":\"ping\"}\n").is::<Retryable>());
        let text = b"data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"name\":\"x\"}}\n";
        assert!(!run(text).is::<Retryable>());
    }

    #[test]
    fn reading_resumes_after_read_timeouts() {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut buf = [0u8; 8192];
            let _ = s.read(&mut buf);
            let chunk = |s: &mut std::net::TcpStream, d: &str| {
                s.write_all(format!("{:x}\r\n{d}\r\n", d.len()).as_bytes()).unwrap();
                s.flush().unwrap();
            };
            s.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n")
                .unwrap();
            chunk(&mut s, "data: {\"type\":\"message_start\",\"message\":{\"usage\":{}}}\n\n");
            sleep(Duration::from_secs(4));
            chunk(
                &mut s,
                "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hel",
            );
            sleep(Duration::from_millis(3500));
            chunk(
                &mut s,
                "lo\"}}\n\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\ndata: {\"type\":\"message_stop\"}\n\n",
            );
            s.write_all(b"0\r\n\r\n").unwrap();
        });
        let client = Client::with_read_timeout("k".into(), Some(base), Duration::from_secs(1));
        let tools = RawValue::from_string("[]".into()).unwrap();
        let req = Request::new("claude-opus-5-5", None, "", &tools);
        let mut text = String::new();
        let reply = client
            .stream(&req, &[] as &[&str], &AtomicBool::new(false), |c| {
                if let Chunk::Text(t) = c {
                    text.push_str(t)
                }
            })
            .expect("the stream should survive read timeouts");
        assert_eq!(text, "hello");
        assert_eq!(reply.stop_reason, "end_turn");
        server.join().unwrap();
    }

    use std::sync::{Arc, Mutex};

    /// What the scripted server saw: the head of each request, lower-cased.
    type Heads = Arc<Mutex<Vec<String>>>;

    /// A server that answers request n with `responses[n]` (status, extra
    /// header lines, body) and records each request's head.
    fn scripted(responses: Vec<(u16, &'static str, String)>) -> (String, Heads) {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let heads: Heads = Arc::default();
        let seen = heads.clone();
        std::thread::spawn(move || {
            let mut responses = responses.into_iter();
            for stream in l.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut out = stream;
                'requests: loop {
                    let (mut head, mut len) = (String::new(), 0);
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 {
                            break 'requests;
                        }
                        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                            len = v.trim().parse().unwrap_or(0);
                        }
                        let end = line == "\r\n";
                        head.push_str(&line.to_ascii_lowercase());
                        if end {
                            break;
                        }
                    }
                    let mut body = vec![0; len];
                    let _ = reader.read_exact(&mut body);
                    head.push_str(&String::from_utf8_lossy(&body));
                    seen.lock().unwrap().push(head);
                    let Some((status, extra, body)) = responses.next() else { return };
                    let reply = format!("HTTP/1.1 {status} X\r\n{extra}content-length: {}\r\n\r\n{body}", body.len());
                    let _ = out.write_all(reply.as_bytes());
                }
            }
        });
        (base, heads)
    }

    fn hello_sse() -> String {
        let events = [
            r#"{"type":"message_start","message":{"usage":{"input_tokens":1}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#,
            r#"{"type":"message_stop"}"#,
        ];
        events.iter().map(|e| format!("data: {e}\n\n")).collect()
    }

    const SSE: &str = "content-type: text/event-stream\r\n";
    const JSON: &str = "content-type: application/json\r\n";

    fn ask(client: &Client, model: &str) -> Result<Reply> {
        let tools = RawValue::from_string("[]".into()).unwrap();
        let req = Request::new(model, None, "sys", &tools);
        client.stream(&req, &[r#"{"role":"user","content":"hi"}"#], &AtomicBool::new(false), |_| {})
    }

    #[test]
    fn a_missing_key_is_reported_before_anything_is_sent() {
        let (base, heads) = scripted(vec![]);
        let client = Client::new(String::new(), Some(base));
        assert!(!client.has_key());
        let err = ask(&client, "claude-opus-5-5").err().unwrap();
        assert_eq!(err.to_string(), "no API key yet: type /login to add one");
        client.set_key("k".into());
        assert!(client.has_key());
        assert!(heads.lock().unwrap().is_empty());
    }

    #[test]
    fn rate_limits_are_retried_after_the_time_the_server_asks_for() {
        let limited = (
            429,
            "retry-after: 0\r\ncontent-type: application/json\r\n",
            r#"{"error":{"message":"slow down"}}"#.to_string(),
        );
        let (base, heads) = scripted(vec![limited, (200, SSE, hello_sse())]);
        let reply = ask(&Client::new("k".into(), Some(base)), "claude-opus-5-5").unwrap();
        assert_eq!(reply.content[0]["text"], "hi");
        assert_eq!(heads.lock().unwrap().len(), 2);
    }

    #[test]
    fn giving_up_after_five_attempts_shows_the_servers_message() {
        let limited = || {
            (
                429,
                "retry-after: 0\r\ncontent-type: application/json\r\n",
                r#"{"error":{"message":"slow down"}}"#.to_string(),
            )
        };
        let (base, heads) = scripted((0..5).map(|_| limited()).collect());
        let err = ask(&Client::new("k".into(), Some(base)), "claude-opus-5-5").err().unwrap();
        assert_eq!(err.to_string(), "API error 429: slow down");
        assert_eq!(heads.lock().unwrap().len(), 5);
    }

    #[test]
    fn other_errors_are_not_retried_and_odd_bodies_are_shortened() {
        let body = "x".repeat(900);
        let (base, heads) = scripted(vec![(400, "", body)]);
        let err = ask(&Client::new("k".into(), Some(base)), "claude-opus-5-5").err().unwrap();
        assert_eq!(err.to_string(), format!("API error 400: {}", "x".repeat(500)));
        assert_eq!(heads.lock().unwrap().len(), 1);
        assert_eq!(error_message(r#"{"error":{"message":"bad"}}"#), "bad");
        assert_eq!(error_message(r#"{"error":{}}"#), r#"{"error":{}}"#);
    }

    #[test]
    fn an_overloaded_stream_before_any_content_is_requested_again() {
        let overloaded = "data: {\"type\":\"error\",\"error\":{\"message\":\"Overloaded\"}}\n\n".to_string();
        let (base, heads) = scripted(vec![(200, SSE, overloaded), (200, SSE, hello_sse())]);
        let start = Instant::now();
        let reply = ask(&Client::new("k".into(), Some(base)), "claude-opus-5-5").unwrap();
        assert_eq!(reply.content[0]["text"], "hi");
        assert_eq!(heads.lock().unwrap().len(), 2);
        assert!(start.elapsed() >= Duration::from_millis(450), "it waited before retrying");
    }

    #[test]
    fn requests_carry_the_key_version_and_the_betas_the_model_has() {
        let (base, heads) = scripted(vec![(200, SSE, hello_sse()), (200, SSE, hello_sse()), (200, JSON, hello_sse())]);
        let client = Client::new("sk-test".into(), Some(format!("{base}/")));
        ask(&client, "claude-opus-5-5").unwrap();
        let mcp = RawValue::from_string(r#"[{"type":"url","url":"https://x.test","name":"x"}]"#.into()).unwrap();
        let tools = RawValue::from_string("[]".into()).unwrap();
        let mut req = Request::new("something-else", None, "", &tools);
        req.mcp_servers = Some(&mcp);
        client.stream(&req, &[] as &[&str], &AtomicBool::new(false), |_| {}).unwrap();
        req.model = "claude-haiku-5-5";
        client.stream(&req, &[] as &[&str], &AtomicBool::new(false), |_| {}).unwrap();

        let heads = heads.lock().unwrap();
        assert!(heads[0].starts_with("post /v1/messages http/1.1"), "{}", heads[0]);
        assert!(heads[0].contains("x-api-key: sk-test"));
        assert!(heads[0].contains(&format!("anthropic-version: {VERSION}")));
        assert!(heads[0].contains(&format!("anthropic-beta: {}", Caps::of("claude-opus-5-5").betas())));
        assert!(!heads[0].contains(BETA_MCP));
        assert!(heads[1].contains(&format!("anthropic-beta: {BETA_MCP}\r\n")), "only the connector beta: {}", heads[1]);
        assert!(heads[1].contains(r#""mcp_servers":[{"type":"url""#));
        assert!(heads[2].contains(&format!("{},{BETA_MCP}", BETA_COMPACT)), "{}", heads[2]);
    }

    #[test]
    fn a_stream_that_ends_early_says_so() {
        let run =
            |data: &str| read_sse(data.as_bytes(), &AtomicBool::new(false), SILENCE_LIMIT, &mut |_| {}).err().unwrap();
        let err = run("");
        assert!(err.is::<Retryable>() && err.to_string() == "the stream ended before any reply");
        let started = "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n";
        let err = run(started);
        assert!(!err.is::<Retryable>() && err.to_string() == "the stream ended early");
        // The connection dropped in the middle of a line.
        let err = run(&format!("{started}data: {{\"type\":\"content_bl"));
        assert_eq!(err.to_string(), "the stream ended early");
        let err = run("data: {not json}\n");
        assert!(err.to_string().contains("key must be a string"), "{err}");
    }

    #[test]
    fn waiting_between_attempts() {
        assert_eq!(backoff(1, None), Duration::from_millis(500));
        assert_eq!(backoff(3, None), Duration::from_secs(2));
        assert_eq!(backoff(7, None), Duration::from_secs(16), "capped");
        assert_eq!(backoff(1, Some(3)), Duration::from_secs(3));
        assert_eq!(backoff(1, Some(900)), Duration::from_secs(60), "a server's long wait is capped too");
        assert!(wait(Duration::ZERO, &AtomicBool::new(false)).is_ok());
        let start = Instant::now();
        assert!(wait(Duration::from_millis(250), &AtomicBool::new(false)).is_ok());
        assert!(start.elapsed() >= Duration::from_millis(250));
        let err = wait(Duration::from_secs(30), &AtomicBool::new(true)).err().unwrap();
        assert!(err.is::<Cancelled>());
        assert_eq!(Cancelled.to_string(), "cancelled");
        assert_eq!(Retryable("x".into()).to_string(), "x");
    }

    #[test]
    fn models_are_described_with_price_and_window() {
        assert_eq!(describe("claude-opus-5-5"), "$4/$20 \u{b7} 1M context");
        assert_eq!(describe("claude-haiku-4-5"), "$1/$5 \u{b7} 200k context");
        assert_eq!(describe("claude-sonnet-5-5"), "$2/$10 \u{b7} 1M context");
        assert_eq!(describe("my-own-model"), "1M context");
        for (id, _) in CATALOGUE {
            assert!(price(id).is_some(), "{id} has a price");
        }
        assert_eq!(family("claude-mythos-5-1"), Some("fable"));
        assert_eq!(family("claude-sonnet-4-5"), None);
        assert_eq!(family("claude-unknown-9"), None);
        assert_eq!(price("claude-unknown-9"), None);
        assert_eq!(price("claude-opus-9-1"), price("claude-opus-5-5"));
    }
}
