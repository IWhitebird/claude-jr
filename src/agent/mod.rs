// The agent thread: owns the conversation, talks to the API, runs tools, and
// reports what happens through a callback. It never touches the terminal.
//
//   mod.rs     the main agent: commands, session, startup
//   turn.rs    one turn: request, tools, repeat (shared with subagents)
//   permit.rs  modes, approval, auto mode's reviewer
//   sub.rs     subagents

mod compact;
mod history;
mod permit;
mod sub;
mod turn;

use crate::api::{Client, Usage};
use crate::permission::Rules;
use crate::store::session::{Loaded, Msg, Pick, Session};
use crate::tools::mcp::{self, ServerConfig};
use crate::tools::{self, Diff, Tools, skill};
use history::OPEN_ELSEWHERE;
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
    /// The model for subagents, if one is set (else see `api::worker`).
    pub worker_model: Option<String>,
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
    /// Providers added in the config file.
    pub providers: crate::api::provider::Providers,
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
    /// Switched to a saved conversation.
    Resumed {
        name: Option<String>,
        messages: usize,
    },
    /// A saved conversation, to be shown from the start: sent after it is
    /// resumed, at startup or with /resume.
    Replay(Vec<Shown>),
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
    /// A tool call from start to finish, for the run log.
    Step(Step),
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

/// What one tool call was, who let it run and how it went.
pub struct Step {
    pub tool: String,
    pub input: Value,
    /// Why it was allowed to run ("mode", "rule", "reviewer", "user", ...),
    /// or None if it was refused.
    pub allowed_by: Option<&'static str>,
    pub ok: bool,
    /// The result as the user is shown it: a line or a few.
    pub result: String,
    pub ms: u64,
    /// The subagent that made the call, if one did.
    pub agent: Option<String>,
}

pub type Emit = Arc<dyn Fn(Event) + Send + Sync>;

