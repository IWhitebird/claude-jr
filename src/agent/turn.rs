// One turn: send the conversation, stream the reply, run the tools it asks
// for, send the results, and repeat until the model stops. The main agent
// and subagents both run this; `Ctx` says which.

use super::permit::SharedMode;
use super::{Emit, Event};
use crate::api::{self, Chunk, Client, Request};
use crate::permission::Rules;
use crate::session::Msg;
use crate::tools::{self, Output, Tools};
use serde_json::value::RawValue;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

const TOO_LONG: &str = "the conversation no longer fits in the model's context. Run /compact, then continue.";

/// What the main agent and its subagents share.
pub struct Core {
    pub client: Client,
    pub model: Mutex<String>,
    /// None: the model's own default.
    pub effort: Mutex<Option<String>>,
    pub auto_model: String,
    pub mode: Arc<SharedMode>,
    pub rules: Mutex<Rules>,
    pub cancel: Arc<AtomicBool>,
    pub emit: Emit,
    pub interactive: bool,
    /// Remote MCP servers, passed to the API's connector with every request.
    pub connector: Option<Box<RawValue>>,
    /// Tool definitions for subagents; rebuilt if /model changes what's available.
    pub sub_defs: Mutex<Box<RawValue>>,
    pub sub_system: OnceLock<String>,
    /// The user's latest message, so auto mode's reviewer knows the intent.
    pub request: Mutex<String>,
}

/// Which conversation a turn belongs to.
pub struct Ctx<'a> {
    pub system: &'a str,
    pub defs: &'a RawValue,
    pub tools: &'a Tools,
    pub emit: &'a (dyn Fn(Event) + Send + Sync),
    /// The main agent (true) or a subagent.
    pub main: bool,
}

struct Call {
    id: Value,
    name: String,
    input: Value,
}

impl Core {
    /// Runs requests and tools until the model stops. Returns the final event.
    /// Runs until the model has nothing more to do. `step` is called with
    /// the conversation after each round of tool calls, so it can be saved:
    /// a turn can run for a long time, and a crash shouldn't lose it.
    pub fn turn(&self, ctx: &Ctx, messages: &mut Vec<Msg>, step: &mut dyn FnMut(&[Msg])) -> Event {
        loop {
            let model = self.model.lock().unwrap().clone();
            let effort = self.effort.lock().unwrap().clone();
            let mut req = Request::new(&model, effort.as_deref(), ctx.system, ctx.defs);
            req.mcp_servers = self.connector.as_deref();
            let result = self.client.stream(&req, messages, &self.cancel, |c| match c {
                Chunk::ServerTool(block) => show_server_tool(ctx, block),
                c => (ctx.emit)(match c {
                    Chunk::Text(t) => Event::Text(t.to_string()),
                    Chunk::Thinking(t) => Event::Thinking(t.to_string()),
                    Chunk::ToolStart(n) => Event::ToolStart(n.to_string()),
                    Chunk::ToolInput(n) => Event::ToolInput(n),
                    _ => Event::BlockEnd,
                }),
            });

            let reply = match result {
                Ok(r) => r,
                Err(e) if e.is::<api::Cancelled>() => return Event::Cancelled,
                Err(e) if format!("{e}").contains("too long") => return Event::Error(TOO_LONG.into()),
                Err(e) => return Event::Error(format!("{e:#}")),
            };
            let cost = api::cost(&model, &reply.usage);
            (ctx.emit)(Event::Usage { usage: reply.usage, cost, main: ctx.main });
            if reply.content.is_empty() {
                return Event::Error(format!("empty reply (stop reason: {})", reply.stop_reason));
            }

            let calls: Vec<Call> = reply
                .content
                .iter()
                .filter(|b| b["type"] == "tool_use")
                .map(|b| Call {
                    id: b["id"].clone(),
                    name: b["name"].as_str().unwrap_or("").to_string(),
                    input: b["input"].clone(),
                })
                .collect();
            let stop = reply.stop_reason;
            messages.push(Msg::new("assistant", Value::Array(reply.content)));

            // A long server-side tool (remote MCP) paused the turn: ask it to go on.
            if stop == "pause_turn" {
                continue;
            }
            if stop != "tool_use" || calls.is_empty() {
                if let Some(results) = messages.last().and_then(|m| unanswered(m, "Not run.")) {
                    messages.push(results);
                }
                return match stop.as_str() {
                    "refusal" => Event::Error("the model declined this request".into()),
                    "max_tokens" => Event::Error("the reply hit the max_tokens limit".into()),
                    "model_context_window_exceeded" => Event::Error(TOO_LONG.into()),
                    _ => Event::Done,
                };
            }

            let mut results = self.run_calls(ctx, &calls);
            if let Some(note) = ctx.main.then(|| job_notes(ctx.tools)).flatten() {
                results.push(json!({ "type": "text", "text": note }));
            }
            messages.push(Msg::new("user", Value::Array(results)));
            step(messages);
            if self.cancel.load(Ordering::Relaxed) {
                return Event::Cancelled;
            }
        }
    }

