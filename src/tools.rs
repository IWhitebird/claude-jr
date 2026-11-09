// The four tools: read, write, edit, bash.

use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

const MAX_LINES: usize = 2000;
const MAX_BYTES: usize = 50 * 1024;
const MAX_LINE_CHARS: usize = 2000;

pub type Output = Result<String, String>;

pub fn definitions() -> Box<serde_json::value::RawValue> {
    let tools = json!([
        {
            "name": "read",
            "description": "Read a text file. Returns lines prefixed with their line number and a tab. \
                Reads up to 2000 lines; use offset and limit for more. You must read a file before editing or overwriting it.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path, absolute or relative to the working directory" },
                    "offset": { "type": "integer", "description": "First line to read, starting at 1" },
                    "limit": { "type": "integer", "description": "Number of lines to read" }
                },
                "required": ["path"]
            }
        },
        {
            "name": "write",
            "description": "Create a file or replace its whole content. Creates parent directories. \
                An existing file must be read first. Prefer edit for changes to existing files.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }
        },
        {
            "name": "edit",
            "description": "Replace exact text in a file. old_string must match exactly once, including whitespace, \
                unless replace_all is true. The file must be read first. Do not include the line number prefixes from read.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old_string": { "type": "string" },
                    "new_string": { "type": "string" },
                    "replace_all": { "type": "boolean" }
                },
                "required": ["path", "old_string", "new_string"]
            }
        },
        {
            "name": "bash",
            "description": "Run a bash command. stdout and stderr are combined. The working directory carries over \
                between calls. Long output keeps the start and the end. Use rg, find and ls to search.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "timeout": { "type": "integer", "description": "Seconds, default 120, max 600" }
                },
                "required": ["command"]
            }
        }
    ]);
    let Value::Array(mut tools) = tools else { unreachable!() };
    for t in &mut tools {
        t["eager_input_streaming"] = json!(true);
    }
    // Serialized once; every request reuses the same bytes.
    serde_json::value::to_raw_value(&tools).expect("tool definitions serialize")
}

/// Before and after text of a change, for showing a diff.
pub struct Diff {
    /// Line number in the file where `old` starts.
    pub line: usize,
    pub old: String,
    pub new: String,
}

/// A few lines describing a tool result, for the UI.
pub fn summary(name: &str, out: &Output) -> String {
    let text = match out {
        Ok(s) if name == "read" => {
            let n = s.lines().filter(|l| l.as_bytes().get(6) == Some(&b'\t')).count();
            return format!("Read {n} line{}", if n == 1 { "" } else { "s" });
        }
        Ok(s) | Err(s) => s.trim_end(),
    };
    let mut lines = text.lines();
    let mut out: Vec<&str> = lines.by_ref().take(3).collect();
    let rest = lines.count();
    let more = format!("… +{rest} lines");
    if rest > 0 {
        out.push(&more);
    }
    out.join("\n")
}

pub fn needs_permission(name: &str) -> bool {
    name != "read"
}

pub struct Tools {
    root: PathBuf,
    cwd: PathBuf,
    /// Files the model has read, with their modified time at that moment.
    seen: HashMap<PathBuf, Option<SystemTime>>,
}

impl Tools {
    pub fn new(root: PathBuf) -> Self {
        Tools { cwd: root.clone(), root, seen: HashMap::new() }
    }

    /// What a call acts on: the command for bash, the path (relative to the
    /// project root when inside it) for file tools. Permission rules match this.
    pub fn subject(&self, name: &str, input: &Value) -> String {
        if name == "bash" {
            return input["command"].as_str().unwrap_or("").to_string();
        }
        let Some(p) = input["path"].as_str() else { return String::new() };
        let full = self.resolve(p);
        full.strip_prefix(&self.root).map(|r| r.display().to_string()).unwrap_or_else(|_| p.to_string())
    }

    /// Short label for the UI, like `Edit(src/main.rs)`.
    pub fn label(&self, name: &str, input: &Value) -> String {
        let subject = self.subject(name, input);
        let arg = shorten(subject.lines().next().unwrap_or(""), 80);
        let mut chars = name.chars();
        let head: String = chars.next().map(|c| c.to_ascii_uppercase()).into_iter().chain(chars).collect();
        format!("{head}({arg})")
    }

