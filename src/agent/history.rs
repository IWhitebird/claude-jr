// Going back and coming back: /rewind cuts the conversation to an earlier
// point, /resume switches to a saved one, and the transcript is what the UI
// shows of a conversation it didn't watch happen.

use super::{Agent, Event, Shown, turn};
use crate::store::session::{Loaded, Msg, Pick, Session};
use crate::tools;
use serde_json::Value;

/// Said when a session being continued is already open in another window.
pub(super) const OPEN_ELSEWHERE: &str =
    "This conversation is open in another claude-jr, so this is a copy of it: what you do here is saved separately.";

/// What to say when a saved conversation's file had lost some of its lines.
pub(super) fn damaged(dropped: usize) -> Option<String> {
    let s = if dropped == 1 { "" } else { "s" };
    let said =
        format!("The saved conversation's file was damaged: {dropped} message{s} that no longer fit were left out.");
    (dropped > 0).then_some(said)
}

impl Agent {
    /// Back to before `turn`: files the agent changed since then are restored
    /// and the conversation is cut there (cutting from the end keeps it valid).
    pub(super) fn rewind(&mut self, turn: usize) {
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

    pub(super) fn resume(&mut self, pick: &Pick) {
        let Some(Loaded { session, system, messages, copy, dropped }) = Session::load(pick) else {
            return (self.core.emit)(Event::Error("couldn't load that conversation".into()));
        };
        self.tools.checkpoints.open(session.checkpoint_dir());
        (self.session, self.system, self.messages, self.told_plan) = (session, system, messages, false);
        self.close_open_calls("Not run: the session ended.");

        (self.core.emit)(Event::Resumed { name: self.session.name.clone(), messages: self.messages.len() });
        (self.core.emit)(Event::Replay(self.transcript()));
        if let Some(notice) = damaged(dropped) {
            (self.core.emit)(Event::Notice(notice));
        }
        if copy {
            (self.core.emit)(Event::Notice(OPEN_ELSEWHERE.into()));
        }
    }

    /// The conversation so far as the UI would have shown it: what the user
    /// said, what the model answered, each tool call and its outcome.
    pub(super) fn transcript(&self) -> Vec<Shown> {
        let mut out = Vec::new();
        // Calls waiting for their result, by id: (tool name, label). A reply
        // can hold several calls and their results come in the next message,
        // so each call is shown when its result is, the two together.
        let mut calls: Vec<(String, String, String)> = Vec::new();
        for m in &self.messages {
            if let Some(prompt) = m.prompt() {
                out.extend(calls.drain(..).map(|(_, _, label)| Shown::Tool(label)));
                out.push(Shown::User(prompt));
                continue;
            }
            for b in m.content() {
                match b["type"].as_str().unwrap_or("") {
                    "text" if !m.user => {
                        if let Some(t) = b["text"].as_str().filter(|t| !t.trim().is_empty()) {
                            out.push(Shown::Text(t.to_string()));
                        }
                    }
                    "tool_use" => {
                        let name = b["name"].as_str().unwrap_or("");
                        let id = b["id"].as_str().unwrap_or("").to_string();
                        calls.push((id, name.to_string(), self.tools.label(name, &b["input"])));
                    }
                    "tool_result" => {
                        let text = match &b["content"] {
                            Value::String(s) => s.clone(),
                            Value::Array(parts) => {
                                parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n")
                            }
                            _ => String::new(),
                        };
                        let result = if b["is_error"] == true { Err(text) } else { Ok(text) };
                        let id = b["tool_use_id"].as_str().unwrap_or("");
                        let mut name = String::new();
                        if let Some(at) = calls.iter().position(|(call, _, _)| call == id) {
                            let (_, tool, label) = calls.remove(at);
                            out.push(Shown::Tool(label));
                            name = tool;
                        }
                        out.push(Shown::Result { ok: result.is_ok(), summary: tools::summary(&name, &result) });
                    }
                    _ => {
                        if let Some(label) = turn::server_label(&b) {
                            out.push(Shown::Tool(label));
                        } else if let Some(result) = turn::server_result(&b) {
                            out.push(Shown::Result { ok: result.is_ok(), summary: tools::summary("server", &result) });
                        }
                    }
                }
            }
        }
        out.extend(calls.into_iter().map(|(_, _, label)| Shown::Tool(label)));
        out
    }
}