    /// Runs one reply's tool calls in order. Neighbouring calls that are safe
    /// to overlap (reads, subagents, skills, MCP) run at the same time.
    fn run_calls(&self, ctx: &Ctx, calls: &[Call]) -> Vec<Value> {
        let mut results = Vec::with_capacity(calls.len());
        let mut i = 0;
        while i < calls.len() {
            let run = calls[i..].iter().take_while(|c| tools::parallel_safe(&c.name)).count().max(1);
            let group = &calls[i..i + run];
            if group.len() == 1 {
                results.push(self.call_one(ctx, &group[0]));
            } else {
                results.extend(self.call_group(ctx, group));
            }
            i += run;
        }
        results
    }

    fn call_one(&self, ctx: &Ctx, c: &Call) -> Value {
        let label = ctx.tools.label(&c.name, &c.input);
        (ctx.emit)(Event::Tool { label: label.clone(), diff: ctx.tools.preview(&c.name, &c.input) });
        let out = self.check(ctx, c, &ctx.tools.full_label(&c.name, &c.input)).and_then(|()| self.exec(ctx, c));
        (ctx.emit)(Event::ToolResult { ok: out.is_ok(), summary: tools::summary(&c.name, &out) });
        tool_result(&c.id, out)
    }

    /// Approvals first, one at a time; then everything runs at once, and the
    /// results are shown in order when all are done.
    fn call_group(&self, ctx: &Ctx, calls: &[Call]) -> Vec<Value> {
        let labels: Vec<String> = calls.iter().map(|c| ctx.tools.label(&c.name, &c.input)).collect();
        let allowed: Vec<Result<(), String>> =
            calls.iter().map(|c| self.check(ctx, c, &ctx.tools.full_label(&c.name, &c.input))).collect();
        (ctx.emit)(Event::Activity(format!("running {} tools at once", calls.len())));

        let outs: Vec<Output> = thread::scope(|s| {
            let handles: Vec<_> = calls
                .iter()
                .zip(&allowed)
                .map(|(c, ok)| s.spawn(move || ok.clone().and_then(|()| self.exec(ctx, c))))
                .collect();
            handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err("the tool crashed".into()))).collect()
        });

        calls
            .iter()
            .zip(labels)
            .zip(outs)
            .map(|((c, label), out)| {
                (ctx.emit)(Event::Tool { label, diff: None });
                (ctx.emit)(Event::ToolResult { ok: out.is_ok(), summary: tools::summary(&c.name, &out) });
                tool_result(&c.id, out)
            })
            .collect()
    }

    /// Everything that has to pass before a call runs.
    fn check(&self, ctx: &Ctx, c: &Call, label: &str) -> Result<(), String> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err("Cancelled by the user.".into());
        }
        if let Some(e) = c.input.get("_invalid_json") {
            return Err(format!("The tool input was not valid JSON ({e}). Try again."));
        }
        self.permit(ctx, &c.name, &c.input, label)
    }

    fn exec(&self, ctx: &Ctx, c: &Call) -> Output {
        match c.name.as_str() {
            "ask_user" => self.ask_user(ctx, &c.input),
            "agent" => self.subagent(ctx, &c.input),
            name => ctx.tools.run(name, &c.input, &self.cancel),
        }
    }

    fn ask_user(&self, ctx: &Ctx, input: &Value) -> Output {
        if !self.interactive {
            return Ok("The user isn't available. Make a reasonable choice and say what you assumed.".into());
        }
        let question = input["question"].as_str().unwrap_or("").to_string();
        let options = input["options"].as_array().into_iter().flatten().filter_map(Value::as_str).map(String::from);
        let (tx, rx) = mpsc::channel();
        (ctx.emit)(Event::Question { question, options: options.collect(), reply: tx });
        match rx.recv() {
            Ok(answer) if !answer.trim().is_empty() => Ok(format!("The user answered: {answer}")),
            _ => Err("The user didn't answer.".into()),
        }
    }
}