/// One piece of a saved conversation, as it was shown when it happened.
#[derive(Debug, PartialEq)]
pub enum Shown {
    User(String),
    Text(String),
    Tool(String),
    Result { ok: bool, summary: String },
}

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
        if loaded.as_ref().is_some_and(|l| l.copy) {
            emit(Event::Notice(OPEN_ELSEWHERE.into()));
        }
        let Loaded { session, system, messages, .. } = loaded.unwrap_or_else(|| Loaded {
            session: Session::new(),
            copy: false,
            system: crate::prompt::system(),
            messages: Vec::new(),
        });

        let (servers, errors) = mcp::Servers::start(mcp::configured(&cfg.mcp));
        for e in errors {
            emit(Event::Notice(e));
        }
        let web = cfg.web && crate::api::Caps::of(&cfg.model).web;
        // Subagents may run on another model, with or without the web tools.
        let sub = cfg.worker_model.clone().unwrap_or_else(|| crate::api::worker(&cfg.model).to_string());
        let sub_web = cfg.web && crate::api::Caps::of(&sub).web;
        let root = std::env::current_dir().unwrap_or_else(|_| ".".into());
        let tools = Arc::new(Tools::new(root, skill::discover(), servers));
        let notify = emit.clone();
        tools.on_job_finished(Box::new(move |msg| notify(Event::Notice(msg))));
        tools.checkpoints.open(session.checkpoint_dir());
        let _ = shared_tools.set(tools.clone());

        let core = Core {
            client: Client::new(cfg.key, cfg.base_url).with_providers(cfg.providers),
            model: Mutex::new(cfg.model),
            worker_model: cfg.worker_model,
            effort: Mutex::new(cfg.effort),
            auto_model: cfg.auto_model,
            mode,
            rules: Mutex::new(Rules::new(cfg.allow)),
            cancel,
            emit,
            interactive: cfg.interactive,
            connector: tools.mcp.connector(),
            sub_defs: Mutex::new(tools.definitions(false, sub_web, false, true)),
            leaf_defs: Mutex::new(tools.definitions(false, sub_web, false, false)),
            sub_system: OnceLock::new(),
            request: Mutex::default(),
        };
        let mut agent = Agent {
            web: cfg.web,
            defs: tools.definitions(true, web, cfg.interactive, true),
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
            has_key: !agent.core.client.needs_login(&agent.core.model.lock().unwrap()),
        });
        if !agent.messages.is_empty() {
            (agent.core.emit)(Event::Replay(agent.transcript()));
        }
        agent
    }

    fn run(mut self, commands: Receiver<Command>) {
        for cmd in commands {
            self.handle(cmd);
        }
    }

    fn handle(&mut self, cmd: Command) {
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

    fn send(&mut self, text: String) {
        self.core.cancel.store(false, Ordering::Relaxed);
        // What the reviewer of auto mode is shown; no more is kept.
        *self.core.request.lock().unwrap() = text.chars().take(2000).collect();
        // This turn starts at the message about to be added (or merged into).
        let turn = self.messages.len() - usize::from(self.mergeable());
        self.tools.checkpoints.begin_turn(turn);
        // Image files mentioned in the message (dragged in, or pasted with
        // ctrl+v) go in as images, before the text.
        for path in crate::store::images::paths_in(&text) {
            match crate::store::images::block(&path) {
                Ok(image) => {
                    let kb = std::fs::metadata(&path).map_or(0, |m| m.len() / 1024);
                    (self.core.emit)(Event::Notice(format!("Attached {} ({kb} KB)", path.display())));
                    self.push_user(image);
                }
                Err(e) => (self.core.emit)(Event::Notice(e)),
            }
        }
        let mut block = serde_json::Map::new();
        block.insert("type".into(), "text".into());
        // Moved in, not copied as json! would: a pasted megabyte is big enough to matter.
        block.insert("text".into(), Value::String(text));
        self.push_user(Value::Object(block));

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
        let ctx =
            Ctx { system: &self.system, defs: &self.defs, tools: &self.tools, emit: &*emit, main: true, depth: 0 };
        // Saved after every round of tool calls, not only at the end: a long
        // turn that is killed can then be continued with -c.
        let (session, system) = (&mut self.session, &self.system);
        let warn = |problem: Option<String>| problem.into_iter().for_each(|p| emit(Event::Notice(p)));
        let end = self.core.turn(&ctx, &mut self.messages, &mut |so_far| warn(session.save(system, so_far)));
        warn(self.session.save(&self.system, &mut self.messages));
        emit(end);
    }

    /// Switches model. If the new one differs in whether it has the web
    /// tools, the tool list is rebuilt to match (the API rejects tools a model
    /// doesn't have); the prompt cache starts over either way, since it's per model.
    fn set_model(&mut self, model: String) {
        let web = |core: &Core| {
            // One lock at a time: sub_model takes the model's lock too.
            let main = crate::api::Caps::of(&core.model.lock().unwrap()).web;
            (main, crate::api::Caps::of(&core.sub_model()).web)
        };
        let had = web(&self.core);
        *self.core.model.lock().unwrap() = model;
        let has = web(&self.core);
        if had != has {
            let (main, sub) = (has.0 && self.web, has.1 && self.web);
            self.defs = self.tools.definitions(true, main, self.core.interactive, true);
            *self.core.sub_defs.lock().unwrap() = self.tools.definitions(false, sub, false, true);
            *self.core.leaf_defs.lock().unwrap() = self.tools.definitions(false, sub, false, false);
        }
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

    /// Whether the last message is a user message that is not saved yet, so
    /// more can be added to it without rewriting the file.
    fn mergeable(&self) -> bool {
        self.messages.last().is_some_and(|m| m.user) && !self.session.is_saved(self.messages.len())
    }

    /// Adds to the last user message if it is still unsaved (after a failed or
    /// cancelled request), so roles keep alternating. A saved one is never
    /// rewritten: the new block becomes its own message, which the API joins
    /// to the one before it. The history stays append-only.
    fn push_user(&mut self, block: Value) {
        if self.mergeable() {
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

#[cfg(test)]
mod tests;
