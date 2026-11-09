// Conversations stored as JSONL: a header line with the system prompt, then one
// message per line, exactly as sent to the API. The history is append-only,
// so the file is too.

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::{Value, json};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
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

pub struct Session {
    path: PathBuf,
    file: Option<File>,
    /// How many messages are already in the file.
    saved: usize,
}

impl Session {
    pub fn new() -> Session {
        let ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        Session { path: dir().join(format!("{ms}.jsonl")), file: None, saved: 0 }
    }

    /// The newest session for this directory: its system prompt and messages.
    pub fn latest() -> Option<(Session, String, Vec<Msg>)> {
        let path = fs::read_dir(dir())
            .ok()?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
            .max()?;
        let mut lines = BufReader::new(File::open(&path).ok()?).lines().map_while(Result::ok);
        let header: Header = serde_json::from_str(&lines.next()?).ok()?;
        let messages: Vec<Msg> = lines.filter_map(|l| Msg::parse(&l)).collect();
        let saved = messages.len();
        Some((Session { path, file: None, saved }, header.system, messages))
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
        if self.file.is_none() {
            self.file = self.open(system);
        }
        let Some(file) = &mut self.file else { return };
        let mut buf = Vec::new();
        for m in &messages[self.saved..end] {
            buf.extend_from_slice(m.json.get().as_bytes());
            buf.push(b'\n');
        }
        if file.write_all(&buf).is_ok() {
            self.saved = end;
        }
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

/// Sessions are grouped by working directory.
fn dir() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_default();
    let name: String = cwd.to_string_lossy().chars().map(|c| if c.is_alphanumeric() { c } else { '-' }).collect();
    crate::config::data_dir().join("sessions").join(name)
}