fn tool_result(id: &Value, out: Output) -> Value {
    let (content, is_error) = match out {
        Ok(s) => (tools::result_content(s), false),
        Err(s) => (Value::String(s), true),
    };
    json!({ "type": "tool_result", "tool_use_id": id, "content": content, "is_error": is_error })
}

/// If `m` is an assistant message with tool calls, a user message answering
/// each of them with `why`. The API needs every call answered.
pub fn unanswered(m: &Msg, why: &str) -> Option<Msg> {
    if m.user {
        return None;
    }
    let results: Vec<Value> = m
        .content()
        .iter()
        .filter(|b| b["type"] == "tool_use")
        .map(|b| tool_result(&b["id"], Err(why.to_string())))
        .collect();
    (!results.is_empty()).then(|| Msg::new("user", Value::Array(results)))
}

/// A reminder about background jobs that finished, for the next message.
pub fn job_notes(tools: &Tools) -> Option<String> {
    let done = tools.finished_jobs();
    if done.is_empty() {
        return None;
    }
    let lines: Vec<String> =
        done.iter().map(|j| format!("Background job {} (`{}`) finished: {}.", j.id, j.command, j.status)).collect();
    Some(format!("<system-reminder>{} Use the jobs tool to read their output.</system-reminder>", lines.join(" ")))
}

/// Tools that run on Anthropic's side (web search and fetch, remote MCP)
/// arrive as finished blocks in the stream, after they have already run, so
/// there is no approval step. Every one is shown, with its arguments.
fn show_server_tool(ctx: &Ctx, b: &Value) {
    if let Some(label) = server_label(b) {
        return (ctx.emit)(Event::Tool { label, diff: None });
    }
    if let Some(out) = server_result(b) {
        (ctx.emit)(Event::ToolResult { ok: out.is_ok(), summary: tools::summary("server", &out) });
    }
}

/// One line, no control characters, at most 200 characters.
fn clean(s: &str) -> String {
    let flat: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(200) {
        Some((i, _)) => format!("{}...", &flat[..i]),
        None => flat,
    }
}

/// The transcript line for a server-side call: the tool and its arguments.
fn server_label(b: &Value) -> Option<String> {
    let name = b["name"].as_str().unwrap_or("");
    let label = match b["type"].as_str()? {
        "server_tool_use" => match name {
            "web_search" => format!("Search({})", clean(b["input"]["query"].as_str().unwrap_or(""))),
            "web_fetch" => format!("Fetch({})", clean(b["input"]["url"].as_str().unwrap_or(""))),
            other => format!("{} (server) {}", clean(other), clean(&tools::mcp::summarize_args(&b["input"]))),
        },
        "mcp_tool_use" => format!(
            "{}: {} (remote) {}",
            clean(b["server_name"].as_str().unwrap_or("mcp")),
            clean(name),
            clean(&tools::mcp::summarize_args(&b["input"]))
        ),
        _ => return None,
    };
    Some(label.trim_end().to_string())
}

