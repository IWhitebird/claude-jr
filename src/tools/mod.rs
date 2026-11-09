// Tools the model can call. Each family lives in its own file; this one holds
// the shared state, the definitions sent to the API, and the dispatch.
//
// `Tools` is shared by reference across threads (parallel calls, subagents),
// so its state sits behind small locks and every method takes `&self`.

mod bash;
pub mod checkpoint;
mod fs;
pub mod mcp;
pub mod memory;
pub mod skill;
mod todo;

use serde_json::value::RawValue;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

pub use bash::{JobEvent, Notify};
pub use skill::Skill;

pub type Output = Result<String, String>;

/// Tool output kept for the model, start and end, in bytes.
const MAX_BYTES: usize = 50 * 1024;

/// What approval a call needs.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Access {
    /// Reads, bookkeeping and questions: never asks.
    Free,
    /// Changes files: asks, unless accept-edits mode or a rule allows it.
    Edit,
    /// Runs commands or external tools: asks, unless a rule allows it.
    Exec,
}

pub fn access(name: &str) -> Access {
    match name {
        "read" | "todo" | "ask_user" | "skill" | "agent" | "jobs" | "memory" => Access::Free,
        "write" | "edit" => Access::Edit,
        _ => Access::Exec, // bash and MCP tools
    }
}

/// Calls that can run at the same time as their neighbours in one reply.
pub fn parallel_safe(name: &str) -> bool {
    matches!(name, "read" | "agent" | "skill") || mcp::is_mcp(name)
}

/// Before and after text of a change, for showing a diff.
pub struct Diff {
    /// Line number in the file where `old` starts.
    pub line: usize,
    pub old: String,
    pub new: String,
}

/// State one agent's tools share. A subagent gets its own (`child`), with
/// its own shell and read history but the same skills and MCP servers.
pub struct Tools {
    root: PathBuf,
    /// The shell's working directory; `cd` carries over between calls.
    cwd: Mutex<PathBuf>,
    /// Files read so far, with their modified time at that moment.
    seen: Mutex<HashMap<PathBuf, Option<SystemTime>>>,
    jobs: bash::Jobs,
    /// Foreground commands running right now (several, with subagents).
    running: Mutex<Vec<u32>>,
    todos: Mutex<Vec<todo::Todo>>,
    skills: Arc<Vec<Skill>>,
    pub mcp: Arc<mcp::Servers>,
    /// Originals of changed files, for /rewind. Shared with subagents.
    pub checkpoints: Arc<checkpoint::Checkpoints>,
}

impl Tools {
    pub fn new(root: PathBuf, skills: Vec<Skill>, mcp: mcp::Servers) -> Tools {
        Tools {
            cwd: Mutex::new(root.clone()),
            root,
            seen: Mutex::default(),
            jobs: bash::Jobs::default(),
            running: Mutex::default(),
            todos: Mutex::default(),
            skills: Arc::new(skills),
            mcp: Arc::new(mcp),
            checkpoints: Arc::default(),
        }
    }

    pub fn child(&self) -> Tools {
        Tools {
            cwd: Mutex::new(self.root.clone()),
            root: self.root.clone(),
            seen: Mutex::default(),
            jobs: bash::Jobs::default(),
            running: Mutex::default(),
            todos: Mutex::default(),
            skills: self.skills.clone(),
            mcp: self.mcp.clone(),
            checkpoints: self.checkpoints.clone(),
        }
    }

    pub fn skills(&self) -> &[Skill] {
        &self.skills
    }

    /// Tool definitions for the API, serialized once per session. Subagents
    /// don't get `agent` (no nesting) or `ask_user` (they can't reach the user).
    /// `web` adds Anthropic's web search and fetch, which run on their side.
    pub fn definitions(&self, main: bool, web: bool) -> Box<RawValue> {
        let mut defs = vec![fs::READ.clone(), fs::WRITE.clone(), fs::EDIT.clone(), bash::BASH.clone()];
        defs.extend([bash::JOBS.clone(), todo::TODO.clone()]);
        if main {
            defs.extend([ASK_USER.clone(), AGENT.clone(), memory::MEMORY.clone()]);
        }
        if !self.skills.is_empty() {
            defs.push(skill::definition(&self.skills));
        }
        defs.extend(self.mcp.definitions());
        if web {
            defs.push(def(json!({ "type": "web_search_20260209", "name": "web_search", "max_uses": 5 })));
            defs.push(def(json!({ "type": "web_fetch_20260209", "name": "web_fetch", "max_uses": 5 })));
        }
        let mut defs: Vec<Value> =
            defs.iter().map(|d| serde_json::from_str(d.get()).expect("definitions are valid JSON")).collect();
        // Stream big inputs (file contents) as they're written. Only for our own
        // tools; server tools and remote MCP toolsets don't take the field.
        for d in defs.iter_mut().filter(|d| d.get("input_schema").is_some()) {
            d["eager_input_streaming"] = json!(true);
        }
        serde_json::value::to_raw_value(&defs).expect("tool definitions serialize")
    }

