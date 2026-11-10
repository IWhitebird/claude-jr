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
            sub_defs: Mutex::new(tools.definitions(false, web, false)),
            sub_system: OnceLock::new(),
            request: Mutex::default(),
        };
        let mut agent = Agent {
            web: cfg.web,
            defs: tools.definitions(true, web, cfg.interactive),
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
        *self.core.request.lock().unwrap() = text.clone();
        // This turn starts at the message about to be added (or merged into).
        let turn = self.messages.len() - usize::from(self.mergeable());
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
            self.defs = self.tools.definitions(true, web, self.core.interactive);
            *self.core.sub_defs.lock().unwrap() = self.tools.definitions(false, web, false);
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
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::time::Duration;

    /// One canned HTTP answer from the fake API.
    pub(crate) struct Response {
        pub status: u16,
        pub body: String,
    }

    pub(crate) fn ok(body: String) -> Response {
        Response { status: 200, body }
    }

    /// The request bodies the fake API has received, in order.
    pub(crate) type Log = Arc<Mutex<Vec<String>>>;

    /// A fake Messages API: answers each request with the next response (SSE
    /// for status 200), and records the request bodies.
    pub(crate) fn fake_http(replies: Vec<Response>) -> (String, Log) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let log: Log = Arc::default();
        let seen = log.clone();
        thread::spawn(move || {
            let mut replies = replies.into_iter();
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut out = stream;
                // Keep-alive: several requests on one connection, until it closes.
                'requests: loop {
                    let mut len = 0;
                    let mut line = String::new();
                    loop {
                        line.clear();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 {
                            break 'requests;
                        }
                        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                            len = v.trim().parse().unwrap_or(0);
                        }
                        if line == "\r\n" {
                            break;
                        }
                    }
                    let mut body = vec![0; len];
                    let _ = reader.read_exact(&mut body);
                    seen.lock().unwrap().push(String::from_utf8_lossy(&body).into_owned());
                    let Some(r) = replies.next() else { return };
                    let kind = if r.status == 200 { "text/event-stream" } else { "application/json" };
                    let head = format!(
                        "HTTP/1.1 {} X\r\ncontent-type: {kind}\r\ncontent-length: {}\r\n\r\n",
                        r.status,
                        r.body.len()
                    );
                    let _ = out.write_all(head.as_bytes()).and_then(|_| out.write_all(r.body.as_bytes()));
                }
            }
        });
        (url, log)
    }

    fn fake_api(replies: Vec<String>) -> String {
        fake_http(replies.into_iter().map(ok).collect()).0
    }

    pub(crate) fn sse(events: &[serde_json::Value]) -> String {
        events.iter().map(|e| format!("data: {e}\n\n")).collect()
    }

    /// A reply made of finished blocks (each streamed whole) that stops for `stop`.
    pub(crate) fn blocks_reply(blocks: &[Value], stop: &str) -> String {
        let mut events = vec![json!({"type": "message_start", "message": {"usage": {"input_tokens": 10}}})];
        for (i, b) in blocks.iter().enumerate() {
            let mut start = b.clone();
            let input = start.get("input").cloned();
            if input.is_some() {
                start["input"] = json!({});
            }
            if start["type"] == "text" {
                start["text"] = json!("");
            }
            events.push(json!({"type": "content_block_start", "index": i, "content_block": start}));
            if let Some(input) = input {
                let delta = json!({"type": "input_json_delta", "partial_json": input.to_string()});
                events.push(json!({"type": "content_block_delta", "index": i, "delta": delta}));
            } else if b["type"] == "text" {
                let delta = json!({"type": "text_delta", "text": b["text"]});
                events.push(json!({"type": "content_block_delta", "index": i, "delta": delta}));
            }
            events.push(json!({"type": "content_block_stop", "index": i}));
        }
        events.push(json!({"type": "message_delta", "delta": {"stop_reason": stop}, "usage": {"output_tokens": 5}}));
        events.push(json!({"type": "message_stop"}));
        sse(&events)
    }

    pub(crate) fn text_reply(text: &str) -> String {
        blocks_reply(&[json!({"type": "text", "text": text})], "end_turn")
    }

    pub(crate) fn tool_use(id: &str, name: &str, input: Value) -> Value {
        json!({"type": "tool_use", "id": id, "name": name, "input": input})
    }

    pub(crate) fn tools_reply(calls: &[Value]) -> String {
        blocks_reply(calls, "tool_use")
    }

    /// Spaces out session ids (they are millisecond timestamps) between tests.
    pub(crate) fn tick() {
        static LOCK: Mutex<()> = Mutex::new(());
        let _held = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        thread::sleep(Duration::from_millis(3));
    }

    /// An agent driven from the test's own thread, with its events collected.
    pub(crate) struct Rig {
        pub agent: Agent,
        pub rx: Receiver<Event>,
        pub dir: PathBuf,
        pub log: Log,
        /// What startup emitted (Ready, notices).
        pub started: Vec<Event>,
        /// Answers to approvals, used one per `Ask`; none left means "no".
        pub answers: Arc<Mutex<VecDeque<Answer>>>,
        /// Answers to `ask_user`, used one per question; none left means an empty answer.
        pub replies: Arc<Mutex<VecDeque<String>>>,
    }

    pub(crate) fn config(url: Option<String>, mode: Mode) -> Config {
        Config {
            key: "test".into(),
            base_url: url,
            model: "claude-opus-5-5".into(),
            effort: None,
            auto_model: "claude-haiku-5-5".into(),
            mode,
            allow: Vec::new(),
            resume: None,
            mcp: BTreeMap::new(),
            web: false,
            interactive: false,
        }
    }

    impl Rig {
        pub fn new(name: &str, mode: Mode, replies: Vec<Response>) -> Rig {
            let (url, log) = fake_http(replies);
            let mut rig = Rig::with(name, config(Some(url), mode));
            rig.log = log;
            rig
        }

        /// Starts an agent for `cfg`, with its tools rooted in a fresh folder.
        pub fn with(name: &str, cfg: Config) -> Rig {
            crate::tools::test_home();
            tick();
            let dir = crate::tools::test_dir(name);
            let (tx, rx) = mpsc::channel();
            let answers: Arc<Mutex<VecDeque<Answer>>> = Arc::default();
            let replies: Arc<Mutex<VecDeque<String>>> = Arc::default();
            let (a, r) = (answers.clone(), replies.clone());
            let emit: Emit = Arc::new(move |e: Event| {
                match &e {
                    Event::Ask { reply, .. } => {
                        let answer = a.lock().unwrap().pop_front().unwrap_or(Answer::No);
                        let _ = reply.send(answer);
                    }
                    Event::Question { reply, .. } => {
                        let answer = r.lock().unwrap().pop_front().unwrap_or_default();
                        let _ = reply.send(answer);
                    }
                    _ => {}
                }
                let _ = tx.send(e);
            });
            let mode = Arc::new(permit::SharedMode(AtomicU8::new(cfg.mode as u8)));
            let mut agent =
                Agent::start(cfg, Arc::new(AtomicBool::new(false)), mode, Arc::new(OnceLock::new()), emit.clone());
            agent.tools = Arc::new(Tools::new(dir.clone(), Vec::new(), mcp::Servers::default()));
            agent.tools.checkpoints.open(agent.session.checkpoint_dir());
            agent.tools.on_job_finished(Box::new(move |msg| emit(Event::Notice(msg))));
            let started = rx.try_iter().collect();
            Rig { agent, rx, dir, log: Arc::default(), started, answers, replies }
        }

        /// Every event since the last call.
        pub fn events(&self) -> Vec<Event> {
            self.rx.try_iter().collect()
        }

        /// Runs `commands` as the agent's command loop would; returns the events.
        pub fn run(&mut self, commands: Vec<Command>) -> Vec<Event> {
            for c in commands {
                self.agent.handle(c);
            }
            self.events()
        }

        pub fn bodies(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }
    }

    pub(crate) fn text_of(events: &[Event]) -> String {
        events.iter().filter_map(|e| if let Event::Text(t) = e { Some(t.as_str()) } else { None }).collect()
    }

    pub(crate) fn errors_of(events: &[Event]) -> Vec<String> {
        events.iter().filter_map(|e| if let Event::Error(t) = e { Some(t.clone()) } else { None }).collect()
    }

    pub(crate) fn notices_of(events: &[Event]) -> Vec<String> {
        events.iter().filter_map(|e| if let Event::Notice(t) = e { Some(t.clone()) } else { None }).collect()
    }

    pub(crate) fn last_is_done(events: &[Event]) -> bool {
        matches!(events.last(), Some(Event::Done))
    }

    #[test]
    fn a_turn_runs_a_tool_and_finishes() {
        crate::tools::test_home();
        let tool_call = tools_reply(&[tool_use("t1", "bash", json!({"command": "echo hi"}))]);
        let url = fake_api(vec![tool_call, text_reply("It printed hi.")]);

        let (tx, rx) = mpsc::channel();
        let agent = spawn(config(Some(url), Mode::Bypass), move |e| {
            let _ = tx.send(e);
        });
        agent.commands.send(Command::Send("say hi".into())).unwrap();

        let (mut text, mut tools, mut results) = (String::new(), Vec::new(), Vec::new());
        loop {
            match rx.recv_timeout(Duration::from_secs(10)).expect("the turn finishes") {
                Event::Text(t) => text.push_str(&t),
                Event::Tool { label, .. } => tools.push(label),
                Event::ToolResult { ok, summary } => results.push((ok, summary)),
                Event::Done => break,
                Event::Error(e) => panic!("turn failed: {e}"),
                _ => {}
            }
        }
        assert_eq!(tools, ["Bash(echo hi)"]);
        assert_eq!(results, [(true, "hi".to_string())]);
        assert_eq!(text, "It printed hi.");
        assert!(agent.tools.get().is_some(), "the tools are shared with the UI once started");
    }

    #[test]
    fn startup_reports_the_key_and_what_was_loaded() {
        let rig = Rig::with("agent-ready", Config { key: String::new(), ..config(None, Mode::Default) });
        let ready = &rig.started;
        let Some(Event::Ready { resumed, name, has_key, .. }) = ready.iter().find(|e| matches!(e, Event::Ready { .. }))
        else {
            panic!("no Ready event");
        };
        assert_eq!((*resumed, name.clone(), *has_key), (0, None, false));
    }

    #[test]
    fn a_missing_named_session_starts_fresh_with_a_notice() {
        let mut cfg = config(None, Mode::Default);
        cfg.resume = Some(Pick::Named("no-such-session-anywhere".into()));
        let rig = Rig::with("agent-missing-session", cfg);
        let events = &rig.started;
        assert_eq!(
            notices_of(events),
            ["No saved session named \"no-such-session-anywhere\" here; starting a new one."]
        );
        assert!(rig.agent.messages.is_empty());
    }

    #[test]
    fn a_resumed_session_answers_calls_the_last_run_left_open() {
        crate::tools::test_home();
        tick();
        let name = "unit-open-calls";
        let mut s = Session::new();
        let msgs = vec![
            Msg::new("user", json!([{ "type": "text", "text": "run it" }])),
            Msg::new("assistant", json!([tool_use("t9", "bash", json!({"command": "x"}))])),
        ];
        s.rename("SYS", name);
        s.save("SYS", &msgs);

        let mut cfg = config(None, Mode::Default);
        cfg.resume = Some(Pick::Named(name.into()));
        let rig = Rig::with("agent-open-calls", cfg);
        let events = &rig.started;
        let Some(Event::Ready { resumed, name: n, .. }) = events.iter().find(|e| matches!(e, Event::Ready { .. }))
        else {
            panic!("no Ready event");
        };
        assert_eq!((*resumed, n.as_deref()), (3, Some(name)));
        assert_eq!(rig.agent.system, "SYS");
        let last = rig.agent.messages.last().unwrap();
        assert!(last.user && last.has_results());
        assert!(last.json.get().contains("Not run: the session ended."));
    }

    #[test]
    fn resume_switches_conversation_and_recaps_it() {
        let mut rig = Rig::new("agent-resume", Mode::Default, vec![ok(text_reply("pong"))]);
        rig.run(vec![Command::Send("ping".into()), Command::Rename("unit-resume-me".into())]);
        assert_eq!(rig.agent.session.name.as_deref(), Some("unit-resume-me"));

        // Start something else, then come back.
        tick();
        rig.run(vec![Command::Clear]);
        assert!(rig.agent.messages.is_empty());
        let events = rig.run(vec![Command::Resume(Pick::Named("unit-resume-me".into()))]);
        let Some(Event::Resumed { name, messages, recap }) =
            events.into_iter().find(|e| matches!(e, Event::Resumed { .. }))
        else {
            panic!("no Resumed event");
        };
        assert_eq!(name.as_deref(), Some("unit-resume-me"));
        assert_eq!(messages, 2);
        assert_eq!(recap, Some(("ping".to_string(), "pong".to_string())));

        let events = rig.run(vec![Command::Resume(Pick::Named("unit-never-saved".into()))]);
        assert_eq!(errors_of(&events), ["couldn't load that conversation"]);
        assert_eq!(rig.agent.messages.len(), 2, "a failed resume changes nothing");
    }

    #[test]
    fn rewind_restores_files_and_cuts_the_conversation() {
        let write = tool_use("w1", "write", json!({"path": "a.txt", "content": "hi\n"}));
        let mut rig =
            Rig::new("agent-rewind", Mode::Bypass, vec![ok(tools_reply(&[write])), ok(text_reply("Wrote it."))]);
        let events = rig.run(vec![Command::Send("make a.txt".into())]);
        assert!(last_is_done(&events));
        assert_eq!(std::fs::read_to_string(rig.dir.join("a.txt")).unwrap(), "hi\n");
        assert_eq!(rig.agent.messages.len(), 4);

        let events = rig.run(vec![Command::Rewind(None)]);
        let Some(Event::RewindPoints(points)) = events.into_iter().next() else { panic!("no points") };
        assert_eq!(points, [(0, "make a.txt".to_string())]);

        let events = rig.run(vec![Command::Rewind(Some(3))]);
        assert_eq!(errors_of(&events), ["nothing to rewind to there"]);
        assert_eq!(rig.agent.messages.len(), 4);

        let events = rig.run(vec![Command::Rewind(Some(0))]);
        let Some(Event::Rewound { prompt, files }) = events.into_iter().next() else { panic!("no Rewound") };
        assert_eq!((prompt.as_str(), files), ("make a.txt", 1));
        assert!(!rig.dir.join("a.txt").exists(), "the file the turn created is gone");
        assert!(rig.agent.messages.is_empty());
    }

    #[test]
    fn compact_replaces_the_conversation_with_a_summary() {
        let mut rig = Rig::new(
            "agent-compact",
            Mode::Default,
            vec![ok(text_reply("hello")), ok(text_reply("They fixed bugs in main.rs today."))],
        );
        assert_eq!(errors_of(&rig.run(vec![Command::Compact(String::new())])), ["nothing to compact yet"]);
        rig.run(vec![Command::Send("hi".into())]);

        let events = rig.run(vec![Command::Compact("the tests".into())]);
        assert!(last_is_done(&events));
        assert_eq!(
            notices_of(&events),
            ["Compacted 2 messages into a 6-word summary. /rewind can't go back past this point."]
        );
        let asked = rig.bodies().pop().unwrap();
        assert!(asked.contains("Pay particular attention to: the tests"));
        assert_eq!(rig.agent.messages.len(), 2);
        assert!(rig.agent.messages[0].json.get().contains("They fixed bugs in main.rs today."));
        assert!(rig.agent.messages[0].json.get().contains("<compacted>"));
        assert!(!rig.agent.messages[1].user);
        assert!(events.iter().any(|e| matches!(e, Event::Activity(a) if a == "summarizing the conversation")));
        assert!(events.iter().any(|e| matches!(e, Event::Usage { main: false, .. })));
    }

    #[test]
    fn compact_joins_a_waiting_user_message_and_survives_bad_summaries() {
        let mut rig = Rig::new(
            "agent-compact-edge",
            Mode::Default,
            vec![
                ok(text_reply("   ")),
                Response { status: 400, body: r#"{"error":{"message":"bad request"}}"#.into() },
                ok(text_reply("fine")),
            ],
        );
        // The last message is a user message nobody has answered.
        rig.run(vec![Command::Remember("uses pnpm".into())]);
        let events = rig.run(vec![Command::Compact(String::new())]);
        assert_eq!(errors_of(&events), ["the summary came back empty; nothing was changed"]);
        let asked = rig.bodies().pop().unwrap();
        let parsed: Value = serde_json::from_str(&asked).unwrap();
        let msgs = parsed["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1, "the ask joined the user message: {asked}");
        assert_eq!(msgs[0]["content"].as_array().unwrap().len(), 2);
        assert!(!asked.contains("Pay particular attention"));
        assert_eq!(rig.agent.messages.len(), 1);

        let events = rig.run(vec![Command::Compact(String::new())]);
        assert_eq!(errors_of(&events), ["couldn't compact: API error 400: bad request"]);
        let events = rig.run(vec![Command::Compact(String::new())]);
        assert!(last_is_done(&events));
    }

    #[test]
    fn switching_model_rebuilds_tools_only_when_web_support_changes() {
        let mut cfg = config(None, Mode::Default);
        cfg.web = true;
        let mut rig = Rig::with("agent-model", cfg);
        let has_web = |rig: &Rig| rig.agent.defs.get().contains("web_search");
        assert!(has_web(&rig), "opus has the web tools");
        rig.run(vec![Command::Model("claude-haiku-5-5".into())]);
        assert_eq!(*rig.agent.core.model.lock().unwrap(), "claude-haiku-5-5");
        assert!(!has_web(&rig), "haiku does not, so they are dropped");
        rig.run(vec![Command::Model("claude-opus-5-5".into())]);
        assert!(has_web(&rig));
        // Between two models with them: nothing to rebuild.
        rig.run(vec![Command::Model("claude-sonnet-5-5".into())]);
        assert!(has_web(&rig));
        assert!(rig.agent.core.sub_defs.lock().unwrap().get().contains("web_fetch"));
        rig.run(vec![Command::Model("claude-haiku-5-5".into())]);
        assert!(!has_web(&rig));
        assert!(!rig.agent.core.sub_defs.lock().unwrap().get().contains("web_fetch"));
    }

    #[test]
    fn effort_key_and_name_commands_apply_at_once() {
        let mut rig = Rig::with("agent-settings", config(None, Mode::Default));
        rig.run(vec![Command::Effort(Some("high".into()))]);
        assert_eq!(rig.agent.core.effort.lock().unwrap().as_deref(), Some("high"));
        rig.run(vec![Command::Effort(None)]);
        assert_eq!(*rig.agent.core.effort.lock().unwrap(), None);
        rig.run(vec![Command::SetKey(String::new())]);
        assert!(!rig.agent.core.client.has_key());
        rig.run(vec![Command::SetKey("sk-new".into())]);
        assert!(rig.agent.core.client.has_key());
    }

    #[test]
    fn the_effort_goes_to_the_api() {
        let mut rig = Rig::new("agent-effort", Mode::Default, vec![ok(text_reply("ok"))]);
        rig.run(vec![Command::Effort(Some("max".into())), Command::Send("go".into())]);
        assert!(rig.bodies()[0].contains(r#""output_config":{"effort":"max"}"#));
    }

    #[test]
    fn shell_commands_run_show_and_tell_the_model() {
        let mut rig = Rig::new("agent-shell", Mode::Default, vec![ok(text_reply("ok"))]);
        let events = rig.run(vec![Command::Shell("echo from-user; exit 3".into())]);
        assert!(matches!(&events[0], Event::Tool { label, .. } if label == "! echo from-user; exit 3"));
        assert!(matches!(&events[1], Event::ToolResult { ok: false, summary } if summary.contains("from-user")));
        assert!(last_is_done(&events));
        let note = rig.agent.messages.last().unwrap().json.get().to_string();
        assert!(note.contains("<user-shell>\\n$ echo from-user; exit 3\\nfrom-user"), "{note}");

        // The model sees it with the next message, as part of the same turn.
        rig.run(vec![Command::Send("what did I run?".into())]);
        let body = rig.bodies().pop().unwrap();
        assert!(body.contains("user-shell") && body.contains("what did I run?"));
        assert_eq!(rig.agent.messages.len(), 2, "the note and the question are one user message");
    }

    #[test]
    fn clear_starts_a_new_conversation() {
        let mut rig = Rig::new("agent-clear", Mode::Default, vec![ok(text_reply("a"))]);
        rig.run(vec![Command::Send("one".into()), Command::Rename("unit-cleared".into())]);
        assert_eq!(rig.agent.messages.len(), 2);
        tick();
        rig.run(vec![Command::Clear]);
        assert!(rig.agent.messages.is_empty());
        assert_eq!(rig.agent.session.name, None);
        assert!(!rig.agent.told_plan);
    }

    #[test]
    fn plan_mode_is_announced_once_and_retracted_once() {
        let mut rig = Rig::new(
            "agent-plan",
            Mode::Plan,
            vec![ok(text_reply("a")), ok(text_reply("b")), ok(text_reply("c")), ok(text_reply("d"))],
        );
        let on = "Plan mode is on.";
        let off = "Plan mode is off.";
        rig.run(vec![Command::Send("one".into())]);
        rig.run(vec![Command::Send("two".into())]);
        rig.agent.core.mode.set(Mode::Default);
        rig.run(vec![Command::Send("three".into())]);
        rig.run(vec![Command::Send("four".into())]);
        let bodies = rig.bodies();
        assert_eq!(bodies[0].matches(on).count(), 1);
        assert_eq!(bodies[1].matches(on).count(), 1, "not repeated: the history already has it");
        assert_eq!(bodies[2].matches(off).count(), 1);
        assert_eq!(bodies[3].matches(off).count(), 1);
        assert!(!bodies[0].contains(off));
    }

    #[test]
    fn image_paths_in_a_message_are_attached() {
        let mut rig = Rig::new("agent-images", Mode::Default, vec![ok(text_reply("nice"))]);
        let shot = rig.dir.join("shot.png");
        std::fs::write(&shot, b"abc").unwrap();
        let huge = rig.dir.join("huge.png");
        std::fs::File::create(&huge).unwrap().set_len(6 * 1024 * 1024).unwrap();

        let events = rig.run(vec![Command::Send(format!("look at {} and {}", shot.display(), huge.display()))]);
        let notices = notices_of(&events);
        assert_eq!(notices[0], format!("Attached {} (0 KB)", shot.display()));
        assert!(notices[1].contains("images can be at most 5 MB"), "{notices:?}");
        let body = rig.bodies().pop().unwrap();
        assert!(body.contains(r#""media_type":"image/png""#));
        assert!(body.contains(r#""data":"YWJj""#), "the file's bytes are sent as base64: {body}");
        // The image comes before the text in one message.
        let first = rig.agent.messages[0].content();
        assert_eq!((first[0]["type"].as_str(), first[1]["type"].as_str()), (Some("image"), Some("text")));
    }

    #[test]
    fn a_failed_request_leaves_the_message_to_be_joined_by_the_next() {
        let mut rig = Rig::new(
            "agent-failed",
            Mode::Default,
            vec![Response { status: 400, body: r#"{"error":{"message":"nope"}}"#.into() }, ok(text_reply("ok"))],
        );
        let events = rig.run(vec![Command::Send("first".into())]);
        assert_eq!(errors_of(&events), ["API error 400: nope"]);
        assert_eq!(rig.agent.messages.len(), 1);
        let events = rig.run(vec![Command::Send("second".into())]);
        assert!(last_is_done(&events));
        assert_eq!(rig.agent.messages.len(), 2, "both prompts are in one user message");
        assert_eq!(rig.agent.messages[0].content().len(), 2);
    }

    #[test]
    fn remembered_notes_reach_the_model_with_the_next_message() {
        let mut rig = Rig::new("agent-remember", Mode::Default, vec![ok(text_reply("ok"))]);
        rig.run(vec![Command::Remember("uses pnpm".into()), Command::Send("hi".into())]);
        let body = rig.bodies().pop().unwrap();
        assert!(body.contains("The user saved this to memory: uses pnpm"));
        assert_eq!(rig.agent.messages.len(), 2);
    }

    #[test]
    fn finished_background_jobs_are_reported_to_the_model() {
        let mut rig = Rig::new("agent-jobs", Mode::Default, vec![ok(text_reply("ok"))]);
        let no = AtomicBool::new(false);
        rig.agent.tools.run("bash", &json!({"command": "exit 2", "background": true}), &no).unwrap();
        let mut notices = Vec::new();
        for _ in 0..100 {
            thread::sleep(Duration::from_millis(50));
            notices.extend(notices_of(&rig.events()));
            if !notices.is_empty() {
                break;
            }
        }
        assert_eq!(notices, ["Background job 1 finished (exit 2): exit 2"]);
        thread::sleep(Duration::from_millis(200));
        rig.run(vec![Command::Send("anything new?".into())]);
        let body = rig.bodies().pop().unwrap();
        assert!(body.contains("Background job 1 (`exit 2`) finished: exit 2."), "{body}");
    }

    #[test]
    fn skills_run_as_a_message_with_their_instructions() {
        crate::tools::test_home();
        let skill = crate::config::config_dir().join("skills/unit-agent-skill");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "---\nname: unit-agent-skill\ndescription: d\n---\nStep one.\n")
            .unwrap();
        let mut rig = Rig::new("agent-skill", Mode::Default, vec![ok(text_reply("ok"))]);
        // The test rig's tools have no skills; the agent's own tools found this one.
        let skills = skill::discover();
        assert!(skills.iter().any(|s| s.name == "unit-agent-skill"));
        rig.agent.tools = Arc::new(Tools::new(rig.dir.clone(), skills, mcp::Servers::default()));

        let events = rig.run(vec![Command::Skill { name: "nope".into(), args: String::new() }]);
        assert_eq!(errors_of(&events), ["no skill named nope"]);
        let events = rig.run(vec![Command::Skill { name: "unit-agent-skill".into(), args: "for src/".into() }]);
        assert!(last_is_done(&events));
        let body = rig.bodies().pop().unwrap();
        assert!(body.contains(r#"<skill name=\"unit-agent-skill\">"#) && body.contains("Step one."));
        assert!(body.contains("for src/"));

        // A skill whose file went away since startup is an error, not a message.
        std::fs::remove_file(skill.join("SKILL.md")).unwrap();
        let events = rig.run(vec![Command::Skill { name: "unit-agent-skill".into(), args: String::new() }]);
        assert!(errors_of(&events)[0].contains("SKILL.md"), "{:?}", errors_of(&events));
        assert_eq!(rig.bodies().len(), 1, "nothing was sent");
    }

    #[test]
    fn rewinding_a_turn_that_changed_nothing_works() {
        let mut rig = Rig::new("agent-rewind-plain", Mode::Default, vec![ok(text_reply("hello"))]);
        rig.run(vec![Command::Send("just talk".into())]);
        let events = rig.run(vec![Command::Rewind(Some(0))]);
        assert!(matches!(events.first(), Some(Event::Rewound { files: 0, .. })), "{:?}", errors_of(&events));
        assert!(rig.agent.messages.is_empty());
    }

    #[test]
    fn rewind_that_cannot_restore_keeps_the_conversation() {
        use std::os::unix::fs::PermissionsExt;
        let write = tool_use("w1", "write", json!({"path": "sub/a.txt", "content": "hi\n"}));
        let mut rig =
            Rig::new("agent-rewind-fail", Mode::Bypass, vec![ok(tools_reply(&[write])), ok(text_reply("Wrote it."))]);
        rig.run(vec![Command::Send("make sub/a.txt".into())]);
        // Removing a file needs write access to its folder.
        let sub = rig.dir.join("sub");
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o500)).unwrap();
        let events = rig.run(vec![Command::Rewind(Some(0))]);
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o700)).unwrap();
        let errors = errors_of(&events);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].starts_with("couldn't restore"), "{errors:?}");
        assert_eq!(rig.agent.messages.len(), 4, "the conversation is left as it was");
        assert!(sub.join("a.txt").exists());

        // The entry stays in the index, so a second try works.
        let events = rig.run(vec![Command::Rewind(Some(0))]);
        assert!(matches!(events.first(), Some(Event::Rewound { files: 1, .. })));
        assert!(!sub.join("a.txt").exists());
    }
}
