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
    pub fn turn(&self, ctx: &Ctx, messages: &mut Vec<Msg>) -> Event {
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
    }
}
