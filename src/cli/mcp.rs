// `claudejr mcp`: claude-jr as an MCP server, so that another agent (Claude
// Code, Cursor, Codex, anything that speaks MCP) can hand work to claude-jr
// workers and collect it. The other agent stays the one the user talks to;
// each worker is a background task (see work/task.rs) of about half a megabyte,
// in its own git worktree and branch.
//
// It speaks JSON-RPC 2.0 over standard input and output, a message a line.
//
// What the workers may do is decided by whoever sets the server up, on its
// command line, and not by the agent calling it:
//
//   claudejr mcp                  workers can read; edits and commands need allow rules
//   claudejr mcp -y               workers can edit and run commands
//   claudejr mcp -y --sandbox     the same, confined to their worktree (Linux)
//   claudejr mcp -m MODEL         the model workers use unless a call names one
//   claudejr mcp --budget USD     all workers of this server together stop at this
//
// Tasks are started from the folder the server runs in.

use crate::work::task::{self, Start};
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const USAGE: &str =
    "usage: claudejr mcp [-y] [--sandbox] [-m MODEL] [-e EFFORT] [--max-cost USD] [--budget USD] [--no-worktree]

Serves claude-jr's background tasks to another agent over MCP (stdio).
The options decide what every worker it starts may do and spend.
Set it up for an agent with: claudejr install claude-code";

/// The newest protocol version this server knows.
const PROTOCOL: &str = "2025-06-18";
/// The longest a wait_tasks call blocks; a calling agent's own time limit for
/// a tool call is often not much more.
const MAX_WAIT: f64 = 300.0;
/// A diff or a log longer than this is cut, with a note.
const MAX_TEXT: usize = 40_000;

const INSTRUCTIONS: &str = "claude-jr runs coding tasks in the background, each as a separate small agent in its own \
git worktree and branch, so several can work on one repository at once without touching each other's files or yours. \
Use it to do independent pieces of work in parallel: start_task for each piece (it returns at once), carry on with \
your own work, then wait_tasks and read each task_result. A task starts from the last commit, so commit anything it \
needs first, and give it everything it needs in the prompt: it can't see this conversation. Its work comes back as a \
branch: look at task_diff, then merge_task to take it or drop_task to discard it.";

/// What the server was started with: how every task is started.
pub struct Options {
    start: Start,
}

pub fn parse(args: &[String]) -> Result<Options, String> {
    let mut start = Start { worktree: true, ..Start::default() };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().ok_or_else(|| format!("{a} needs a value"));
        match a.as_str() {
            "-y" | "--yes" | "--dangerously-skip-permissions" | "--sandbox" => start.pass.push(a.clone()),
            "-m" | "--model" | "-e" | "--effort" | "--max-cost" => {
                let v = value()?;
                start.pass.extend([a.clone(), v]);
            }
            "--no-worktree" => start.worktree = false,
            "--budget" => {
                let v = value()?;
                let usd = v.trim_start_matches('$').parse::<f64>().ok().filter(|usd| *usd > 0.0);
                start.budget = Some(usd.ok_or_else(|| format!("--budget needs an amount in dollars, got {v}"))?);
                start.group = Some(format!("mcp-{}", crate::work::worktree::new_id()));
            }
            "-h" | "--help" => return Err(USAGE.into()),
            other => return Err(format!("unknown option for mcp: {other}\n\n{USAGE}")),
        }
    }
    Ok(Options { start })
}

/// Serves until standard input closes.
pub fn run(options: Options) -> Result<(), String> {
    if crate::store::config::api_key().unwrap_or_default().is_empty()
        && !options.start.pass.iter().any(|a| a == "-m" || a == "--model")
    {
        // Said on stderr, which MCP clients show in their logs; the server
        // still starts, and a call says the same.
        eprintln!(
            "claudejr mcp: no Anthropic API key is set; tasks will fail until one is (ANTHROPIC_API_KEY or /login)"
        );
    }
    let out = Arc::new(Mutex::new(std::io::stdout()));
    let server = Arc::new(Server { start: Mutex::new(options.start) });
    let mut calls = Vec::new();
    crate::sys::mem::trim();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let (server, out) = (server.clone(), out.clone());
        // Each on its own thread: a long wait must not hold up a status check.
        calls.push(std::thread::spawn(move || {
            if let Some(reply) = server.handle(&line) {
                let mut out = out.lock().unwrap();
                let _ = writeln!(out, "{reply}").and_then(|()| out.flush());
            }
        }));
        calls.retain(|c| !c.is_finished());
        // Between calls the server only waits: what it loaded to start goes back.
        crate::sys::mem::trim();
    }
    Ok(())
}

