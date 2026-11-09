// MCP servers.
//
// Local servers (a command to run) speak JSON-RPC over stdin/stdout. We start
// them, list their tools once, and expose each as `mcp__<server>__<tool>`.
// The server runs as its own process, so its memory is its own; ours is a
// reader thread and a little bookkeeping per server.
//
// Remote servers (a URL) aren't contacted by us at all: they're passed to the
// API's MCP connector and Anthropic calls them. That costs nothing locally,
// but the URL must be reachable from the internet.
//
// Config, in .mcp.json (Claude Code's format) or under "mcp_servers" in
// claude-jr's config:
//   { "mcpServers": {
//       "files":  { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "."] },
//       "linear": { "url": "https://mcp.linear.app/mcp", "headers": { "Authorization": "Bearer ${LINEAR_TOKEN}" } }
//   } }

use super::{Output, def};
use serde::Deserialize;
use serde_json::value::RawValue;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const PROTOCOL: &str = "2025-06-18";
const START_TIMEOUT: Duration = Duration::from_secs(20);
const CALL_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_DESCRIPTION: usize = 1024;
const MAX_OUTPUT: usize = 50 * 1024;

#[derive(Deserialize, Clone, Default)]
pub struct ServerConfig {
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    url: Option<String>,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    authorization_token: Option<String>,
}

/// Every configured server: `.mcp.json` first, then claude-jr's config.
pub fn configured(extra: &BTreeMap<String, ServerConfig>) -> BTreeMap<String, ServerConfig> {
    #[derive(Deserialize)]
    struct McpJson {
        #[serde(rename = "mcpServers", default)]
        servers: BTreeMap<String, ServerConfig>,
    }
    let mut all = std::fs::read_to_string(".mcp.json")
        .ok()
        .and_then(|t| serde_json::from_str::<McpJson>(&t).ok())
        .map(|m| m.servers)
        .unwrap_or_default();
    all.extend(extra.iter().map(|(k, v)| (k.clone(), v.clone())));
    all
}

#[derive(Default)]
pub struct Servers {
    local: Vec<Local>,
    remote: Vec<Remote>,
}

struct Local {
    name: String,
    tools: Vec<ToolInfo>,
    conn: Arc<Conn>,
}

struct Remote {
    name: String,
    url: String,
    token: Option<String>,
}

struct ToolInfo {
    /// The name the model sees: mcp__server__tool.
    full: String,
    /// The name the server knows.
    name: String,
    description: String,
    schema: Value,
}

impl Servers {
    /// Starts every local server in parallel. Returns the servers that came
    /// up and a message for each one that didn't.
    pub fn start(configs: BTreeMap<String, ServerConfig>) -> (Servers, Vec<String>) {
        let mut servers = Servers::default();
        let mut errors = Vec::new();
        let mut pending = Vec::new();
        for (name, cfg) in configs {
            if let Some(url) = &cfg.url {
                let token = cfg.authorization_token.as_deref().map(expand).or_else(|| {
                    let auth = cfg.headers.get("Authorization").or(cfg.headers.get("authorization"))?;
                    Some(expand(auth).trim_start_matches("Bearer ").to_string())
                });
                servers.remote.push(Remote { name, url: expand(url), token });
            } else if cfg.command.is_some() {
                pending.push((name.clone(), thread::spawn(move || Local::start(name, &cfg))));
            } else {
                errors.push(format!("MCP server {name}: needs a command or a url"));
            }
        }
        for (name, handle) in pending {
            match handle.join() {
                Ok(Ok(local)) => servers.local.push(local),
                Ok(Err(e)) => errors.push(format!("MCP server {name}: {e}")),
                Err(_) => errors.push(format!("MCP server {name}: crashed while starting")),
            }
        }
        (servers, errors)
    }

    /// One line per server, for the UI.
    pub fn describe(&self) -> Vec<String> {
        let local = self.local.iter().map(|l| format!("{} (local, {} tools)", l.name, l.tools.len()));
        let remote = self.remote.iter().map(|r| format!("{} (remote, via the API)", r.name));
        local.chain(remote).collect()
    }

    pub fn definitions(&self) -> Vec<Box<RawValue>> {
        let local = self
            .local
            .iter()
            .flat_map(|l| &l.tools)
            .map(|t| def(json!({ "name": t.full, "description": t.description, "input_schema": t.schema })));
        let remote = self.remote.iter().map(|r| def(json!({ "type": "mcp_toolset", "mcp_server_name": r.name })));
        local.chain(remote).collect()
    }

