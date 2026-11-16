// What every command-line test shares: a world of its own for each test (home,
// config, data and project folders, and a fake API) and ways to run the real
// binary in it.

pub use std::io::{Read, Write};
pub use std::net::TcpListener;
pub use std::path::{Path, PathBuf};
pub use std::process::{Child, Command, Stdio};
pub use std::sync::atomic::{AtomicUsize, Ordering};
pub use std::time::{Duration, Instant};

pub const BIN: &str = env!("CARGO_BIN_EXE_claudejr");
pub const TIMEOUT: Duration = Duration::from_secs(120);

pub static COUNTER: AtomicUsize = AtomicUsize::new(0);

pub struct Mock(Child);

impl Drop for Mock {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub struct Out {
    pub code: i32,
    pub out: String,
    pub err: String,
}

/// One isolated world: dirs, a mock API and the knobs to run the binary in it.
pub struct Env {
    pub root: PathBuf,
    pub home: PathBuf,
    pub config: PathBuf,
    pub data: PathBuf,
    pub proj: PathBuf,
    pub port: u16,
    pub log: PathBuf,
    pub _mock: Mock,
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

pub fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

impl Env {
    pub fn new() -> Env {
        Env::with(&[])
    }

    pub fn with(mock_env: &[(&str, &str)]) -> Env {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        sweep_old_runs("cjr-cli-");
        let root = std::env::temp_dir().join(format!("cjr-cli-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (home, config, data, proj) = (root.join("home"), root.join("config"), root.join("data"), root.join("proj"));
        for d in [&home, &config, &data, &proj] {
            std::fs::create_dir_all(d).unwrap();
        }
        let proj = proj.canonicalize().unwrap();
        let log = root.join("requests.jsonl");
        // The fake API picks its own port and says which: a port picked here
        // could be given to another test's before this one had bound it.
        let mut cmd = Command::new("python3");
        cmd.args(["-I", &format!("{}/bench/mock.py", env!("CARGO_MANIFEST_DIR")), "0"])
            .env("MOCK_LOG", &log)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (k, v) in mock_env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().expect("python3 is needed for the fake API");
        let mut said = String::new();
        std::io::BufRead::read_line(&mut std::io::BufReader::new(child.stdout.take().unwrap()), &mut said).unwrap();
        let port: u16 = said.trim().parse().unwrap_or_else(|_| panic!("the fake API did not start (it said {said:?})"));
        let mock = Mock(child);
        Env { root, home, config, data, proj, port, log, _mock: mock }
    }

    pub fn config_dir(&self) -> PathBuf {
        self.config.join("claude-jr")
    }

    pub fn write_config(&self, json: &str) {
        std::fs::create_dir_all(self.config_dir()).unwrap();
        std::fs::write(self.config_dir().join("config.json"), json).unwrap();
    }

    pub fn trust_project(&self) {
        std::fs::create_dir_all(self.config_dir()).unwrap();
        std::fs::write(self.config_dir().join("trusted"), format!("{}\n", self.proj.display())).unwrap();
    }

    pub fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(BIN);
        cmd.args(args)
            .current_dir(&self.proj)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.config)
            .env("XDG_DATA_HOME", &self.data)
            .env("ANTHROPIC_API_KEY", "mock")
            .env("CLAUDE_JR_OFFLINE_SECS", "15")
            .env("ANTHROPIC_BASE_URL", format!("http://127.0.0.1:{}", self.port));
        // Coverage runs (bench/coverage.sh) collect the binary's profile through this.
        if let Some(p) = std::env::var_os("LLVM_PROFILE_FILE") {
            cmd.env("LLVM_PROFILE_FILE", p);
        }
        cmd
    }

    pub fn run(&self, args: &[&str]) -> Out {
        exec(self.command(args))
    }