struct Server {
    /// How tasks are started. (The budget is taken by the first, which sets
    /// the group up; the rest join it.)
    start: Mutex<Start>,
}

impl Server {
    /// The reply to one message, or None if it wants none (a notification).
    fn handle(&self, line: &str) -> Option<Value> {
        let Ok(msg) = serde_json::from_str::<Value>(line) else {
            return Some(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "not JSON" } }));
        };
        let id = msg.get("id").filter(|id| !id.is_null())?.clone();
        let reply = |result: Value| Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }));
        match msg["method"].as_str().unwrap_or("") {
            "initialize" => {
                // The client's version if it is one we know by its shape; ours otherwise.
                let asked = msg["params"]["protocolVersion"].as_str().filter(|v| v.len() == 10 && *v <= PROTOCOL);
                reply(json!({
                    "protocolVersion": asked.unwrap_or(PROTOCOL),
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "claude-jr", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": INSTRUCTIONS,
                }))
            }
            "ping" => reply(json!({})),
            "tools/list" => reply(json!({ "tools": tools() })),
            "tools/call" => {
                let name = msg["params"]["name"].as_str().unwrap_or("");
                let outcome = self.call(name, &msg["params"]["arguments"]);
                let (text, failed) = match outcome {
                    Ok(text) => (text, false),
                    Err(why) => (why, true),
                };
                reply(json!({ "content": [{ "type": "text", "text": text }], "isError": failed }))
            }
            other => Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("no such method: {other}") },
            })),
        }
    }

    fn call(&self, name: &str, args: &Value) -> Result<String, String> {
        let id = || args["id"].as_str().ok_or("give the task's id");
        let ids = || -> Vec<String> {
            args["ids"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(String::from)).collect()
        };
        match name {
            "start_task" => {
                let prompt = args["prompt"].as_str().ok_or("give the task as prompt")?;
                let start = {
                    let mut shared = self.start.lock().unwrap();
                    let mut pass = shared.pass.clone();
                    if let Some(model) = args["model"].as_str().filter(|m| !m.trim().is_empty()) {
                        // Later on the worker's command line, so it wins.
                        pass.extend(["-m".to_string(), model.to_string()]);
                    }
                    Start {
                        prompt: prompt.to_string(),
                        pass,
                        worktree: shared.worktree,
                        group: shared.group.clone(),
                        budget: shared.budget.take(),
                    }
                };
                let id = task::start(&start)?;
                Ok(json!({ "id": id, "status": "running" }).to_string())
            }
            "list_tasks" => Ok(json!(task::list(false).iter().map(brief).collect::<Vec<_>>()).to_string()),
            "wait_tasks" => {
                let seconds = args["timeout_seconds"].as_f64().unwrap_or(120.0).clamp(0.0, MAX_WAIT);
                let states = task::wait(&ids(), Some(Duration::from_secs_f64(seconds)))?;
                Ok(json!(states.iter().map(brief).collect::<Vec<_>>()).to_string())
            }
            "task_result" => {
                let state = task::result(id()?)?;
                Ok(format!("{:#}", full(&state)))
            }
            "task_log" => task::steps(id()?, args["last"].as_u64().unwrap_or(30) as usize).map(|t| cut(&t)),
            "task_diff" => task::diff(id()?).map(|t| cut(&t)),
            "cancel_task" => task::cancel(id()?),
            "merge_task" => task::merge(id()?),
            "drop_task" => task::drop(id()?),
            other => Err(format!("no such tool: {other}")),
        }
    }
}

