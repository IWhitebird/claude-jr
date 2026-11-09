// Whether a tool call may run: the mode, the allow rules, the user, and in
// auto mode a small model that reviews commands before they run.

use super::Answer;
use super::Event;
use super::turn::{Core, Ctx};
use crate::api::{self, Request};
use crate::permission::Rules;
use crate::session::Msg;
use crate::tools::{self, Access};
use serde_json::value::RawValue;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc;

/// How tool calls get approved. Shift+Tab cycles through these in order.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode {
    /// Ask before edits and commands, unless a rule allows them.
    Default,
    /// Edits go through; commands still ask.
    AcceptEdits,
    /// Read-only: no edits, commands still ask. The model is told to plan.
    Plan,
    /// Edits inside the project go through; a reviewer model checks each
    /// command and asks you only about the risky ones.
    Auto,
    /// Everything runs without asking.
    Bypass,
}

impl Mode {
    const ALL: [Mode; 5] = [Mode::Default, Mode::AcceptEdits, Mode::Plan, Mode::Auto, Mode::Bypass];

    /// The names used in the config file.
    pub const NAMES: [&str; 5] = ["normal", "accept-edits", "plan", "auto", "skip-permissions"];

    pub fn parse(name: &str) -> Option<Mode> {
        Mode::NAMES.iter().position(|n| *n == name).map(|i| Mode::ALL[i])
    }

    pub fn next(self) -> Mode {
        Mode::ALL[(self as usize + 1) % Mode::ALL.len()]
    }
}

/// The mode, shared between the UI (which changes it) and the agent (which
/// reads it before each tool call), so a change applies right away.
pub struct SharedMode(pub AtomicU8);

impl SharedMode {
    pub fn get(&self) -> Mode {
        Mode::ALL[self.0.load(Ordering::Relaxed) as usize % Mode::ALL.len()]
    }

    pub fn set(&self, m: Mode) {
        self.0.store(m as u8, Ordering::Relaxed);
    }
}

pub const PLAN_ON: &str = "<system-reminder>Plan mode is on. Explore and explain, but don't change any files. \
End with a concrete plan for the user to approve.</system-reminder>";
pub const PLAN_OFF: &str = "<system-reminder>Plan mode is off. You can make changes now.</system-reminder>";

const REVIEW_SYSTEM: &str = "You review actions that a coding agent wants to take on a developer's machine. \
Reply with exactly one line: ALLOW, or BLOCK: <short reason>.

Allow normal development work that the user's request plausibly needs: reading and searching files, \
building, testing, linting, running project scripts, starting local servers, installing the project's \
dependencies, and git commands that only change the local repository (status, diff, log, add, commit, \
branch, checkout of new branches).

Block: deleting or overwriting anything outside the project or anything not clearly disposable; \
git push, force push, history rewrites, hard resets; sending files, secrets or environment variables \
over the network; sudo, system packages and system settings; killing processes the agent didn't \
start; anything destructive or hard to undo that the user didn't ask for. When unsure, BLOCK.";

enum Verdict {
    Allow,
    Block(String),
    /// The reviewer couldn't be reached or answered oddly; ask the user.
    Unsure,
}

impl Core {
    /// Ok if the call may run; otherwise the reason, which goes back to the model.
    pub fn permit(&self, ctx: &Ctx, name: &str, input: &Value, label: &str) -> Result<(), String> {
        let access = tools::access(name);
        let mode = self.mode.get();
        let subject = ctx.tools.subject(name, input);
        let inside_project = !subject.starts_with('/') && !subject.starts_with("..");
        match (mode, access) {
            (_, Access::Free) | (Mode::Bypass, _) | (Mode::AcceptEdits, Access::Edit) => return Ok(()),
            (Mode::Auto, Access::Edit) if inside_project => return Ok(()),
            (Mode::Plan, Access::Edit) => {
                return Err("Plan mode is on: don't change files. Put the change in your plan.".into());
            }
            _ => {}
        }
        if self.rules.lock().unwrap().allows(name, &subject) {
            return Ok(());
        }

        let mut why = None;
        if mode == Mode::Auto {
            (ctx.emit)(Event::Activity(format!("reviewing {label}")));
            match self.review(ctx, name, &subject) {
                Verdict::Allow => return Ok(()),
                Verdict::Block(reason) => why = Some(reason),
                Verdict::Unsure => {}
            }
        }
        if !self.interactive {
            let reason = why.map(|w| format!(" ({w})")).unwrap_or_default();
            return Err(format!("Not allowed: nobody is here to approve it{reason}."));
        }

        let label = match &why {
            Some(w) => format!("{label} — flagged: {w}"),
            None => label.to_string(),
        };
        let (tx, rx) = mpsc::channel();
        (ctx.emit)(Event::Ask { label, reply: tx });
        match rx.recv() {
            Ok(Answer::Yes) => Ok(()),
            Ok(Answer::Always) => {
                self.rules.lock().unwrap().add(Rules::always(name, &subject));
                Ok(())
            }
            _ => Err("The user did not allow this tool call.".into()),
        }
    }

    /// Asks a small model whether an action fits the user's request.
    fn review(&self, ctx: &Ctx, name: &str, subject: &str) -> Verdict {
        let request: String = self.request.lock().unwrap().chars().take(2000).collect();
        let subject: String = subject.chars().take(4000).collect();
        let cwd = std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default();
        let prompt = format!(
            "The user asked:\n<request>\n{request}\n</request>\n\nThe agent wants to run this ({name}):\n\
             <action>\n{subject}\n</action>\n\nProject directory: {cwd}"
        );
        let no_tools = RawValue::from_string("[]".into()).expect("valid JSON");
        let mut req = Request::new(&self.auto_model, Some("low"), REVIEW_SYSTEM, &no_tools);
        req.max_tokens = 1024;
        let messages = [Msg::new("user", json!([{ "type": "text", "text": prompt }]))];

        let Ok(reply) = self.client.stream(&req, &messages, &self.cancel, |_| {}) else { return Verdict::Unsure };
        let cost = api::cost(&self.auto_model, &reply.usage);
        (ctx.emit)(Event::Usage { usage: reply.usage, cost, main: false });
        let text: String = reply.content.iter().filter_map(|b| b["text"].as_str()).collect();
        parse_verdict(&text)
    }
}

fn parse_verdict(text: &str) -> Verdict {
    let line = text.trim().lines().next().unwrap_or("").trim();
    if line.eq_ignore_ascii_case("allow") || line.starts_with("ALLOW") {
        Verdict::Allow
    } else if let Some(reason) = line.strip_prefix("BLOCK") {
        Verdict::Block(reason.trim_start_matches([':', ' ']).to_string())
    } else {
        Verdict::Unsure
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdicts() {
        assert!(matches!(parse_verdict("ALLOW"), Verdict::Allow));
        assert!(matches!(parse_verdict("  allow\n"), Verdict::Allow));
        assert!(matches!(parse_verdict("BLOCK: pushes to remote"), Verdict::Block(r) if r == "pushes to remote"));
        assert!(matches!(parse_verdict("I think this is fine"), Verdict::Unsure));
    }

    #[test]
    fn mode_cycle() {
        let order: Vec<Mode> = std::iter::successors(Some(Mode::Default), |m| Some(m.next())).take(6).collect();
        assert_eq!(order, [Mode::Default, Mode::AcceptEdits, Mode::Plan, Mode::Auto, Mode::Bypass, Mode::Default]);
    }
}
