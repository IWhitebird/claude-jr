// The agent thread: owns the conversation, talks to the API, runs tools, and
// reports what happens through a callback. It never touches the terminal.
//
//   mod.rs     the main agent: commands, session, startup
//   turn.rs    one turn: request, tools, repeat (shared with subagents)
//   permit.rs  modes, approval, auto mode's reviewer
//   sub.rs     subagents

mod permit;
mod sub;
mod turn;

use crate::api::{Client, Usage};
use crate::permission::Rules;
use crate::session::{Loaded, Msg, Pick, Session};
use crate::tools::mcp::{self, ServerConfig};
use crate::tools::{self, Diff, Tools, skill};
use serde_json::value::RawValue;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

pub use permit::Mode;
use turn::{Core, Ctx};

pub struct Config {
    pub key: String,
    pub base_url: Option<String>,
    pub model: String,
    pub effort: Option<String>,
    /// The model auto mode uses to review actions.
    pub auto_model: String,
    pub mode: Mode,
    pub allow: Vec<String>,
    pub resume: Option<Pick>,
    pub mcp: BTreeMap<String, ServerConfig>,
    /// Offer Anthropic's web search and fetch tools (on models that have them).
    pub web: bool,
    /// Whether someone is there to answer questions and approvals.
    pub interactive: bool,
}

pub enum Command {
    Send(String),
    /// A command the user typed with `!`: run it, show it, and tell the model.
    Shell(String),
    /// The user typed /name for a skill.
    Skill {
        name: String,
        args: String,
    },
    /// Start a new conversation.
    Clear,
    Model(String),
    /// low, medium, high, xhigh or max; None for the model's default.
    Effort(Option<String>),
    Rename(String),
    /// A note the user just saved to memory; the model sees it with the next message.
    Remember(String),
    /// Use this API key from now on ("" to forget it).
    SetKey(String),
    /// None: list the points /rewind can go back to. Some(turn): go back to
    /// before that turn, restoring files and cutting the conversation.
    Rewind(Option<usize>),
    /// Switch to a saved conversation.
    Resume(Pick),
    /// Replace the conversation with a summary of it; extra instructions for
    /// the summary, if any.
    Compact(String),
}

pub enum Answer {
    Yes,
    Always,
    No,
}

pub enum Event {
    /// Startup finished. `resumed` messages were loaded from a saved session.
    Ready {
        resumed: usize,
        name: Option<String>,
        skills: Vec<String>,
        mcp: Vec<String>,
        /// Whether an API key is set; if not, the UI asks for one.
        has_key: bool,
    },
    /// Something worth telling the user that isn't an error.
    Notice(String),
    /// A subagent started, did a step, finished. Shown in the agent panel.
    SubStart {
        id: usize,
        task: String,
    },
    SubStep {
        id: usize,
        label: String,
    },
    SubEnd {
        id: usize,
    },
    /// Where /rewind can go: (turn, what you typed), newest first.
    RewindPoints(Vec<(usize, String)>),
    /// Went back to before `prompt`; `files` were restored.
    Rewound {
        prompt: String,
        files: usize,
    },
    /// Switched to a saved conversation. `recap`: your last message and the
    /// reply to it.
    Resumed {
        name: Option<String>,
        messages: usize,
        recap: Option<(String, String)>,
    },
    Text(String),
    /// Progress notes the model writes between tool calls.
    Thinking(String),
    BlockEnd,
    /// The model started writing a tool call; its input is still streaming.
    ToolStart(String),
    /// More of that tool call's input arrived (this many bytes).
    ToolInput(usize),
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
    /// The model asks the user something; answer on `reply`.
    Question {
        question: String,
        options: Vec<String>,
        reply: Sender<String>,
    },
    ToolResult {
        ok: bool,
        summary: String,
    },
    /// What's going on right now, for the status line (subagents, reviews).
    Activity(String),
    /// Tokens used by one request. `main` is false for subagents and reviews,
    /// which cost money but don't fill the main context.
    Usage {
        usage: Usage,
        cost: Option<f64>,
        main: bool,
    },
    Done,
    Cancelled,
    Error(String),
}

pub type Emit = Arc<dyn Fn(Event) + Send + Sync>;

pub struct Handle {
    pub commands: Sender<Command>,
    pub cancel: Arc<AtomicBool>,
    pub mode: Arc<permit::SharedMode>,
    /// The main agent's tools, once started; for /stats and the job count.
    pub tools: Arc<OnceLock<Arc<Tools>>>,
}

