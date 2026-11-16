// What the agent reports, and what the app does with each report.

use super::app::{App, Prompt, size};
use super::history::Block;
use super::text::{DIM, RED};
use super::{Msg, text, view};
use crate::agent::{self, Event};
use crossterm::event::Event as TermEvent;
use std::time::Instant;

impl App {
    pub(super) fn handle(&mut self, m: Msg) {
        let was_approve = matches!(self.prompts.front(), Some(Prompt::Approve { .. }));
        self.dispatch(m);
        if !was_approve && matches!(self.prompts.front(), Some(Prompt::Approve { .. })) {
            self.prompt_since = Instant::now();
        }
    }

    pub(super) fn dispatch(&mut self, m: Msg) {
        match m {
            Msg::Agent(e) => self.on_agent(e),
            Msg::Term(TermEvent::Key(k)) => self.on_key(k),
            Msg::Term(TermEvent::Paste(s)) if !matches!(self.prompts.front(), Some(Prompt::Approve { .. })) => {
                self.paste(&s);
            }
            Msg::Term(TermEvent::Resize(c, r)) => {
                (self.cols, self.rows) = (c as usize, r as usize);
            }
            Msg::Term(_) => {}
        }
    }

    pub(super) fn on_agent(&mut self, e: Event) {
        match e {
            Event::Ready { resumed, name, skills, mcp, has_key } => {
                self.has_key = has_key;
                if !has_key {
                    self.prompts.push_back(Prompt::Key);
                }
                if resumed > 0 {
                    let which = name.as_deref().map_or("the last session".to_string(), |n| format!("\"{n}\""));
                    self.note(&format!("Resumed {which} ({resumed} messages)."), DIM);
                }
                let mut loaded = Vec::new();
                if !skills.is_empty() {
                    loaded.push(format!("{} skill{}", skills.len(), if skills.len() == 1 { "" } else { "s" }));
                }
                if !mcp.is_empty() {
                    loaded.push(format!("MCP: {}", mcp.join(", ")));
                }
                if !loaded.is_empty() {
                    self.note(&format!("Loaded {}.", loaded.join(" · ")), DIM);
                }
                (self.name, self.skills, self.mcp) = (name, skills, mcp);
            }
            Event::Notice(msg) => self.note(&msg, DIM),
            Event::SubStart { id, task } => self.subagents.push((id, task, "starting".into())),
            Event::SubStep { id, label } => {
                if let Some(s) = self.subagents.iter_mut().find(|s| s.0 == id) {
                    s.2 = label;
                }
            }
            Event::SubEnd { id } => self.subagents.retain(|s| s.0 != id),
            Event::RewindPoints(points) if points.is_empty() => self.note("Nothing to rewind to yet.", DIM),
            Event::RewindPoints(points) => self.prompts.push_front(Prompt::Rewind(points)),
            Event::Resumed { name, messages } => {
                let which = name.as_deref().map_or("the conversation".to_string(), |n| format!("\"{n}\""));
                self.note(&format!("Resumed {which} ({messages} messages)."), DIM);
                (self.name, self.context) = (name, 0);
            }
            // The whole conversation again, as it looked when it happened.
            Event::Replay(shown) => {
                for piece in shown {
                    match piece {
                        agent::Shown::User(text) => {
                            self.gap();
                            self.add(Block::User(text::clean(&text)));
                            self.stream.block_start = true;
                        }
                        agent::Shown::Text(text) => {
                            self.stream.block_start = true;
                            for line in text.lines() {
                                self.commit_text_line(line);
                            }
                            self.flush();
                            self.stream.md.reset();
                            self.stream.block_start = true;
                        }
                        agent::Shown::Tool(label) => {
                            self.gap();
                            self.add(Block::Tool { label, diff: None });
                        }
                        agent::Shown::Result { ok, summary } => {
                            self.add(Block::Result { ok, summary });
                            self.stream.block_start = true;
                        }
                    }
                }
            }
            Event::Rewound { prompt, files } => {
                let restored = match files {
                    0 => "No files needed restoring".to_string(),
                    1 => "Restored 1 file".to_string(),
                    n => format!("Restored {n} files"),
                };
                let first = prompt.lines().next().unwrap_or("");
                self.note(
                    &format!(
                        "Rewound to before \"{first}\". {restored}; changes made by shell commands aren't undone."
                    ),
                    DIM,
                );
                // Your message is back in the input box to edit and send again.
                self.input.clear();
                self.input.insert(&prompt);
            }
            Event::Text(t) => {
                if let Some(w) = &mut self.work {
                    w.streamed += t.len();
                    w.activity = "writing".into();
                }
                self.stream.text.push_str(&t);
                while let Some(i) = self.stream.text.find('\n') {
                    let line: String = self.stream.text.drain(..=i).collect();
                    self.commit_text_line(line.trim_end_matches('\n'));
                }
                self.commit_finished_rows();
            }
            Event::Thinking(t) => {
                self.set_activity("thinking".into());
                self.stream.thinking.push_str(&t);
                while let Some(i) = self.stream.thinking.find('\n') {
                    let line: String = self.stream.thinking.drain(..=i).collect();
                    self.commit_thinking_line(line.trim_end_matches('\n'));
                }
            }
            Event::BlockEnd => {
                self.flush();
                self.stream.block_start = true;
            }
            Event::ToolStart(name) => {
                if let Some(w) = &mut self.work {
                    w.activity = format!("preparing {name}");
                    w.tool = name;
                    w.tool_bytes = 0;
                }
            }
            Event::ToolInput(n) => {
                if let Some(w) = &mut self.work {
                    w.tool_bytes += n;
                    w.activity = format!("writing {} · {}", w.tool, size(w.tool_bytes));
                }
            }
            Event::Tool { label, diff } => {
                self.flush();
                self.gap();
                self.set_activity("running".into());
                // Only the changed lines are kept, not whole files.
                self.add(Block::Tool { label, diff: diff.as_ref().map(view::trim_diff) });
            }
            Event::Ask { label, reply } => self.prompts.push_back(Prompt::Approve { label, reply }),
            Event::Question { question, options, reply } => {
                self.prompts.push_back(Prompt::Question { question, options, reply });
            }
            Event::ToolResult { ok, summary } => {
                self.add(Block::Result { ok, summary });
                self.stream.block_start = true;
                self.set_activity("listening".into());
            }
            Event::Activity(a) => self.set_activity(a),
            Event::Step(_) => {}
            Event::Usage { usage, cost, main } => {
                self.cost += cost.unwrap_or(0.0);
                if main {
                    self.context = usage.context();
                }
            }
            Event::Done => self.finish(None),
            Event::Cancelled => self.finish(Some(("Stopped.", DIM))),
            Event::Error(e) => self.finish(Some((&format!("Error: {e}"), RED))),
        }
    }

    pub(super) fn finish(&mut self, note: Option<(&str, &'static str)>) {
        self.flush();
        self.stream.md.reset();
        self.work = None;
        self.subagents.clear();
        self.prompts.retain(|p| matches!(p, Prompt::Key));
        if let Some((msg, style)) = note {
            self.note(msg, style);
        }
        self.send_queued();
    }

    /// Sends the next queued message, if any.
    pub(super) fn send_queued(&mut self) {
        if let Some(q) = self.queued.pop_front() {
            self.submit(q);
        }
    }
}