fn cut(text: &str) -> String {
    if text.len() <= MAX_TEXT {
        return text.to_string();
    }
    let mut end = MAX_TEXT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… ({} more bytes not shown)", &text[..end], text.len() - end)
}

/// A task in a line or two: enough to decide what to look at next.
fn brief(state: &Value) -> Value {
    let result = &state["result"];
    let mut out = json!({
        "id": state["id"],
        "status": state["status"],
        "task": state["prompt"].as_str().unwrap_or("").lines().next().unwrap_or("").chars().take(80).collect::<String>(),
    });
    for (key, value) in [
        ("branch", &result["worktree"]["branch"]),
        ("files_changed", &json!(result["files"].as_array().map(Vec::len))),
        ("cost_usd", &result["cost_usd"]),
        ("error", &result["error"]),
        ("steps_so_far", &state["steps"]),
        ("last_step", &state["last_step"]),
        ("error", &state["error"]),
    ] {
        if !value.is_null() {
            out[key] = value.clone();
        }
    }
    out
}

/// A task's whole outcome, without what only the machinery needs.
fn full(state: &Value) -> Value {
    let mut out = brief(state);
    let result = &state["result"];
    for (key, value) in [("answer", &result["result"]), ("files", &result["files"]), ("worktree", &result["worktree"])]
    {
        if !value.is_null() {
            out[key] = value.clone();
        }
    }
    out
}