pub fn spawn(cfg: Config, emit: impl Fn(Event) + Send + Sync + 'static) -> Handle {
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let mode = Arc::new(permit::SharedMode(AtomicU8::new(cfg.mode as u8)));
    let tools = Arc::new(OnceLock::new());
    let handle = Handle { commands: tx, cancel: cancel.clone(), mode: mode.clone(), tools: tools.clone() };
    thread::Builder::new()
        .name("agent".into())
        .spawn(move || Agent::start(cfg, cancel, mode, tools, Arc::new(emit)).run(rx))
        .expect("can start the agent thread");
    handle
}

struct Agent {
    core: Arc<Core>,
    /// Whether web tools are wanted (the config); used when /model switches.
    web: bool,
    tools: Arc<Tools>,
    system: String,
    defs: Box<RawValue>,
    messages: Vec<Msg>,
    session: Session,
    /// Whether the model was last told that plan mode is on.
    told_plan: bool,
}

impl Agent {
    fn start(
        cfg: Config,
        cancel: Arc<AtomicBool>,
        mode: Arc<permit::SharedMode>,
        shared_tools: Arc<OnceLock<Arc<Tools>>>,
        emit: Emit,
    ) -> Agent {
        let loaded = cfg.resume.as_ref().and_then(Session::load);
        if let (Some(Pick::Named(name)), None) = (&cfg.resume, &loaded) {
            emit(Event::Notice(format!("No saved session named \"{name}\" here; starting a new one.")));
        }
        let Loaded { session, system, messages } = loaded.unwrap_or_else(|| Loaded {
            session: Session::new(),
            system: crate::prompt::system(),
            messages: Vec::new(),
        });

        let (servers, errors) = mcp::Servers::start(mcp::configured(&cfg.mcp));
        for e in errors {
            emit(Event::Notice(e));
        }
        let web = cfg.web && crate::api::Caps::of(&cfg.model).web;
        let root = std::env::current_dir().unwrap_or_else(|_| ".".into());
        let tools = Arc::new(Tools::new(root, skill::discover(), servers));
        let notify = emit.clone();
        tools.on_job_finished(Box::new(move |msg| notify(Event::Notice(msg))));
        tools.checkpoints.open(session.checkpoint_dir());
        let _ = shared_tools.set(tools.clone());

        let core = Core {
            client: Client::new(cfg.key, cfg.base_url),
            model: Mutex::new(cfg.model),
            effort: Mutex::new(cfg.effort),
            auto_model: cfg.auto_model,
            mode,
            rules: Mutex::new(Rules::new(cfg.allow)),
            cancel,
            emit,
            interactive: cfg.interactive,
            connector: tools.mcp.connector(),
            sub_defs: Mutex::new(tools.definitions(false, web)),
            sub_system: OnceLock::new(),
            request: Mutex::default(),
        };
        let mut agent = Agent {
            web: cfg.web,
            defs: tools.definitions(true, web),
            core: Arc::new(core),
            tools,
            system,
            messages,
            session,
            told_plan: false,
        };
        agent.close_open_calls("Not run: the session ended.");
        (agent.core.emit)(Event::Ready {
            resumed: agent.messages.len(),
            name: agent.session.name.clone(),
            skills: agent.tools.skills().iter().map(|s| s.name.clone()).collect(),
            mcp: agent.tools.mcp.describe(),
            has_key: agent.core.client.has_key(),
        });
        agent
    }

    fn run(mut self, commands: Receiver<Command>) {
        for cmd in commands {
            match cmd {
                Command::Send(text) => self.send(text),
                Command::Skill { name, args } => match self.tools.skills().iter().find(|s| s.name == name) {
                    Some(s) => match skill::body(s) {
                        Ok(body) => self.send(format!("<skill name=\"{name}\">\n{body}\n</skill>\n\n{args}")),
                        Err(e) => (self.core.emit)(Event::Error(e)),
                    },
                    None => (self.core.emit)(Event::Error(format!("no skill named {name}"))),
                },
                Command::Shell(cmd) => self.shell(&cmd),
                Command::Clear => {
                    // A new Vec, not clear(): that would keep the old capacity.
                    self.messages = Vec::new();
                    self.session = Session::new();
                    self.tools.checkpoints.open(self.session.checkpoint_dir());
                    self.system = crate::prompt::system();
                    self.told_plan = false;
                }
                Command::Model(m) => self.set_model(m),
                Command::Effort(e) => *self.core.effort.lock().unwrap() = e,
                Command::Rename(name) => self.session.rename(&self.system, &name),
                Command::SetKey(key) => self.core.client.set_key(key),
                Command::Rewind(None) => {
                    let points: Vec<(usize, String)> = self
                        .messages
                        .iter()
                        .enumerate()
                        .rev()
                        .filter_map(|(i, m)| m.prompt().map(|p| (i, p)))
                        .take(9)
                        .collect();
                    (self.core.emit)(Event::RewindPoints(points));
                }
                Command::Rewind(Some(turn)) => self.rewind(turn),
                Command::Resume(pick) => self.resume(&pick),
                Command::Compact(focus) => {
                    let end = self.compact(&focus);
                    (self.core.emit)(end);
                }
                Command::Remember(note) => {
                    let text = format!("<system-reminder>The user saved this to memory: {note}</system-reminder>");
                    self.push_user(json!({ "type": "text", "text": text }));
                }
            }
        }
    }

