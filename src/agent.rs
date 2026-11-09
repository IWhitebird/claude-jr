// The agent thread: owns the conversation, talks to the API, runs tools, and
// reports what happens through a callback. It never touches the terminal.

use crate::api::{self, Chunk, Client, Usage};
use crate::permission::Rules;
use crate::session::{Msg, Session};
use crate::tools::{self, Diff, Tools};
use serde_json::value::RawValue;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    Ask,
    Allow,
    Deny,
}

pub struct Config {
    pub key: String,
    pub base_url: Option<String>,
    pub model: String,
    pub effort: Option<String>,
    pub mode: Mode,
    pub allow: Vec<String>,
    pub resume: bool,
}

pub enum Command {
    Send(String),
    /// Start a new conversation.
    Clear,
    Model(String),
}

pub enum Answer {
    Yes,
    Always,
    No,
}

pub enum Event {
    /// Startup finished; `resumed` messages were loaded from the last session.
    Ready {
        resumed: usize,
    },
    Text(String),
    /// Progress notes the model writes between tool calls.
    Thinking(String),
    BlockEnd,
    /// The model started writing a tool call; its input is still streaming.
    ToolStart(String),
    /// A tool call is about to run.
    Tool {
        label: String,
        diff: Option<Diff>,
    },
    /// Approval needed; answer on `reply`.
    Ask {
        label: String,
        reply: Sender<Answer>,
    },
    ToolResult {
        ok: bool,
        summary: String,
    },
    Usage(Usage),
    Done,
    Cancelled,
    Error(String),
}

pub struct Handle {
    pub commands: Sender<Command>,
    pub cancel: Arc<AtomicBool>,
}

pub fn spawn(cfg: Config, emit: impl Fn(Event) + Send + 'static) -> Handle {
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    thread::spawn(move || Agent::start(cfg, flag, Box::new(emit)).run(rx));
    Handle { commands: tx, cancel }
}

struct Agent {
    client: Client,
    model: String,
    effort: Option<String>,
    mode: Mode,
    rules: Rules,
    system: String,
    tool_defs: Box<RawValue>,
    tools: Tools,
    messages: Vec<Msg>,
    session: Session,
    cancel: Arc<AtomicBool>,
    emit: Box<dyn Fn(Event) + Send>,
}

impl Agent {
    fn start(cfg: Config, cancel: Arc<AtomicBool>, emit: Box<dyn Fn(Event) + Send>) -> Agent {
        let root = std::env::current_dir().unwrap_or_else(|_| ".".into());
        let (session, system, messages) = match cfg.resume.then(Session::latest).flatten() {
            Some((session, system, messages)) => (session, system, messages),
            None => (Session::new(), crate::prompt::system(), Vec::new()),
        };
        let mut agent = Agent {
            client: Client::new(cfg.key, cfg.base_url),
            model: cfg.model,
            effort: cfg.effort,
            mode: cfg.mode,
            rules: Rules::new(cfg.allow),
            system,
            tool_defs: tools::definitions(),
            tools: Tools::new(root),
            messages,
            session,
            cancel,
            emit,
        };
        agent.close_open_calls("Not run: the session ended.");
        (agent.emit)(Event::Ready { resumed: agent.messages.len() });
        agent
    }

    fn run(mut self, commands: Receiver<Command>) {
        for cmd in commands {
            match cmd {
                Command::Send(text) => {
                    self.cancel.store(false, Ordering::Relaxed);
                    self.push_user(json!({ "type": "text", "text": text }));
                    let end = self.turn();
                    self.session.save(&self.system, &self.messages);
                    (self.emit)(end);
                }
                Command::Clear => {
                    self.messages.clear();
                    self.session = Session::new();
                    self.system = crate::prompt::system();
                }
                Command::Model(m) => self.model = m,
            }
        }
    }

    /// Adds to the last user message if there is one (after a failed or
    /// cancelled request), so roles keep alternating. Only the newest message
    /// changes, so the history stays append-only for the API.
    fn push_user(&mut self, block: Value) {
        if self.messages.last().is_some_and(|m| m.user) {
            let last = self.messages.pop().unwrap();
            let mut content = last.content();
            content.push(block);
            self.messages.push(Msg::new("user", Value::Array(content)));
        } else {
            self.messages.push(Msg::new("user", json!([block])));
        }
    }

