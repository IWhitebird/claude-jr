// Conversations stored as JSONL: a header line with the system prompt, then one
// message per line, exactly as sent to the API. The history is append-only,
// so the file is too.

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::{Value, json};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// One message, kept as the JSON text that goes over the wire.
pub struct Msg {
    pub user: bool,
    pub json: Box<RawValue>,
}

impl Msg {
    pub fn new(role: &str, content: Value) -> Msg {
        let json =
            serde_json::value::to_raw_value(&json!({ "role": role, "content": content })).expect("message serializes");
        Msg { user: role == "user", json }
    }

    fn parse(line: &str) -> Option<Msg> {
        #[derive(Deserialize)]
        struct Role<'a> {
            role: &'a str,
        }
        let json = RawValue::from_string(line.to_string()).ok()?;
        let user = serde_json::from_str::<Role>(json.get()).ok()?.role == "user";
        Some(Msg { user, json })
    }

    /// The text the user typed, if this message started a turn (not tool
    /// results, reminders or shell output).
    pub fn prompt(&self) -> Option<String> {
        if !self.user {
            return None;
        }
        let content = self.content();
        if content.iter().any(|b| b["type"] == "tool_result") {
            return None;
        }
        content.iter().filter_map(|b| b["text"].as_str()).find(|t| !t.starts_with('<')).map(String::from)
    }

    pub fn content(&self) -> Vec<Value> {
        let v: Value = serde_json::from_str(self.json.get()).unwrap_or_default();
        match v.get("content") {
            Some(Value::Array(a)) => a.clone(),
            Some(Value::String(s)) => vec![json!({ "type": "text", "text": s })],
            _ => Vec::new(),
        }
    }
}

impl AsRef<str> for Msg {
    fn as_ref(&self) -> &str {
        self.json.get()
    }
}

#[derive(Serialize, Deserialize)]
struct Header {
    system: String,
}

/// Which saved session to open.
pub enum Pick {
    Latest,
    /// By name (set with /rename) or by id (the file name without .jsonl).
    Named(String),
}

/// A loaded session: where to keep saving, plus what it held.
pub struct Loaded {
    pub session: Session,
    pub system: String,
    pub messages: Vec<Msg>,
}

/// One line of `--sessions`.
pub struct Summary {
    pub id: String,
    pub name: Option<String>,
    pub messages: usize,
    pub first_prompt: String,
    pub modified: SystemTime,
}

pub struct Session {
    path: PathBuf,
    file: Option<File>,
    /// How many messages are already in the file.
    saved: usize,
    pub name: Option<String>,
}

/// Lines starting with this hold session metadata, not messages.
const META: &str = "{\"meta\":";

/// A metadata line: `{"meta":{"name":"..."}}` or `{"meta":{"truncate":12}}`.
/// They're replayed in order when a session is loaded.
#[derive(Serialize, Deserialize)]
struct Meta {
    meta: MetaFields,
}

#[derive(Serialize, Deserialize, Default)]
struct MetaFields {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    /// Keep only the first this-many messages (/rewind, /compact).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    truncate: Option<usize>,
}

impl Session {
    pub fn new() -> Session {
        let ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        Session { path: dir().join(format!("{ms}.jsonl")), file: None, saved: 0, name: None }
    }

    pub fn load(pick: &Pick) -> Option<Loaded> {
        let mut files = files();
        let path = match pick {
            Pick::Latest => files.pop()?,
            Pick::Named(want) => files
                .into_iter()
                .rev()
                .find(|p| stem(p) == *want || read(p).is_some_and(|r| r.name.as_deref() == Some(want)))?,
        };
        let Read { system, name, messages } = read(&path)?;
        let saved = messages.len();
        Some(Loaded { session: Session { path, file: None, saved, name }, system, messages })
    }