    /// The change an edit or write would make, without making it. Skipped for
    /// files over 1 MB, which edit and write still handle.
    pub fn preview(&self, name: &str, input: &Value) -> Option<Diff> {
        const MAX_PREVIEW: u64 = 1024 * 1024;
        let path = self.resolve(input["path"].as_str()?);
        if fs::metadata(&path).is_ok_and(|m| m.len() > MAX_PREVIEW) {
            return None;
        }
        match name {
            "write" => {
                let old = fs::read_to_string(&path).unwrap_or_default();
                Some(Diff { line: 1, old, new: input["content"].as_str()?.to_string() })
            }
            "edit" => {
                let old = input["old_string"].as_str()?;
                let text = fs::read_to_string(&path).ok()?;
                let at = text.find(old)?;
                let line = text[..at].bytes().filter(|&b| b == b'\n').count() + 1;
                Some(Diff { line, old: old.to_string(), new: input["new_string"].as_str()?.to_string() })
            }
            _ => None,
        }
    }

    pub fn run(&mut self, name: &str, input: &Value, cancel: &AtomicBool) -> Output {
        match name {
            "read" => self.read(input),
            "write" => self.write(input),
            "edit" => self.edit(input),
            "bash" => self.bash(input, cancel),
            _ => Err(format!("unknown tool: {name}")),
        }
    }

    fn resolve(&self, p: &str) -> PathBuf {
        let p = Path::new(p);
        if p.is_absolute() { p.to_path_buf() } else { self.root.join(p) }
    }

    fn path(&self, input: &Value) -> Result<PathBuf, String> {
        input["path"].as_str().map(|p| self.resolve(p)).ok_or_else(|| "missing path".into())
    }

    fn check_fresh(&self, path: &Path) -> Result<(), String> {
        if !path.exists() {
            return Ok(());
        }
        match self.seen.get(path) {
            None => Err(format!("{} has not been read yet. Read it first.", path.display())),
            Some(t) if *t != mtime(path) => {
                Err(format!("{} changed since it was last read. Read it again.", path.display()))
            }
            Some(_) => Ok(()),
        }
    }

    fn read(&mut self, input: &Value) -> Output {
        let path = self.path(input)?;
        if path.is_dir() {
            return Err(format!("{} is a directory. Use bash ls.", path.display()));
        }
        let offset = input["offset"].as_u64().unwrap_or(1).max(1) as usize;
        let limit = input["limit"].as_u64().map_or(MAX_LINES, |n| n as usize).min(MAX_LINES);
        let file = fs::File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut r = BufReader::new(file);

        if r.fill_buf().map_err(|e| e.to_string())?.contains(&0) {
            return Err(format!("{} looks like a binary file", path.display()));
        }

        let mut out = String::new();
        let mut buf = Vec::new();
        let (mut n, mut shown, mut more) = (0, 0, false);
        loop {
            buf.clear();
            if r.read_until(b'\n', &mut buf).map_err(|e| e.to_string())? == 0 {
                break;
            }
            n += 1;
            if n < offset {
                continue;
            }
            if shown == limit || out.len() >= MAX_BYTES {
                more = true;
                break;
            }
            let line = String::from_utf8_lossy(&buf);
            let line = line.trim_end_matches(['\n', '\r']);
            let _ = writeln!(out, "{n:>6}\t{}", shorten(line, MAX_LINE_CHARS));
            shown += 1;
        }
        self.seen.insert(path.clone(), mtime(&path));

        if n == 0 {
            return Ok("(empty file)".into());
        }
        if shown == 0 {
            return Err(format!("offset {offset} is past the end of the file ({n} lines)"));
        }
        if more {
            let _ = write!(out, "\n(file continues after line {}; use offset to read more)", offset + shown - 1);
        }
        Ok(out)
    }

