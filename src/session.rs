// Conversations stored as JSONL: a header line with the system prompt, then one
// message per line, exactly as sent to the API. The history is append-only,
// so the file is too.
//
// The file is also where the history lives while the app runs: once a message
// is saved, memory keeps only where its line is, and requests read it from
// there. A long session then takes no more memory than a short one.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::borrow::Cow;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, Write};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// One message: the JSON text that goes over the wire, or where that text is
/// in the session file.
pub struct Msg {
    pub user: bool,
    body: Body,
}

enum Body {
    Memory(String),
    Saved { file: Arc<File>, at: u64, len: usize },
}

impl Msg {
    pub fn new(role: &str, content: Value) -> Msg {
        let json = serde_json::to_string(&json!({ "role": role, "content": content })).expect("message serializes");
        Msg { user: role == "user", body: Body::Memory(json) }
    }

    /// A message line of a session file, which starts at byte `at` of `file`.
    fn parse(line: &str, file: &Arc<File>, at: u64) -> Option<Msg> {
        #[derive(Deserialize)]
        struct Role<'a> {
            role: &'a str,
        }
        let user = serde_json::from_str::<Role>(line).ok()?.role == "user";
        let mut msg = Msg { user, body: Body::Memory(String::new()) };
        // A message with stored images is spliced together when it's sent,
        // which needs its text at hand.
        if crate::images::pieces(line).len() > 1 {
            msg.body = Body::Memory(line.to_string());
        } else {
            msg.body = Body::Saved { file: file.clone(), at, len: line.len() };
        }
        Some(msg)
    }

    /// The message's JSON, read back from the session file if that's where it is.
    pub fn json(&self) -> Cow<'_, str> {
        match &self.body {
            Body::Memory(json) => Cow::Borrowed(json),
            Body::Saved { file, at, len } => {
                let mut buf = vec![0; *len];
                let read = file.read_exact_at(&mut buf, *at).ok().and_then(|()| String::from_utf8(buf).ok());
                Cow::Owned(read.unwrap_or_default())
            }
        }
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

    pub fn has_results(&self) -> bool {
        self.content().iter().any(|b| b["type"] == "tool_result")
    }

    pub fn content(&self) -> Vec<Value> {
        let v: Value = serde_json::from_str(&self.json()).unwrap_or_default();
        match v.get("content") {
            Some(Value::Array(a)) => a.clone(),
            Some(Value::String(s)) => vec![json!({ "type": "text", "text": s })],
            _ => Vec::new(),
        }
    }
}

impl crate::api::Message for Msg {
    fn text(&self) -> crate::api::Text<'_> {
        match &self.body {
            Body::Memory(json) => crate::api::Text::Memory(json),
            Body::Saved { file, at, len } => crate::api::Text::Saved { file, at: *at, len: *len },
        }
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
    file: Option<Arc<File>>,
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

    /// Appends finished messages, and from then on they are read from the
    /// file instead of being kept in memory. A trailing user prompt is held
    /// back because the next turn may still add to it. Tool results are
    /// written at once: they are what tells a resumed session the tools ran.
    pub fn save(&mut self, system: &str, messages: &mut [Msg]) {
        let end = match messages.last() {
            Some(m) if m.user && !m.has_results() => messages.len() - 1,
            _ => messages.len(),
        };
        if end <= self.saved {
            return;
        }
        // Written one message at a time: a copy of everything unsaved would
        // double the memory a long turn needs.
        for m in &mut messages[self.saved..end] {
            let Body::Memory(json) = &m.body else {
                self.saved += 1;
                continue;
            };
            if !(self.append(system, json.as_bytes()) && self.append(system, b"\n")) {
                break;
            }
            self.saved += 1;
            // Where the file now ends is where this line ends, whoever else
            // has been appending to it.
            let end = self.file.as_ref().and_then(|f| (&**f).stream_position().ok());
            if let (Some(file), Some(end)) = (&self.file, end)
                && crate::images::pieces(json).len() == 1
            {
                let len = json.len();
                m.body = Body::Saved { file: file.clone(), at: end - 1 - len as u64, len };
            }
        }
    }

    /// Names the session; the newest name wins when it's loaded again.
    pub fn rename(&mut self, system: &str, name: &str) {
        if self.meta(system, MetaFields { name: Some(name.to_string()), ..Default::default() }) {
            self.name = Some(name.to_string());
        }
    }

