// Talking to models: one streaming request at a time, sent to whichever
// provider the model id names, retried when that is safe, and stopped at
// once when the user presses Esc.
//
//   mod.rs        the client: where a request goes, retries, cancelling
//   body.rs       the request body, streamed from memory, files and images
//   stream.rs     reading an event stream line by line
//   anthropic.rs  the Messages format
//   openai.rs     the Chat Completions format, translated both ways
//   provider.rs   which provider a model id names
//   cloud.rs      Claude through Amazon Bedrock or Google Vertex AI
//   bedrock.rs    Bedrock's address, login and request signing
//   vertex.rs     Vertex's address and access token
//   models.rs     prices, context sizes, what each Claude model supports

mod anthropic;
mod bedrock;
mod body;
pub mod cloud;
#[cfg(test)]
mod cloud_tests;
mod models;
pub mod openai;
pub mod provider;
mod stream;
#[cfg(test)]
mod tests;
mod vertex;

pub use anthropic::Request;
pub(crate) use body::{Body, Part};
pub use body::{Message, Text};
pub use models::{CATALOGUE, Caps, cost, describe, window, worker};
pub(crate) use stream::Lines;

use anthropic::{VERSION, read_sse};
use anyhow::{Result, bail};
use cloud::Cloud;
use models::BETA_MCP;
use provider::{Provider, Providers, Shape};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::BufReader;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant};

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

/// The longest reply asked of another vendor's Messages API; theirs often
/// allow less than Anthropic's.
const OTHER_MAX_TOKENS: u32 = 16_384;

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

