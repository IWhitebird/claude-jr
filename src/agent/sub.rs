// Subagents: the `agent` tool runs a fresh conversation for one task, with
// its own context and shell, and returns its final reply. Several can run at
// once. They can't start subagents or ask the user questions, but their
// tool calls still go through approval.

use super::Event;
use super::turn::{Core, Ctx};
use crate::session::Msg;
use crate::tools::Output;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Ids for subagents, so the UI can track several at once.
static NEXT_ID: AtomicUsize = AtomicUsize::new(1);

const SUB_PROMPT: &str = "\n\nYou are a subagent: another agent gave you the task in the first message. \
Work on your own; you can't ask questions. When you're done, reply with a concise report of what you \
found or did, including file paths and anything the other agent needs to act on. That reply is all it sees.";

impl Core {
    pub fn subagent(&self, ctx: &Ctx, input: &Value) -> Output {
        let task = input["description"].as_str().unwrap_or("subagent");
        let prompt = input["prompt"].as_str().ok_or("missing prompt")?;
        let tools = ctx.tools.child();
        let system = self.sub_system.get_or_init(|| crate::prompt::system() + SUB_PROMPT);
        let mut messages = vec![Msg::new("user", json!([{ "type": "text", "text": prompt }]))];

        // Its streaming text stays private; its steps show in the UI's agent
        // panel, approvals go to the user, and its cost is counted.
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let parent = ctx.emit;
        parent(Event::SubStart { id, task: task.to_string() });
        let emit = |e: Event| match e {
            Event::Tool { label, .. } => parent(Event::SubStep { id, label }),
            Event::Ask { label, reply } => parent(Event::Ask { label: format!("{task} · {label}"), reply }),
            Event::Usage { usage, cost, .. } => parent(Event::Usage { usage, cost, main: false }),
            _ => {}
        };
        let defs = self.sub_defs.lock().unwrap().clone();
        let sub = Ctx { system, defs: &defs, tools: &tools, emit: &emit, main: false };

        let result = match self.turn(&sub, &mut messages) {
            Event::Done => Ok(final_text(&messages)),
            Event::Cancelled => Err("Cancelled by the user.".into()),
            Event::Error(e) => Err(format!("The subagent failed: {e}")),
            _ => Err("The subagent stopped unexpectedly.".into()),
        };
        parent(Event::SubEnd { id });
        result
    }
}

/// The text of the last assistant message.
fn final_text(messages: &[Msg]) -> String {
    let Some(last) = messages.iter().rev().find(|m| !m.user) else { return String::new() };
    let text: Vec<String> = last.content().iter().filter_map(|b| b["text"].as_str().map(String::from)).collect();
    if text.is_empty() { "(the subagent finished without a report)".into() } else { text.join("\n") }
}
