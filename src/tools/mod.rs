// Tools the model can call. Each family lives in its own file; this one holds
// the shared state, the definitions sent to the API, and the dispatch.
//
// `Tools` is shared by reference across threads (parallel calls, subagents),
// so its state sits behind small locks and every method takes `&self`.

mod bash;
pub use bash::kill_group;
pub mod checkpoint;
mod fs;
pub mod mcp;
pub mod memory;
pub mod skill;
#[cfg(test)]
mod stress;
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
        "read" | "todo" | "ask_user" | "skill" | "agent" | "jobs" => Access::Free,
        // Notes are loaded into every future session's system prompt.
        "write" | "edit" | "memory" => Access::Edit,
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
    /// A subagent's tools: no memory.
    sub: bool,
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
            sub: false,
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
            sub: true,
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
    /// `user`: someone is there to answer `ask_user` (not so in -p mode).
    pub fn definitions(&self, main: bool, web: bool, user: bool) -> Box<RawValue> {
        let mut defs = vec![fs::READ.clone(), fs::WRITE.clone(), fs::EDIT.clone(), bash::BASH.clone()];
        defs.extend([bash::JOBS.clone(), todo::TODO.clone()]);
        if main {
            if user {
                defs.push(ASK_USER.clone());
            }
            defs.extend([AGENT.clone(), memory::MEMORY.clone()]);
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
                if let Some(real) = self.escapes(&full) {
                    return real.display().to_string();
                }
                // Outside the project: the full resolved path, never the raw text.
                full.strip_prefix(&self.root).unwrap_or(&full).display().to_string()
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
        self.labelled(name, input, &shorten(subject.lines().next().unwrap_or(""), 80))
    }

    /// Like `label`, but with every line of the subject, for approval prompts.
    pub fn full_label(&self, name: &str, input: &Value) -> String {
        let subject = self.subject(name, input);
        self.labelled(name, input, &shorten(&subject, 2000))
    }

    fn labelled(&self, name: &str, input: &Value, arg: &str) -> String {
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
            "memory" if self.sub => Err("subagents can't change memory".into()),
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

    /// An absolute path with `.` and `..` worked out, so `src/../../x` can't
    /// pass for a path inside the project in rules, auto mode or labels.
    fn resolve(&self, p: &str) -> PathBuf {
        let p = Path::new(p);
        normalize(&if p.is_absolute() { p.to_path_buf() } else { self.root.join(p) })
    }
}

impl Tools {
    /// If `full` (already normalized) is inside the project by its text but
    /// really leads outside through a symlink, the real path. The deepest
    /// existing ancestor is resolved; the part that doesn't exist yet can't
    /// be a link.
    fn escapes(&self, full: &Path) -> Option<PathBuf> {
        full.strip_prefix(&self.root).ok()?;
        let root = self.root.canonicalize().ok()?;
        let mut existing = full;
        let mut rest = Vec::new();
        let real = loop {
            if let Ok(r) = existing.canonicalize() {
                break r;
            }
            rest.push(existing.file_name()?);
            existing = existing.parent()?;
        };
        if real.starts_with(&root) {
            return None;
        }
        Some(rest.iter().rev().fold(real, |p, c| p.join(c)))
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

/// Resolves `.` and `..` without touching the disk (the file may not exist yet).
fn normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            c => out.push(c),
        }
    }
    out
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

/// Points claude-jr's data and config folders at a private temp folder, once
/// per test run, so tests never touch the real ones. (Environment variables
/// are shared by every test in the process, so they're set only here.)
#[cfg(test)]
pub(crate) fn test_home() -> PathBuf {
    static HOME: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let d = test_dir("home");
        unsafe {
            std::env::set_var("XDG_DATA_HOME", d.join("data"));
            std::env::set_var("XDG_CONFIG_HOME", d.join("config"));
        }
        d
    })
    .clone()
}

