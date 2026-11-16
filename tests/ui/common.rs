// What every UI test shares: a world of its own for each test, the app
// running in a pseudo-terminal, and ways to type at it and read its screen.

pub use std::io::{Read, Write};
pub use std::net::{TcpListener, TcpStream};
pub use std::os::unix::fs::PermissionsExt;
pub use std::path::{Path, PathBuf};
pub use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
pub use std::sync::{Arc, Mutex};
pub use std::thread::{self, JoinHandle, sleep};
pub use std::time::{Duration, Instant};

/// How long to wait for something to show up before the test fails.
pub const WAIT: Duration = Duration::from_secs(30);
/// An approval prompt ignores answers for its first 400 ms.
pub const APPROVE_GUARD: Duration = Duration::from_millis(450);

pub const ESC: &str = "\x1b";
pub const SHIFT_TAB: &str = "\x1b[Z";
pub const UP: &str = "\x1b[A";
pub const DOWN: &str = "\x1b[B";
pub const RIGHT: &str = "\x1b[C";
pub const LEFT: &str = "\x1b[D";
pub const HOME: &str = "\x1b[H";
pub const END: &str = "\x1b[F";
pub const DELETE: &str = "\x1b[3~";
pub const PAGE_UP: &str = "\x1b[5~";
/// With the kitty keyboard protocol: shift+enter, and the `a` key going up.
pub const SHIFT_ENTER: &str = "\x1b[13;2u";
pub const A_RELEASED: &str = "\x1b[97;1:3u";
pub const ALT_ENTER: &str = "\x1b\r";
pub const ALT_BACKSPACE: &str = "\x1b\x7f";
pub const BACKSPACE: &str = "\x7f";
pub const CTRL_A: &str = "\x01";
pub const CTRL_C: &str = "\x03";
pub const CTRL_D: &str = "\x04";
pub const CTRL_E: &str = "\x05";
pub const CTRL_J: &str = "\n";
pub const CTRL_K: &str = "\x0b";
pub const CTRL_U: &str = "\x15";
pub const CTRL_V: &str = "\x16";
pub const CTRL_W: &str = "\x17";
pub const CTRL_S: &str = "\x13";

pub const SKILL: &str = "---\nname: demo\ndescription: A demo skill.\n---\nSay hello from the demo skill.\n";

/// A tiny MCP server with one tool, `echo`.
pub const MCP_SERVER: &str = r#"import json, sys
for line in sys.stdin:
    m = json.loads(line)
    if "id" not in m:
        continue
    method = m.get("method")
    if method == "initialize":
        r = {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}}, "serverInfo": {"name": "demo", "version": "1"}}
    elif method == "tools/list":
        r = {"tools": [{"name": "echo", "description": "Echo text", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}}]}
    elif method == "tools/call":
        r = {"content": [{"type": "text", "text": "echoed " + m["params"]["arguments"].get("text", "")}]}
    else:
        r = {}
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": m["id"], "result": r}) + "\n")
    sys.stdout.flush()
"#;

pub fn path_var() -> String {
    std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into())
}

