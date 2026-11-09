// Runs the real binary against the fake API in bench/mock.py: command-line
// handling, -p mode, config files, trust, sessions, MCP and the sandbox.
//
// Every test gets its own HOME, XDG dirs, project folder and mock server, and
// the child never sees the caller's ANTHROPIC_API_KEY.

use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_claudejr");
const TIMEOUT: Duration = Duration::from_secs(120);

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct Mock(Child);

impl Drop for Mock {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Out {
    code: i32,
    out: String,
    err: String,
}

/// One isolated world: dirs, a mock API and the knobs to run the binary in it.
struct Env {
    root: PathBuf,
    home: PathBuf,
    config: PathBuf,
    data: PathBuf,
    proj: PathBuf,
    port: u16,
    log: PathBuf,
    _mock: Mock,
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

impl Env {
    fn new() -> Env {
        Env::with(&[])
    }

    fn with(mock_env: &[(&str, &str)]) -> Env {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!("cjr-cli-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (home, config, data, proj) = (root.join("home"), root.join("config"), root.join("data"), root.join("proj"));
        for d in [&home, &config, &data, &proj] {
            std::fs::create_dir_all(d).unwrap();
        }
        let proj = proj.canonicalize().unwrap();
        let log = root.join("requests.jsonl");
        let port = free_port();
        let mut cmd = Command::new("python3");
        cmd.args(["-I", &format!("{}/bench/mock.py", env!("CARGO_MANIFEST_DIR")), &port.to_string()])
            .env("MOCK_LOG", &log)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for (k, v) in mock_env {
            cmd.env(k, v);
        }
        let mock = Mock(cmd.spawn().expect("python3 is needed for the fake API"));
        let start = Instant::now();
        while TcpStream::connect(("127.0.0.1", port)).is_err() {
            assert!(start.elapsed() < Duration::from_secs(20), "the fake API did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
        Env { root, home, config, data, proj, port, log, _mock: mock }
    }

    fn config_dir(&self) -> PathBuf {
        self.config.join("claude-jr")
    }

    fn write_config(&self, json: &str) {
        std::fs::create_dir_all(self.config_dir()).unwrap();
        std::fs::write(self.config_dir().join("config.json"), json).unwrap();
    }

    fn trust_project(&self) {
        std::fs::create_dir_all(self.config_dir()).unwrap();
        std::fs::write(self.config_dir().join("trusted"), format!("{}\n", self.proj.display())).unwrap();
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(BIN);
        cmd.args(args)
            .current_dir(&self.proj)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.config)
            .env("XDG_DATA_HOME", &self.data)
            .env("ANTHROPIC_API_KEY", "mock")
            .env("ANTHROPIC_BASE_URL", format!("http://127.0.0.1:{}", self.port));
        // Coverage runs (bench/coverage.sh) collect the binary's profile through this.
        if let Some(p) = std::env::var_os("LLVM_PROFILE_FILE") {
            cmd.env("LLVM_PROFILE_FILE", p);
        }
        cmd
    }

    fn run(&self, args: &[&str]) -> Out {
        exec(self.command(args))
    }

    /// Every request body the mock saw, in order.
    fn requests(&self) -> Vec<serde_json::Value> {
        self.requests_text().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    fn requests_text(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

fn exec(mut cmd: Command) -> Out {
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let mut so = child.stdout.take().unwrap();
    let mut se = child.stderr.take().unwrap();
    let t_out = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = so.read_to_string(&mut s);
        s
    });
    let t_err = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = se.read_to_string(&mut s);
        s
    });
    let start = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if start.elapsed() > TIMEOUT {
            let _ = child.kill();
            panic!("claudejr did not exit within {TIMEOUT:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    Out { code: status.code().unwrap_or(-1), out: t_out.join().unwrap(), err: t_err.join().unwrap() }
}

fn ok(o: &Out) {
    assert_eq!(o.code, 0, "stdout: {}\nstderr: {}", o.out, o.err);
}

/// Some process still has `dir` as its working directory (a job the agent started).
fn process_in(dir: &std::path::Path) -> bool {
    let Ok(rd) = std::fs::read_dir("/proc") else { return false };
    rd.flatten().any(|e| {
        e.file_name().to_string_lossy().bytes().all(|b| b.is_ascii_digit())
            && std::fs::read_link(e.path().join("cwd")).is_ok_and(|c| c == dir)
    })
}

// ---- arguments ----------------------------------------------------------

#[test]
fn help_and_version() {
    let e = Env::new();
    for flag in ["-h", "--help"] {
        let o = e.run(&[flag]);
        ok(&o);
        assert!(o.out.starts_with("usage: claudejr"), "{}", o.out);
        assert!(o.out.contains("--sandbox") && o.out.contains("CLAUDE_JR_MODEL"));
    }
    for flag in ["-V", "--version"] {
        let o = e.run(&[flag]);
        ok(&o);
        assert_eq!(o.out.trim(), format!("claude-jr {}", env!("CARGO_PKG_VERSION")));
    }
    assert!(e.requests().is_empty());
}

#[test]
fn bad_arguments_are_rejected() {
    let e = Env::new();
    let o = e.run(&["--frobnicate"]);
    assert_eq!(o.code, 2);
    assert!(o.err.contains("unknown argument: --frobnicate") && o.err.contains("usage: claudejr"), "{}", o.err);
    for flag in ["-p", "-m", "-e", "-r", "--model"] {
        let o = e.run(&[flag]);
        assert_eq!(o.code, 2, "{flag}");
        assert!(o.err.contains(&format!("{flag} needs a value")), "{}", o.err);
    }
    assert!(e.requests().is_empty());
}

#[test]
fn sessions_listing() {
    let e = Env::new();
    let o = e.run(&["--sessions"]);
    ok(&o);
    assert!(o.out.contains("No saved conversations in this directory."), "{}", o.out);

    ok(&e.run(&["-p", "first prompt here", "-y"]));
    let o = e.run(&["--sessions"]);
    ok(&o);
    assert!(o.out.contains("first prompt here") && o.out.contains("msgs"), "{}", o.out);
    assert!(o.out.contains("Resume with: claudejr -r NAME_OR_ID"));

    // Another folder has its own list.
    let other = e.root.join("other");
    std::fs::create_dir_all(&other).unwrap();
    let mut cmd = e.command(&["--sessions"]);
    cmd.current_dir(&other);
    let o = exec(cmd);
    assert!(o.out.contains("No saved conversations"), "{}", o.out);
}

#[test]
fn no_api_key_in_print_mode() {
    let e = Env::new();
    let mut cmd = e.command(&["-p", "hi"]);
    cmd.env_remove("ANTHROPIC_API_KEY");
    let o = exec(cmd);
    assert_eq!(o.code, 2);
    assert!(o.err.contains("no API key"), "{}", o.err);
    assert!(e.requests().is_empty());
}

#[test]
fn saved_credentials_are_used() {
    let e = Env::new();
    std::fs::create_dir_all(e.config_dir()).unwrap();
    std::fs::write(e.config_dir().join("credentials"), "from-file\n").unwrap();
    let mut cmd = e.command(&["-p", "hi"]);
    cmd.env_remove("ANTHROPIC_API_KEY");
    let o = exec(cmd);
    ok(&o);
    assert!(o.out.contains("Done."), "{}", o.out);
}

#[test]
fn interactive_needs_a_terminal() {
    let e = Env::new();
    let o = e.run(&[]);
    assert_eq!(o.code, 2);
    assert!(o.err.contains("not a terminal; use -p PROMPT"), "{}", o.err);
}

// ---- -p mode: the default scenario -----------------------------------------

#[test]
fn default_scenario_runs_tools() {
    let e = Env::new();
    let report = e.root.join("report.json");
    let mut cmd = e.command(&["-p", "do the demo", "-y"]);
    cmd.env("CLAUDE_JR_REPORT", &report);
    let o = exec(cmd);
    ok(&o);
    assert!(o.out.contains("Done. Here's what happened:") && o.out.contains("| file | change"), "{}", o.out);
    assert!(o.err.contains("● ") && o.err.contains("demo.txt"), "{}", o.err);
    assert!(o.err.contains("Round 1: checking the output."), "thinking goes to stderr: {}", o.err);
    assert_eq!(std::fs::read_to_string(e.proj.join("demo.txt")).unwrap(), "one\nTWO\nthree\n");

    let r: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
    assert_eq!(r["ok"], true);
    assert_eq!(r["requests"], 3);
    assert!(r["input_tokens"].as_u64().unwrap() >= 150);
    assert!(r["output_tokens"].as_u64().unwrap() >= 120);
    assert!(r["cache_read_tokens"].as_u64().unwrap() > 0);
    assert!(r["cost_usd"].as_f64().unwrap() > 0.0);
    assert!(r.get("cache_write_tokens").is_some());

    let reqs = e.requests();
    assert_eq!(reqs.len(), 3);
    assert_eq!(reqs[0]["model"], "claude-opus-5-5");
    let tools: Vec<&str> = reqs[0]["tools"].as_array().unwrap().iter().filter_map(|t| t["name"].as_str()).collect();
    for t in ["bash", "read", "write", "edit"] {
        assert!(tools.contains(&t), "{tools:?}");
    }
}

#[test]
fn api_failure_is_an_error() {
    let e = Env::new();
    let dead = free_port();
    let mut cmd = e.command(&["-p", "hi", "-y"]);
    cmd.env("ANTHROPIC_BASE_URL", format!("http://127.0.0.1:{dead}"));
    let report = e.root.join("report.json");
    cmd.env("CLAUDE_JR_REPORT", &report);
    let o = exec(cmd);
    assert_eq!(o.code, 1, "{}{}", o.out, o.err);
    assert!(o.err.contains("error:"), "{}", o.err);
    let r: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
    assert_eq!(r["ok"], false);
    assert_eq!(r["requests"], 0);
}

#[test]
fn model_and_effort_precedence() {
    let e = Env::with(&[("MOCK_ROUNDS", "0")]);
    e.write_config(r#"{"model":"from-config","effort":"low"}"#);
    let run = |args: &[&str], env_model: Option<&str>| {
        let mut cmd = e.command(args);
        if let Some(m) = env_model {
            cmd.env("CLAUDE_JR_MODEL", m);
        }
        ok(&exec(cmd));
    };
    run(&["-p", "aaa", "-y"], None);
    run(&["-p", "bbb", "-y"], Some("from-env"));
    run(&["-p", "ccc", "-y", "-m", "from-flag", "-e", "high"], Some("from-env"));
    let reqs = e.requests();
    let first_of = |prompt: &str| {
        reqs.iter()
            .find(|r| r["messages"][0]["content"].to_string().contains(prompt))
            .unwrap_or_else(|| panic!("no request for {prompt}"))
            .clone()
    };
    assert_eq!(first_of("aaa")["model"], "from-config");
    assert_eq!(first_of("bbb")["model"], "from-env");
    assert_eq!(first_of("ccc")["model"], "from-flag");
    assert!(first_of("aaa")["output_config"].to_string().contains("low"), "{}", first_of("aaa"));
    assert!(first_of("ccc")["output_config"].to_string().contains("high"), "effort from the flag wins");
}

#[test]
fn both_yes_flags_skip_permissions() {
    for flag in ["-y", "--yes", "--dangerously-skip-permissions"] {
        let e = Env::new();
        let o = e.run(&["-p", "go", flag]);
        ok(&o);
        assert!(e.proj.join("demo.txt").exists(), "{flag}: {}", o.err);
    }
}

#[test]
fn print_mode_refuses_without_yes() {
    let e = Env::new();
    let o = e.run(&["-p", "go"]);
    ok(&o); // the turn finishes; the tools were refused
    assert!(o.err.contains("nobody is here to approve"), "{}", o.err);
    assert!(!e.proj.join("demo.txt").exists());
    assert!(e.requests_text().contains("nobody is here to approve"), "the model is told");
}

#[test]
fn allow_rules_let_tools_run() {
    let e = Env::new();
    e.write_config(r#"{"allow":["write","edit","bash"]}"#);
    let o = e.run(&["-p", "go"]);
    ok(&o);
    assert!(!o.err.contains("nobody is here"), "{}", o.err);
    assert_eq!(std::fs::read_to_string(e.proj.join("demo.txt")).unwrap(), "one\nTWO\nthree\n");
}

#[test]
fn partial_allow_rule() {
    let e = Env::with(&[("MOCK_ROUNDS", "1"), ("MOCK_CMD", "echo hello-from-bash")]);
    e.write_config(r#"{"allow":["bash:echo *"]}"#);
    let o = e.run(&["-p", "go"]);
    ok(&o);
    assert!(o.err.contains("hello-from-bash"), "bash ran: {}", o.err);
    assert!(o.err.contains("nobody is here"), "write was refused: {}", o.err);
    assert!(!e.proj.join("demo.txt").exists());
}

#[test]
fn config_mode_from_file() {
    let e = Env::new();
    e.write_config(r#"{"mode":"skip-permissions"}"#);
    ok(&e.run(&["-p", "go"]));
    assert!(e.proj.join("demo.txt").exists());
}

#[test]
fn bad_config_names_the_file() {
    let e = Env::new();
    e.write_config("{ not json");
    let o = e.run(&["-p", "hi"]);
    assert_eq!(o.code, 2);
    assert!(o.err.contains("config.json"), "{}", o.err);
    let e = Env::new();
    e.write_config(r#"{"bogus_key": 1}"#);
    let o = e.run(&["-p", "hi"]);
    assert_eq!(o.code, 2);
    assert!(o.err.contains("config.json") && o.err.contains("bogus_key"), "{}", o.err);
}

#[test]
fn web_tools_setting() {
    let has_web = |r: &serde_json::Value| r["tools"].to_string().contains("web_search");
    let e = Env::with(&[("MOCK_SCENARIO", "web")]);
    let o = e.run(&["-p", "how do I configure vite?", "-y"]);
    ok(&o);
    assert!(o.out.contains("Vite reads vite.config.ts."), "{}", o.out);
    assert!(has_web(&e.requests()[0]));

    let e = Env::with(&[("MOCK_SCENARIO", "web")]);
    e.write_config(r#"{"web_tools":false}"#);
    ok(&e.run(&["-p", "q", "-y"]));
    assert!(!has_web(&e.requests()[0]));
}

// ---- sessions: continue and resume ------------------------------------------

#[test]
fn continue_sends_the_earlier_messages_again() {
    let e = Env::with(&[("MOCK_ROUNDS", "0")]);
    ok(&e.run(&["-p", "remember-the-walrus", "-y"]));
    let o = e.run(&["-c", "-p", "and now the second thing", "-y"]);
    ok(&o);
    let reqs = e.requests();
    let last = reqs.last().unwrap();
    let text = last["messages"].to_string();
    assert!(text.contains("remember-the-walrus") && text.contains("and now the second thing"), "{text}");
    assert!(last["messages"].as_array().unwrap().len() >= 3);
    assert!(!reqs[0]["messages"].to_string().contains("second thing"));
}

#[test]
fn resume_by_id_and_unknown_name() {
    let e = Env::with(&[("MOCK_ROUNDS", "0")]);
    ok(&e.run(&["-p", "the-first-topic", "-y"]));
    let list = e.run(&["--sessions"]).out;
    let id = list.lines().next().unwrap().split_whitespace().next().unwrap().to_string();
    std::thread::sleep(Duration::from_millis(1100));
    ok(&e.run(&["-p", "an unrelated topic", "-y"])); // a newer session

    let o = e.run(&["-r", &id, "-p", "continuing", "-y"]);
    ok(&o);
    let reqs = e.requests();
    let last = reqs.last().unwrap()["messages"].to_string();
    assert!(last.contains("the-first-topic") && last.contains("continuing"), "{last}");
    assert!(!last.contains("unrelated"), "resumed the right one");

    let before = e.requests().len();
    // An unknown name is not an error: it says so and starts a fresh session.
    let o = e.run(&["-r", "no-such-session", "-p", "fresh-start", "-y"]);
    ok(&o);
    assert!(o.err.contains("No saved session named \"no-such-session\" here; starting a new one."), "{}", o.err);
    let reqs = e.requests();
    assert!(reqs.len() > before);
    let first = reqs[before]["messages"].to_string();
    assert!(first.contains("fresh-start") && !first.contains("the-first-topic"), "{first}");
}

// ---- project config and trust -----------------------------------------------

#[test]
fn untrusted_project_files_are_ignored() {
    let e = Env::with(&[("MOCK_ROUNDS", "0")]);
    std::fs::write(e.proj.join(".claude-jr.json"), r#"{"model":"project-model"}"#).unwrap();
    std::fs::write(e.proj.join(".mcp.json"), r#"{"mcpServers":{"evil":{"command":"/bin/false"}}}"#).unwrap();
    let o = e.run(&["-p", "hi", "-y"]);
    ok(&o);
    assert!(o.err.contains("ignoring .claude-jr.json and .mcp.json: this folder isn't trusted"), "{}", o.err);
    assert_eq!(e.requests()[0]["model"], "claude-opus-5-5");
    assert!(!o.err.contains("MCP server evil"), "{}", o.err);
}

#[test]
fn trusted_project_files_are_honoured() {
    let e = Env::with(&[("MOCK_ROUNDS", "0")]);
    std::fs::write(e.proj.join(".claude-jr.json"), r#"{"model":"project-model"}"#).unwrap();
    std::fs::write(e.proj.join(".mcp.json"), r#"{"mcpServers":{"broken":{"command":"/nonexistent/server"}}}"#).unwrap();
    e.trust_project();
    let o = e.run(&["-p", "hi", "-y"]);
    ok(&o);
    assert!(!o.err.contains("isn't trusted"), "{}", o.err);
    assert_eq!(e.requests()[0]["model"], "project-model");
    assert!(o.err.contains("MCP server broken"), "the project's .mcp.json was read: {}", o.err);
}

// ---- features: todo, ask_user, subagents, jobs, skills, MCP, auto mode --------

const MCP_ECHO: &str = r#"
import sys, json
for line in sys.stdin:
    m = json.loads(line)
    if "id" not in m:
        continue
    method = m["method"]
    if method == "initialize":
        r = {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}}, "serverInfo": {"name": "demo", "version": "1"}}
    elif method == "tools/list":
        r = {"tools": [{"name": "echo", "description": "Echo the text back",
                        "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}}]}
    elif method == "tools/call":
        r = {"content": [{"type": "text", "text": "echoed: " + m["params"]["arguments"].get("text", "")}]}
    else:
        r = {}
    print(json.dumps({"jsonrpc": "2.0", "id": m["id"], "result": r}), flush=True)
"#;

fn mcp_config(e: &Env) -> String {
    let script = e.root.join("echo_server.py");
    std::fs::write(&script, MCP_ECHO).unwrap();
    serde_json::json!({
        "mcp_servers": { "demo": { "command": "python3", "args": ["-I", script] } }
    })
    .to_string()
}

#[test]
fn features_scenario() {
    let e = Env::with(&[("MOCK_SCENARIO", "features")]);
    e.write_config(&mcp_config(&e));
    std::fs::write(e.proj.join("notes.txt"), "line one\nline two\n").unwrap();
    let skill = e.proj.join(".claude/skills/demo");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: demo\ndescription: A demo skill for tests\n---\nSKILL-BODY-MARKER: do the demo thing.\n",
    )
    .unwrap();

    let report = e.root.join("report.json");
    let mut cmd = e.command(&["-p", "exercise everything", "--dangerously-skip-permissions"]);
    cmd.env("CLAUDE_JR_REPORT", &report);
    let o = exec(cmd);
    ok(&o);
    assert!(o.out.contains("All features checked."), "{}", o.out);
    assert!(o.err.contains("● agent: check notes"), "{}", o.err);
    assert!(o.err.contains("● agent: check notes again"), "{}", o.err);
    let sent = e.requests_text();
    assert!(sent.contains("The user isn't available"), "ask_user result missing");
    assert!(sent.contains("Subagent report: notes.txt has 2 lines."), "subagent report not returned");
    assert!(sent.contains("served"), "jobs wait did not see the output");
    assert!(sent.contains("SKILL-BODY-MARKER"), "skill body not returned");
    assert!(sent.contains("echoed: ping"), "MCP tool result missing");
    assert!(sent.contains("mcp__demo__echo"), "MCP tool was not offered");
    assert!(sent.contains("reviewed-and-allowed"));
    let r: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
    assert_eq!(r["ok"], true);
    assert!(r["requests"].as_u64().unwrap() >= 8);
    assert!(!process_in(&e.proj), "background job left running");
}

#[test]
fn features_with_the_auto_mode_reviewer() {
    let e = Env::with(&[("MOCK_SCENARIO", "features")]);
    std::fs::write(e.proj.join("notes.txt"), "a\nb\n").unwrap();
    e.write_config(r#"{"mode":"auto"}"#);
    let o = e.run(&["-p", "exercise everything"]);
    ok(&o);
    let reviews: Vec<_> =
        e.requests().into_iter().filter(|r| r["system"].to_string().contains("You review actions")).collect();
    assert!(!reviews.is_empty(), "the reviewer was never asked; stderr: {}", o.err);
    let text = reviews.iter().map(|r| r.to_string()).collect::<String>();
    assert!(text.contains("rm -rf ./build"), "{text}");
    assert!(e.requests_text().contains("deletes files"), "{}", o.err);
}

#[test]
fn agents_scenario_stops_background_jobs() {
    let e = Env::with(&[("MOCK_SCENARIO", "agents")]);
    let o = e.run(&["-p", "start the dev server", "-y"]);
    ok(&o);
    assert!(o.out.contains("Started the dev server."), "{}", o.out);
    assert!(o.err.contains("● agent: scan components") && o.err.contains("● agent: check styles"), "{}", o.err);
    assert!(e.requests_text().matches("Found it.").count() >= 2, "both subagents reported");
    assert!(!process_in(&e.proj), "the background server was left running");
}

#[test]
fn memory_scenario_writes_the_note() {
    let e = Env::with(&[("MOCK_SCENARIO", "memory")]);
    let o = e.run(&["-p", "remember that we use pnpm", "-y"]);
    ok(&o);
    assert!(o.out.contains("Noted."), "{}", o.out);
    let mut found = false;
    let mut stack = vec![e.data.clone()];
    while let Some(d) = stack.pop() {
        for ent in std::fs::read_dir(&d).unwrap().flatten() {
            let p = ent.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_none_or(|x| x != "jsonl")
                && std::fs::read_to_string(&p).is_ok_and(|t| t.contains("uses pnpm, not npm"))
            {
                found = true;
            }
        }
    }
    assert!(found, "no memory file under {}", e.data.display());
    ok(&e.run(&["-p", "what do you know", "-y"]));
    let last = e.requests().pop().unwrap();
    assert!(last["system"].to_string().contains("uses pnpm, not npm"), "memory not in the system prompt");
}

// ---- MCP configuration problems ---------------------------------------------

#[test]
fn mcp_server_that_cannot_start_is_reported() {
    let e = Env::with(&[("MOCK_ROUNDS", "0")]);
    e.write_config(
        r#"{"mcp_servers":{"ghost":{"command":"/nonexistent/binary"},"empty":{},"remote":{"url":"https://example.invalid/mcp","authorization_token":"tok"}}}"#,
    );
    let o = e.run(&["-p", "hi", "-y"]);
    ok(&o);
    assert!(o.err.contains("MCP server ghost"), "{}", o.err);
    assert!(o.err.contains("MCP server empty: needs a command or a url"), "{}", o.err);
    let req = &e.requests()[0];
    assert_eq!(req["mcp_servers"][0]["url"], "https://example.invalid/mcp");
    assert_eq!(req["mcp_servers"][0]["authorization_token"], "tok");
}

// ---- sandbox --------------------------------------------------------------

fn bwrap_works() -> bool {
    Command::new("bwrap")
        .args(["--ro-bind", "/", "/", "true"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[test]
fn sandbox_confines_writes() {
    if !bwrap_works() {
        eprintln!("bwrap is not usable here; sandbox_error_paths covers the error side");
        return;
    }
    let outside = PathBuf::from(format!("/var/tmp/cjr-outside-{}", std::process::id()));
    std::fs::create_dir_all(&outside).unwrap();
    let target = outside.join("new.txt");
    let cmd = format!("touch {}; echo ok > inside.txt", target.display());
    let e = Env::with(&[("MOCK_ROUNDS", "1"), ("MOCK_CMD", &cmd)]);
    let o = e.run(&["--sandbox", "-y", "-p", "try to escape"]);
    let escaped = target.exists();
    let _ = std::fs::remove_dir_all(&outside);
    ok(&o);
    assert!(!escaped, "wrote outside the project");
    assert!(e.proj.join("inside.txt").exists(), "write inside the project failed: {}", o.err);
    assert!(e.requests_text().contains("Read-only file system"), "touch should have failed: {}", o.err);
}

#[test]
fn sandbox_error_paths() {
    let e = Env::new();
    let mut cmd = e.command(&["--sandbox", "-p", "x", "-y"]);
    cmd.current_dir(&e.home);
    let o = exec(cmd);
    assert_eq!(o.code, 2);
    assert!(o.err.contains("run --sandbox from a project directory"), "{}", o.err);
    let mut cmd = e.command(&["--sandbox", "-p", "x", "-y"]);
    cmd.current_dir("/");
    assert!(exec(cmd).err.contains("run --sandbox from a project directory"));
    // bwrap missing from PATH.
    let mut cmd = e.command(&["--sandbox", "-p", "x", "-y"]);
    cmd.env("PATH", e.root.join("empty"));
    let o = exec(cmd);
    assert_eq!(o.code, 2);
    assert!(o.err.contains("--sandbox needs bubblewrap"), "{}", o.err);
    // The config can ask for it too.
    e.write_config(r#"{"sandbox":{"enabled":true}}"#);
    let mut cmd = e.command(&["-p", "x", "-y"]);
    cmd.env("PATH", e.root.join("empty"));
    assert!(exec(cmd).err.contains("needs bubblewrap"));
    assert!(e.requests().is_empty());
}