    /// The `mcp_servers` request field for remote servers, if there are any.
    pub fn connector(&self) -> Option<Box<RawValue>> {
        if self.remote.is_empty() {
            return None;
        }
        let list: Vec<Value> = self
            .remote
            .iter()
            .map(|r| {
                let mut v = json!({ "type": "url", "url": r.url, "name": r.name });
                if let Some(t) = &r.token {
                    v["authorization_token"] = json!(t);
                }
                v
            })
            .collect();
        serde_json::value::to_raw_value(&list).ok()
    }

    pub fn call(&self, full: &str, input: &Value, cancel: &AtomicBool) -> Output {
        let (local, tool) = self
            .local
            .iter()
            .find_map(|l| l.tools.iter().find(|t| t.full == full).map(|t| (l, t)))
            .ok_or_else(|| format!("unknown MCP tool {full}"))?;
        let params = json!({ "name": tool.name, "arguments": input });
        let result = local.conn.request("tools/call", params, CALL_TIMEOUT, Some(cancel))?;
        let text = result_text(&result);
        if result["isError"] == true { Err(text) } else { Ok(text) }
    }

    pub fn pids(&self) -> Vec<u32> {
        self.local.iter().map(|l| l.conn.pid).collect()
    }

    pub fn stop(&self) {
        for l in &self.local {
            l.conn.stop();
        }
    }
}

pub fn is_mcp(name: &str) -> bool {
    name.starts_with("mcp__")
}

/// `mcp__github__create_issue` -> `github: create_issue`.
pub fn display_name(full: &str) -> String {
    let rest = full.trim_start_matches("mcp__");
    match rest.split_once("__") {
        Some((server, tool)) => format!("{server}: {tool}"),
        None => rest.to_string(),
    }
}

/// A short look at the arguments, for labels and permission rules.
pub fn summarize_args(input: &Value) -> String {
    let Some(obj) = input.as_object() else { return String::new() };
    obj.iter()
        .map(|(k, v)| match v {
            Value::String(s) => format!("{k}: {s}"),
            v => format!("{k}: {v}"),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

impl Local {
    fn start(name: String, cfg: &ServerConfig) -> Result<Local, String> {
        let command = expand(cfg.command.as_deref().unwrap_or_default());
        let mut cmd = Command::new(&command);
        cmd.args(cfg.args.iter().map(|a| expand(a)))
            .envs(cfg.env.iter().map(|(k, v)| (k, expand(v))))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let conn = Conn::spawn(cmd).map_err(|e| format!("can't run {command}: {e}"))?;

        let init = json!({
            "protocolVersion": PROTOCOL,
            "capabilities": {},
            "clientInfo": { "name": "claude-jr", "version": env!("CARGO_PKG_VERSION") }
        });
        conn.request("initialize", init, START_TIMEOUT, None)?;
        conn.notify("notifications/initialized");

        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let params = cursor.as_ref().map_or(json!({}), |c| json!({ "cursor": c }));
            let page = conn.request("tools/list", params, START_TIMEOUT, None)?;
            for t in page["tools"].as_array().into_iter().flatten() {
                let Some(tool_name) = t["name"].as_str() else { continue };
                let mut description = t["description"].as_str().unwrap_or("").to_string();
                if description.len() > MAX_DESCRIPTION {
                    let cut = (0..=MAX_DESCRIPTION).rev().find(|&i| description.is_char_boundary(i)).unwrap_or(0);
                    description.truncate(cut);
                }
                let mut schema = t["inputSchema"].clone();
                if !schema.is_object() {
                    schema = json!({ "type": "object", "properties": {} });
                }
                tools.push(ToolInfo {
                    full: tool_id(&name, tool_name),
                    name: tool_name.to_string(),
                    description,
                    schema,
                });
            }
            cursor = page["nextCursor"].as_str().map(String::from);
            if cursor.is_none() {
                break;
            }
        }
        Ok(Local { name, tools, conn: Arc::new(conn) })
    }
}

/// API tool names allow letters, digits, _ and -, up to 64 characters.
fn tool_id(server: &str, tool: &str) -> String {
    let clean = |s: &str| -> String {
        s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
    };
    let mut id = format!("mcp__{}__{}", clean(server), clean(tool));
    id.truncate(64);
    id
}

/// Joins the text parts of a tools/call result.
fn result_text(result: &Value) -> String {
    let mut out = String::new();
    for part in result["content"].as_array().into_iter().flatten() {
        if !out.is_empty() {
            out.push('\n');
        }
        match part["type"].as_str() {
            Some("text") => out.push_str(part["text"].as_str().unwrap_or("")),
            Some(other) => out.push_str(&format!("[{other} content]")),
            None => {}
        }
    }
    if out.len() > MAX_OUTPUT {
        let cut = (0..=MAX_OUTPUT).rev().find(|&i| out.is_char_boundary(i)).unwrap_or(0);
        out.truncate(cut);
        out.push_str("\n... [truncated]");
    }
    if out.is_empty() { "(no output)".into() } else { out }
}

/// Replaces ${VAR} with the environment variable's value.
fn expand(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('}') else { break };
        let var = &rest[start + 2..start + end];
        out.push_str(&std::env::var(var).unwrap_or_default());
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    out
}

// ---- JSON-RPC over stdio ------------------------------------------------

type Reply = Result<Value, String>;

/// A running server. A reader thread routes each reply to whoever is waiting
/// for its id, so calls from several threads can be in flight at once.
struct Conn {
    stdin: Mutex<ChildStdin>,
    pending: Arc<Mutex<HashMap<u64, Sender<Reply>>>>,
    next: AtomicU64,
    child: Mutex<Child>,
    pid: u32,
}

impl Conn {
    fn spawn(mut cmd: Command) -> std::io::Result<Conn> {
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");
        let pending: Arc<Mutex<HashMap<u64, Sender<Reply>>>> = Arc::default();
        let routes = pending.clone();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
                // Requests from the server (like ping) aren't supported; the
                // server will time them out. Notifications are ignored.
                let Some(id) = msg["id"].as_u64().filter(|_| msg.get("method").is_none()) else { continue };
                let reply = match msg.get("error") {
                    Some(e) => Err(e["message"].as_str().unwrap_or("error").to_string()),
                    None => Ok(msg["result"].clone()),
                };
                if let Some(tx) = routes.lock().unwrap().remove(&id) {
                    let _ = tx.send(reply);
                }
            }
            // The server exited: fail everything still waiting.
            for (_, tx) in routes.lock().unwrap().drain() {
                let _ = tx.send(Err("the server exited".into()));
            }
        });
        let pid = child.id();
        Ok(Conn { stdin: Mutex::new(stdin), pending, next: AtomicU64::new(1), child: Mutex::new(child), pid })
    }

    fn send(&self, msg: &Value) -> Result<(), String> {
        let mut line = serde_json::to_vec(msg).map_err(|e| e.to_string())?;
        line.push(b'\n');
        let mut stdin = self.stdin.lock().unwrap();
        stdin.write_all(&line).and_then(|_| stdin.flush()).map_err(|e| format!("server stopped reading: {e}"))
    }

    fn notify(&self, method: &str) {
        let _ = self.send(&json!({ "jsonrpc": "2.0", "method": method }));
    }

    fn request(&self, method: &str, params: Value, timeout: Duration, cancel: Option<&AtomicBool>) -> Reply {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(id, tx);
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;

        let start = Instant::now();
        loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(reply) => return reply,
                Err(RecvTimeoutError::Disconnected) => return Err("the server exited".into()),
                Err(RecvTimeoutError::Timeout) => {}
            }
            let cancelled = cancel.is_some_and(|c| c.load(Ordering::Relaxed));
            if cancelled || start.elapsed() > timeout {
                self.pending.lock().unwrap().remove(&id);
                let note =
                    json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": id } });
                let _ = self.send(&note);
                return Err(if cancelled { "Cancelled by the user".into() } else { format!("{method} timed out") });
            }
        }
    }
}

