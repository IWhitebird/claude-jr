// /compact: the conversation so far is summarised by the model and replaced
// by that summary, so the work can go on in a fresh context.

use super::{Agent, Event};
use crate::store::session::Msg;
use serde_json::{Value, json};
use std::sync::atomic::Ordering;

impl Agent {
    /// Asks the model to summarize the conversation, then starts over from
    /// the summary. No earlier thinking is replayed, so this is safe with
    /// models that check their earlier reasoning wasn't edited.
    pub(super) fn compact(&mut self, focus: &str) -> Event {
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
        let mut parts: Vec<crate::api::Text> = self.messages.iter().map(crate::api::Message::text).collect();
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
        parts.push(crate::api::Message::text(&tail));

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
        if let Some(problem) = self.session.save(&self.system, &mut self.messages) {
            (self.core.emit)(Event::Notice(problem));
        }
        self.tools.checkpoints.clear();
        self.told_plan = false;
        let words = summary.split_whitespace().count();
        (self.core.emit)(Event::Notice(format!(
            "Compacted {before} messages into a {words}-word summary. /rewind can't go back past this point."
        )));
        Event::Done
    }
}
