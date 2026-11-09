// The agent thread: owns the conversation, talks to the API, runs tools, reports events.

use crate::api::{self, CacheControl, Chunk, Client, OutputConfig, Usage};
use crate::tools::{self, Tools};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

#[derive(Clone, Copy, PartialEq)]
pub enum Permission {
    Ask,
    Allow,
    Deny,
}

pub struct Config {
    pub key: String,
    pub base_url: Option<String>,
    pub model: String,
    pub effort: Option<String>,
    pub max_tokens: u32,
    pub permission: Permission,
}

pub enum Command {
    Send(String),
}

pub enum Event {
    Text(String),
    /// The model started writing a tool call (input still streaming).
    #[allow(dead_code)] // spinner label in the TUI (phase 3)
    ToolStart(String),
    /// A tool call is about to run.
    Tool { label: String },
    /// Approval needed; answer on `reply`.
    Ask { label: String, reply: Sender<bool> },
    ToolResult { ok: bool, output: String },
    Usage(Usage),
    Done,
    Cancelled,
    Error(String),
}

pub struct Handle {
    pub commands: Sender<Command>,
    pub events: Receiver<Event>,
    #[allow(dead_code)] // Esc in the TUI (phase 3)
    pub cancel: Arc<AtomicBool>,
}

pub fn spawn(cfg: Config) -> Handle {
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (ev_tx, ev_rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let root = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let agent = Agent {
        client: Client::new(cfg.key.clone(), cfg.base_url.clone()),
        system: crate::prompt::system(&cfg.model),
        tool_defs: tools::definitions(),
        tools: Tools::new(root),
        messages: Vec::new(),
        cancel: cancel.clone(),
        events: ev_tx,
        cfg,
    };
    thread::spawn(move || agent.run(cmd_rx));
    Handle { commands: cmd_tx, events: ev_rx, cancel }
}

struct Agent {
    cfg: Config,
    client: Client,
    system: String,
    tool_defs: Vec<Value>,
    tools: Tools,
    messages: Vec<Value>,
    cancel: Arc<AtomicBool>,
    events: Sender<Event>,
}

impl Agent {
    fn run(mut self, commands: Receiver<Command>) {
        for cmd in commands {
            match cmd {
                Command::Send(text) => {
                    self.cancel.store(false, Ordering::Relaxed);
                    self.push_user(json!({ "type": "text", "text": text }));
                    let end = self.turn();
                    let _ = self.events.send(end);
                }
            }
        }
    }

    fn emit(&self, e: Event) {
        let _ = self.events.send(e);
    }

    /// Appends to the last user message if there is one (after a failed or
    /// cancelled request), so roles keep alternating.
    fn push_user(&mut self, block: Value) {
        if let Some(last) = self.messages.last_mut() {
            if last["role"] == "user" {
                if let Some(arr) = last["content"].as_array_mut() {
                    arr.push(block);
                    return;
                }
            }
        }
        self.messages.push(json!({ "role": "user", "content": [block] }));
    }

    /// Runs requests and tools until the model stops. Returns the final event.
    fn turn(&mut self) -> Event {
        loop {
            let req = api::Request {
                model: &self.cfg.model,
                max_tokens: self.cfg.max_tokens,
                system: &self.system,
                messages: &self.messages,
                tools: &self.tool_defs,
                stream: true,
                cache_control: CacheControl { kind: "ephemeral" },
                fallbacks: "default",
                output_config: self.cfg.effort.as_deref().map(|effort| OutputConfig { effort }),
            };

            let events = &self.events;
            let result = self.client.stream(&req, &self.cancel, |c| {
                let _ = events.send(match c {
                    Chunk::Text(t) => Event::Text(t.to_string()),
                    Chunk::ToolStart(n) => Event::ToolStart(n.to_string()),
                });
            });

            let reply = match result {
                Ok(r) => r,
                Err(e) if e.is::<api::Cancelled>() => return Event::Cancelled,
                Err(e) => return Event::Error(format!("{e:#}")),
            };
            self.emit(Event::Usage(reply.usage));

            if reply.content.is_empty() {
                return Event::Error(format!("empty reply (stop reason: {})", reply.stop_reason));
            }

            let calls: Vec<(String, String, Value)> = reply
                .content
                .iter()
                .filter(|b| b["type"] == "tool_use")
                .map(|b| {
                    let s = |k: &str| b[k].as_str().unwrap_or("").to_string();
                    (s("id"), s("name"), b["input"].clone())
                })
                .collect();
            self.messages.push(json!({ "role": "assistant", "content": reply.content }));

            let stop = reply.stop_reason.as_str();
            if stop == "tool_use" && !calls.is_empty() {
                let mut results = Vec::with_capacity(calls.len());
                for (id, name, input) in &calls {
                    let out = if self.cancel.load(Ordering::Relaxed) {
                        Err("Cancelled by the user".to_string())
                    } else {
                        self.call(name, input)
                    };
                    let (content, is_error) = match out {
                        Ok(s) => (s, false),
                        Err(s) => (s, true),
                    };
                    results.push(json!({
                        "type": "tool_result", "tool_use_id": id, "content": content, "is_error": is_error
                    }));
                }
                self.messages.push(json!({ "role": "user", "content": results }));
                if self.cancel.load(Ordering::Relaxed) {
                    return Event::Cancelled;
                }
                continue;
            }

            // Any tool call we won't run still needs a result, or the next request fails.
            if !calls.is_empty() {
                let results: Vec<Value> = calls
                    .iter()
                    .map(|(id, ..)| json!({
                        "type": "tool_result", "tool_use_id": id, "content": "Not run.", "is_error": true
                    }))
                    .collect();
                self.messages.push(json!({ "role": "user", "content": results }));
            }

            return match stop {
                "refusal" => Event::Error("the model declined this request".into()),
                "max_tokens" => Event::Error("the reply hit the max_tokens limit".into()),
                _ => Event::Done,
            };
        }
    }

    fn call(&mut self, name: &str, input: &Value) -> tools::Output {
        if let Some(e) = input.get("_invalid_json") {
            return Err(format!("The tool input was not valid JSON ({e}). Try again."));
        }
        let label = self.tools.describe(name, input);
        self.emit(Event::Tool { label: label.clone() });

        if tools::needs_permission(name) && !self.allowed(label) {
            let out = Err("The user did not allow this tool call.".to_string());
            self.emit(Event::ToolResult { ok: false, output: "Denied".into() });
            return out;
        }

        let out = self.tools.run(name, input, &self.cancel);
        let (ok, text) = match &out {
            Ok(s) => (true, s.clone()),
            Err(s) => (false, s.clone()),
        };
        self.emit(Event::ToolResult { ok, output: text });
        out
    }

    fn allowed(&self, label: String) -> bool {
        match self.cfg.permission {
            Permission::Allow => true,
            Permission::Deny => false,
            Permission::Ask => {
                let (tx, rx) = mpsc::channel();
                self.emit(Event::Ask { label, reply: tx });
                rx.recv().unwrap_or(false)
            }
        }
    }
}