    /// What a call acts on: the command for bash, the path (relative to the
    /// project root when inside it) for file tools. Permission rules match this.
    pub fn subject(&self, name: &str, input: &Value) -> String {
        match name {
            "bash" => input["command"].as_str().unwrap_or("").to_string(),
            "write" | "edit" | "read" => {
                let p = input["path"].as_str().unwrap_or("");
                let full = self.resolve(p);
                full.strip_prefix(&self.root).map(|r| r.display().to_string()).unwrap_or_else(|_| p.to_string())
            }
            "agent" => input["description"].as_str().unwrap_or("").to_string(),
            "skill" => input["name"].as_str().unwrap_or("").to_string(),
            "jobs" => format!("{} {}", input["action"].as_str().unwrap_or(""), input["id"]).trim().to_string(),
            "memory" => format!("{}: {}", input["action"].as_str().unwrap_or(""), input["note"].as_str().unwrap_or("")),
            "ask_user" | "todo" => String::new(),
            _ => mcp::summarize_args(input),
        }
    }

    /// Short label for the UI, like `Edit(src/main.rs)`.
    pub fn label(&self, name: &str, input: &Value) -> String {
        let subject = self.subject(name, input);
        let arg = shorten(subject.lines().next().unwrap_or(""), 80);
        let head = match name {
            "ask_user" => return "Question".into(),
            "todo" => return "Todos".into(),
            n if mcp::is_mcp(n) => mcp::display_name(n),
            n => capitalize(n),
        };
        let bg = if name == "bash" && input["background"] == true { " &" } else { "" };
        format!("{head}({arg}){bg}")
    }

    /// The change an edit or write would make, without making it.
    pub fn preview(&self, name: &str, input: &Value) -> Option<Diff> {
        fs::preview(self, name, input)
    }

    pub fn run(&self, name: &str, input: &Value, cancel: &AtomicBool) -> Output {
        match name {
            "read" => fs::read(self, input),
            "write" => fs::write(self, input),
            "edit" => fs::edit(self, input),
            "bash" => bash::run(self, input, cancel),
            "jobs" => bash::jobs(self, input, cancel),
            "todo" => todo::update(self, input),
            "skill" => skill::load(&self.skills, input),
            "memory" => memory::update(input),
            n if mcp::is_mcp(n) => self.mcp.call(n, input, cancel),
            _ => Err(format!("unknown tool: {name}")),
        }
    }

    /// Called with a one-line message whenever a background job ends.
    pub fn on_job_finished(&self, f: Notify) {
        self.jobs.on_finish(f);
    }

    /// Background jobs that finished since the last call, for telling the model.
    pub fn finished_jobs(&self) -> Vec<JobEvent> {
        self.jobs.take_finished()
    }

    /// (id, command, running) for every background job.
    pub fn jobs(&self) -> Vec<(usize, String, bool)> {
        self.jobs.summary()
    }

    pub fn kill_job(&self, id: usize) -> bool {
        self.jobs.kill(id)
    }

    pub fn running_jobs(&self) -> usize {
        self.jobs.running()
    }

    /// Process ids of everything we started that's still alive, for /stats.
    pub fn child_pids(&self) -> Vec<u32> {
        let mut pids = self.running.lock().unwrap().clone();
        pids.extend(self.jobs.pids());
        pids.extend(self.mcp.pids());
        pids
    }

    /// Stops background jobs and local MCP servers. Called on exit, because
    /// jobs run in their own process group and would otherwise outlive us.
    pub fn shutdown(&self) {
        for &pid in self.running.lock().unwrap().iter() {
            bash::kill_group(pid);
        }
        self.jobs.kill_all();
        self.mcp.stop();
    }