#[cfg(test)]
pub(crate) fn test_dir(name: &str) -> PathBuf {
    // Once per run, remove the folders that earlier runs left behind: the
    // name ends with the process id, and that process is gone.
    static SWEPT: std::sync::Once = std::sync::Once::new();
    SWEPT.call_once(|| {
        for entry in std::fs::read_dir(std::env::temp_dir()).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let pid = name.strip_prefix("cjr-test-").and_then(|n| n.rsplit('-').next().map(str::to_string));
            if pid.is_some_and(|pid| !Path::new("/proc").join(pid).exists()) {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    });
    let d = std::env::temp_dir().join(format!("cjr-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[cfg(test)]
pub(crate) fn test_tools(name: &str) -> (Tools, PathBuf) {
    test_home();
    let dir = test_dir(name);
    (Tools::new(dir.clone(), Vec::new(), mcp::Servers::default()), dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_that_climb_out_are_not_inside_the_project() {
        let (t, dir) = test_tools("paths");
        let subject = |p: &str| t.subject("edit", &json!({ "path": p }));
        assert_eq!(subject("src/./app.css"), "src/app.css");
        assert_eq!(subject("src/../index.html"), "index.html");
        let escaped = subject("src/../../outside.txt");
        assert!(escaped.starts_with('/'), "{escaped}");
        assert_eq!(PathBuf::from(&escaped), dir.parent().unwrap().join("outside.txt"));
        let rules = crate::permission::Rules::new(vec!["edit:src/*".into()]);
        assert!(rules.allows("edit", &subject("src/a.css")));
        assert!(!rules.allows("edit", &subject("src/../../etc/passwd")));
    }

    #[test]
    fn symlinks_out_of_the_project_count_as_outside() {
        let (t, dir) = test_tools("symlink");
        let out = test_dir("symlink-target");
        std::fs::write(out.join("f.txt"), "x").unwrap();
        std::os::unix::fs::symlink(&out, dir.join("link")).unwrap();
        std::fs::create_dir(dir.join("real")).unwrap();
        std::os::unix::fs::symlink(dir.join("real"), dir.join("inner")).unwrap();
        let subject = |p: &str| t.subject("edit", &json!({ "path": p }));
        let canon = out.canonicalize().unwrap();
        assert_eq!(PathBuf::from(subject("link/f.txt")), canon.join("f.txt"));
        assert_eq!(PathBuf::from(subject("link/new/deep.txt")), canon.join("new/deep.txt"));
        assert!(crate::permission::outside_project(&subject("link/f.txt")));
        // A link that stays inside the project is fine.
        assert_eq!(subject("inner/a.txt"), "inner/a.txt");
        assert_eq!(subject("fresh/a.txt"), "fresh/a.txt");
    }

    #[test]
    fn memory_asks_and_subagents_cannot_use_it() {
        let (t, _dir) = test_tools("memory_access");
        assert_eq!(access("memory"), Access::Edit);
        let input = json!({ "action": "add", "note": "uses pnpm" });
        assert_eq!(t.subject("memory", &input), "add: uses pnpm");
        assert!(t.child().run("memory", &input, &AtomicBool::new(false)).is_err());
        for main in [true, false] {
            let defs = t.child().definitions(main, false, true);
            let names: Vec<String> = serde_json::from_str::<Vec<Value>>(defs.get())
                .unwrap()
                .iter()
                .map(|d| d["name"].as_str().unwrap().to_string())
                .collect();
            for n in ["agent", "memory", "ask_user"] {
                assert_eq!(names.iter().any(|x| x == n), main, "{n} main={main}");
            }
        }
    }

    #[test]
    fn full_label_shows_every_line() {
        let (t, _dir) = test_tools("full_label");
        let input = json!({ "command": "ls\nrm -rf /tmp/x" });
        assert!(!t.label("bash", &input).contains("rm"));
        assert!(t.full_label("bash", &input).contains("rm -rf /tmp/x"));
    }

    fn run(t: &Tools, name: &str, input: Value) -> Output {
        t.run(name, &input, &AtomicBool::new(false))
    }

    #[test]
    fn what_each_tool_needs_and_what_can_overlap() {
        for n in ["read", "todo", "ask_user", "skill", "agent", "jobs"] {
            assert_eq!(access(n), Access::Free, "{n}");
        }
        for n in ["write", "edit", "memory"] {
            assert_eq!(access(n), Access::Edit, "{n}");
        }
        for n in ["bash", "mcp__srv__tool", "anything-new"] {
            assert_eq!(access(n), Access::Exec, "{n}");
        }
        for n in ["read", "agent", "skill", "mcp__a__b"] {
            assert!(parallel_safe(n), "{n}");
        }
        for n in ["bash", "write", "edit", "ask_user", "memory", "jobs", "todo"] {
            assert!(!parallel_safe(n), "{n}");
        }
    }

    #[test]
    fn unknown_and_memory_calls_are_dispatched() {
        let (t, _dir) = test_tools("mod-dispatch");
        assert_eq!(run(&t, "nope", json!({})).unwrap_err(), "unknown tool: nope");
        assert_eq!(run(&t, "memory", json!({"action": "add", "note": "  "})).unwrap_err(), "the note is empty");
        assert_eq!(run(&t, "skill", json!({"name": "missing"})).unwrap_err(), "no skill named missing");
        assert_eq!(run(&t, "mcp__none__tool", json!({})).unwrap_err(), "unknown MCP tool mcp__none__tool");
    }

    #[test]
    fn subjects_and_labels_for_every_tool() {
        let (t, _dir) = test_tools("mod-labels");
        assert_eq!(t.subject("agent", &json!({"description": "find usages"})), "find usages");
        assert_eq!(t.subject("skill", &json!({"name": "release"})), "release");
        assert_eq!(t.subject("jobs", &json!({"action": "kill", "id": 3})), "kill 3");
        assert_eq!(t.subject("todo", &json!({})), "");
        assert_eq!(t.subject("mcp__gh__issue", &json!({"title": "x"})), "title: x");

        assert_eq!(t.label("ask_user", &json!({})), "Question");
        assert_eq!(t.label("todo", &json!({})), "Todos");
        assert_eq!(t.label("bash", &json!({"command": "npm run dev", "background": true})), "Bash(npm run dev) &");
        assert_eq!(t.label("mcp__gh__issue", &json!({"title": "x"})), "gh: issue(title: x)");
        assert_eq!(t.label("edit", &json!({"path": "src/a.rs"})), "Edit(src/a.rs)");
        // The short label keeps the first line, cut at 80 characters.
        let long = "x".repeat(100);
        assert_eq!(
            t.label("bash", &json!({"command": format!("{long}\nsecond")})),
            format!("Bash({}\u{2026})", "x".repeat(80))
        );
        assert_eq!(t.full_label("bash", &json!({"command": "a\nb"})), "Bash(a\nb)");
    }

    #[test]
    fn definitions_carry_streaming_and_optional_web_tools() {
        let (t, _dir) = test_tools("mod-defs");
        let defs = |main: bool, web: bool| -> Vec<Value> {
            serde_json::from_str(t.definitions(main, web, true).get()).unwrap()
        };
        let plain = defs(true, false);
        assert!(plain.iter().all(|d| d["name"] != "web_search"));
        assert!(plain.iter().all(|d| d["eager_input_streaming"] == true), "our own tools stream their input");
        let web = defs(true, true);
        assert_eq!(web.len(), plain.len() + 2);
        let search = web.iter().find(|d| d["name"] == "web_search").unwrap();
        assert_eq!(search["type"], "web_search_20260209");
        assert!(search.get("eager_input_streaming").is_none(), "server tools don't take the field");
        assert!(web.iter().any(|d| d["name"] == "web_fetch"));
        // Subagents get the basics only.
        let sub: Vec<String> = defs(false, false).iter().map(|d| d["name"].as_str().unwrap().to_string()).collect();
        assert_eq!(sub, ["read", "write", "edit", "bash", "jobs", "todo"]);
    }

    #[test]
    fn images_come_back_as_blocks_or_a_reason() {
        let (t, dir) = test_tools("mod-images");
        std::fs::write(dir.join("ok.png"), b"abc").unwrap();
        let out = run(&t, "read", json!({"path": "ok.png"})).unwrap();
        let content = result_content(out);
        assert_eq!(content[0]["type"], "image");
        assert_eq!(content[1]["text"], format!("Image: {}", dir.join("ok.png").display()));

        let big = dir.join("big.png");
        std::fs::File::create(&big).unwrap().set_len(crate::images::MAX_BYTES + 1).unwrap();
        let err = run(&t, "read", json!({"path": "big.png"})).unwrap_err();
        assert_eq!(err, format!("{} is 5 MB; images can be at most 5 MB", big.display()));

        // An image that vanished after being read becomes a plain error string.
        let gone = result_content(format!("{IMAGE}{}", dir.join("gone.png").display()));
        assert!(gone.as_str().unwrap().contains("gone.png"), "{gone}");
        assert_eq!(result_content("plain".into()), json!("plain"));
    }

    #[test]
    fn summaries_are_short() {
        let ok = |s: &str| Ok(s.to_string());
        assert_eq!(summary("bash", &ok("a\nb\nc")), "a\nb\nc");
        assert_eq!(summary("bash", &ok("1\n2\n3\n4\n5\n")), "1\n2\n3\n\u{2026} +2 lines");
        assert_eq!(summary("bash", &Err("boom\n".into())), "boom");
        assert_eq!(summary("read", &ok("     1\ta\n     2\tb\n")), "Read 2 lines");
        assert_eq!(summary("read", &ok("     1\ta\n")), "Read 1 line");
        let todos: String = (1..=25).map(|i| format!("{i}\n")).collect();
        let s = summary("todo", &ok(&todos));
        assert_eq!(s.lines().count(), 21);
        assert!(s.ends_with("\u{2026} +5 lines"));
        assert_eq!(summary("ask_user", &ok("The user answered: yes")), "yes");
    }

    #[test]
    fn paths_and_text_helpers() {
        assert_eq!(normalize(Path::new("/a/./b/../c")), PathBuf::from("/a/c"));
        assert_eq!(normalize(Path::new("a/b/../../..")), PathBuf::from(""));
        assert_eq!(normalize(Path::new("/..")), PathBuf::from("/"));
        assert_eq!(shorten("hello", 5), "hello");
        assert_eq!(shorten("h\u{e9}llo w\u{f6}rld", 6), "h\u{e9}llo \u{2026}");
        assert_eq!(capitalize("bash"), "Bash");
        assert_eq!(capitalize(""), "");
    }
}