    fn write(&mut self, input: &Value) -> Output {
        let path = self.path(input)?;
        let content = input["content"].as_str().ok_or("missing content")?;
        self.check_fresh(&path)?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        fs::write(&path, content).map_err(|e| format!("{}: {e}", path.display()))?;
        self.seen.insert(path.clone(), mtime(&path));
        Ok(format!("Wrote {} lines to {}", content.lines().count(), path.display()))
    }

    fn edit(&mut self, input: &Value) -> Output {
        let path = self.path(input)?;
        let old = input["old_string"].as_str().ok_or("missing old_string")?;
        let new = input["new_string"].as_str().ok_or("missing new_string")?;
        let all = input["replace_all"].as_bool().unwrap_or(false);
        if !path.exists() {
            return Err(format!("{} does not exist. Use write to create it.", path.display()));
        }
        if old.is_empty() {
            return Err("old_string is empty".into());
        }
        if old == new {
            return Err("old_string and new_string are the same".into());
        }
        self.check_fresh(&path)?;
        let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let count = text.matches(old).count();
        if count == 0 {
            return Err("old_string not found in the file. Check whitespace and indentation.".into());
        }
        if count > 1 && !all {
            return Err(format!(
                "old_string matches {count} places. Add surrounding lines to make it unique, or set replace_all."
            ));
        }
        let text = if all { text.replace(old, new) } else { text.replacen(old, new, 1) };
        fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        self.seen.insert(path.clone(), mtime(&path));
        let s = if count == 1 { "" } else { "s" };
        Ok(format!("Edited {} ({count} replacement{s})", path.display()))
    }

    fn bash(&mut self, input: &Value, cancel: &AtomicBool) -> Output {
        let cmd = input["command"].as_str().ok_or("missing command")?;
        let timeout = Duration::from_secs(input["timeout"].as_u64().unwrap_or(120).clamp(1, 600));
        let cwd_file = std::env::temp_dir().join(format!("claude-jr-{}.cwd", std::process::id()));
        let script = format!("exec 2>&1\n{{\n{cmd}\n}}\n__rc=$?\npwd > '{}'\nexit $__rc", cwd_file.display());

        let mut child = Command::new("bash")
            .arg("-c")
            .arg(&script)
            .current_dir(&self.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .map_err(|e| format!("could not start bash: {e}"))?;

        // Read output on a side thread, keeping only the head and tail.
        let out = Arc::new(Mutex::new(Capped::new(MAX_BYTES)));
        let (done_tx, done_rx) = mpsc::channel();
        let mut stdout = child.stdout.take().unwrap();
        let sink = out.clone();
        thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n @ 1..) = stdout.read(&mut buf) {
                sink.lock().unwrap().push(&buf[..n]);
            }
            let _ = done_tx.send(());
        });

        let start = Instant::now();
        let mut stopped = None;
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break Some(s),
                Ok(None) => {}
                Err(e) => return Err(e.to_string()),
            }
            if cancel.load(Ordering::Relaxed) {
                stopped = Some("Cancelled by the user".to_string());
            } else if start.elapsed() > timeout {
                stopped = Some(format!("Timed out after {}s", timeout.as_secs()));
            }
            if stopped.is_some() {
                let _ = Command::new("kill").args(["-KILL", "--", &format!("-{}", child.id())]).status();
                let _ = child.wait();
                break None;
            }
            let poll = if start.elapsed() < Duration::from_secs(1) { 10 } else { 100 };
            thread::sleep(Duration::from_millis(poll));
        };

        // A background process can keep the pipe open; don't wait on it forever.
        let _ = done_rx.recv_timeout(Duration::from_millis(300));
        let text = out.lock().unwrap().text();

        if let Ok(dir) = fs::read_to_string(&cwd_file) {
            let dir = PathBuf::from(dir.trim_end());
            if dir.is_dir() {
                self.cwd = dir;
            }
            let _ = fs::remove_file(&cwd_file);
        }

        match (status, stopped) {
            (_, Some(why)) => Err(join(&text, &why)),
            (Some(s), _) if s.success() => Ok(if text.is_empty() { "(no output)".into() } else { text }),
            (Some(s), _) => Err(join(&text, &format!("Exit code {}", s.code().unwrap_or(-1)))),
            (None, None) => unreachable!(),
        }
    }
}

