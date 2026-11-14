// The run log: what an unattended run did, one JSON object per line, written
// as it happens. It is what a person or another program reads to follow a run
// or to decide afterwards whether to trust its result: each tool call with
// its input, what allowed it to run (or that it was refused) and how it went.
//
//   {"t":0,"type":"start","prompt":"...","model":"...","cwd":"..."}
//   {"t":812,"type":"text","text":"I'll look at the tests first."}
//   {"t":840,"type":"step","tool":"bash","input":{"command":"cargo test"},
//    "allowed_by":"allow rule","ok":true,"result":"...","ms":2210}
//   {"t":3050,"type":"usage","cost_usd":0.0123,"input_tokens":..,"output_tokens":..}
//   {"t":9000,"type":"result","ok":true,"result":"...","files":[...],"cost_usd":..}
//
// `t` is milliseconds since the run started. A refused call has
// "allowed_by":null and the reason as its result.

use crate::agent::Event;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::io::Write;
use std::time::Instant;

/// Longer strings in a tool's input (a whole file being written) are cut.
const MAX_STRING: usize = 2000;

pub struct Log {
    sinks: Vec<Box<dyn Write + Send>>,
    start: Instant,
    /// The model's text since the last line was written.
    text: String,
    /// Files the write and edit tools changed.
    edited: BTreeSet<String>,
}

impl Log {
    pub fn new(sinks: Vec<Box<dyn Write + Send>>) -> Log {
        Log { sinks, start: Instant::now(), text: String::new(), edited: BTreeSet::new() }
    }

    fn line(&mut self, kind: &str, mut fields: Value) {
        if self.sinks.is_empty() {
            return;
        }
        let mut line = json!({ "t": self.start.elapsed().as_millis() as u64, "type": kind });
        if let (Some(line), Some(fields)) = (line.as_object_mut(), fields.as_object_mut()) {
            line.append(fields);
        }
        let text = format!("{line}\n");
        for sink in &mut self.sinks {
            // One write per line, flushed, so a reader following the file
            // never sees half of one.
            let _ = sink.write_all(text.as_bytes()).and_then(|()| sink.flush());
        }
    }

    pub fn started(&mut self, prompt: &str, model: &str) {
        let cwd = std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default();
        self.line("start", json!({ "prompt": prompt, "model": model, "cwd": cwd }));
    }

    /// Records what an event says, if it belongs in the log.
    pub fn see(&mut self, event: &Event) {
        match event {
            Event::Text(t) => self.text.push_str(t),
            Event::Step(step) => {
                self.flush_text();
                if step.ok
                    && matches!(step.tool.as_str(), "write" | "edit")
                    && let Some(path) = step.input["path"].as_str()
                {
                    self.edited.insert(path.to_string());
                }
                let mut fields = json!({
                    "tool": step.tool,
                    "input": brief(&step.input),
                    "allowed_by": step.allowed_by,
                    "ok": step.ok,
                    "result": step.result,
                    "ms": step.ms,
                });
                if let Some(agent) = &step.agent {
                    fields["agent"] = json!(agent);
                }
                self.line("step", fields);
            }
            Event::SubStart { task, .. } => {
                self.flush_text();
                self.line("agent", json!({ "task": task }));
            }
            Event::Notice(msg) => self.line("notice", json!({ "text": msg })),
            Event::Usage { usage, cost, main } => self.line(
                "usage",
                json!({
                    "cost_usd": cost,
                    "input_tokens": usage.input_tokens,
                    "output_tokens": usage.output_tokens,
                    "cache_read_tokens": usage.cache_read_input_tokens,
                    "main": main,
                }),
            ),
            Event::Done | Event::Cancelled | Event::Error(_) => self.flush_text(),
            _ => {}
        }
    }

    fn flush_text(&mut self) {
        let text = std::mem::take(&mut self.text);
        if !text.trim().is_empty() {
            self.line("text", json!({ "text": text.trim() }));
        }
    }

    /// The files the run changed, as they stand now.
    pub fn files(&self) -> Value {
        json!(changed_files(&self.edited))
    }

    /// The last line: the run's outcome.
    pub fn finish(&mut self, fields: &Value) {
        self.flush_text();
        self.line("result", fields.clone());
    }
}