/// The outcome of a server-side call, if the block is a result.
fn server_result(b: &Value) -> Option<Result<String, String>> {
    let kind = b["type"].as_str()?;
    let code = |c: &Value| clean(c["error_code"].as_str().unwrap_or("error"));
    Some(match kind {
        // A list of results on success, an object with error_code on failure.
        "web_search_tool_result" => match &b["content"] {
            Value::Array(results) => Ok(format!("{} results", results.len())),
            e => Err(format!("search failed: {}", code(e))),
        },
        "web_fetch_tool_result" => match b["content"]["type"].as_str() {
            Some("web_fetch_result") => {
                Ok(format!("Fetched {}", clean(b["content"]["url"].as_str().unwrap_or("the page"))))
            }
            _ => Err(format!("fetch failed: {}", code(&b["content"]))),
        },
        "mcp_tool_result" => {
            let text = match &b["content"] {
                Value::String(s) => s.clone(),
                Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n"),
                _ => String::new(),
            };
            if b["is_error"] == true { Err(if text.is_empty() { "error".into() } else { text }) } else { Ok(text) }
        }
        // Any other server tool's result: say whether it failed.
        k if k.ends_with("_tool_result") => {
            let what = k.trim_end_matches("_tool_result");
            match b["content"]["error_code"].as_str() {
                Some(c) => Err(format!("{what} failed: {}", clean(c))),
                None => Ok(format!("{what} done")),
            }
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_calls_are_labelled_with_arguments() {
        let mcp =
            json!({"type":"mcp_tool_use","server_name":"linear","name":"create_issue","input":{"title":"a\nb","n":2}});
        assert_eq!(server_label(&mcp).unwrap(), "linear: create_issue (remote) n: 2, title: a b");
        let search = json!({"type":"server_tool_use","name":"web_search","input":{"query":"rust\u{1b}[0m"}});
        assert_eq!(server_label(&search).unwrap(), "Search(rust [0m)");
        let other = json!({"type":"server_tool_use","name":"code_execution","input":{"code":"1+1"}});
        assert_eq!(server_label(&other).unwrap(), "code_execution (server) code: 1+1");
        assert!(server_label(&json!({"type":"text"})).is_none());
        assert_eq!(clean(&"x".repeat(300)).len(), 203);
    }

    #[test]
    fn server_results_report_failures() {
        let ok = json!({"type":"mcp_tool_result","content":[{"type":"text","text":"done"}]});
        assert_eq!(server_result(&ok), Some(Ok("done".into())));
        let bad = json!({"type":"mcp_tool_result","is_error":true,"content":"denied"});
        assert_eq!(server_result(&bad), Some(Err("denied".into())));
        let empty = json!({"type":"mcp_tool_result","is_error":true});
        assert_eq!(server_result(&empty), Some(Err("error".into())));
        let fetch = json!({"type":"web_fetch_tool_result","content":{"type":"web_fetch_tool_error","error_code":"unavailable"}});
        assert_eq!(server_result(&fetch), Some(Err("fetch failed: unavailable".into())));
        let other = json!({"type":"code_execution_tool_result","content":{"error_code":"timeout"}});
        assert_eq!(server_result(&other), Some(Err("code_execution failed: timeout".into())));
        assert!(server_result(&json!({"type":"text"})).is_none());

        let found = json!({"type":"web_search_tool_result","content":[{"title":"a"},{"title":"b"}]});
        assert_eq!(server_result(&found), Some(Ok("2 results".into())));
        let failed = json!({"type":"web_search_tool_result","content":{"type":"web_search_tool_result_error","error_code":"max_uses_exceeded"}});
        assert_eq!(server_result(&failed), Some(Err("search failed: max_uses_exceeded".into())));
        let page =
            json!({"type":"web_fetch_tool_result","content":{"type":"web_fetch_result","url":"https://x.test/a\nb"}});
        assert_eq!(server_result(&page), Some(Ok("Fetched https://x.test/a b".into())));
        let page = json!({"type":"web_fetch_tool_result","content":{"type":"web_fetch_result"}});
        assert_eq!(server_result(&page), Some(Ok("Fetched the page".into())));
        let done = json!({"type":"bash_code_execution_tool_result","content":{"stdout":"2"}});
        assert_eq!(server_result(&done), Some(Ok("bash_code_execution done".into())));
        let plain = json!({"type":"mcp_tool_result","content":"all good"});
        assert_eq!(server_result(&plain), Some(Ok("all good".into())));
    }

    use crate::agent::Command;
    use crate::agent::permit::Mode;
    use crate::agent::tests::{
        Response, Rig, blocks_reply, errors_of, ok, sse, text_of, text_reply, tool_use, tools_reply,
    };
    use std::time::Duration;

    fn read_call(id: &str, path: &str) -> Value {
        tool_use(id, "read", json!({ "path": path }))
    }

    /// (label, ok, summary) for each tool shown, in order.
    fn shown(events: &[Event]) -> (Vec<String>, Vec<(bool, String)>) {
        let mut labels = Vec::new();
        let mut results = Vec::new();
        for e in events {
            match e {
                Event::Tool { label, .. } => labels.push(label.clone()),
                Event::ToolResult { ok, summary } => results.push((*ok, summary.clone())),
                _ => {}
            }
        }
        (labels, results)
    }

    /// The tool results of message `i`: (tool_use_id, is_error, content text).
    fn results_of(rig: &Rig, i: usize) -> Vec<(String, bool, String)> {
        rig.agent.messages[i]
            .content()
            .iter()
            .filter(|b| b["type"] == "tool_result")
            .map(|b| {
                (
                    b["tool_use_id"].as_str().unwrap().to_string(),
                    b["is_error"] == true,
                    b["content"].as_str().unwrap_or("").to_string(),
                )
            })
            .collect()
    }

    fn send(rig: &mut Rig, text: &str) -> Vec<Event> {
        rig.run(vec![Command::Send(text.into())])
    }

    #[test]
    fn neighbouring_reads_run_together_and_report_in_order() {
        let calls = [read_call("r1", "a.txt"), read_call("r2", "b.txt"), read_call("r3", "missing.txt")];
        let mut rig =
            Rig::new("turn-group", Mode::Default, vec![ok(tools_reply(&calls)), ok(text_reply("Read them."))]);
        std::fs::write(rig.dir.join("a.txt"), "alpha\n").unwrap();
        std::fs::write(rig.dir.join("b.txt"), "beta\ngamma\n").unwrap();
        let events = send(&mut rig, "read all");

        assert!(events.iter().any(|e| matches!(e, Event::Activity(a) if a == "running 3 tools at once")));
        let (labels, results) = shown(&events);
        assert_eq!(labels, ["Read(a.txt)", "Read(b.txt)", "Read(missing.txt)"]);
        assert_eq!(results.iter().map(|r| r.0).collect::<Vec<_>>(), [true, true, false]);
        assert_eq!(results[0].1, "Read 1 line");
        assert_eq!(results[1].1, "Read 2 lines");

        let r = results_of(&rig, 2);
        assert_eq!(
            r.iter().map(|x| (x.0.as_str(), x.1)).collect::<Vec<_>>(),
            [("r1", false), ("r2", false), ("r3", true)]
        );
        assert!(r[0].2.contains("alpha"));
        assert!(r[2].2.contains("No such file"), "{}", r[2].2);
        assert_eq!(text_of(&events), "Read them.");
    }

    #[test]
    fn a_call_between_reads_splits_the_groups_and_can_be_refused() {
        let calls = [
            read_call("r1", "a.txt"),
            tool_use("b1", "bash", json!({"command": "echo no"})),
            read_call("r2", "a.txt"),
            read_call("r3", "a.txt"),
        ];
        let mut rig = Rig::new("turn-split", Mode::Default, vec![ok(tools_reply(&calls)), ok(text_reply("ok"))]);
        std::fs::write(rig.dir.join("a.txt"), "alpha\n").unwrap();
        let events = send(&mut rig, "go");
        let activity: Vec<_> =
            events.iter().filter_map(|e| if let Event::Activity(a) = e { Some(a.as_str()) } else { None }).collect();
        assert_eq!(activity, ["running 2 tools at once"], "only the last two reads overlap");
        let (labels, _) = shown(&events);
        assert_eq!(labels, ["Read(a.txt)", "Bash(echo no)", "Read(a.txt)", "Read(a.txt)"]);
        let r = results_of(&rig, 2);
        assert_eq!(r.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), ["r1", "b1", "r2", "r3"]);
        assert!(r[1].1 && r[1].2.starts_with("Not allowed"), "{:?}", r[1]);
    }

    #[test]
    fn tools_that_fail_are_reported_and_the_turn_goes_on() {
        let calls =
            [tool_use("b1", "bash", json!({"command": "echo oops; exit 4"})), tool_use("u1", "frobnicate", json!({}))];
        let mut rig = Rig::new("turn-errors", Mode::Bypass, vec![ok(tools_reply(&calls)), ok(text_reply("I see."))]);
        let events = send(&mut rig, "go");
        assert!(matches!(events.last(), Some(Event::Done)));
        let (_, results) = shown(&events);
        assert!(!results[0].0 && results[0].1.contains("oops") && results[0].1.contains("Exit code 4"));
        assert_eq!(results[1], (false, "unknown tool: frobnicate".to_string()));
        let r = results_of(&rig, 2);
        assert!(r[0].1 && r[1].1);
        // The model's next request carried both results.
        assert!(rig.bodies()[1].contains("unknown tool: frobnicate"));
    }

    #[test]
    fn invalid_tool_input_is_sent_back_for_a_retry() {
        let rig = Rig::new("turn-badjson", Mode::Bypass, vec![]);
        let call = Call { id: json!("x1"), name: "bash".into(), input: json!({"_invalid_json": "EOF while parsing"}) };
        let emit = rig.agent.core.emit.clone();
        let ctx = Ctx { system: "", defs: &rig.agent.defs, tools: &rig.agent.tools, emit: &*emit, main: true };
        let out = rig.agent.core.call_one(&ctx, &call);
        assert_eq!(out["is_error"], true);
        assert_eq!(out["tool_use_id"], "x1");
        assert_eq!(out["content"], "The tool input was not valid JSON (\"EOF while parsing\"). Try again.");
    }

    #[test]
    fn ask_user_reaches_the_user_or_assumes() {
        let ask = tool_use("q1", "ask_user", json!({"question": "Which colour?", "options": ["red", "blue"]}));
        // Nobody there: the model is told to choose.
        let mut rig = Rig::new(
            "turn-ask-none",
            Mode::Default,
            vec![ok(tools_reply(std::slice::from_ref(&ask))), ok(text_reply("ok"))],
        );
        send(&mut rig, "paint");
        assert!(results_of(&rig, 2)[0].2.starts_with("The user isn't available."));
        assert!(!results_of(&rig, 2)[0].1);

        // Someone there.
        let (url, _) =
            crate::agent::tests::fake_http(vec![ok(tools_reply(std::slice::from_ref(&ask))), ok(text_reply("ok"))]);
        let mut cfg = crate::agent::tests::config(Some(url), Mode::Default);
        cfg.interactive = true;
        let mut rig = Rig::with("turn-ask-user", cfg);
        rig.replies.lock().unwrap().push_back("blue".into());
        let events = send(&mut rig, "paint");
        let question = events.iter().find_map(|e| match e {
            Event::Question { question, options, .. } => Some((question.clone(), options.clone())),
            _ => None,
        });
        assert_eq!(question, Some(("Which colour?".to_string(), vec!["red".to_string(), "blue".to_string()])));
        assert_eq!(results_of(&rig, 2)[0].2, "The user answered: blue");
        let (_, results) = shown(&events);
        assert_eq!(results[0], (true, "blue".to_string()), "the summary drops the lead-in");

        // An empty answer is no answer.
        let (url, _) = crate::agent::tests::fake_http(vec![ok(tools_reply(&[ask])), ok(text_reply("ok"))]);
        let mut cfg = crate::agent::tests::config(Some(url), Mode::Default);
        cfg.interactive = true;
        let mut rig = Rig::with("turn-ask-empty", cfg);
        send(&mut rig, "paint");
        let r = results_of(&rig, 2);
        assert!(r[0].1 && r[0].2 == "The user didn't answer.");
    }

    #[test]
    fn cancelling_during_a_tool_ends_the_turn_cleanly() {
        let call = tool_use("b1", "bash", json!({"command": "sleep 5"}));
        let mut rig = Rig::new("turn-cancel", Mode::Bypass, vec![ok(tools_reply(&[call]))]);
        let cancel = rig.agent.core.cancel.clone();
        let stopper = thread::spawn(move || {
            thread::sleep(Duration::from_millis(300));
            cancel.store(true, Ordering::Relaxed);
        });
        let started = std::time::Instant::now();
        let events = send(&mut rig, "slow");
        stopper.join().unwrap();
        assert!(matches!(events.last(), Some(Event::Cancelled)));
        assert!(started.elapsed() < Duration::from_secs(4));
        let r = results_of(&rig, 2);
        assert!(r[0].1 && r[0].2.contains("Cancelled by the user"), "{:?}", r[0]);
    }

    #[test]
    fn a_turn_cancelled_before_the_reply_ends_with_cancelled() {
        let rig = Rig::new("turn-cancel-early", Mode::Bypass, vec![ok(text_reply("never read"))]);
        rig.agent.core.cancel.store(true, Ordering::Relaxed);
        let emit = rig.agent.core.emit.clone();
        let ctx = Ctx { system: "", defs: &rig.agent.defs, tools: &rig.agent.tools, emit: &*emit, main: true };
        let mut messages = vec![Msg::new("user", json!([{ "type": "text", "text": "hi" }]))];
        let end = rig.agent.core.turn(&ctx, &mut messages, &mut |_| {});
        assert!(matches!(end, Event::Cancelled));
        assert_eq!(messages.len(), 1);
    }

    #[test]
    fn calls_after_a_cancel_are_not_run() {
        let rig = Rig::new("turn-cancel-check", Mode::Bypass, vec![]);
        rig.agent.core.cancel.store(true, Ordering::Relaxed);
        let call = Call { id: json!("c"), name: "bash".into(), input: json!({"command": "touch ran.txt"}) };
        let emit = rig.agent.core.emit.clone();
        let ctx = Ctx { system: "", defs: &rig.agent.defs, tools: &rig.agent.tools, emit: &*emit, main: true };
        let out = rig.agent.core.call_one(&ctx, &call);
        assert_eq!(out["content"], "Cancelled by the user.");
        assert!(!rig.dir.join("ran.txt").exists());
    }

    #[test]
    fn replies_that_end_the_turn_badly_say_why() {
        let text = [json!({"type": "text", "text": "partial"})];
        let cases: Vec<(Response, String)> = vec![
            (
                Response { status: 400, body: r#"{"error":{"message":"prompt is too long: 9 tokens"}}"#.into() },
                TOO_LONG.into(),
            ),
            (ok(blocks_reply(&text, "model_context_window_exceeded")), TOO_LONG.into()),
            (ok(blocks_reply(&text, "refusal")), "the model declined this request".into()),
            (ok(blocks_reply(&text, "max_tokens")), "the reply hit the max_tokens limit".into()),
            (ok(blocks_reply(&[], "end_turn")), "empty reply (stop reason: end_turn)".into()),
            (
                Response { status: 400, body: r#"{"error":{"message":"bad things"}}"#.into() },
                "API error 400: bad things".into(),
            ),
        ];
        for (i, (response, expected)) in cases.into_iter().enumerate() {
            let mut rig = Rig::new(&format!("turn-bad-{i}"), Mode::Default, vec![response]);
            let events = send(&mut rig, "hi");
            assert_eq!(errors_of(&events), [expected], "case {i}");
            assert!(matches!(events.last(), Some(Event::Error(_))));
        }
    }

    #[test]
    fn open_tool_calls_are_closed_when_the_reply_stops_short() {
        let calls =
            [tool_use("t1", "bash", json!({"command": "ls"})), tool_use("t2", "bash", json!({"command": "pwd"}))];
        let mut rig = Rig::new("turn-open", Mode::Bypass, vec![ok(blocks_reply(&calls, "max_tokens"))]);
        let events = send(&mut rig, "hi");
        assert_eq!(errors_of(&events), ["the reply hit the max_tokens limit"]);
        let r = results_of(&rig, 2);
        assert_eq!(
            r.iter().map(|x| (x.0.as_str(), x.1, x.2.as_str())).collect::<Vec<_>>(),
            [("t1", true, "Not run."), ("t2", true, "Not run.")]
        );
    }

    #[test]
    fn a_paused_turn_is_continued() {
        let mut rig = Rig::new(
            "turn-pause",
            Mode::Default,
            vec![
                ok(blocks_reply(&[json!({"type": "text", "text": "part one, "})], "pause_turn")),
                ok(text_reply("part two")),
            ],
        );
        let events = send(&mut rig, "long");
        assert_eq!(text_of(&events), "part one, part two");
        assert!(matches!(events.last(), Some(Event::Done)));
        // Two assistant messages in a row: the second request just continues the first.
        assert_eq!(rig.agent.messages.iter().map(|m| m.user).collect::<Vec<_>>(), [true, false, false]);
    }

    #[test]
    fn server_side_tools_and_thinking_are_shown_as_they_stream() {
        let thinking = sse(&[
            json!({"type": "message_start", "message": {"usage": {"input_tokens": 3}}}),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "hmm"}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "message_delta", "delta": {"stop_reason": "pause_turn"}, "usage": {"output_tokens": 1}}),
            json!({"type": "message_stop"}),
        ]);
        let blocks = [
            json!({"type": "server_tool_use", "id": "s1", "name": "web_search", "input": {"query": "rust lifetimes"}}),
            json!({"type": "web_search_tool_result", "tool_use_id": "s1", "content": [{"u": 1}, {"u": 2}]}),
            json!({"type": "text", "text": "Found it."}),
        ];
        let mut rig = Rig::new("turn-server", Mode::Default, vec![ok(thinking), ok(blocks_reply(&blocks, "end_turn"))]);
        let events = send(&mut rig, "search");
        assert!(events.iter().any(|e| matches!(e, Event::Thinking(t) if t == "hmm")));
        assert!(events.iter().any(|e| matches!(e, Event::ToolStart(n) if n == "web_search")));
        let (labels, results) = shown(&events);
        assert_eq!(labels, ["Search(rust lifetimes)"]);
        assert_eq!(results, [(true, "2 results".to_string())]);
        assert_eq!(text_of(&events), "Found it.");
        // Server-side blocks are kept exactly, for the next request.
        let content = rig.agent.messages.last().unwrap().content();
        assert_eq!(content[0]["input"]["query"], "rust lifetimes");
        assert_eq!(content[1]["type"], "web_search_tool_result");
    }

    #[test]
    fn usage_is_reported_with_its_cost() {
        let mut rig = Rig::new("turn-usage", Mode::Default, vec![ok(text_reply("hi"))]);
        let events = send(&mut rig, "hi");
        let usage = events.iter().find_map(|e| match e {
            Event::Usage { usage, cost, main } => Some((usage.input_tokens, usage.output_tokens, *cost, *main)),
            _ => None,
        });
        let (input, output, cost, main) = usage.expect("usage event");
        assert_eq!((input, output, main), (10, 5, true));
        let expected = (10.0 * 4.0 + 5.0 * 20.0) / 1e6;
        assert!((cost.unwrap() - expected).abs() < 1e-12, "{cost:?}");
    }

    #[test]
    fn finished_jobs_ride_along_with_tool_results() {
        // The job finishes while the second command sleeps, so it is reported
        // with this reply's results rather than with the next message.
        let calls = [
            tool_use("b1", "bash", json!({"command": "true", "background": true})),
            tool_use("b2", "bash", json!({"command": "sleep 0.6"})),
        ];
        let mut rig = Rig::new("turn-jobs", Mode::Bypass, vec![ok(tools_reply(&calls)), ok(text_reply("ok"))]);
        send(&mut rig, "go");
        let last = rig.agent.messages[2].content();
        let note = last.last().unwrap()["text"].as_str().unwrap();
        assert_eq!(
            note,
            "<system-reminder>Background job 1 (`true`) finished: exit 0. Use the jobs tool to read their output.</system-reminder>"
        );
        assert!(job_notes(&rig.agent.tools).is_none(), "each finished job is reported once");
    }

    #[test]
    fn unanswered_calls_get_a_result_each() {
        let user = Msg::new("user", json!([{ "type": "text", "text": "hi" }]));
        assert!(unanswered(&user, "x").is_none());
        let plain = Msg::new("assistant", json!([{ "type": "text", "text": "hi" }]));
        assert!(unanswered(&plain, "x").is_none());
        let calls = Msg::new("assistant", json!([tool_use("a", "bash", json!({})), tool_use("b", "read", json!({}))]));
        let results = unanswered(&calls, "Not run.").unwrap();
        assert!(results.user);
        let c = results.content();
        assert_eq!((c[0]["tool_use_id"].as_str(), c[1]["tool_use_id"].as_str()), (Some("a"), Some("b")));
        assert!(c.iter().all(|b| b["is_error"] == true && b["content"] == "Not run."));
    }
}