    /// Every request body the mock saw, in order.
    pub fn requests(&self) -> Vec<serde_json::Value> {
        self.requests_text().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    pub fn requests_text(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

pub fn exec(cmd: Command) -> Out {
    exec_with(cmd, Stdio::null())
}

/// Runs `cmd` with `stdin` as its standard input.
pub fn exec_with(mut cmd: Command, stdin: Stdio) -> Out {
    cmd.stdin(stdin).stdout(Stdio::piped()).stderr(Stdio::piped());
    // A binary copied a moment ago can be "busy": a child process another
    // test's thread just forked still holds it open until it execs.
    let mut child = loop {
        match cmd.spawn() {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(Duration::from_millis(10))
            }
            child => break child.unwrap(),
        }
    };
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

pub fn ok(o: &Out) {
    assert_eq!(o.code, 0, "stdout: {}\nstderr: {}", o.out, o.err);
}

/// Some process still has `dir` as its working directory (a job the agent started).
pub fn process_in(dir: &std::path::Path) -> bool {
    let Ok(rd) = std::fs::read_dir("/proc") else { return false };
    rd.flatten().any(|e| {
        e.file_name().to_string_lossy().bytes().all(|b| b.is_ascii_digit())
            && std::fs::read_link(e.path().join("cwd")).is_ok_and(|c| c == dir)
    })
}

pub fn json_lines(text: &str) -> Vec<serde_json::Value> {
    text.lines().map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("not a JSON line ({e}): {l}"))).collect()
}

pub fn mcp_config(e: &Env) -> String {
    let script = e.root.join("echo_server.py");
    std::fs::write(&script, MCP_ECHO).unwrap();
    serde_json::json!({
        "mcp_servers": { "demo": { "command": "python3", "args": ["-I", script] } }
    })
    .to_string()
}

pub fn bwrap_works() -> bool {
    Command::new("bwrap")
        .args(["--ro-bind", "/", "/", "true"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// `args` run against the fake API as if it were a local Ollama, with no
/// Anthropic key anywhere.
pub fn ollama(e: &Env, args: &[&str]) -> Out {
    let mut cmd = e.command(args);
    cmd.env("OLLAMA_HOST", format!("127.0.0.1:{}", e.port))
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ANTHROPIC_BASE_URL");
    exec(cmd)
}

/// Points `cmd` at the fake API as if it were Bedrock and Vertex, with a
/// login for each and none for Anthropic. Bedrock's requests are signed with
/// the secret the fake API expects.
pub fn through_cloud(e: &Env, cmd: &mut Command) {
    let base = format!("http://127.0.0.1:{}", e.port);
    cmd.env_remove("ANTHROPIC_API_KEY")
        .env_remove("ANTHROPIC_BASE_URL")
        .env("ANTHROPIC_BEDROCK_BASE_URL", format!("{base}/anthropic"))
        .env("AWS_ACCESS_KEY_ID", "AKIDEXAMPLE")
        .env("AWS_SECRET_ACCESS_KEY", "mock-secret")
        .env("AWS_SESSION_TOKEN", "a-session-token")
        .env("ANTHROPIC_VERTEX_BASE_URL", format!("{base}/v1"))
        .env("ANTHROPIC_VERTEX_PROJECT_ID", "test-project")
        .env("VERTEX_ACCESS_TOKEN", "ya29.test");
}

/// `args` run through a cloud's door (see `through_cloud`).
pub fn cloud(e: &Env, args: &[&str]) -> Out {
    let mut cmd = e.command(args);
    through_cloud(e, &mut cmd);
    exec(cmd)
}

pub fn headers(e: &Env) -> Vec<serde_json::Value> {
    json_lines(&std::fs::read_to_string(e.root.join("requests.jsonl.headers")).unwrap_or_default())
}

pub fn nothing_refused(e: &Env) {
    let refused = std::fs::read_to_string(e.root.join("requests.jsonl.invalid")).unwrap_or_default();
    assert!(refused.is_empty(), "a request the API would refuse was sent: {refused}");
}

/// A repository in the project folder with one commit, and a way to ask git about it.
pub fn repository(e: &Env) -> impl Fn(&[&str]) -> String + '_ {
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com", "-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(&e.proj)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.name", "Test"]);
    git(&["config", "user.email", "test@example.com"]);
    git(&["config", "commit.gpgsign", "false"]);
    std::fs::create_dir_all(e.proj.join("sub")).unwrap();
    std::fs::write(e.proj.join("kept.txt"), "as committed\n").unwrap();
    std::fs::write(e.proj.join("sub/inner.txt"), "x\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "first"]);
    git
}

/// Runs `claudejr task ...` and returns what it printed.
pub fn task(e: &Env, args: &[&str]) -> Out {
    let mut all = vec!["task"];
    all.extend_from_slice(args);
    e.run(&all)
}

pub const MCP_ECHO: &str = r#"
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

/// Removes the folders that earlier runs of these tests left behind: their
/// names start with `prefix` and the id of a process that is gone.
pub fn sweep_old_runs(prefix: &str) {
    static SWEPT: std::sync::Once = std::sync::Once::new();
    SWEPT.call_once(|| {
        for entry in std::fs::read_dir(std::env::temp_dir()).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let pid = name.strip_prefix(prefix).and_then(|n| n.split('-').next().map(str::to_string));
            if pid.is_some_and(|pid| !Path::new("/proc").join(pid).exists()) {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    });
}