    fn send(&mut self, text: String) {
        self.core.cancel.store(false, Ordering::Relaxed);
        *self.core.request.lock().unwrap() = text.clone();
        // This turn starts at the message about to be added (or merged into).
        let turn = self.messages.len() - usize::from(self.messages.last().is_some_and(|m| m.user));
        self.tools.checkpoints.begin_turn(turn);
        // Image files mentioned in the message (dragged in, or pasted with
        // ctrl+v) go in as images, before the text.
        for path in crate::images::paths_in(&text) {
            match crate::images::block(&path) {
                Ok(image) => {
                    let kb = std::fs::metadata(&path).map_or(0, |m| m.len() / 1024);
                    (self.core.emit)(Event::Notice(format!("Attached {} ({kb} KB)", path.display())));
                    self.push_user(image);
                }
                Err(e) => (self.core.emit)(Event::Notice(e)),
            }
        }
        self.push_user(json!({ "type": "text", "text": text }));

        // Mode changes and finished jobs go into this message, not the system
        // prompt, so the history before it stays exactly as it was.
        let plan = self.core.mode.get() == Mode::Plan;
        if plan != self.told_plan {
            let note = if plan { permit::PLAN_ON } else { permit::PLAN_OFF };
            self.push_user(json!({ "type": "text", "text": note }));
            self.told_plan = plan;
        }
        if let Some(note) = turn::job_notes(&self.tools) {
            self.push_user(json!({ "type": "text", "text": note }));
        }

        let emit = self.core.emit.clone();
        let ctx = Ctx { system: &self.system, defs: &self.defs, tools: &self.tools, emit: &*emit, main: true };
        let end = self.core.turn(&ctx, &mut self.messages);
        self.session.save(&self.system, &self.messages);
        emit(end);
    }

    /// Back to before `turn`: files the agent changed since then are restored
    /// and the conversation is cut there (cutting from the end keeps it valid).
    fn rewind(&mut self, turn: usize) {
        let Some(prompt) = self.messages.get(turn).and_then(Msg::prompt) else {
            return (self.core.emit)(Event::Error("nothing to rewind to there".into()));
        };
        let files = match self.tools.checkpoints.restore_from(turn) {
            Ok(n) => n,
            Err(e) => return (self.core.emit)(Event::Error(e)),
        };
        self.messages.truncate(turn);
        self.session.truncate(&self.system, turn);
        (self.core.emit)(Event::Rewound { prompt, files });
    }