pub fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Whether the server on `port` is the fake API that writes to `log`: it is
/// sent a request carrying `marker`, which must then be in that log. (A port
/// that answers could also be another test's fake API.)
pub fn answers_as(port: u16, log: &Path, marker: &str) -> bool {
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) else { return false };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let body = format!(r#"{{"messages": [], "probe": "{marker}"}}"#);
    let request = format!(
        "POST /v1/messages HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let _ = stream.read_to_end(&mut Vec::new());
    std::fs::read_to_string(log).is_ok_and(|text| text.contains(marker))
}

/// One test's surroundings: its folders and its fake API.
pub struct World {
    /// The home folder; the project, config and data folders are inside it.
    pub root: PathBuf,
    pub port: u16,
    pub mock: Child,
    /// The terminal says it can tell shift+enter from enter (the kitty keyboard protocol).
    pub enhanced_keys: bool,
    /// The terminal says it has 24-bit color.
    pub truecolor: bool,
}

impl World {
    /// `name` must be unique in this file. `mock_env` is MOCK_SCENARIO and friends.
    pub fn new(name: &str, mock_env: &[(&str, &str)]) -> World {
        sweep_old_runs("cjr-ui-");
        let root = std::env::temp_dir().join(format!("cjr-ui-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["bin", "cfg/claude-jr", "data", "proj"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("bench/mock.py");
        let log = root.join("mock.log");
        // The port is free when picked, but another test can be given the
        // same one before the fake API binds it. Whoever loses that race
        // exits, and tries another port here.
        for _ in 0..10 {
            let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
            let mut mock = Command::new("python3")
                .arg("-I")
                .arg(&script)
                .arg(port.to_string())
                .env_clear()
                .env("PATH", path_var())
                .env("MOCK_LOG", &log)
                .envs(mock_env.iter().copied())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("python3 runs bench/mock.py");
            let end = Instant::now() + WAIT;
            while Instant::now() < end && mock.try_wait().unwrap().is_none() {
                if answers_as(port, &log, &root.display().to_string()) {
                    let _ = std::fs::remove_file(&log);
                    return World { root, port, mock, enhanced_keys: false, truecolor: false };
                }
                sleep(Duration::from_millis(10));
            }
            let _ = mock.kill();
            let _ = mock.wait();
        }
        panic!("the fake API didn't start");
    }

    pub fn proj(&self) -> PathBuf {
        self.root.join("proj")
    }

    pub fn config_dir(&self) -> PathBuf {
        self.root.join("cfg/claude-jr")
    }

    pub fn data_dir(&self) -> PathBuf {
        self.root.join("data/claude-jr")
    }

    /// Writes a file in the project folder.
    pub fn write(&self, rel: &str, text: &str) {
        let path = self.proj().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// The user's config file.
    pub fn config(&self, json: &str) {
        std::fs::write(self.config_dir().join("config.json"), json).unwrap();
    }

    pub fn read_config(&self) -> String {
        std::fs::read_to_string(self.config_dir().join("config.json")).unwrap_or_default()
    }

    /// A demo skill in the project.
    pub fn with_skill(&self) {
        self.write(".claude/skills/demo/SKILL.md", SKILL);
    }

    /// An MCP server with one tool, in the user's config.
    pub fn with_mcp(&self) {
        let server = self.root.join("mcp.py");
        std::fs::write(&server, MCP_SERVER).unwrap();
        self.config(&format!(
            r#"{{"mcp_servers": {{"demo": {{"command": "python3", "args": ["-I", "{}"]}}}}}}"#,
            server.display()
        ));
    }

    /// Every request body the app has sent to the fake API, one JSON object
    /// per line. (The app always streams; another test's probe doesn't.)
    pub fn log(&self) -> String {
        let text = std::fs::read_to_string(self.root.join("mock.log")).unwrap_or_default();
        text.lines().filter(|l| l.contains("\"stream\": true")).flat_map(|l| [l, "\n"]).collect()
    }

    /// Waits until the fake API has been sent `text`.
    pub fn sent(&self, text: &str) {
        let end = Instant::now() + WAIT;
        while !self.log().contains(text) {
            assert!(Instant::now() < end, "the API was never sent {text:?}");
            sleep(Duration::from_millis(10));
        }
    }

    /// Starts the app and waits for its input box.
    pub fn start(&self, args: &[&str]) -> App {
        let mut app = self.launch(args, Some("mock"));
        app.expect("ask anything");
        app
    }

    /// Starts the app in a 100x40 pseudo-terminal. Nothing from the outside
    /// environment reaches it except PATH and where to write coverage data.
    pub fn launch(&self, args: &[&str], key: Option<&str>) -> App {
        let quoted: Vec<String> = args.iter().map(|a| format!("'{a}'")).collect();
        let line = format!(
            "stty rows 40 cols 100; echo PID:$$; exec '{}' {}",
            env!("CARGO_BIN_EXE_claudejr"),
            quoted.join(" ")
        );
        let mut cmd = Command::new("script");
        cmd.args(["-qfec", &line, "/dev/null"])
            .env_clear()
            .env("PATH", format!("{}:{}", self.root.join("bin").display(), path_var()))
            .env("TERM", "xterm-256color")
            .env("SHELL", "/bin/sh")
            .env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", self.root.join("cfg"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("ANTHROPIC_BASE_URL", format!("http://127.0.0.1:{}", self.port))
            .env("CLAUDE_JR_OFFLINE_SECS", "15")
            .current_dir(self.proj())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(key) = key {
            cmd.env("ANTHROPIC_API_KEY", key);
        }
        if self.truecolor {
            cmd.env("COLORTERM", "truecolor");
        }
        if let Ok(profile) = std::env::var("LLVM_PROFILE_FILE") {
            cmd.env("LLVM_PROFILE_FILE", profile);
        }
        let mut child = cmd.spawn().expect("util-linux `script` is installed");
        let stdin = Arc::new(Mutex::new(child.stdin.take().unwrap()));
        let screen = Arc::new(Mutex::new(Vec::new()));
        let codes = Arc::new(Mutex::new(Vec::new()));
        let out = child.stdout.take().unwrap();
        let reader = {
            let (stdin, screen, codes, enhanced) = (stdin.clone(), screen.clone(), codes.clone(), self.enhanced_keys);
            thread::spawn(move || read_screen(out, &stdin, &screen, &codes, enhanced))
        };
        let mut app = App { child, stdin, screen, codes, pos: 0, pid: 0, reader: Some(reader) };
        app.expect("PID:");
        let end = Instant::now() + WAIT;
        loop {
            let rest = app.since();
            if let Some((pid, _)) = rest.split_once('\n') {
                app.pid = pid.trim().parse().expect("the shell printed its pid");
                break;
            }
            assert!(Instant::now() < end, "no pid line");
            sleep(Duration::from_millis(10));
        }
        app
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = self.mock.kill();
        let _ = self.mock.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Collects what the app draws, without the escape codes, and answers what it
/// asks the terminal at startup: where the cursor is (the bottom layout needs
/// it), what kind of terminal this is and, with `enhanced_keys`, that the
/// kitty keyboard protocol is there. Two kinds of escape code are noted in
/// `codes`: keyboard protocol changes (`>1u`, `<1u`) and 24-bit colors.
pub fn read_screen(
    mut out: ChildStdout,
    stdin: &Mutex<ChildStdin>,
    screen: &Mutex<Vec<u8>>,
    codes: &Mutex<Vec<String>>,
    enhanced_keys: bool,
) {
    enum State {
        Text,
        Esc,
        Csi,
        Osc,
    }
    let mut state = State::Text;
    let mut params = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        let n = match out.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        let mut plain = Vec::with_capacity(n);
        for &b in &buf[..n] {
            match state {
                State::Text => match b {
                    0x1b => state = State::Esc,
                    b'\r' => {}
                    _ => plain.push(b),
                },
                State::Esc => match b {
                    b'[' => {
                        params.clear();
                        state = State::Csi;
                    }
                    b']' => state = State::Osc,
                    _ => state = State::Text,
                },
                State::Csi if (0x40..=0x7e).contains(&b) => {
                    let answer: &[u8] = match (params.as_slice(), b) {
                        (b"6", b'n') => b"\x1b[1;1R",
                        (b"", b'c') => b"\x1b[?62c",
                        (b"?", b'u') if enhanced_keys => b"\x1b[?0u",
                        _ => b"",
                    };
                    let code = match b {
                        b'u' => Some(format!("{}u", String::from_utf8_lossy(&params))),
                        b'm' if params.starts_with(b"38;2;") => Some("truecolor".to_string()),
                        _ => None,
                    };
                    if let Some(code) = code {
                        let mut codes = codes.lock().unwrap();
                        if !codes.contains(&code) {
                            codes.push(code);
                        }
                    }
                    if !answer.is_empty() {
                        let mut stdin = stdin.lock().unwrap();
                        let _ = stdin.write_all(answer).and_then(|_| stdin.flush());
                    }
                    state = State::Text;
                }
                State::Csi => params.push(b),
                State::Osc if b == 0x07 => state = State::Text,
                State::Osc => {}
            }
        }
        screen.lock().unwrap().extend_from_slice(&plain);
    }
}

/// The app running in its pseudo-terminal.
pub struct App {
    pub child: Child,
    pub stdin: Arc<Mutex<ChildStdin>>,
    /// Everything drawn so far, escape codes removed.
    pub screen: Arc<Mutex<Vec<u8>>>,
    /// The few escape codes `read_screen` notes.
    pub codes: Arc<Mutex<Vec<String>>>,
    /// How far `expect` has read.
    pub pos: usize,
    pub pid: u32,
    pub reader: Option<JoinHandle<()>>,
}

impl App {
    /// Types `keys`.
    pub fn send(&self, keys: &str) {
        let mut stdin = self.stdin.lock().unwrap();
        stdin.write_all(keys.as_bytes()).and_then(|_| stdin.flush()).expect("the app is still reading");
    }

    /// Waits for `text` to be drawn after everything matched so far.
    pub fn expect(&mut self, text: &str) {
        let end = Instant::now() + WAIT;
        loop {
            if let Some(i) = find(&self.screen.lock().unwrap()[self.pos..], text.as_bytes()) {
                self.pos += i + text.len();
                return;
            }
            if Instant::now() > end {
                let seen = self.since();
                let tail: String = seen.chars().skip(seen.chars().count().saturating_sub(3000)).collect();
                panic!("{text:?} never appeared. The screen since the last match:\n{tail}");
            }
            sleep(Duration::from_millis(10));
        }
    }

    pub fn expect_all(&mut self, texts: &[&str]) {
        for text in texts {
            self.expect(text);
        }
    }

    /// Types a line and waits for the turn it starts to finish.
    pub fn turn(&mut self, message: &str) {
        self.send(&format!("{message}\r"));
        self.expect("esc to stop");
        self.expect("ask anything");
    }

    /// Answers the approval prompt on screen, once it takes answers.
    pub fn approve(&self, key: &str) {
        sleep(APPROVE_GUARD);
        self.send(key);
    }

    /// Waits for `text` to be drawn, wherever it is among what was drawn so far.
    pub fn shown(&self, text: &str) {
        let end = Instant::now() + WAIT;
        while find(&self.screen.lock().unwrap(), text.as_bytes()).is_none() {
            assert!(Instant::now() < end, "{text:?} never appeared. The screen:\n{}", self.all());
            sleep(Duration::from_millis(10));
        }
    }

    /// What was drawn after the last match.
    pub fn since(&self) -> String {
        String::from_utf8_lossy(&self.screen.lock().unwrap()[self.pos..]).into_owned()
    }

    /// Everything drawn so far.
    pub fn all(&self) -> String {
        self.all_from(0)
    }

    /// Everything drawn from `mark` (an earlier `pos`) on.
    pub fn all_from(&self, mark: usize) -> String {
        String::from_utf8_lossy(&self.screen.lock().unwrap()[mark..]).into_owned()
    }

    /// Changes the terminal's size (the app is told with SIGWINCH) and waits
    /// for the screen to be drawn again at the new width: an edge of the box
    /// around the input, the top one unless the screen is too short for it.
    ///
    /// Nothing is typed until then. crossterm can leave a key that arrives
    /// together with the resize unread until the next key press.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        let pts = std::fs::read_link(format!("/proc/{}/fd/0", self.pid)).expect("the app has a terminal");
        let ok = Command::new("stty")
            .arg("-F")
            .arg(pts)
            .args(["rows", &rows.to_string(), "cols", &cols.to_string()])
            .status()
            .unwrap();
        assert!(ok.success(), "stty could not resize the terminal");
        let line = "─".repeat(cols as usize - 2);
        self.expect(&if rows >= 8 { format!("╭{line}╮") } else { format!("╰{line}╯") });
    }

    pub fn signal(&self, name: &str) {
        let ok = Command::new("kill").args(["-s", name, &self.pid.to_string()]).status().unwrap();
        assert!(ok.success(), "kill -s {name} failed");
    }

    /// Types /exit and waits for a clean exit.
    pub fn exit(self) {
        self.send("/exit\r");
        self.exited(0);
    }

    /// Waits for the app to end by itself, with this exit code.
    pub fn exited(mut self, code: i32) {
        let end = Instant::now() + WAIT;
        let status = loop {
            match self.child.try_wait().unwrap() {
                Some(status) => break status,
                None if Instant::now() > end => panic!("the app didn't exit. The screen:\n{}", self.since()),
                None => sleep(Duration::from_millis(10)),
            }
        };
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        assert_eq!(status.code(), Some(code), "exit status. The screen since the last match:\n{}", self.since());
    }
}

impl Drop for App {
    /// Only reached with the app still running when a test has already failed.
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = Command::new("kill").args(["-s", "KILL", &self.pid.to_string()]).status();
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// The session files saved for the project, oldest first.
pub fn sessions(world: &World) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(projects) = std::fs::read_dir(world.data_dir().join("sessions")) else { return found };
    for project in projects.flatten() {
        for file in std::fs::read_dir(project.path()).into_iter().flatten().flatten() {
            if file.path().extension().is_some_and(|e| e == "jsonl") {
                found.push(file.path());
            }
        }
    }
    found.sort();
    found
}

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
