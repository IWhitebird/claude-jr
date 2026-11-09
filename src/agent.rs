// The agent thread: owns the conversation, talks to the API, reports events.

use crate::api::{self, CacheControl, Chunk, Client, OutputConfig, Usage};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

pub struct Config {
    pub key: String,
    pub base_url: Option<String>,
    pub model: String,
    pub effort: Option<String>,
    pub max_tokens: u32,
}

pub enum Command {
    Send(String),
}

pub enum Event {
    Text(String),
    ToolStart(String),
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
    let agent = Agent {
        client: Client::new(cfg.key.clone(), cfg.base_url.clone()),
        system: crate::prompt::system(&cfg.model),
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
                    self.turn(text);
                }
            }
        }
    }

    fn turn(&mut self, text: String) {
        let mark = self.messages.len();
        self.messages.push(json!({ "role": "user", "content": text }));

        loop {
            let req = api::Request {
                model: &self.cfg.model,
                max_tokens: self.cfg.max_tokens,
                system: &self.system,
                messages: &self.messages,
                tools: &[],
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
                Err(e) => {
                    // Drop the unfinished turn so the history stays valid.
                    self.messages.truncate(mark);
                    let _ = self.events.send(if e.is::<api::Cancelled>() {
                        Event::Cancelled
                    } else {
                        Event::Error(format!("{e:#}"))
                    });
                    return;
                }
            };

            let _ = self.events.send(Event::Usage(reply.usage));

            if reply.content.is_empty() {
                self.messages.truncate(mark);
                let _ = self.events.send(Event::Error(format!("empty reply ({})", reply.stop_reason)));
                return;
            }
            self.messages.push(json!({ "role": "assistant", "content": reply.content }));

            match reply.stop_reason.as_str() {
                "refusal" => {
                    let _ = self.events.send(Event::Error("the model declined this request".into()));
                    return;
                }
                "max_tokens" => {
                    let _ = self.events.send(Event::Error("reply hit the max_tokens limit".into()));
                    return;
                }
                // Tools come in phase 2.
                _ => {
                    let _ = self.events.send(Event::Done);
                    return;
                }
            }
        }
    }
}