    /// Asks the model to summarize the conversation, then starts over from
    /// the summary. No earlier thinking is replayed, so this is safe with
    /// models that check their earlier reasoning wasn't edited.
    fn compact(&mut self, focus: &str) -> Event {
        if self.messages.is_empty() {
            return Event::Error("nothing to compact yet".into());
        }
        self.core.cancel.store(false, Ordering::Relaxed);
        self.close_open_calls("Not run.");
        (self.core.emit)(Event::Activity("summarizing the conversation".into()));

        let ask = format!(
            "Summarize this conversation so the work can continue in a fresh context, where this summary is all \
             that remains. Include: what the user wants and any constraints they gave; decisions made and why; \
             every file created or changed, with exact paths and what changed; commands that matter and their \
             results; errors and how they were solved; the current state; and the next steps. Be specific and \
             complete rather than short. Reply with the summary as plain text, without calling tools.{}",
            if focus.is_empty() { String::new() } else { format!(" Pay particular attention to: {focus}") }
        );
        // The request is the conversation plus the ask; the ask joins the last
        // message if that's already a user message, so roles keep alternating.
        let mut parts: Vec<&str> = self.messages.iter().map(|m| m.json.get()).collect();
        let ask = json!({ "type": "text", "text": ask });
        let tail = match self.messages.last() {
            Some(last) if last.user => {
                parts.pop();
                let mut content = last.content();
                content.push(ask);
                Msg::new("user", Value::Array(content))
            }
            _ => Msg::new("user", json!([ask])),
        };
        parts.push(tail.json.get());

        let model = self.core.model.lock().unwrap().clone();
        let effort = self.core.effort.lock().unwrap().clone();
        let req = crate::api::Request::new(&model, effort.as_deref(), &self.system, &self.defs);
        let reply = match self.core.client.stream(&req, &parts, &self.core.cancel, |_| {}) {
            Ok(r) => r,
            Err(e) if e.is::<crate::api::Cancelled>() => return Event::Cancelled,
            Err(e) => return Event::Error(format!("couldn't compact: {e:#}")),
        };
        let cost = crate::api::cost(&model, &reply.usage);
        (self.core.emit)(Event::Usage { usage: reply.usage, cost, main: false });
        let summary: String = reply.content.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n");
        if summary.trim().is_empty() {
            return Event::Error("the summary came back empty; nothing was changed".into());
        }

        let before = self.messages.len();
        let note = format!(
            "<compacted>This conversation was compacted to save space. Summary of everything before this \
             point:\n\n{summary}\n</compacted>"
        );
        self.messages = vec![
            Msg::new("user", json!([{ "type": "text", "text": note }])),
            Msg::new("assistant", json!([{ "type": "text", "text": "Got it. I'll continue from this summary." }])),
        ];
        self.session.truncate(&self.system, 0);
        self.session.save(&self.system, &self.messages);
        self.tools.checkpoints.clear();
        self.told_plan = false;
        let words = summary.split_whitespace().count();
        (self.core.emit)(Event::Notice(format!(
            "Compacted {before} messages into a {words}-word summary. /rewind can't go back past this point."
        )));
        Event::Done
    }

    /// Switches model. If the new one differs in whether it has the web
    /// tools, the tool list is rebuilt to match (the API rejects tools a model
    /// doesn't have); the prompt cache starts over either way, since it's per model.
    fn set_model(&mut self, model: String) {
        let had = crate::api::Caps::of(&self.core.model.lock().unwrap()).web;
        let has = crate::api::Caps::of(&model).web;
        *self.core.model.lock().unwrap() = model;
        if had != has {
            let web = has && self.web;
            self.defs = self.tools.definitions(true, web);
            *self.core.sub_defs.lock().unwrap() = self.tools.definitions(false, web);
        }
    }

    fn resume(&mut self, pick: &Pick) {
        let Some(Loaded { session, system, messages }) = Session::load(pick) else {
            return (self.core.emit)(Event::Error("couldn't load that conversation".into()));
        };
        self.tools.checkpoints.open(session.checkpoint_dir());
        (self.session, self.system, self.messages, self.told_plan) = (session, system, messages, false);
        self.close_open_calls("Not run: the session ended.");

        let prompt = self.messages.iter().rev().find_map(Msg::prompt);
        let reply = self
            .messages
            .iter()
            .rev()
            .find(|m| !m.user)
            .map(|m| m.content().iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n"));
        (self.core.emit)(Event::Resumed {
            name: self.session.name.clone(),
            messages: self.messages.len(),
            recap: prompt.zip(reply),
        });
    }

    fn shell(&mut self, cmd: &str) {
        self.core.cancel.store(false, Ordering::Relaxed);
        let emit = self.core.emit.clone();
        emit(Event::Tool { label: format!("! {cmd}"), diff: None });
        let out = self.tools.user_shell(cmd, &self.core.cancel);
        emit(Event::ToolResult { ok: out.is_ok(), summary: tools::summary("bash", &out) });

        // The model sees what the user ran, with the next message.
        let text = match &out {
            Ok(s) | Err(s) => s,
        };
        let text = crate::tools::shorten(text, 10_000);
        let note = format!("<user-shell>\n$ {cmd}\n{text}\n</user-shell>");
        self.push_user(json!({ "type": "text", "text": note }));
        emit(Event::Done);
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
        if let Some(results) = self.messages.last().and_then(|m| turn::unanswered(m, why)) {
            self.messages.push(results);
        }
    }
}