/// A tool call's input from the JSON text a model wrote. It has to be an
/// object: the API takes nothing else back. Text that isn't one becomes an
/// input that says so, which the tool then reports to the model.
pub(crate) fn tool_input(json_text: &str) -> serde_json::Value {
    match serde_json::from_str::<serde_json::Value>(json_text) {
        Ok(input) if input.is_object() => input,
        Ok(_) => serde_json::json!({ "_invalid_json": "the input must be a JSON object" }),
        Err(e) => serde_json::json!({ "_invalid_json": e.to_string() }),
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
pub(crate) struct Retryable(pub(crate) String);

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
    /// Providers added in the config file.
    providers: Providers,
    /// Saved messages already put into the Chat Completions format.
    chat: std::sync::Mutex<crate::api::openai::Translated>,
    /// Where the clouds' settings are read from: the environment.
    env: cloud::Settings,
    /// Vertex's access token, when gcloud gave it.
    token: vertex::Kept,
}

/// Where one request goes and how it is spoken to.
struct Target {
    url: String,
    key: Option<String>,
    /// None: Anthropic's own API.
    provider: Option<Provider>,
    /// For Claude models: the beta features the request uses.
    betas: String,
    /// Claude asked through a cloud.
    cloud: Option<Cloud>,
    /// Access keys the request is signed with (Bedrock without an API key).
    aws: Option<bedrock::Keys>,
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
            providers: Providers::new(),
            chat: Default::default(),
            env: Box::new(cloud::environment),
            token: Default::default(),
        }
    }

    #[cfg(test)]
    fn with_env(mut self, env: impl Fn(&str) -> Option<String> + Send + Sync + 'static) -> Self {
        self.env = Box::new(env);
        self
    }

    pub fn with_providers(mut self, providers: Providers) -> Self {
        self.providers = providers;
        self
    }

    /// Whether the user has to log in before `model` can be asked. Only
    /// Anthropic's key is kept by /login; other providers' come from the
    /// environment, and a missing one is reported when it is needed.
    pub fn needs_login(&self, model: &str) -> bool {
        provider::resolve(model, &self.providers).is_none() && cloud::of(model).is_none() && !self.has_key()
    }

    /// Claude through a cloud: the same request, to the cloud's address with
    /// its login, without the features it doesn't have.
    fn cloud_target(&self, cloud: Cloud, id: &str, req: &Request) -> Result<(Target, Vec<u8>)> {
        let env = &*self.env;
        let mut head: Request = Request { fallbacks: None, mcp_servers: None, ..req.clone() };
        let wire;
        let (url, key, aws) = match cloud {
            Cloud::Vertex => {
                head.model = "";
                head.anthropic_version = Some(VERSION);
                (vertex::address(env, id)?, Some(vertex::token(env, &self.token)?), None)
            }
            Cloud::Bedrock => {
                wire = bedrock::model(id);
                head.model = &wire;
                match bedrock::login(env)? {
                    bedrock::Login::Key(key) => (bedrock::address(env), Some(key), None),
                    bedrock::Login::Keys(keys) => (bedrock::address(env), None, Some(keys)),
                }
            }
        };
        let betas = Caps::of(req.model).betas();
        let target = Target { url, key, provider: None, betas, cloud: Some(cloud), aws };
        Ok((target, serde_json::to_vec(&head)?))
    }

    /// Where `req` goes, and its head: everything but the messages.
    fn target(&self, req: &Request) -> Result<(Target, Vec<u8>)> {
        let Some((p, id)) = provider::resolve(req.model, &self.providers) else {
            if let Some((cloud, id)) = cloud::of(req.model) {
                return self.cloud_target(cloud, id, req);
            }
            let key = self.key.read().unwrap().clone();
            if key.is_empty() {
                bail!("no API key yet: type /login to add one");
            }
            let mut betas = Caps::of(req.model).betas();
            if req.mcp_servers.is_some() {
                betas = if betas.is_empty() { BETA_MCP.to_string() } else { format!("{betas},{BETA_MCP}") };
            }
            let target =
                Target { url: self.url.clone(), key: Some(key), provider: None, betas, cloud: None, aws: None };
            return Ok((target, serde_json::to_vec(req)?));
        };
        if let Some(why) = p.missing_key() {
            bail!(why);
        }
        let (path, head) = match p.shape {
            Shape::OpenAi => ("/chat/completions", crate::api::openai::head(req, id, &p)),
            // The Messages API as others serve it: the plain request, without
            // what only Anthropic's own models take.
            Shape::Anthropic => {
                let tools: Value = serde_json::from_str(req.tools.get())?;
                let head = json!({
                    "model": id,
                    "max_tokens": p.max_tokens.unwrap_or(OTHER_MAX_TOKENS),
                    "system": req.system,
                    "tools": tools,
                    "stream": true,
                });
                ("/v1/messages", head)
            }
        };
        let target = Target {
            url: format!("{}{path}", p.base_url),
            key: p.key(),
            provider: Some(p),
            betas: String::new(),
            cloud: None,
            aws: None,
        };
        Ok((target, serde_json::to_vec(&head)?))
    }

    pub fn set_key(&self, key: String) {
        *self.key.write().unwrap() = key;
    }

    pub fn has_key(&self) -> bool {
        !self.key.read().unwrap().is_empty()
    }

    /// Sends a request and streams the reply. `messages` are JSON texts.
    pub fn stream<M: Message>(
        &self,
        req: &Request,
        messages: &[M],
        cancel: &AtomicBool,
        mut on: impl FnMut(Chunk),
    ) -> Result<Reply> {
        let (target, head) = self.target(req)?;
        let http = self.http.lock().unwrap().clone();
        // Esc must stop a request at once, even while it waits for the reply
        // to begin, which no read timeout can pick up again: a watcher closes
        // the connection, and a fresh agent is put in place for the next one.
        let done = AtomicBool::new(false);
        let result = std::thread::scope(|s| {
            let watcher = s.spawn(|| {
                while !done.load(Ordering::Relaxed) {
                    if cancel.load(Ordering::Relaxed) {
                        // On Unix this wakes a read that is blocked on the
                        // connection. On Windows it hangs up, and the read
                        // wakes when the server closes its side in answer.
                        *self.http.lock().unwrap() = agent(self.read);
                        http.clone().shutdown();
                        return;
                    }
                    std::thread::park_timeout(Duration::from_millis(50));
                }
            });
            let result = self.send(&http, &target, &head, req.system, messages, cancel, &mut on);
            done.store(true, Ordering::Relaxed);
            // Woken at once, so a finished request never waits out the watcher's nap.
            watcher.thread().unpark();
            result
        });
        // Whatever the closed connection made the request fail with, it was Esc.
        if cancel.load(Ordering::Relaxed) && result.is_err() {
            return Err(Cancelled.into());
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn send<M: Message>(
        &self,
        http: &ureq::Agent,
        target: &Target,
        head: &[u8],
        system: &str,
        messages: &[M],
        cancel: &AtomicBool,
        on: &mut impl FnMut(Chunk),
    ) -> Result<Reply> {
        let mut attempt = 0;
        // A signed request's signature covers the body, so that is read through once first.
        let digest = match &target.aws {
            Some(_) => Some(bedrock::hash(Body::new(head, messages))?),
            None => None,
        };
        let vertex = target.cloud == Some(Cloud::Vertex);
        // Since when the API hasn't been reachable, and how often that was tried.
        let mut offline: Option<(Instant, u32)> = None;
        loop {
            attempt += 1;
            let openai = target.provider.as_ref().is_some_and(|p| p.shape == Shape::OpenAi);
            let mut call = http.post(&target.url).header("content-type", "application/json");
            // Vertex takes the version in the body and its token as a bearer.
            if !openai && !vertex {
                call = call.header("anthropic-version", VERSION);
                if let Some(key) = &target.key {
                    call = call.header("x-api-key", key);
                }
            }
            // Other vendors' servers, whichever format they speak, mostly take the key this way.
            if let Some(key) = &target.key
                && (target.provider.is_some() || vertex)
            {
                call = call.header("authorization", format!("Bearer {key}"));
            }
            // Signed anew for every attempt: the time is part of it.
            if let (Some(keys), Some(digest)) = (&target.aws, &digest) {
                for (name, value) in keys.sign(&target.url, digest, std::time::SystemTime::now()) {
                    call = call.header(name, value);
                }
            }
            if !target.betas.is_empty() {
                call = call.header("anthropic-beta", &target.betas);
            }

            let mut body = match openai {
                true => crate::api::openai::body(head, system, messages, &self.chat)?,
                false => Body::new(head, messages),
            };
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
                // An address that can't be one won't get better by waiting.
                Err(e @ (ureq::Error::BadUri(_) | ureq::Error::Http(_))) => {
                    let fix = match (&target.provider, target.cloud) {
                        (Some(_), _) => "the provider's base_url",
                        (None, Some(_)) => "the model id and the cloud's region",
                        (None, None) => "ANTHROPIC_BASE_URL",
                    };
                    bail!("the API address {:?} isn't valid ({e}); check {fix}", target.url)
                }
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
                // A proxy or a sign-in page in the way answers 200 too, with
                // a web page. (Gateways label a real stream in various ways.)
                let kind = res.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                if kind.starts_with("text/html") {
                    let start: String = res.body_mut().read_to_string().unwrap_or_default().chars().take(200).collect();
                    bail!("{} answered, but not as the API does ({kind}): {}", target.url, start.trim());
                }
                let reader = BufReader::new(res.body_mut().as_reader());
                let reply = match openai {
                    true => crate::api::openai::read(reader, cancel, self.silence, on),
                    false => read_sse(reader, cancel, self.silence, on),
                };
                match reply {
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
            if vertex && status == 401 {
                // The token ran out early: the next request asks gcloud for a new one.
                *self.token.lock().unwrap() = None;
            }
            let retryable = matches!(status, 408 | 409 | 429) || status >= 500;
            if retryable && attempt < MAX_ATTEMPTS {
                wait(backoff(attempt, retry_after), cancel)?;
                continue;
            }
            bail!("API error {status}: {}", error_message(&text));
        }
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

/// What an API's error reply says, without the JSON around it. Each provider
/// shapes it its own way: {"error":{"message"}} (most), the same inside a
/// list (Gemini), {"error":"..."} (xAI), {"detail":"..."} or {"message":"..."}.
fn error_message(body: &str) -> String {
    let said = |v: &Value| -> Option<String> {
        let v = v.as_array().and_then(|a| a.first()).unwrap_or(v);
        [&v["error"]["message"], &v["error"], &v["detail"], &v["message"]]
            .iter()
            .find_map(|m| m.as_str().filter(|m| !m.trim().is_empty()).map(String::from))
    };
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| said(&v))
        .unwrap_or_else(|| body.trim().chars().take(500).collect())
}