    /// Saved sessions for this directory, oldest first.
    pub fn list() -> Vec<Summary> {
        files()
            .iter()
            .filter_map(|p| {
                let Read { name, messages, .. } = read(p)?;
                let first_prompt = messages.iter().find_map(Msg::prompt).unwrap_or_default();
                let modified = fs::metadata(p).and_then(|m| m.modified()).unwrap_or(UNIX_EPOCH);
                Some(Summary { id: stem(p), name, messages: messages.len(), first_prompt, modified })
            })
            .collect()
    }

    /// Appends finished messages. A trailing user message is held back because
    /// the next turn may still add to it.
    pub fn save(&mut self, system: &str, messages: &[Msg]) {
        let end = match messages.last() {
            Some(m) if m.user => messages.len() - 1,
            _ => messages.len(),
        };
        if end <= self.saved {
            return;
        }
        let mut buf = Vec::new();
        for m in &messages[self.saved..end] {
            buf.extend_from_slice(m.json.get().as_bytes());
            buf.push(b'\n');
        }
        if self.append(system, &buf) {
            self.saved = end;
        }
    }

    /// Names the session; the newest name wins when it's loaded again.
    pub fn rename(&mut self, system: &str, name: &str) {
        if self.meta(system, MetaFields { name: Some(name.to_string()), ..Default::default() }) {
            self.name = Some(name.to_string());
        }
    }

    /// Records that the conversation was cut back to its first `len` messages.
    pub fn truncate(&mut self, system: &str, len: usize) {
        if len < self.saved && self.meta(system, MetaFields { truncate: Some(len), ..Default::default() }) {
            self.saved = len;
        }
    }

    /// The folder for this session's /rewind copies.
    pub fn checkpoint_dir(&self) -> PathBuf {
        let parent = self.path.parent().map(Path::to_path_buf).unwrap_or_default();
        parent.join("checkpoints").join(stem(&self.path))
    }

    fn meta(&mut self, system: &str, fields: MetaFields) -> bool {
        let Ok(mut line) = serde_json::to_vec(&Meta { meta: fields }) else { return false };
        line.push(b'\n');
        self.append(system, &line)
    }

    fn append(&mut self, system: &str, bytes: &[u8]) -> bool {
        if self.file.is_none() {
            self.file = self.open(system);
        }
        self.file.as_mut().is_some_and(|f| f.write_all(bytes).is_ok())
    }

    fn open(&self, system: &str) -> Option<File> {
        let fresh = !self.path.exists();
        fs::create_dir_all(self.path.parent()?).ok()?;
        let mut f = OpenOptions::new().create(true).append(true).open(&self.path).ok()?;
        if fresh {
            let mut line = serde_json::to_vec(&Header { system: system.to_string() }).ok()?;
            line.push(b'\n');
            f.write_all(&line).ok()?;
        }
        Some(f)
    }
}

/// Session files for this directory, oldest first (names are millisecond timestamps).
fn files() -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir()) else { return Vec::new() };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    files.sort();
    files
}

struct Read {
    system: String,
    name: Option<String>,
    messages: Vec<Msg>,
}

/// Replays a session file: header, then messages and metadata in order.
fn read(path: &Path) -> Option<Read> {
    let mut lines = BufReader::new(File::open(path).ok()?).lines().map_while(Result::ok);
    let header: Header = serde_json::from_str(&lines.next()?).ok()?;
    let (mut name, mut messages) = (None, Vec::new());
    for line in lines {
        if !line.starts_with(META) {
            messages.extend(Msg::parse(&line));
            continue;
        }
        let Ok(Meta { meta }) = serde_json::from_str::<Meta>(&line) else { continue };
        if let Some(n) = meta.truncate {
            messages.truncate(n);
        }
        name = meta.name.or(name);
    }
    Some(Read { system: header.system, name, messages })
}

fn stem(p: &Path) -> String {
    p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Sessions are grouped by working directory.
fn dir() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_default();
    let name: String = cwd.to_string_lossy().chars().map(|c| if c.is_alphanumeric() { c } else { '-' }).collect();
    crate::config::data_dir().join("sessions").join(name)
}