    /// Runs a command the user typed (`!cmd`): same shell, no approval.
    pub fn user_shell(&self, cmd: &str, cancel: &AtomicBool) -> Output {
        bash::run(self, &json!({ "command": cmd }), cancel)
    }

    fn resolve(&self, p: &str) -> PathBuf {
        let p = Path::new(p);
        if p.is_absolute() { p.to_path_buf() } else { self.root.join(p) }
    }
}

/// Tools return text. An image is returned as this marker followed by its
/// path, and turned into an image block by `result_content`.
const IMAGE: &str = "\u{0}image:";

fn image_output(path: &Path) -> Output {
    let size = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?.len();
    if size > crate::images::MAX_BYTES {
        return Err(format!("{} is {} MB; images can be at most 5 MB", path.display(), size / 1_048_576));
    }
    Ok(format!("{IMAGE}{}", path.display()))
}

/// The `content` of a tool_result: the text, or for an image an image block
/// plus a line saying which file it is.
pub fn result_content(text: String) -> Value {
    match text.strip_prefix(IMAGE) {
        Some(path) => match crate::images::block(Path::new(path)) {
            Ok(image) => json!([image, { "type": "text", "text": format!("Image: {path}") }]),
            Err(e) => Value::String(e),
        },
        None => Value::String(text),
    }
}

/// A few lines describing a tool result, for the UI.
pub fn summary(name: &str, out: &Output) -> String {
    let (text, max) = match out {
        Ok(s) if s.starts_with(IMAGE) => {
            let path = &s[IMAGE.len()..];
            let kb = std::fs::metadata(path).map_or(0, |m| m.len() / 1024);
            return format!("Read image ({kb} KB)");
        }
        Ok(s) if name == "read" => {
            let n = s.lines().filter(|l| l.as_bytes().get(6) == Some(&b'\t')).count();
            return format!("Read {n} line{}", if n == 1 { "" } else { "s" });
        }
        Ok(s) if name == "todo" => (s.trim_end(), 20),
        Ok(s) if name == "ask_user" => return s.trim_start_matches("The user answered: ").to_string(),
        Ok(s) | Err(s) => (s.trim_end(), 3),
    };
    let mut lines = text.lines();
    let mut out: Vec<&str> = lines.by_ref().take(max).collect();
    let rest = lines.count();
    let more = format!("… +{rest} lines");
    if rest > 0 {
        out.push(&more);
    }
    out.join("\n")
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map(|c| c.to_ascii_uppercase()).into_iter().chain(chars).collect()
}

pub(crate) fn shorten(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

fn def(v: Value) -> Box<RawValue> {
    serde_json::value::to_raw_value(&v).expect("tool definition serializes")
}

// Handled by the agent, which can reach the user and start subagents.
static ASK_USER: std::sync::LazyLock<Box<RawValue>> = std::sync::LazyLock::new(|| {
    def(json!({
        "name": "ask_user",
        "description": "Ask the user a question when you need a decision or information only they have. \
            Offer 2-4 short options when the choices are clear; the user can also type their own answer. \
            Don't use this to ask permission for tool calls; the user is asked about those automatically.",
        "input_schema": {
            "type": "object",
            "properties": {
                "question": { "type": "string" },
                "options": { "type": "array", "items": { "type": "string" } }
            },
            "required": ["question"]
        }
    }))
});

static AGENT: std::sync::LazyLock<Box<RawValue>> = std::sync::LazyLock::new(|| {
    def(json!({
        "name": "agent",
        "description": "Start a subagent with its own fresh context for a self-contained task, such as \
            searching a large codebase or researching a question, and get back its final report. \
            Several agent calls in one reply run in parallel. The subagent can't see this conversation, \
            so give it everything it needs in the prompt. It can't ask the user questions.",
        "input_schema": {
            "type": "object",
            "properties": {
                "description": { "type": "string", "description": "3-6 words, shown to the user" },
                "prompt": { "type": "string", "description": "The full task" }
            },
            "required": ["description", "prompt"]
        }
    }))
});

#[cfg(test)]
pub(crate) fn test_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("cjr-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[cfg(test)]
pub(crate) fn test_tools(name: &str) -> (Tools, PathBuf) {
    let dir = test_dir(name);
    (Tools::new(dir.clone(), Vec::new(), mcp::Servers::default()), dir)
}
