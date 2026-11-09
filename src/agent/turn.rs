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
/// arrive as finished blocks in the stream; show them like local ones.
fn show_server_tool(ctx: &Ctx, b: &Value) {
    {
        let kind = b["type"].as_str().unwrap_or("");
        let label = match kind {
            "server_tool_use" if b["name"] == "web_search" => {
                format!("Search({})", b["input"]["query"].as_str().unwrap_or(""))
            }
            "server_tool_use" if b["name"] == "web_fetch" => {
                format!("Fetch({})", b["input"]["url"].as_str().unwrap_or(""))
            }
            "mcp_tool_use" => {
                format!("{}: {} (remote)", b["server_name"].as_str().unwrap_or("mcp"), b["name"].as_str().unwrap_or(""))
            }
            _ => String::new(),
        };
        if !label.is_empty() {
            return (ctx.emit)(Event::Tool { label, diff: None });
        }
        let out = match kind {
            // A list of results on success, an object with error_code on failure.
            "web_search_tool_result" => match &b["content"] {
                Value::Array(results) => Ok(format!("{} results", results.len())),
                e => Err(format!("search failed: {}", e["error_code"].as_str().unwrap_or("error"))),
            },
            "web_fetch_tool_result" => match b["content"]["type"].as_str() {
                Some("web_fetch_result") => {
                    Ok(format!("Fetched {}", b["content"]["url"].as_str().unwrap_or("the page")))
                }
                _ => Err(format!("fetch failed: {}", b["content"]["error_code"].as_str().unwrap_or("error"))),
            },
            "mcp_tool_result" => {
                let text: Vec<&str> =
                    b["content"].as_array().into_iter().flatten().filter_map(|p| p["text"].as_str()).collect();
                if b["is_error"] == true { Err(text.join("\n")) } else { Ok(text.join("\n")) }
            }
            _ => return,
        };
        (ctx.emit)(Event::ToolResult { ok: out.is_ok(), summary: tools::summary("server", &out) });
    }
}