impl Conn {
    fn stop(&self) {
        let mut child = self.child.lock().unwrap();
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny MCP server in bash: answers initialize, tools/list and one tool.
    const SERVER: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"initialize"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-06-18","capabilities":{}}}\n' "$id" ;;
    *'"tools/list"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object"}}]}}\n' "$id" ;;
    *'"tools/call"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"pong"}]}}\n' "$id" ;;
  esac
done"#;

    #[test]
    fn local_server_lists_and_calls_tools() {
        let mut configs = BTreeMap::new();
        configs.insert(
            "demo".to_string(),
            ServerConfig { command: Some("bash".into()), args: vec!["-c".into(), SERVER.into()], ..Default::default() },
        );
        configs.insert("bad".to_string(), ServerConfig::default());
        let (servers, errors) = Servers::start(configs);
        assert_eq!(errors, ["MCP server bad: needs a command or a url"]);
        assert_eq!(servers.describe(), ["demo (local, 1 tools)"]);
        let out = servers.call("mcp__demo__echo", &json!({"text": "ping"}), &AtomicBool::new(false)).unwrap();
        assert_eq!(out, "pong");
    }

    #[test]
    fn names_and_expansion() {
        assert_eq!(tool_id("my server", "do.thing"), "mcp__my_server__do_thing");
        assert_eq!(display_name("mcp__github__create_issue"), "github: create_issue");
        unsafe { std::env::set_var("CJR_TEST_TOKEN", "abc") };
        assert_eq!(expand("Bearer ${CJR_TEST_TOKEN}!"), "Bearer abc!");
    }
}