fn join(text: &str, note: &str) -> String {
    if text.is_empty() { note.to_string() } else { format!("{}\n{note}", text.trim_end()) }
}

fn mtime(p: &Path) -> Option<SystemTime> {
    fs::metadata(p).and_then(|m| m.modified()).ok()
}

fn shorten(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

/// Keeps the first and last `cap / 2` bytes of a stream and counts the rest.
struct Capped {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    half: usize,
    total: usize,
}

impl Capped {
    fn new(cap: usize) -> Self {
        Capped { head: Vec::new(), tail: VecDeque::new(), half: cap / 2, total: 0 }
    }

    fn push(&mut self, mut b: &[u8]) {
        self.total += b.len();
        if self.head.len() < self.half {
            let n = (self.half - self.head.len()).min(b.len());
            self.head.extend_from_slice(&b[..n]);
            b = &b[n..];
        }
        if b.len() >= self.half {
            self.tail.clear();
            self.tail.extend(&b[b.len() - self.half..]);
        } else {
            let overflow = (self.tail.len() + b.len()).saturating_sub(self.half);
            self.tail.drain(..overflow);
            self.tail.extend(b);
        }
    }

    fn text(&mut self) -> String {
        let mut s = String::from_utf8_lossy(&self.head).into_owned();
        let omitted = self.total - self.head.len() - self.tail.len();
        if omitted > 0 {
            let _ = write!(s, "\n\n... [{omitted} bytes omitted] ...\n\n");
        }
        s.push_str(&String::from_utf8_lossy(self.tail.make_contiguous()));
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cjr-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn edit_rules() {
        let dir = tmp("edit");
        let mut t = Tools::new(dir.clone());
        let no = AtomicBool::new(false);
        fs::write(dir.join("a.txt"), "one\ntwo\ntwo\n").unwrap();

        let edit =
            |old: &str, all: bool| json!({"path": "a.txt", "old_string": old, "new_string": "X", "replace_all": all});
        assert!(t.run("edit", &edit("one", false), &no).unwrap_err().contains("not been read"));
        assert!(t.run("read", &json!({"path": "a.txt"}), &no).unwrap().contains("     1\tone"));
        assert!(t.run("edit", &edit("two", false), &no).unwrap_err().contains("matches 2"));
        assert!(t.run("edit", &edit("nope", false), &no).unwrap_err().contains("not found"));
        t.run("edit", &edit("two", true), &no).unwrap();
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "one\nX\nX\n");

        // Changed on disk after the read: refuse.
        thread::sleep(Duration::from_millis(20));
        fs::write(dir.join("a.txt"), "changed\n").unwrap();
        assert!(t.run("edit", &edit("changed", false), &no).unwrap_err().contains("changed since"));
    }

    #[test]
    fn bash_keeps_cwd_and_reports_exit_code() {
        let dir = tmp("bash");
        fs::create_dir_all(dir.join("sub")).unwrap();
        let mut t = Tools::new(dir.clone());
        let no = AtomicBool::new(false);
        t.run("bash", &json!({"command": "cd sub"}), &no).unwrap();
        assert!(t.run("bash", &json!({"command": "pwd"}), &no).unwrap().ends_with("/sub\n"));
        let err = t.run("bash", &json!({"command": "echo oops >&2; exit 3"}), &no).unwrap_err();
        assert_eq!(err, "oops\nExit code 3");
    }

    #[test]
    fn bash_timeout_kills() {
        let mut t = Tools::new(tmp("timeout"));
        let start = Instant::now();
        let err = t.run("bash", &json!({"command": "sleep 30", "timeout": 1}), &AtomicBool::new(false)).unwrap_err();
        assert!(err.contains("Timed out"));
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn capped_keeps_head_and_tail() {
        let mut c = Capped::new(10);
        for chunk in [&b"abc"[..], b"defgh", b"ijklmn", b"opqrst"] {
            c.push(chunk);
        }
        assert_eq!(c.text(), "abcde\n\n... [10 bytes omitted] ...\n\npqrst");
    }
}
