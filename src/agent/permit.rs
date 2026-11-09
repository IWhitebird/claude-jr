// Whether a tool call may run: the mode, the allow rules, the user, and in
// auto mode a small model that reviews commands before they run.

use super::Answer;
use super::Event;
use super::turn::{Core, Ctx};
use crate::api::{self, Request};
use crate::permission::{self, Rules};
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

/// Whether an edit-level call may skip asking in accept-edits and auto modes:
/// inside the project, and not a place that changes what runs or what the
/// agent trusts (git hooks, settings, memory notes).
fn auto_edit_ok(name: &str, subject: &str) -> bool {
    name != "memory" && !permission::outside_project(subject) && !permission::sensitive(subject)
}

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
        let inside_project = auto_edit_ok(name, &subject);
        match (mode, access) {
            (_, Access::Free) | (Mode::Bypass, _) => return Ok(()),
            (Mode::AcceptEdits | Mode::Auto, Access::Edit) if inside_project => return Ok(()),
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
    fn auto_edits_skip_sensitive_paths() {
        assert!(auto_edit_ok("edit", "src/main.rs"));
        assert!(!auto_edit_ok("edit", ".git/hooks/pre-commit"));
        assert!(!auto_edit_ok("write", ".mcp.json"));
        assert!(!auto_edit_ok("write", ".claude/settings.json"));
        assert!(!auto_edit_ok("edit", "/etc/hosts"));
        assert!(!auto_edit_ok("edit", "../x"));
        assert!(!auto_edit_ok("memory", "add: uses pnpm"));
    }

    #[test]
    fn mode_cycle() {
        let order: Vec<Mode> = std::iter::successors(Some(Mode::Default), |m| Some(m.next())).take(6).collect();
        assert_eq!(order, [Mode::Default, Mode::AcceptEdits, Mode::Plan, Mode::Auto, Mode::Bypass, Mode::Default]);
    }

    use crate::agent::tests::{Response, Rig, config, fake_http, notices_of, ok, text_reply};

    fn check(rig: &Rig, name: &str, input: Value) -> Result<(), String> {
        let emit = rig.agent.core.emit.clone();
        let ctx = Ctx { system: "", defs: &rig.agent.defs, tools: &rig.agent.tools, emit: &*emit, main: true };
        rig.agent.core.permit(&ctx, name, &input, &format!("{name}(label)"))
    }

    fn bash(c: &str) -> Value {
        json!({ "command": c })
    }

    fn file(p: &str) -> Value {
        json!({ "path": p, "content": "x" })
    }

    fn rig(name: &str, mode: Mode, interactive: bool, allow: &[&str], replies: Vec<Response>) -> Rig {
        let (url, log) = fake_http(replies);
        let mut cfg = config(Some(url), mode);
        cfg.interactive = interactive;
        cfg.allow = allow.iter().map(|s| s.to_string()).collect();
        let mut rig = Rig::with(name, cfg);
        rig.log = log;
        rig
    }

    fn asks(r: &Rig) -> Vec<String> {
        r.events()
            .into_iter()
            .filter_map(|e| if let Event::Ask { label, .. } = e { Some(label) } else { None })
            .collect()
    }

    const NOBODY: &str = "Not allowed: nobody is here to approve it.";
    const DENIED: &str = "The user did not allow this tool call.";

    #[test]
    fn every_mode_and_access_level() {
        let outcomes = |mode: Mode| {
            let r = rig(&format!("permit-{mode:?}").to_lowercase(), mode, false, &[], vec![]);
            [
                check(&r, "read", json!({"path": "a"})),
                check(&r, "todo", json!({})),
                check(&r, "write", file("a.txt")),
                check(&r, "memory", json!({"action": "add", "note": "n"})),
                check(&r, "bash", bash("ls")),
                check(&r, "mcp__srv__tool", json!({})),
            ]
        };
        let nobody = || Err(NOBODY.to_string());
        let plan = || Err("Plan mode is on: don't change files. Put the change in your plan.".to_string());
        // read, todo, write, memory, bash, mcp
        assert_eq!(outcomes(Mode::Default), [Ok(()), Ok(()), nobody(), nobody(), nobody(), nobody()]);
        assert_eq!(outcomes(Mode::AcceptEdits), [Ok(()), Ok(()), Ok(()), nobody(), nobody(), nobody()]);
        assert_eq!(outcomes(Mode::Plan), [Ok(()), Ok(()), plan(), plan(), nobody(), nobody()]);
        assert_eq!(outcomes(Mode::Bypass), [Ok(()), Ok(()), Ok(()), Ok(()), Ok(()), Ok(())]);
    }

    #[test]
    fn edits_outside_the_project_or_to_sensitive_files_still_ask() {
        let r = rig("permit-edits", Mode::AcceptEdits, false, &[], vec![]);
        assert_eq!(check(&r, "write", file("src/new.rs")), Ok(()));
        assert_eq!(check(&r, "edit", file(".git/hooks/pre-commit")), Err(NOBODY.into()));
        assert_eq!(check(&r, "write", file("/etc/passwd")), Err(NOBODY.into()));
        assert_eq!(check(&r, "write", file("../elsewhere")), Err(NOBODY.into()));
        assert_eq!(check(&r, "write", file(".mcp.json")), Err(NOBODY.into()));
    }

    #[test]
    fn allow_rules_let_matching_calls_through() {
        let r = rig("permit-rules", Mode::Default, false, &["bash:echo *", "write:docs/*"], vec![]);
        assert_eq!(check(&r, "bash", bash("echo hi")), Ok(()));
        assert_eq!(check(&r, "bash", bash("rm -rf x")), Err(NOBODY.into()));
        assert_eq!(check(&r, "write", file("docs/a.md")), Ok(()));
        assert_eq!(check(&r, "write", file("src/a.rs")), Err(NOBODY.into()));
        // Plan mode wins over a rule for edits.
        r.agent.core.mode.set(Mode::Plan);
        assert!(check(&r, "write", file("docs/a.md")).unwrap_err().starts_with("Plan mode is on"));
        assert_eq!(check(&r, "bash", bash("echo hi")), Ok(()));
    }

    #[test]
    fn the_user_can_allow_deny_or_always_allow() {
        let r = rig("permit-ask", Mode::Default, true, &[], vec![]);
        r.answers.lock().unwrap().extend([Answer::Yes, Answer::No, Answer::Always]);
        assert_eq!(check(&r, "bash", bash("make")), Ok(()));
        assert_eq!(check(&r, "bash", bash("make")), Err(DENIED.into()));
        assert_eq!(check(&r, "bash", bash("make test")), Ok(()));
        assert_eq!(asks(&r), ["bash(label)", "bash(label)", "bash(label)"]);
        // "Always" added a rule for that command: no more questions (and no answers left).
        assert_eq!(check(&r, "bash", bash("make test")), Ok(()));
        assert!(asks(&r).is_empty());
        // With no answer queued the rig says no.
        assert_eq!(check(&r, "bash", bash("make other")), Err(DENIED.into()));
    }

    #[test]
    fn auto_mode_lets_the_reviewer_allow() {
        let r = rig("permit-auto-allow", Mode::Auto, false, &[], vec![ok(text_reply("ALLOW"))]);
        *r.agent.core.request.lock().unwrap() = "run the tests please".into();
        assert_eq!(check(&r, "bash", bash("cargo test")), Ok(()));
        let events = r.events();
        assert!(events.iter().any(|e| matches!(e, Event::Activity(a) if a == "reviewing bash(label)")));
        assert!(events.iter().any(|e| matches!(e, Event::Usage { main: false, .. })));
        let body = r.bodies().pop().unwrap();
        assert!(body.contains("run the tests please") && body.contains("cargo test"));
        assert!(body.contains("claude-haiku-5-5") && body.contains(r#""tools":[]"#));
        // Edits inside the project never reach the reviewer.
        assert_eq!(check(&r, "write", file("a.txt")), Ok(()));
        assert_eq!(r.bodies().len(), 1);
    }

    #[test]
    fn auto_mode_block_reason_goes_back_or_to_the_user() {
        let block = || ok(text_reply("BLOCK: pushes to a remote"));
        let r = rig("permit-auto-block", Mode::Auto, false, &[], vec![block()]);
        assert_eq!(
            check(&r, "bash", bash("git push")),
            Err("Not allowed: nobody is here to approve it (pushes to a remote).".into())
        );

        let r = rig("permit-auto-block-ask", Mode::Auto, true, &[], vec![block(), block()]);
        r.answers.lock().unwrap().push_back(Answer::Yes);
        assert_eq!(check(&r, "bash", bash("git push")), Ok(()), "the user can overrule the reviewer");
        assert_eq!(asks(&r), ["bash(label) \u{2014} flagged: pushes to a remote"]);
        assert_eq!(check(&r, "bash", bash("git push")), Err(DENIED.into()));
    }

    #[test]
    fn auto_mode_falls_back_to_asking_when_the_reviewer_is_no_help() {
        let garbage = ok(text_reply("Hmm, hard to say."));
        let refused = Response { status: 400, body: r#"{"error":{"message":"no"}}"#.into() };
        let r = rig("permit-auto-unsure", Mode::Auto, false, &[], vec![garbage, refused]);
        assert_eq!(check(&r, "bash", bash("ls")), Err(NOBODY.into()));
        assert_eq!(check(&r, "bash", bash("ls")), Err(NOBODY.into()));

        // With someone there, they are asked, with no "flagged" note.
        let r = rig("permit-auto-unsure-ask", Mode::Auto, true, &[], vec![ok(text_reply("maybe"))]);
        r.answers.lock().unwrap().push_back(Answer::Yes);
        assert_eq!(check(&r, "bash", bash("ls")), Ok(()));
        assert_eq!(asks(&r), ["bash(label)"]);
    }

    #[test]
    fn auto_mode_with_an_unreachable_reviewer_asks_instead() {
        // Nothing listens here; a cancelled request gives up at once instead of retrying.
        let mut cfg = config(Some("http://127.0.0.1:9".into()), Mode::Auto);
        cfg.interactive = true;
        let r = Rig::with("permit-unreachable", cfg);
        r.agent.core.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        r.answers.lock().unwrap().push_back(Answer::No);
        assert_eq!(check(&r, "bash", bash("ls")), Err(DENIED.into()));
        assert!(notices_of(&r.events()).is_empty());
    }

    #[test]
    fn shared_mode_and_names() {
        let m = SharedMode(AtomicU8::new(Mode::Plan as u8));
        assert_eq!(m.get(), Mode::Plan);
        m.set(Mode::Auto);
        assert_eq!(m.get(), Mode::Auto);
        assert_eq!(Mode::parse("skip-permissions"), Some(Mode::Bypass));
        assert_eq!(Mode::parse("accept-edits"), Some(Mode::AcceptEdits));
        assert_eq!(Mode::parse("bypass"), None);
        assert!(matches!(parse_verdict("BLOCK"), Verdict::Block(r) if r.is_empty()));
        assert!(matches!(parse_verdict("ALLOW: fine\nsecond line"), Verdict::Allow));
        assert!(matches!(parse_verdict(""), Verdict::Unsure));
    }
}