    /// Whether the first `len` messages are already in the file.
    pub fn is_saved(&self, len: usize) -> bool {
        len <= self.saved
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
        self.file.as_ref().is_some_and(|f| (&**f).write_all(bytes).is_ok())
    }

    fn open(&self, system: &str) -> Option<Arc<File>> {
        let fresh = !self.path.exists();
        fs::create_dir_all(self.path.parent()?).ok()?;
        let mut f = OpenOptions::new().create(true).read(true).append(true).open(&self.path).ok()?;
        if fresh {
            let mut line = serde_json::to_vec(&Header { system: system.to_string() }).ok()?;
            line.push(b'\n');
            f.write_all(&line).ok()?;
        } else if !ends_with_newline(&f) {
            // The last write was cut off (a crash, a full disk). What comes
            // next must start its own line, or it would be lost with it.
            f.write_all(b"\n").ok()?;
        }
        Some(Arc::new(f))
    }
}

fn ends_with_newline(f: &File) -> bool {
    let Ok(len) = f.metadata().map(|m| m.len()) else { return true };
    let mut last = [0];
    len == 0 || (f.read_exact_at(&mut last, len - 1).is_ok() && last[0] == b'\n')
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

/// Replays a session file: header, then messages and metadata in order. The
/// messages stay in the file; only where each one is gets remembered.
fn read(path: &Path) -> Option<Read> {
    let file = Arc::new(File::open(path).ok()?);
    let mut lines = BufReader::new(&*file);
    let mut line = String::new();
    let mut at = lines.read_line(&mut line).ok()? as u64;
    let header: Header = serde_json::from_str(&line).ok()?;
    let (mut name, mut messages) = (None, Vec::new());
    loop {
        line.clear();
        let start = at;
        match lines.read_line(&mut line) {
            Ok(n) if n > 0 => at += n as u64,
            _ => break,
        }
        let line = line.trim_end_matches('\n');
        if !line.starts_with(META) {
            messages.extend(Msg::parse(line, &file, start));
            continue;
        }
        let Ok(Meta { meta }) = serde_json::from_str::<Meta>(line) else { continue };
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_rename_truncate_and_load() {
        crate::tools::test_home();
        let msg = |role: &str, text: &str| Msg::new(role, json!([{ "type": "text", "text": text }]));
        let mut s = Session::new();
        // Agent tests start sessions too; the same millisecond must not mean the same file.
        s.path.set_file_name(format!("{}-main.jsonl", stem(&s.path)));
        let id = stem(&s.path);
        let mut messages = vec![msg("user", "first"), msg("assistant", "one"), msg("user", "second")];

        // A trailing user message is held back until its turn finishes.
        s.save("SYSTEM", &mut messages);
        assert_eq!(s.saved, 2);
        messages.push(msg("assistant", "two"));
        s.save("SYSTEM", &mut messages);
        s.rename("SYSTEM", "my work");
        s.truncate("SYSTEM", 2); // rewind to before "second"
        s.save(
            "SYSTEM",
            &mut [msg("user", "first"), msg("assistant", "one"), msg("user", "again"), msg("assistant", "ok")],
        );

        let loaded = Session::load(&Pick::Named("my work".into())).expect("found by name");
        assert_eq!(loaded.system, "SYSTEM");
        assert_eq!(loaded.session.name.as_deref(), Some("my work"));
        let prompts: Vec<String> = loaded.messages.iter().filter_map(Msg::prompt).collect();
        assert_eq!(prompts, ["first", "again"]);
        assert!(Session::load(&Pick::Named(id)).is_some(), "found by id too");
        assert!(Session::list().iter().any(|s| s.name.as_deref() == Some("my work") && s.messages == 4));
    }

    #[test]
    fn tool_results_are_saved_even_when_they_end_the_conversation() {
        crate::tools::test_home();
        let mut s = Session::new();
        // Another test may have taken the same millisecond-stamped name.
        s.path.set_file_name(format!("{}-results.jsonl", stem(&s.path)));
        let id = stem(&s.path);
        let mut messages = vec![
            Msg::new("user", json!([{ "type": "text", "text": "run it" }])),
            Msg::new("assistant", json!([{ "type": "tool_use", "id": "t1", "name": "bash", "input": {} }])),
            Msg::new("user", json!([{ "type": "tool_result", "tool_use_id": "t1", "content": "hi" }])),
        ];
        // The turn stopped (error, cancel) right after the tools ran.
        s.save("SYSTEM", &mut messages);
        let loaded = Session::load(&Pick::Named(id)).expect("found by id");
        assert_eq!(loaded.messages.len(), 3, "the real result was saved");
        assert!(loaded.messages[2].user && loaded.messages[2].json().contains("\"hi\""));
        assert!(s.is_saved(3));
    }

    #[test]
    fn saved_messages_live_in_the_file_and_read_back_the_same() {
        crate::tools::test_home();
        let mut s = Session::new();
        s.path.set_file_name(format!("{}-disk.jsonl", stem(&s.path)));
        let id = stem(&s.path);
        let text = |n: usize| json!([{ "type": "text", "text": format!("message {n} \u{1F600} \"quoted\"\nline") }]);
        let mut messages = vec![Msg::new("user", text(0)), Msg::new("assistant", text(1))];
        let before: Vec<String> = messages.iter().map(|m| m.json().into_owned()).collect();
        s.save("SYSTEM", &mut messages);
        assert!(messages.iter().all(|m| matches!(m.body, Body::Saved { .. })), "nothing is kept in memory");
        // Something else appending to the file (a second window on the same
        // session) must not shift where later messages are found.
        fs::OpenOptions::new().append(true).open(&s.path).unwrap().write_all(b"{\"meta\":{}}\n").unwrap();
        s.rename("SYSTEM", "disk");
        messages.push(Msg::new("user", text(2)));
        messages.push(Msg::new("assistant", text(3)));
        s.save("SYSTEM", &mut messages);
        let after: Vec<String> = messages.iter().map(|m| m.json().into_owned()).collect();
        assert_eq!(after[..2], before[..]);
        assert_eq!(messages[3].content()[0]["text"], text(3)[0]["text"]);

        let loaded = Session::load(&Pick::Named(id)).unwrap();
        assert!(loaded.messages.iter().all(|m| matches!(m.body, Body::Saved { .. })));
        let reloaded: Vec<String> = loaded.messages.iter().map(|m| m.json().into_owned()).collect();
        assert_eq!(reloaded, after);
        assert_eq!(loaded.messages.iter().map(|m| m.user).collect::<Vec<_>>(), [true, false, true, false]);
    }

    #[test]
    fn a_message_with_a_stored_image_stays_in_memory() {
        crate::tools::test_home();
        let mut s = Session::new();
        s.path.set_file_name(format!("{}-image.jsonl", stem(&s.path)));
        let id = stem(&s.path);
        let png = crate::tools::test_dir("session-image").join("x.png");
        fs::write(&png, b"\x89PNG\r\n\x1a\nnot much of a picture").unwrap();
        let image = crate::images::block(&png).unwrap();
        let mut messages =
            vec![Msg::new("user", json!([image])), Msg::new("assistant", json!([{ "type": "text", "text": "ok" }]))];
        assert!(crate::images::pieces(&messages[0].json()).len() > 1, "the test's marker is a real one");
        s.save("SYSTEM", &mut messages);
        assert!(matches!(messages[0].body, Body::Memory(_)), "its image is spliced in when it's sent");
        assert!(matches!(messages[1].body, Body::Saved { .. }));
        let loaded = Session::load(&Pick::Named(id)).unwrap();
        assert!(matches!(loaded.messages[0].body, Body::Memory(_)));
        assert_eq!(loaded.messages[0].json(), messages[0].json());
    }

    /// A session file with `body` after the header, and its id.
    fn file_with(tag: &str, body: &str) -> (PathBuf, String) {
        crate::tools::test_home();
        let mut s = Session::new();
        s.path.set_file_name(format!("{}-{tag}.jsonl", stem(&s.path)));
        fs::create_dir_all(s.path.parent().unwrap()).unwrap();
        fs::write(&s.path, format!("{{\"system\":\"SYSTEM\"}}\n{body}")).unwrap();
        (s.path.clone(), stem(&s.path))
    }

    const USER: &str = r#"{"role":"user","content":[{"type":"text","text":"hello"}]}"#;
    const REPLY: &str = r#"{"role":"assistant","content":[{"type":"text","text":"hi"}]}"#;

    #[test]
    fn a_damaged_file_loads_what_is_whole() {
        // Cut off mid-line (a crash while writing), junk lines, blank lines, a
        // truncate past the end: none of it may lose the lines that are whole.
        let body = format!(
            "{USER}\n\nnot json at all\n{{\"meta\":{{\"truncate\":99}}}}\n{{\"meta\":broken\n{REPLY}\n{{\"role\":\"us"
        );
        let (_, id) = file_with("damaged", &body);
        let loaded = Session::load(&Pick::Named(id)).expect("it still loads");
        let texts: Vec<String> = loaded.messages.iter().map(|m| m.json().into_owned()).collect();
        assert_eq!(texts, [USER, REPLY]);
    }

    #[test]
    fn files_with_nothing_usable_do_not_load_or_crash() {
        for (tag, whole) in
            [("empty", ""), ("binary", "\u{0}\u{1}\u{2}\n\u{3}"), ("no-header", "{\"role\":\"user\"}\n")]
        {
            let (path, id) = file_with(tag, "");
            fs::write(&path, whole).unwrap();
            assert!(Session::load(&Pick::Named(id)).is_none(), "{tag}");
        }
        let (_, id) = file_with("header-only", "");
        let loaded = Session::load(&Pick::Named(id)).expect("a session with no messages yet");
        assert!(loaded.messages.is_empty());
        let _ = Session::list();
    }

    #[test]
    fn saving_after_a_cut_off_line_starts_a_new_line() {
        let (path, id) = file_with("cut", &format!("{USER}\n{REPLY}\n{{\"role\":\"assis"));
        let Loaded { mut session, mut messages, .. } = Session::load(&Pick::Named(id.clone())).unwrap();
        assert_eq!(messages.len(), 2);
        messages.push(Msg::new("user", json!([{ "type": "text", "text": "more" }])));
        messages.push(Msg::new("assistant", json!([{ "type": "text", "text": "done" }])));
        session.save("SYSTEM", &mut messages);
        assert_eq!(messages[3].content()[0]["text"], "done", "read back from where it was written");

        let again = Session::load(&Pick::Named(id)).unwrap();
        let prompts: Vec<String> = again.messages.iter().filter_map(Msg::prompt).collect();
        assert_eq!(prompts, ["hello", "more"], "{}", fs::read_to_string(&path).unwrap());
        assert_eq!(again.messages.len(), 4);
    }

    #[test]
    fn a_message_whose_file_is_gone_reads_as_empty_not_a_crash() {
        let (path, id) = file_with("shrunk", &format!("{USER}\n{REPLY}\n"));
        let loaded = Session::load(&Pick::Named(id)).unwrap();
        fs::write(&path, "").unwrap();
        assert_eq!(loaded.messages[1].json(), "");
        assert!(loaded.messages[1].content().is_empty());
        assert_eq!(loaded.messages[0].prompt(), None);
    }

    #[test]
    fn big_and_odd_messages_round_trip_through_the_file() {
        crate::tools::test_home();
        let mut s = Session::new();
        s.path.set_file_name(format!("{}-odd.jsonl", stem(&s.path)));
        let id = stem(&s.path);
        let odd = [
            "x".repeat(3_000_000),
            "line one\nline two\r\n\ttabbed \u{0} nul \u{1b}[31m escape".to_string(),
            "quotes \" and \\ backslashes \\n and {\"meta\": 1}".to_string(),
            "{\"meta\":{\"truncate\":0}}".to_string(),
            "emoji 👨‍👩‍👧‍👦 漢字 \u{202e} \u{feff}".to_string(),
            String::new(),
        ];
        let mut messages: Vec<Msg> = odd
            .iter()
            .enumerate()
            .map(|(i, t)| {
                Msg::new(if i % 2 == 0 { "user" } else { "assistant" }, json!([{ "type": "text", "text": t }]))
            })
            .collect();
        s.save("SYSTEM", &mut messages);
        let loaded = Session::load(&Pick::Named(id)).unwrap();
        assert_eq!(loaded.messages.len(), odd.len());
        for (i, want) in odd.iter().enumerate() {
            assert_eq!(messages[i].content()[0]["text"], want.as_str(), "message {i}, as saved");
            assert_eq!(loaded.messages[i].content()[0]["text"], want.as_str(), "message {i}, as loaded");
        }
    }
}
