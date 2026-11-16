// Subagents: the `agent` tool runs a fresh conversation for one task, with
// its own context and shell, and returns its final reply. Several can run at
// once. They can't ask the user questions, but their tool calls still go
// through approval.
//
// One that is given a worktree works in its own copy of the repository, on
// its own branch (see worktree.rs), so subagents changing files at the same
// time can't collide. Its work is committed when it finishes and its reply
// ends with the branch for the agent that started it to merge.
//
// A subagent may start subagents of its own, one level down and no further,
// and only so many run at once: delegation that goes on unchecked multiplies
// cost without getting more done.

use super::Event;
use super::turn::{Core, Ctx};
use crate::session::Msg;
use crate::tools::Output;
use crate::work::worktree::Worktree;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Ids for subagents, so the UI can track several at once.
static NEXT_ID: AtomicUsize = AtomicUsize::new(1);
/// How many subagents are running now, at every level.
static RUNNING: AtomicUsize = AtomicUsize::new(0);

/// A subagent this deep can't start another: main agent, subagent, its subagents.
const MAX_DEPTH: u8 = 2;
/// The most subagents running at the same time.
const MAX_RUNNING: usize = 16;

const SUB_PROMPT: &str = "\n\nYou are a subagent: another agent gave you the task in the first message. \
Work on your own; you can't ask questions. When you're done, reply with a concise report of what you \
found or did, including file paths and anything the other agent needs to act on. That reply is all it sees.";

/// Counts a subagent as running for as long as it is held.
struct Running;

impl Running {
    fn start() -> Result<Running, String> {
        let now = RUNNING.fetch_add(1, Ordering::Relaxed);
        if now >= MAX_RUNNING {
            RUNNING.fetch_sub(1, Ordering::Relaxed);
            return Err(format!(
                "{MAX_RUNNING} subagents are already running; wait for some to finish before starting more"
            ));
        }
        Ok(Running)
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        RUNNING.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Core {
    pub fn subagent(&self, ctx: &Ctx, input: &Value) -> Output {
        if ctx.depth >= MAX_DEPTH {
            return Err("a subagent this deep can't start subagents; do the work yourself".into());
        }
        let task = input["description"].as_str().unwrap_or("subagent");
        let prompt = input["prompt"].as_str().ok_or("missing prompt")?;
        let _running = Running::start()?;

        // In a worktree of its own if asked: made before anything runs, so a
        // folder that can't have one is said straight away.
        let worktree = match input["worktree"].as_bool() {
            Some(true) => {
                let made = Worktree::make(ctx.tools.root(), &crate::work::worktree::new_id());
                Some(made.map_err(|e| format!("{e}. Start the subagent without worktree, or commit first."))?)
            }
            _ => None,
        };
        let (tools, prompt) = match &worktree {
            Some((w, inside)) => {
                let note = format!(
                    "{prompt}\n\n(You are working in {}, a git worktree of the repository on its own branch, {}. \
                     Change files only there. Your changes are committed for you when you finish.)",
                    inside.display(),
                    w.branch
                );
                (ctx.tools.child_in(inside.clone()), note)
            }
            None => (ctx.tools.child(), prompt.to_string()),
        };
        let system = self.sub_system.get_or_init(|| crate::prompt::system() + SUB_PROMPT);
        let mut messages = vec![Msg::new("user", json!([{ "type": "text", "text": prompt }]))];

        // Its streaming text stays private; its steps show in the UI's agent
        // panel, approvals go to the user, and its cost is counted. What its
        // own subagents report passes through.
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let parent = ctx.emit;
        parent(Event::SubStart { id, task: task.to_string() });
        let emit = |e: Event| match e {
            Event::Tool { label, .. } => parent(Event::SubStep { id, label }),
            Event::Ask { label, reply } => parent(Event::Ask { label: format!("{task} · {label}"), reply }),
            Event::Usage { usage, cost, .. } => parent(Event::Usage { usage, cost, main: false }),
            Event::Step(step) => {
                let agent = Some(step.agent.map_or(task.to_string(), |inner| format!("{task} > {inner}")));
                parent(Event::Step(super::Step { agent, ..step }))
            }
            e @ (Event::SubStart { .. } | Event::SubStep { .. } | Event::SubEnd { .. }) => parent(e),
            _ => {}
        };
        let depth = ctx.depth + 1;
        let defs = if depth < MAX_DEPTH { &self.sub_defs } else { &self.leaf_defs }.lock().unwrap().clone();
        let sub = Ctx { system, defs: &defs, tools: &tools, emit: &emit, main: false, depth };

        let result = match self.turn(&sub, &mut messages, &mut |_| {}) {
            Event::Done => Ok(final_text(&messages)),
            Event::Cancelled => Err("Cancelled by the user.".into()),
            Event::Error(e) => Err(format!("The subagent failed: {e}")),
            _ => Err("The subagent stopped unexpectedly.".into()),
        };
        let result = match &worktree {
            Some((w, _)) => {
                // The commands it left running are stopped before its folder goes.
                tools.shutdown();
                let left = handed_back(w, task);
                match result {
                    Ok(report) => Ok(format!("{report}\n\n{left}")),
                    Err(why) => Err(format!("{why}\n\n{left}")),
                }
            }
            None => result,
        };
        parent(Event::SubEnd { id });
        result
    }
}

/// Wraps a subagent's worktree up and says, for the agent that started it,
/// where the work is: committed on its branch, with the folder itself gone.
fn handed_back(w: &Worktree, task: &str) -> String {
    let left = w.finish(Some(task));
    if left.is_null() {
        return "(It changed no files.)".into();
    }
    if left["uncommitted"] == true {
        let problem = left["problem"].as_str().unwrap_or("its changes could not be committed");
        return format!("(Its work is in {} on branch {}, but {problem}.)", w.path.display(), w.branch);
    }
    let files = w.changed();
    w.remove_folder();
    let commits = left["commits"].as_u64().unwrap_or(0);
    format!(
        "(Its work is committed on branch {branch}: {commits} commit{s}.\n{files}\nTake it with: git merge {branch})",
        branch = w.branch,
        s = if commits == 1 { "" } else { "s" },
    )
}

/// The text of the last assistant message.
fn final_text(messages: &[Msg]) -> String {
    let Some(last) = messages.iter().rev().find(|m| !m.user) else { return String::new() };
    let text: Vec<String> = last.content().iter().filter_map(|b| b["text"].as_str().map(String::from)).collect();
    if text.is_empty() { "(the subagent finished without a report)".into() } else { text.join("\n") }
}