/// `value` with long strings cut short.
fn brief(value: &Value) -> Value {
    match value {
        Value::String(s) if s.len() > MAX_STRING => {
            let mut end = MAX_STRING;
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            Value::String(format!("{}… (+{} bytes)", &s[..end], s.len() - end))
        }
        Value::Array(a) => Value::Array(a.iter().map(brief).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), brief(v))).collect()),
        other => other.clone(),
    }
}

/// What changed in the working directory: from git where there is a
/// repository (so changes made by commands count too), otherwise the files
/// the edit tools touched.
fn changed_files(edited: &BTreeSet<String>) -> Vec<Value> {
    let git = std::process::Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=all"])
        .stderr(std::process::Stdio::null())
        .output();
    match git {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| Some(json!({ "path": l.get(3..)?.trim_matches('"'), "status": status(l.get(..2)?) })))
            .collect(),
        _ => edited.iter().map(|path| json!({ "path": path, "status": "edited" })).collect(),
    }
}

/// git's two status letters as a word.
fn status(code: &str) -> &'static str {
    match code {
        "??" => "new",
        c if c.contains('D') => "deleted",
        c if c.contains('R') => "renamed",
        c if c.contains('A') => "new",
        _ => "changed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Step;
    use std::sync::{Arc, Mutex};

    /// A sink the test can read back.
    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn lines(buffer: &Buffer) -> Vec<Value> {
        String::from_utf8(buffer.0.lock().unwrap().clone())
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn step(tool: &str, input: Value, allowed_by: Option<&'static str>, ok: bool) -> Event {
        Event::Step(Step { tool: tool.into(), input, allowed_by, ok, result: "done".into(), ms: 7, agent: None })
    }

    #[test]
    fn a_run_is_logged_line_by_line() {
        let buffer = Buffer::default();
        let mut log = Log::new(vec![Box::new(buffer.clone())]);
        log.started("fix it", "claude-opus-5-5");
        log.see(&Event::Text("I'll look ".into()));
        log.see(&Event::Text("at the tests.\n".into()));
        log.see(&step("bash", json!({ "command": "cargo test" }), Some("allow rule"), true));
        log.see(&step("write", json!({ "path": "a.txt", "content": "x".repeat(5000) }), None, false));
        log.see(&Event::Text("All done.".into()));
        log.see(&Event::Done);
        assert!(log.files().is_array());
        log.finish(&json!({ "ok": true }));

        let lines = lines(&buffer);
        let kinds: Vec<&str> = lines.iter().map(|l| l["type"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["start", "text", "step", "step", "text", "result"]);
        assert_eq!(lines[0]["prompt"], "fix it");
        assert_eq!(lines[1]["text"], "I'll look at the tests.");
        assert_eq!(lines[2]["allowed_by"], "allow rule");
        assert_eq!(lines[2]["input"]["command"], "cargo test");
        // Refused: nothing allowed it. And a whole file isn't copied into the log.
        assert!(lines[3]["allowed_by"].is_null() && lines[3]["ok"] == false);
        let content = lines[3]["input"]["content"].as_str().unwrap();
        assert!(content.len() < 2100 && content.ends_with("(+3000 bytes)"), "{}", content.len());
        assert_eq!(lines[5]["ok"], true);
        assert!(lines.iter().all(|l| l["t"].is_u64()));
    }

    #[test]
    fn a_subagents_steps_carry_its_task() {
        let buffer = Buffer::default();
        let mut log = Log::new(vec![Box::new(buffer.clone())]);
        log.see(&Event::SubStart { id: 1, task: "scan the tests".into() });
        log.see(&Event::Step(Step {
            tool: "read".into(),
            input: json!({ "path": "a.rs" }),
            allowed_by: Some("needs no approval"),
            ok: true,
            result: "Read 3 lines".into(),
            ms: 1,
            agent: Some("scan the tests".into()),
        }));
        let lines = lines(&buffer);
        assert_eq!(lines[0]["type"], "agent");
        assert_eq!(lines[1]["agent"], "scan the tests");
    }

    #[test]
    fn long_strings_are_cut_on_a_character_and_statuses_are_words() {
        let cut = brief(&json!({ "a": ["é".repeat(1500)], "n": 5 }));
        assert!(cut["a"][0].as_str().unwrap().contains("… (+"));
        assert_eq!(cut["n"], 5);
        assert_eq!(
            [status("??"), status(" M"), status("D "), status("R "), status("A ")],
            ["new", "changed", "deleted", "renamed", "new"]
        );
    }
}