fn tools() -> Value {
    let id = json!({ "type": "object", "properties": { "id": { "type": "string", "description": "The task's id" } }, "required": ["id"] });
    let tool = |name: &str, description: &str, schema: Value| json!({ "name": name, "description": description, "inputSchema": schema });
    json!([
        tool(
            "start_task",
            "Start a coding task in the background and return its id at once. A separate claude-jr agent does it, in its \
             own git worktree and branch (starting from the last commit), so it can't disturb your files or other tasks. \
             It can't see this conversation: put everything it needs in the prompt (the goal, the files, how to check the \
             result). Start several for work that can be done side by side.",
            json!({
                "type": "object",
                "properties": {
                    "prompt": { "type": "string", "description": "The whole task, self-contained" },
                    "model": { "type": "string", "description": "Optional: a model for this task, e.g. a cheaper one for simple work" }
                },
                "required": ["prompt"]
            }),
        ),
        tool(
            "list_tasks",
            "The tasks started from this folder: id, status (running, done, failed, cancelled, died), branch, cost.",
            json!({ "type": "object", "properties": {} })
        ),
        tool(
            "wait_tasks",
            "Wait until tasks have finished and return where each stands. With no ids, waits for every running task. \
             Returns after timeout_seconds even if some are still running (default 120, at most 300): call it again.",
            json!({
                "type": "object",
                "properties": {
                    "ids": { "type": "array", "items": { "type": "string" } },
                    "timeout_seconds": { "type": "number" }
                }
            }),
        ),
        tool(
            "task_result",
            "A task's outcome: its final answer, the files it changed, its branch and its cost. For a running task, how far it is.",
            id.clone()
        ),
        tool(
            "task_log",
            "The steps a task took: each tool call, whether it was allowed, and how it went. The last 30 unless `last` says otherwise.",
            json!({ "type": "object", "properties": { "id": { "type": "string" }, "last": { "type": "integer" } }, "required": ["id"] }),
        ),
        tool("task_diff", "What a finished task changed, as a diff against the commit it started from.", id.clone()),
        tool(
            "merge_task",
            "Merge a finished task's branch into the current branch and remove its worktree. A merge that conflicts is undone and reported.",
            id.clone()
        ),
        tool("drop_task", "Discard a task: stop it if it is running and delete its worktree and branch.", id.clone()),
        tool("cancel_task", "Stop a running task. What it had done so far stays on its branch.", id),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server() -> Server {
        Server { start: Mutex::new(Start { worktree: true, ..Start::default() }) }
    }

    fn ask(server: &Server, msg: Value) -> Value {
        server.handle(&msg.to_string()).expect("a reply")
    }

    #[test]
    fn the_handshake_and_the_tool_list() {
        let s = server();
        let init = ask(
            &s,
            json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-03-26" } }),
        );
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26", "an older client is answered in its own version");
        assert_eq!(init["result"]["serverInfo"]["name"], "claude-jr");
        assert!(init["result"]["instructions"].as_str().unwrap().contains("start_task"));
        let newer = ask(
            &s,
            json!({ "jsonrpc": "2.0", "id": 2, "method": "initialize", "params": { "protocolVersion": "2099-01-01" } }),
        );
        assert_eq!(newer["result"]["protocolVersion"], PROTOCOL);
        // A notification gets no reply; a ping gets an empty one.
        assert!(s.handle(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).is_none());
        assert_eq!(
            ask(&s, json!({ "jsonrpc": "2.0", "id": "p", "method": "ping" })),
            json!({ "jsonrpc": "2.0", "id": "p", "result": {} })
        );

        let list = ask(&s, json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list" }));
        let tools = list["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            [
                "start_task",
                "list_tasks",
                "wait_tasks",
                "task_result",
                "task_log",
                "task_diff",
                "merge_task",
                "drop_task",
                "cancel_task"
            ]
        );
        for t in tools {
            assert_eq!(t["inputSchema"]["type"], "object", "{t}");
            assert!(t["description"].as_str().unwrap().len() > 30);
        }
    }

    #[test]
    fn mistakes_are_answered_not_fatal() {
        let s = server();
        assert_eq!(s.handle("this is not json").unwrap()["error"]["code"], -32700);
        let unknown = ask(&s, json!({ "jsonrpc": "2.0", "id": 9, "method": "resources/list" }));
        assert_eq!((unknown["error"]["code"].as_i64(), unknown["id"].as_i64()), (Some(-32601), Some(9)));
        // A tool that fails says so in its result, as MCP has it, not as a protocol error.
        for (name, args, want) in [
            ("no_such_tool", json!({}), "no such tool"),
            ("start_task", json!({}), "give the task as prompt"),
            ("task_result", json!({}), "give the task's id"),
            ("task_result", json!({ "id": "does-not-exist" }), "no task"),
            ("merge_task", json!({ "id": 7 }), "give the task's id"),
        ] {
            let r = ask(
                &s,
                json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": name, "arguments": args } }),
            );
            assert_eq!(r["result"]["isError"], true, "{name}");
            assert!(r["result"]["content"][0]["text"].as_str().unwrap().contains(want), "{name}: {r}");
        }
    }

    #[test]
    fn options_are_the_server_owners_and_summaries_are_short() {
        let args: Vec<String> =
            ["-y", "--sandbox", "-m", "claude-haiku-5-5", "--budget", "2.50"].iter().map(|s| s.to_string()).collect();
        let options = parse(&args).unwrap();
        assert_eq!(options.start.pass, ["-y", "--sandbox", "-m", "claude-haiku-5-5"]);
        assert_eq!(options.start.budget, Some(2.5));
        assert!(options.start.group.as_deref().unwrap().starts_with("mcp-") && options.start.worktree);
        assert!(parse(&["--wat".to_string()]).is_err_and(|e| e.contains("unknown option for mcp")));
        assert!(parse(&["--budget".to_string(), "x".to_string()]).is_err());

        let state = json!({
            "id": "abc", "status": "done", "prompt": "fix the thing\nwith details", "pid": 1, "cwd": "/x",
            "result": { "result": "Fixed.", "files": [{ "path": "a" }, { "path": "b" }], "cost_usd": 0.02, "error": null,
                        "worktree": { "branch": "claudejr/abc", "path": "/w" }, "input_tokens": 5 },
        });
        assert_eq!(
            brief(&state),
            json!({ "id": "abc", "status": "done", "task": "fix the thing", "branch": "claudejr/abc", "files_changed": 2, "cost_usd": 0.02 })
        );
        let whole = full(&state);
        assert_eq!((whole["answer"].as_str(), whole["files"].as_array().map(Vec::len)), (Some("Fixed."), Some(2)));
        assert!(whole.get("pid").is_none() && whole.get("input_tokens").is_none());
        assert!(cut(&"é".repeat(30_000)).ends_with("more bytes not shown)"));
        assert_eq!(cut("short"), "short");
    }
}