    /// Every tool_use needs a tool_result before the next request. Answers any
    /// the last assistant message left open.
    fn close_open_calls(&mut self, why: &str) {
        let Some(last) = self.messages.last().filter(|m| !m.user) else { return };
        let results: Vec<Value> = last
            .content()
            .iter()
            .filter(|b| b["type"] == "tool_use")
            .map(|b| json!({ "type": "tool_result", "tool_use_id": b["id"], "content": why, "is_error": true }))
            .collect();
        if !results.is_empty() {
            self.messages.push(Msg::new("user", Value::Array(results)));
        }
    }

    /// Runs requests and tools until the model stops. Returns the final event.
    fn turn(&mut self) -> Event {
        loop {
            let req = api::Request::new(&self.model, self.effort.as_deref(), &self.system, &self.tool_defs);
            let emit = &self.emit;
            let result = self.client.stream(&req, &self.messages, &self.cancel, |c| {
                emit(match c {
                    Chunk::Text(t) => Event::Text(t.to_string()),
                    Chunk::Thinking(t) => Event::Thinking(t.to_string()),
                    Chunk::ToolStart(n) => Event::ToolStart(n.to_string()),
                    Chunk::BlockEnd => Event::BlockEnd,
                })
            });

            let reply = match result {
                Ok(r) => r,
                Err(e) if e.is::<api::Cancelled>() => return Event::Cancelled,
                Err(e) => return Event::Error(format!("{e:#}")),
            };
            (self.emit)(Event::Usage(reply.usage));
            if reply.content.is_empty() {
                return Event::Error(format!("empty reply (stop reason: {})", reply.stop_reason));
            }

            let calls: Vec<(Value, String, Value)> = reply
                .content
                .iter()
                .filter(|b| b["type"] == "tool_use")
                .map(|b| (b["id"].clone(), b["name"].as_str().unwrap_or("").to_string(), b["input"].clone()))
                .collect();
            self.messages.push(Msg::new("assistant", Value::Array(reply.content)));

            if reply.stop_reason != "tool_use" || calls.is_empty() {
                self.close_open_calls("Not run.");
                return match reply.stop_reason.as_str() {
                    "refusal" => Event::Error("the model declined this request".into()),
                    "max_tokens" => Event::Error("the reply hit the max_tokens limit".into()),
                    _ => Event::Done,
                };
            }

            let mut results = Vec::with_capacity(calls.len());
            for (id, name, input) in &calls {
                let out = if self.cancel.load(Ordering::Relaxed) {
                    Err("Cancelled by the user.".to_string())
                } else {
                    self.call(name, input)
                };
                let (content, is_error) = match out {
                    Ok(s) => (s, false),
                    Err(s) => (s, true),
                };
                results.push(
                    json!({ "type": "tool_result", "tool_use_id": id, "content": content, "is_error": is_error }),
                );
            }
            self.messages.push(Msg::new("user", Value::Array(results)));
            if self.cancel.load(Ordering::Relaxed) {
                return Event::Cancelled;
            }
        }
    }

    fn call(&mut self, name: &str, input: &Value) -> tools::Output {
        if let Some(e) = input.get("_invalid_json") {
            return Err(format!("The tool input was not valid JSON ({e}). Try again."));
        }
        let label = self.tools.label(name, input);
        let diff = self.tools.preview(name, input);
        (self.emit)(Event::Tool { label: label.clone(), diff });

        let out = if self.permitted(name, input, label) {
            self.tools.run(name, input, &self.cancel)
        } else {
            Err("The user did not allow this tool call.".to_string())
        };
        (self.emit)(Event::ToolResult { ok: out.is_ok(), summary: tools::summary(name, &out) });
        out
    }

    fn permitted(&mut self, name: &str, input: &Value, label: String) -> bool {
        if !tools::needs_permission(name) {
            return true;
        }
        let subject = self.tools.subject(name, input);
        if self.rules.allows(name, &subject) {
            return true;
        }
        match self.mode {
            Mode::Allow => true,
            Mode::Deny => false,
            Mode::Ask => {
                let (tx, rx) = mpsc::channel();
                (self.emit)(Event::Ask { label, reply: tx });
                match rx.recv() {
                    Ok(Answer::Yes) => true,
                    Ok(Answer::Always) => {
                        self.rules.add(Rules::always(name, &subject));
                        true
                    }
                    _ => false,
                }
            }
        }
    }
}
