// The interactive UI, end to end: the real binary runs in a pseudo-terminal
// (made by util-linux `script`) against the fake API in bench/mock.py, and
// the tests type at it and read what it draws.
//
// Every test has its own fake API, port, home, config, data and project
// folders, and never sees the real ANTHROPIC_API_KEY. The app is always left
// through /exit, ctrl-d, ctrl-c twice or a signal it handles, never killed.
//
// Needs: script (util-linux), stty, kill, touch, python3.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle, sleep};
use std::time::{Duration, Instant};

/// How long to wait for something to show up before the test fails.
const WAIT: Duration = Duration::from_secs(30);
/// An approval prompt ignores answers for its first 400 ms.
const APPROVE_GUARD: Duration = Duration::from_millis(450);

const ESC: &str = "\x1b";
const SHIFT_TAB: &str = "\x1b[Z";
const UP: &str = "\x1b[A";
const DOWN: &str = "\x1b[B";
const RIGHT: &str = "\x1b[C";
const LEFT: &str = "\x1b[D";
const HOME: &str = "\x1b[H";
const END: &str = "\x1b[F";
const DELETE: &str = "\x1b[3~";
const PAGE_UP: &str = "\x1b[5~";
/// With the kitty keyboard protocol: shift+enter, and the `a` key going up.
const SHIFT_ENTER: &str = "\x1b[13;2u";
const A_RELEASED: &str = "\x1b[97;1:3u";
const ALT_ENTER: &str = "\x1b\r";
const ALT_BACKSPACE: &str = "\x1b\x7f";
const BACKSPACE: &str = "\x7f";
const CTRL_A: &str = "\x01";
const CTRL_C: &str = "\x03";
const CTRL_D: &str = "\x04";
const CTRL_E: &str = "\x05";
const CTRL_J: &str = "\n";
const CTRL_K: &str = "\x0b";
const CTRL_U: &str = "\x15";
const CTRL_V: &str = "\x16";
const CTRL_W: &str = "\x17";

const SKILL: &str = "---\nname: demo\ndescription: A demo skill.\n---\nSay hello from the demo skill.\n";

/// A tiny MCP server with one tool, `echo`.
const MCP_SERVER: &str = r#"import json, sys
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

fn path_var() -> String {
    std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into())
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Whether the server on `port` is the fake API that writes to `log`: it is
/// sent a request carrying `marker`, which must then be in that log. (A port
/// that answers could also be another test's fake API.)
fn answers_as(port: u16, log: &Path, marker: &str) -> bool {
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
struct World {
    /// The home folder; the project, config and data folders are inside it.
    root: PathBuf,
    port: u16,
    mock: Child,
    /// The terminal says it can tell shift+enter from enter (the kitty keyboard protocol).
    enhanced_keys: bool,
    /// The terminal says it has 24-bit color.
    truecolor: bool,
}

impl World {
    /// `name` must be unique in this file. `mock_env` is MOCK_SCENARIO and friends.
    fn new(name: &str, mock_env: &[(&str, &str)]) -> World {
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

    fn proj(&self) -> PathBuf {
        self.root.join("proj")
    }

    fn config_dir(&self) -> PathBuf {
        self.root.join("cfg/claude-jr")
    }

    fn data_dir(&self) -> PathBuf {
        self.root.join("data/claude-jr")
    }

    /// Writes a file in the project folder.
    fn write(&self, rel: &str, text: &str) {
        let path = self.proj().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// The user's config file.
    fn config(&self, json: &str) {
        std::fs::write(self.config_dir().join("config.json"), json).unwrap();
    }

    fn read_config(&self) -> String {
        std::fs::read_to_string(self.config_dir().join("config.json")).unwrap_or_default()
    }

    /// A demo skill in the project.
    fn with_skill(&self) {
        self.write(".claude/skills/demo/SKILL.md", SKILL);
    }

    /// An MCP server with one tool, in the user's config.
    fn with_mcp(&self) {
        let server = self.root.join("mcp.py");
        std::fs::write(&server, MCP_SERVER).unwrap();
        self.config(&format!(
            r#"{{"mcp_servers": {{"demo": {{"command": "python3", "args": ["-I", "{}"]}}}}}}"#,
            server.display()
        ));
    }

    /// Every request body the app has sent to the fake API, one JSON object
    /// per line. (The app always streams; another test's probe doesn't.)
    fn log(&self) -> String {
        let text = std::fs::read_to_string(self.root.join("mock.log")).unwrap_or_default();
        text.lines().filter(|l| l.contains("\"stream\": true")).flat_map(|l| [l, "\n"]).collect()
    }

    /// Waits until the fake API has been sent `text`.
    fn sent(&self, text: &str) {
        let end = Instant::now() + WAIT;
        while !self.log().contains(text) {
            assert!(Instant::now() < end, "the API was never sent {text:?}");
            sleep(Duration::from_millis(10));
        }
    }

    /// Starts the app and waits for its input box.
    fn start(&self, args: &[&str]) -> App {
        let mut app = self.launch(args, Some("mock"));
        app.expect("ask anything");
        app
    }

    /// Starts the app in a 100x40 pseudo-terminal. Nothing from the outside
    /// environment reaches it except PATH and where to write coverage data.
    fn launch(&self, args: &[&str], key: Option<&str>) -> App {
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
fn read_screen(
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
struct App {
    child: Child,
    stdin: Arc<Mutex<ChildStdin>>,
    /// Everything drawn so far, escape codes removed.
    screen: Arc<Mutex<Vec<u8>>>,
    /// The few escape codes `read_screen` notes.
    codes: Arc<Mutex<Vec<String>>>,
    /// How far `expect` has read.
    pos: usize,
    pid: u32,
    reader: Option<JoinHandle<()>>,
}

impl App {
    /// Types `keys`.
    fn send(&self, keys: &str) {
        let mut stdin = self.stdin.lock().unwrap();
        stdin.write_all(keys.as_bytes()).and_then(|_| stdin.flush()).expect("the app is still reading");
    }

    /// Waits for `text` to be drawn after everything matched so far.
    fn expect(&mut self, text: &str) {
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

    fn expect_all(&mut self, texts: &[&str]) {
        for text in texts {
            self.expect(text);
        }
    }

    /// Types a line and waits for the turn it starts to finish.
    fn turn(&mut self, message: &str) {
        self.send(&format!("{message}\r"));
        self.expect("esc to stop");
        self.expect("ask anything");
    }

    /// Answers the approval prompt on screen, once it takes answers.
    fn approve(&self, key: &str) {
        sleep(APPROVE_GUARD);
        self.send(key);
    }

    /// Waits for `text` to be drawn, wherever it is among what was drawn so far.
    fn shown(&self, text: &str) {
        let end = Instant::now() + WAIT;
        while find(&self.screen.lock().unwrap(), text.as_bytes()).is_none() {
            assert!(Instant::now() < end, "{text:?} never appeared. The screen:\n{}", self.all());
            sleep(Duration::from_millis(10));
        }
    }

    /// What was drawn after the last match.
    fn since(&self) -> String {
        String::from_utf8_lossy(&self.screen.lock().unwrap()[self.pos..]).into_owned()
    }

    /// Everything drawn so far.
    fn all(&self) -> String {
        self.all_from(0)
    }

    /// Everything drawn from `mark` (an earlier `pos`) on.
    fn all_from(&self, mark: usize) -> String {
        String::from_utf8_lossy(&self.screen.lock().unwrap()[mark..]).into_owned()
    }

    /// Changes the terminal's size (the app is told with SIGWINCH) and waits
    /// for the screen to be drawn again at the new width: an edge of the box
    /// around the input, the top one unless the screen is too short for it.
    ///
    /// Nothing is typed until then. crossterm can leave a key that arrives
    /// together with the resize unread until the next key press.
    fn resize(&mut self, rows: u16, cols: u16) {
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

    fn signal(&self, name: &str) {
        let ok = Command::new("kill").args(["-s", name, &self.pid.to_string()]).status().unwrap();
        assert!(ok.success(), "kill -s {name} failed");
    }

    /// Types /exit and waits for a clean exit.
    fn exit(self) {
        self.send("/exit\r");
        self.exited(0);
    }

    /// Waits for the app to end by itself, with this exit code.
    fn exited(mut self, code: i32) {
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
fn sessions(world: &World) -> Vec<PathBuf> {
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

// ---- a turn -------------------------------------------------------------

#[test]
fn a_turn_shows_thinking_tools_a_diff_and_markdown() {
    let world = World::new("turn", &[("MOCK_DELAY_MS", "8")]);
    let mut app = world.launch(&["-y"], Some("mock"));
    app.expect_all(&[
        "claude-jr",
        "claude-opus-5-5 · ~/proj",
        "/help for commands",
        "ask anything",
        "skip permissions",
    ]);

    app.send("hello\r");
    app.expect_all(&[
        "> hello",
        "Round 1: checking the output.",
        "Bash(head -c 1024",
        "Write(demo.txt)",
        "Read(demo.txt)",
        "Edit(demo.txt)",
        "- two",
        "+ TWO",
        "Round 2: checking the output.",
        "Done. Here's what happened:",
        "wrote demo.txt",
        "│ file",
        "cat demo.txt",
        "ask anything",
    ]);
    assert_eq!(std::fs::read_to_string(world.proj().join("demo.txt")).unwrap(), "one\nTWO\nthree\n");
    let saved = sessions(&world);
    assert_eq!(saved.len(), 1, "one session file");
    let saved = std::fs::read_to_string(&saved[0]).unwrap();
    assert!(saved.contains("hello") && saved.contains("tool_result"), "the session holds the turn");

    // Too narrow for the table, its rows are shown as text; wide again, as a table.
    let mark = app.pos;
    app.resize(40, 20);
    let narrow = app.all_from(mark);
    assert!(narrow.contains("notes.md") && !narrow.contains('┌'), "no table at 20 columns:\n{narrow}");
    let mark = app.pos;
    app.resize(40, 100);
    assert!(app.all_from(mark).contains("┌──────────┬"), "the table is back");

    app.send("/cost\r");
    app.expect("so far ·");
    app.expect("tokens in context");
    assert!(!app.all().contains("$0.0000 so far"), "the turn cost something");
    app.send("/stats\r");
    app.expect_all(&["claude-jr", "memory (peak", "threads"]);
    app.exit();
}

#[test]
fn web_searches_are_listed() {
    let world = World::new("web", &[("MOCK_SCENARIO", "web")]);
    let mut app = world.start(&[]);
    app.send("how does vite find its config\r");
    app.expect_all(&["Search(vite config)", "3 results", "Vite reads vite.config.ts.", "ask anything"]);
    app.exit();
}

#[test]
fn the_memory_tool_asks_and_saves_a_note() {
    let world = World::new("memory", &[("MOCK_SCENARIO", "memory")]);
    let mut app = world.start(&[]);
    app.send("remember what I use\r");
    app.expect("Allow Memory(add: uses pnpm, not npm)?");
    app.approve("y");
    app.expect_all(&["Saved. 1 notes in memory.", "Noted.", "ask anything"]);
    let mut notes = Vec::new();
    for project in std::fs::read_dir(world.data_dir().join("memory")).unwrap().flatten() {
        notes.push(std::fs::read_to_string(project.path().join("MEMORY.md")).unwrap());
    }
    assert_eq!(notes, ["- uses pnpm, not npm\n"]);
    app.exit();
}

// ---- prompts ------------------------------------------------------------

#[test]
fn approvals_take_yes_always_no_esc_and_ctrl_c() {
    let world = World::new("approve", &[]);
    let mut app = world.start(&[]);

    // First conversation: yes, always, a mode change in the middle, no.
    app.send("go\r");
    app.expect("Allow Bash(head -c 1024");
    // Not answers: another key, and a paste.
    app.send("x\x1b[200~y\x1b[201~");
    app.approve("xy");
    app.expect("Allow Write(demo.txt)?");
    app.approve("a");
    app.expect("Allow Edit(demo.txt)?");
    app.approve(SHIFT_TAB);
    app.expect("accept edits");
    app.send("1");
    app.expect("Round 2: checking the output.");
    app.expect("Allow Bash(head -c 1024");
    app.approve("n");
    app.expect_all(&["did not allow", "Done. Here's what happened:", "ask anything"]);
    assert_eq!(std::fs::read_to_string(world.proj().join("demo.txt")).unwrap(), "one\nTWO\nthree\n");

    // Second: esc refuses; edits no longer ask; ctrl-c refuses and stops the turn.
    app.send("/clear\r");
    app.expect("/help for commands");
    app.send("again\r");
    app.expect("Allow Bash(head -c 1024");
    app.send(ESC);
    app.expect_all(&["did not allow", "Write(demo.txt)", "Edit(demo.txt)", "Allow Bash(head -c 1024"]);
    app.send(CTRL_C);
    app.expect_all(&["Stopped.", "ask anything"]);

    // Third: "always" for a command covers the same command later.
    app.send("/clear\r");
    app.expect("/help for commands");
    app.send("third\r");
    app.expect("Allow Bash(head -c 1024");
    app.approve("A");
    app.expect("Round 2: checking the output.");
    let mark = app.pos;
    app.expect_all(&["Done. Here's what happened:", "ask anything"]);
    assert!(!app.all_from(mark).contains("Allow "), "nothing more to approve");
    app.exit();
}

#[test]
fn questions_subagents_jobs_skills_and_mcp() {
    let world = World::new("features", &[("MOCK_SCENARIO", "features")]);
    world.with_skill();
    world.with_mcp();
    world.write("notes.txt", "a\nb\n");
    world.write("build/out.o", "x");
    let mut app = world.launch(&["-y"], Some("mock"));
    app.expect("Loaded 1 skill · MCP: demo (local, 1 tools).");
    app.expect("ask anything");

    // Answer by number.
    app.send("check everything\r");
    app.expect_all(&["Todos", "look around", "Which color?", "1. red", "2. blue", "type an answer"]);
    app.send("2\r");
    app.expect_all(&[
        "⎿ blue",
        "Agent(check notes)",
        "Subagent report: notes.txt has 2 lines.",
        "Started job 1 in the background",
        "served",
        "Skill(demo)",
        "Say hello from the demo skill.",
        "demo: echo(text: ping)",
        "echoed ping",
        "reviewed-and-allowed",
        "Bash(rm -rf ./build)",
        "All features checked.",
        "ask anything",
    ]);
    assert!(!world.proj().join("build").exists(), "skip-permissions mode ran the rm");
    let screen = app.all();
    assert!(screen.contains("▶ job 1 · sleep 0.3; echo served; sleep 30"), "the job is listed under the input");
    assert!(screen.contains("· 1 job"), "the footer counts the job");

    app.send("/jobs\r");
    app.expect_all(&["Background jobs (stop one with /kill N):", "1. sleep 0.3; echo served; sleep 30  (running)"]);
    app.send("/mcp\r");
    app.expect_all(&["MCP servers:", "demo (local, 1 tools)"]);
    app.send("/skills\r");
    app.expect_all(&["Skills (run with /name):", "demo"]);
    app.send("/kill 1\r");
    app.expect("Stopped job 1.");
    app.send("/kill 7\r");
    app.expect("No such job. /jobs lists them.");

    // Answer in words.
    app.send("/clear\r");
    app.expect("/help for commands");
    app.send("once more\r");
    app.expect("Which color?");
    app.send("purple\r");
    app.expect_all(&["⎿ purple", "All features checked.", "ask anything"]);
    let log = world.log();
    assert!(log.contains("blue") && log.contains("purple"), "both answers went to the model");

    // Esc at a question stops the turn.
    app.send("/clear\r");
    app.expect("/help for commands");
    app.send("and again\r");
    app.expect("Which color?");
    app.send(ESC);
    app.expect_all(&["Stopped.", "ask anything"]);
    app.exit();
}

#[test]
fn auto_mode_asks_only_about_what_the_reviewer_flags() {
    let world = World::new("auto", &[("MOCK_SCENARIO", "features")]);
    world.config(r#"{"mode": "auto"}"#);
    world.with_skill();
    world.write("notes.txt", "a\nb\n");
    world.write("build/out.o", "x");
    let mut app = world.start(&[]);
    app.expect("◆ auto mode");

    app.send("check everything\r");
    app.expect("Which color?");
    app.send("1\r");
    app.expect_all(&["⎿ red", "Started job 1 in the background", "reviewed-and-allowed"]);
    app.expect("Allow Bash(rm -rf ./build) — flagged: deletes files?");
    app.approve("n");
    app.expect_all(&["did not allow", "All features checked.", "ask anything"]);
    assert!(world.proj().join("build/out.o").exists(), "the flagged command did not run");
    assert!(world.log().contains("You review actions"), "the reviewer was asked");
    app.exit();
}

#[test]
fn shift_tab_cycles_modes_and_plan_mode_refuses_edits() {
    let world = World::new("modes", &[("MOCK_ROUNDS", "1")]);
    let mut app = world.start(&[]);
    for footer in ["⏵⏵ accept edits", "⏸ plan mode", "◆ auto mode", "⏵⏵ skip permissions"] {
        app.send(SHIFT_TAB);
        app.expect(footer);
    }
    // Round to normal again, then on to plan.
    app.send(SHIFT_TAB);
    app.send(SHIFT_TAB);
    app.expect("⏵⏵ accept edits");
    app.send(SHIFT_TAB);
    app.expect("⏸ plan mode");

    app.send("make the change\r");
    app.expect("Allow Bash(head -c 1024");
    app.approve("y");
    app.expect_all(&["Plan mode is on", "Done. Here's what happened:", "ask anything"]);
    assert!(!world.proj().join("demo.txt").exists(), "nothing was written in plan mode");
    world.sent("Plan mode is on. Explore and explain");

    for footer in ["◆ auto mode", "⏵⏵ skip permissions"] {
        app.send(SHIFT_TAB);
        app.expect(footer);
    }
    app.send(SHIFT_TAB);
    app.turn("carry on");
    world.sent("Plan mode is off.");
    app.exit();
}

#[test]
fn the_key_prompt_takes_a_pasted_key() {
    let world = World::new("key", &[("MOCK_ROUNDS", "0")]);
    let credentials = world.config_dir().join("credentials");
    let mut app = world.launch(&[], None);
    app.expect_all(&["Paste your Anthropic API key", "esc to skip", "sk-ant-"]);
    app.send(ESC);
    app.expect("No key set. Type /login when you're ready.");

    // Shell commands need no key; a message waits for one.
    app.send("!echo no-key-needed\r");
    app.expect_all(&["! echo no-key-needed", "⎿ no-key-needed", "ask anything"]);
    app.send("hello there\r");
    app.expect_all(&["Paste your Anthropic API key", "queued: hello there"]);
    app.send("\r"); // an empty key is not saved
    app.send("\x1b[200~sk-ant-test-0123\x1b[201~");
    app.expect("••••••••••••••••");
    assert!(!app.all().contains("sk-ant-test-0123"), "the key is never drawn");
    app.send("\r");
    app.expect_all(&["Saved your API key to", "> hello there", "Done. Here's what happened:", "ask anything"]);
    assert_eq!(std::fs::read_to_string(&credentials).unwrap(), "sk-ant-test-0123");
    assert_eq!(std::fs::metadata(&credentials).unwrap().permissions().mode() & 0o777, 0o600);

    app.send("/login\r");
    app.expect("Paste your Anthropic API key");
    app.send("wrong-key\r");
    app.expect_all(&["Saved your API key to", "That doesn't look like an Anthropic key"]);
    assert_eq!(std::fs::read_to_string(&credentials).unwrap(), "wrong-key");

    app.send("/logout\r");
    app.expect("Removed the saved API key.");
    assert!(!credentials.exists());
    assert!(!app.all().contains("is still set in your environment"));
    app.exit();
}

#[test]
fn rewind_works_before_any_file_was_changed() {
    let world = World::new("rewind-nofiles", &[("MOCK_ROUNDS", "0")]);
    let mut app = world.start(&["-y"]);
    app.turn("just a question");
    app.send("/rewind\r");
    app.expect("1. just a question");
    app.send("1");
    app.expect_all(&["Rewound to before \"just a question\". No files needed restoring;", "> just a question"]);
    app.send(CTRL_U);
    app.exit();
}

#[test]
fn rewind_and_resume_pick_from_a_list() {
    let world = World::new("rewind", &[("MOCK_ROUNDS", "1")]);
    let demo = world.proj().join("demo.txt");
    let mut app = world.start(&["-y"]);
    app.turn("first message");
    assert!(demo.exists());
    app.send("/rename alpha\r");
    app.expect("Renamed to \"alpha\". Resume it later with: claudejr -r \"alpha\"");

    // The picker: other keys do nothing, esc closes it.
    app.send("/rewind\r");
    app.expect_all(&["Rewind: restores the files the agent changed since then", "1. first message", "esc to cancel"]);
    app.send("x9");
    app.send(ESC);
    app.expect("ask anything");
    app.send("/rewind\r");
    app.expect("1. first message");
    app.send("1");
    app.expect("Rewound to before \"first message\". Restored 1 file;");
    assert!(!demo.exists(), "the file the agent wrote is gone again");
    // The message is back in the input box.
    app.expect("> first message");
    app.send(" again\r");
    app.expect_all(&["> first message again", "Done. Here's what happened:", "ask anything"]);
    assert!(demo.exists());

    // A turn that changed no files: nothing to restore, the newest message first in the list.
    app.turn("just talk");
    app.send("/rewind\r");
    app.expect_all(&["1. just talk", "2. first message again"]);
    app.send("1");
    app.expect_all(&["Rewound to before \"just talk\". No files needed restoring;", "> just talk"]);
    app.send(CTRL_U);
    app.expect("ask anything");
    assert!(demo.exists());

    app.send("/clear\r");
    app.expect("/help for commands");
    app.turn("other topic");
    let saved = sessions(&world);
    assert_eq!(saved.len(), 2);
    assert!(Command::new("touch").args(["-d", "2 hours ago"]).arg(&saved[0]).status().unwrap().success());
    app.send("/resume\r");
    app.expect_all(&["Resume a conversation", "1. other topic  (just now, ", "2. alpha  (2 h ago, ", "press a number"]);
    app.send("x9");
    app.send(ESC);
    app.expect("ask anything");
    app.send("/resume\r");
    app.expect("2. alpha");
    app.send("2");
    app.expect_all(&["Resumed \"alpha\" (", "> first message again", "Done. Here's what happened:"]);
    app.send("/rename\r");
    app.expect("This session is \"alpha\".");
    app.exit();

    // From the command line: the newest, one by name, one that isn't there.
    let mut app = world.launch(&["-y", "-c"], Some("mock"));
    app.expect("Resumed the last session (");
    app.expect("ask anything");
    app.exit();
    let mut app = world.launch(&["-y", "-r", "alpha"], Some("mock"));
    app.expect("Resumed \"alpha\" (");
    app.expect("ask anything");
    app.exit();
    let mut app = world.launch(&["-r", "nope"], Some("mock"));
    app.expect("No saved session named \"nope\" here; starting a new one.");
    app.exit();
}

#[test]
fn the_settings_picker_saves_to_the_config_file() {
    let world = World::new("settings", &[]);
    let mut app = world.start(&[]);
    app.send("/settings\r");
    app.expect_all(&[
        "Settings (~/.config/claude-jr/config.json)",
        "1. Input at the bottom of the screen: bottom",
        "2. Model for new sessions: claude-opus-5-5",
        "3. Effort: default",
        "4. Start in mode: normal",
        "5. Web search and fetch: on",
        "6. Sandbox (writes only in the project): off",
    ]);
    app.send("1");
    app.expect("1. Input at the bottom of the screen: inline");
    app.send("2");
    app.expect("2. Model for new sessions: claude-sonnet-5-5");
    app.send("3");
    app.expect_all(&["Effort set to low.", "3. Effort: low", "claude-opus-5-5 low"]);
    app.send("4");
    app.expect("4. Start in mode: accept-edits");
    app.send("5");
    app.expect("5. Web search and fetch: off");
    app.send("6");
    app.expect("6. Sandbox (writes only in the project): on");
    app.send("6");
    app.expect("6. Sandbox (writes only in the project): off");
    app.send("8x"); // not settings
    let config = world.read_config();
    for line in [
        "\"layout\": \"inline\"",
        "\"model\": \"claude-sonnet-5-5\"",
        "\"effort\": \"low\"",
        "\"mode\": \"accept-edits\"",
        "\"web_tools\": false",
        "\"enabled\": false",
    ] {
        assert!(config.contains(line), "{line} in {config}");
    }
    // Effort goes round to the default, which is no value at all.
    for level in ["medium", "high", "xhigh", "max"] {
        app.send("3");
        app.expect(&format!("Effort set to {level}."));
    }
    app.send("3");
    app.expect("Effort set to the model's default.");
    assert!(world.read_config().contains("\"effort\": null"));
    app.send(ESC);
    app.expect("ask anything");
    app.exit();

    // The next start uses them.
    let mut app = world.start(&[]);
    assert!(app.all().contains("claude-sonnet-5-5 · "), "the saved model is used");
    app.expect("⏵⏵ accept edits");
    app.exit();
}

#[test]
fn a_project_config_is_used_once_the_folder_is_trusted() {
    let world = World::new("trust", &[]);
    world.write(".claude-jr.json", r#"{"mode": "plan"}"#);
    let mut app = world.launch(&[], Some("mock"));
    app.expect("Trust this folder? [y/N]");
    app.send("n\r");
    app.expect("ask anything");
    assert!(!app.all().contains("plan mode"));
    app.exit();

    let mut app = world.launch(&[], Some("mock"));
    app.expect_all(&["This folder has .claude-jr.json", "Trust this folder? [y/N]"]);
    app.send("y\r");
    app.expect_all(&["ask anything", "⏸ plan mode"]);
    app.exit();
    assert!(world.config_dir().join("trusted").exists());

    let mut app = world.start(&[]);
    app.expect("⏸ plan mode");
    assert!(!app.all().contains("Trust this folder?"), "asked only once");
    app.exit();
}

#[test]
fn failed_saves_are_reported() {
    let world = World::new("unsaved", &[("MOCK_ROUNDS", "0")]);
    // Folders where the note, the key and the settings should go.
    for dir in
        [world.proj().join("AGENTS.md"), world.config_dir().join("credentials"), world.config_dir().join("config.json")]
    {
        std::fs::create_dir_all(dir).unwrap();
    }
    let mut app = world.launch(&[], None);
    app.expect("Paste your Anthropic API key");
    app.send("sk-ant-for-now\r");
    app.expect_all(&["Couldn't save the key (", "it's used for this session only."]);
    app.turn("hello");
    world.sent("\"text\": \"hello\"");

    app.send("#a note\r");
    app.expect("Could not save the note:");
    app.send("/settings\r");
    app.expect("1. Input at the bottom of the screen: bottom");
    app.send("1");
    app.expect("Couldn't save the setting:");
    app.send(ESC);
    app.expect("ask anything");
    app.exit();
}

#[test]
fn a_big_paste_shows_as_a_label_and_is_sent_in_full() {
    let world = World::new("bigpaste", &[("MOCK_ROUNDS", "0")]);
    // A clipboard tool with text only: asking it for an image fails.
    let tool = world.root.join("bin/wl-paste");
    let script = "#!/bin/sh\ncase \"$*\" in *image*) exit 1;; esac\nprintf 'from the clipboard'\n";
    std::fs::write(&tool, script).unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut app = world.start(&["-y"]);

    // Ten pasted lines appear as one short label, not as ten rows.
    let lines: Vec<String> = (1..=10).map(|i| format!("pasted-line-{i}")).collect();
    app.send("explain ");
    app.send(&format!("\x1b[200~{}\x1b[201~", lines.join("\r")));
    app.expect("> explain [Pasted #1: 10 lines, 140 chars]");
    assert!(!app.all().contains("pasted-line-3"), "the pasted text itself is not drawn");
    // Backspace removes the whole label; pasting again gives the next one.
    app.send(BACKSPACE);
    app.expect("> explain ");
    app.send(&format!("\x1b[200~{}\x1b[201~", lines.join("\n")));
    app.expect("[Pasted #2: 10 lines, 140 chars]");
    app.send("\r");
    app.expect_all(&["> explain [Pasted #2: 10 lines, 140 chars]", "Done. Here's what happened:", "ask anything"]);
    // The API got the text, not the label.
    world.sent("pasted-line-1\\npasted-line-2");
    world.sent("pasted-line-10");
    assert!(!world.log().contains("[Pasted #"), "{}", world.log());

    // A short paste goes in as it is, and ctrl+v pastes text when there is no image.
    app.send("\x1b[200~short paste\x1b[201~ and ");
    app.send(CTRL_V);
    app.expect("> short paste and from the clipboard");
    app.send("\r");
    app.expect_all(&["Done. Here's what happened:", "ask anything"]);
    world.sent("short paste and from the clipboard");
    app.exit();
}

#[test]
fn images_come_from_the_clipboard_or_a_dragged_file() {
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
    let world = World::new("images", &[("MOCK_ROUNDS", "0")]);
    // A clipboard tool that always has a (very small) PNG.
    let tool = world.root.join("bin/wl-paste");
    std::fs::write(&tool, "#!/bin/sh\nprintf '\\211PNG\\r\\n\\032\\n'\n").unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    let shot = world.proj().join("shot.png");
    std::fs::write(&shot, [PNG, b"pixels".as_slice()].concat()).unwrap();
    let mut app = world.start(&["-y"]);

    // Ctrl-v saves the image and puts its path in the input.
    app.send("what is this ");
    app.send(CTRL_V);
    app.expect("paste-");
    app.send("\r");
    app.expect_all(&["Attached ", "Done. Here's what happened:", "ask anything"]);
    let images = world.data_dir().join("images");
    let pasted: Vec<PathBuf> = std::fs::read_dir(&images).unwrap().flatten().map(|f| f.path()).collect();
    assert!(pasted.iter().any(|p| std::fs::read(p).unwrap() == PNG), "the pasted image is saved: {pasted:?}");
    world.sent("\"media_type\": \"image/png\"");

    // A dragged file arrives as a pasted path.
    app.send(&format!("and this \x1b[200~{}\x1b[201~\r", shot.display()));
    app.expect_all(&["Attached ", "Done. Here's what happened:", "ask anything"]);
    // A message that starts with the path is still a message, not a command.
    app.send(&format!("\x1b[200~{}\x1b[201~ what is in it\r", shot.display()));
    app.expect_all(&["Attached ", "Done. Here's what happened:", "ask anything"]);
    let stored = std::fs::read_dir(&images).unwrap().flatten().map(|f| f.path());
    assert!(stored.into_iter().any(|p| std::fs::read(p).unwrap().ends_with(b"pixels")), "the dragged image is stored");
    app.exit();
}

#[test]
fn a_terminal_with_the_kitty_keyboard_protocol_and_24_bit_color() {
    let mut world = World::new("kitty", &[("MOCK_ROUNDS", "0")]);
    (world.enhanced_keys, world.truecolor) = (true, true);
    let mut app = world.start(&["-y"]);
    let codes = app.codes.clone();
    assert!(codes.lock().unwrap().contains(&">1u".to_string()), "the protocol is turned on: {codes:?}");
    assert!(codes.lock().unwrap().contains(&"truecolor".to_string()), "the mascot is in 24-bit color: {codes:?}");

    // Shift+enter is a new line; a key going up is nothing.
    app.send(&format!("one{SHIFT_ENTER}two{A_RELEASED}"));
    app.expect("two");
    app.turn("");
    world.sent("\"text\": \"one\\ntwo\"");
    app.exit();
    assert!(codes.lock().unwrap().contains(&"<1u".to_string()), "and off again on the way out: {codes:?}");
}

// ---- commands -----------------------------------------------------------

#[test]
fn every_command_answers() {
    let world = World::new("commands", &[("MOCK_ROUNDS", "0")]);
    let mut app = world.start(&["-y"]);
    let mut run = |command: &str, shown: &[&str]| {
        app.send(&format!("{command}\r"));
        app.expect_all(shown);
    };
    run("/help", &["/clear, /new", "type / to list commands", "up/down for history"]);
    run("/cost", &["$0.0000 so far · 0 tokens in context"]);
    run("/stats", &["memory (peak", "threads"]);
    run("/memory", &["Instruction files (AGENTS.md / CLAUDE.md):", "Start a message with # to add a note."]);
    run("/skills", &["No skills found. Put them in .claude/skills/<name>/SKILL.md."]);
    run("/mcp", &["No MCP servers."]);
    run("/jobs", &["No background jobs."]);
    run("/kill 3", &["No such job. /jobs lists them."]);
    run("/kill", &["No such job. /jobs lists them."]);
    run("/effort", &["Effort: the model's default. Choose with /effort low|medium|high|xhigh|max or default."]);
    run("/effort high", &["Effort set to high.", "claude-opus-5-5 high"]);
    run("/effort", &["Effort: high."]);
    run("/effort silly", &["Unknown effort \"silly\". Use low, medium, high, xhigh, max or default."]);
    run("/effort default", &["Effort set to the model's default."]);
    run("/rename", &["This session has no name yet. /rename NAME to set one."]);
    run("/rename night shift", &["Renamed to \"night shift\"."]);
    run("/rename", &["This session is \"night shift\". /rename NAME to change it."]);
    run("/model claude-sonnet-5-5", &["Model set to claude-sonnet-5-5."]);
    run("/model", &["Choose a model", "1. claude-opus-5-5", "2. claude-sonnet-5-5  (current)", "press a number"]);
    run("3", &["Model set to claude-haiku-5-5."]);
    run("/model", &["Choose a model", "3. claude-haiku-5-5  (current)"]);
    app.send("x0");
    app.send(ESC);
    app.expect("ask anything");
    let mut run = |command: &str, shown: &[&str]| {
        app.send(&format!("{command}\r"));
        app.expect_all(shown);
    };
    run("/nonsense", &["Unknown command /nonsense. Try /help."]);
    run("/rewind", &["Nothing to rewind to yet."]);
    run("/resume", &["No saved conversations in this folder yet."]);
    run("/compact", &["Error: nothing to compact yet"]);

    // A note, a shell command, and a message that carries both to the model.
    run("#use tabs here", &["Saved to AGENTS.md."]);
    assert_eq!(std::fs::read_to_string(world.proj().join("AGENTS.md")).unwrap(), "- use tabs here\n");
    run("/memory", &["Instruction files (AGENTS.md / CLAUDE.md):", "AGENTS.md"]);
    run("!echo from-the-shell", &["! echo from-the-shell", "⎿ from-the-shell", "ask anything"]);
    run("what now", &["> what now", "Done. Here's what happened:", "ask anything"]);
    let log = world.log();
    assert!(log.contains("The user saved this to memory: use tabs here"));
    assert!(log.contains("<user-shell>") && log.contains("from-the-shell"));
    assert!(log.contains("\"model\": \"claude-haiku-5-5\""), "the picked model is the one used");

    run(
        "/compact keep the table",
        &["Compacted ", "-word summary. /rewind can't go back past this point.", "ask anything"],
    );
    assert!(world.log().contains("Pay particular attention to: keep the table"));
    run("/new", &["/help for commands", "ask anything"]);
    run("/rename", &["This session has no name yet."]);
    run("/logout", &["Removed the saved API key. ANTHROPIC_API_KEY is still set in your environment"]);
    app.send("/quit\r");
    app.exited(0);
}

#[test]
fn the_slash_menu_filters_picks_completes_and_runs() {
    let world = World::new("menu", &[("MOCK_ROUNDS", "0")]);
    world.with_skill();
    let mut app = world.launch(&["-y"], Some("mock"));
    app.expect_all(&["Loaded 1 skill.", "ask anything"]);

    app.send("/");
    app.expect_all(&["› /help", "commands and keys", "14 more · up/down to pick · tab completes · enter runs"]);
    app.send("re");
    app.expect("› /resume");
    assert!(!app.since().contains("more ·"), "three matches fit");
    for (key, picked) in
        [(DOWN, "› /rename"), (DOWN, "› /rewind"), (DOWN, "› /resume"), (UP, "› /rewind"), (UP, "› /rename")]
    {
        app.send(key);
        app.expect(picked);
    }
    app.send("\t");
    app.expect("> /rename ");
    app.send("tabbed\r");
    app.expect("Renamed to \"tabbed\".");

    // Further down than the list is tall.
    app.send("/");
    app.expect("› /help");
    app.send(&DOWN.repeat(9));
    app.expect("› /jobs");
    app.send(CTRL_U);
    app.expect("ask anything");

    // A skill is in the list and runs like a command.
    app.send("/de");
    app.expect_all(&["› /demo", "skill"]);
    app.send("\r");
    app.expect_all(&["> /demo", "Done. Here's what happened:", "ask anything"]);
    world.sent("Say hello from the demo skill.");
    app.turn("/demo with these words");
    world.sent("with these words");

    // The first match runs on enter; alt-enter is a new line instead.
    app.send("/he\r");
    app.expect("type / to list commands");
    app.send("/he");
    app.expect("› /help");
    app.send(ALT_ENTER);
    app.send(CTRL_C);
    app.expect("ask anything");
    app.send(CTRL_D);
    app.exited(0);
}

// ---- typing -------------------------------------------------------------

#[test]
fn editing_keys_history_paste_and_ctrl_c() {
    let world = World::new("editing", &[("MOCK_ROUNDS", "0")]);
    world.config(r#"{"layout": "inline"}"#);
    let mut app = world.start(&["-y"]);
    let sent = |text: &str| world.sent(&format!("\"text\": \"{text}\""));

    // Word and line deletes, both ends, arrows, delete and backspace.
    app.send("one two three");
    app.send(CTRL_W);
    app.send("four");
    app.send(CTRL_A);
    app.send("A");
    app.send(CTRL_E);
    app.send("Z");
    app.send(&format!("{LEFT}{LEFT}{CTRL_K}{BACKSPACE}{HOME}{RIGHT}{DELETE}{PAGE_UP}{END}!"));
    app.expect("> Ane two fo!");
    app.turn("");
    sent("Ane two fo!");

    // History: up and down, and what was being typed comes back.
    app.turn("second entry");
    app.send(&format!("{UP}{UP}{DOWN}{DOWN}draft{UP}{DOWN}ed"));
    app.expect("> drafted");
    app.turn("");
    sent("drafted");
    app.send(&format!("{UP}{UP} again"));
    app.expect("> second entry again");
    app.turn("");
    sent("second entry again");

    // Several lines: ctrl-j, a backslash before enter, alt-enter; up and down move between them.
    app.send(&format!("line1{CTRL_J}line2{UP}X{DOWN}\\\rline3{ALT_ENTER}line4"));
    app.expect("line4");
    app.turn("");
    sent("line1X\\nline2\\nline3\\nline4");

    // Alt-backspace takes a word, ctrl-u the line.
    app.send(&format!("alpha beta{ALT_BACKSPACE}gamma"));
    app.expect("> alpha gamma");
    app.send(CTRL_U);
    app.turn("delta");
    sent("delta");

    // A line as wide as the box, then wider: it wraps, and up and down move between its rows.
    let wide = "w".repeat(94);
    app.send(&wide);
    app.expect(&wide);
    app.send(&format!("123456{UP}^{DOWN}$"));
    app.expect("123456$");
    app.turn("");
    sent(&format!("wwwwww^{}123456$", "w".repeat(88)));

    // A paste keeps its line breaks, whatever kind they were.
    app.send("\x1b[200~p1\r\np2\rp3\x1b[201~");
    app.expect("p3");
    app.turn("");
    sent("p1\\np2\\np3");

    // No clipboard here (and no display to find one on).
    app.send(CTRL_V);
    app.expect("Nothing to paste: the clipboard is empty, or wl-paste / xclip isn't installed.");

    // Ctrl-c clears what's typed, then asks, then quits.
    app.send("never sent");
    app.expect("> never sent");
    app.send(CTRL_C);
    app.expect("ask anything");
    app.send(CTRL_C);
    app.expect("press ctrl-c again to quit");
    app.send(CTRL_C);
    app.exited(0);
    assert!(!world.log().contains("never sent"));
}

#[test]
fn typing_while_busy_queues_and_esc_stops_the_turn() {
    let world = World::new("busy", &[("MOCK_SCENARIO", "bench"), ("MOCK_DELAY_MS", "20"), ("MOCK_TEXT_CHARS", "6000")]);
    world.with_skill();
    let mut app = world.start(&["-y"]);
    app.send("go\r");
    app.expect_all(&["type to queue a message", "esc to stop", "writing"]);
    app.send("second\r");
    app.expect("queued: second");
    app.send("third\r");
    app.expect("queued: second (+1 more)");
    for command in ["/clear", "/compact", "/resume", "/rewind", "/demo"] {
        app.send(&format!("{command}\r"));
        app.expect("Wait for the current turn to finish.");
    }

    // Each stop lets the next queued message go.
    app.send(ESC);
    app.expect_all(&["Stopped.", "> second", "writing"]);
    app.send(CTRL_C);
    app.expect_all(&["Stopped.", "> third", "writing"]);
    app.send(ESC);
    app.expect_all(&["Stopped.", "ask anything"]);
    let log = world.log();
    assert!(log.contains("second") && log.contains("third"), "the queued messages were sent");
    app.exit();
}

/// A reply long enough to be written out row by row while it streams, with
/// the terminal resized on the way and made tiny afterwards.
fn stream_and_resize(name: &str, layout: &str) {
    let world = World::new(
        name,
        &[("MOCK_SCENARIO", "bench"), ("MOCK_DELAY_MS", "3"), ("MOCK_TEXT_CHARS", "3000"), ("MOCK_ROUNDS", "2")],
    );
    world.config(&format!(r#"{{"layout": "{layout}"}}"#));
    let mut app = world.start(&["-y"]);
    app.send("go\r");
    app.expect("The build output looks fine");
    app.resize(30, 80);
    app.resize(24, 120);
    app.expect("ask anything");
    assert_eq!(world.log().lines().count(), 3, "two rounds of tools, then the answer");
    assert!(app.all().contains("Bash(head -c 1024"));

    // Smaller than the input box and its surroundings.
    app.resize(4, 30);
    app.send("/jobs\r");
    app.expect("No background jobs.");
    app.resize(40, 100);
    app.send("/help\r");
    app.expect("type / to list commands");
    app.exit();
}

#[test]
fn a_long_reply_streams_through_resizes_at_the_bottom() {
    stream_and_resize("stream-bottom", "bottom");
}

#[test]
fn a_long_reply_streams_through_resizes_inline() {
    stream_and_resize("stream-inline", "inline");
}

// ---- signals ------------------------------------------------------------

#[test]
fn subagents_and_jobs_are_listed_and_sigterm_stops_the_jobs() {
    let world = World::new("agents", &[("MOCK_SCENARIO", "agents")]);
    let mut app = world.start(&["-y"]);
    app.send("start the app\r");
    // Both subagents run at once, each with its latest step beside it
    // (which of the two gets there first varies).
    app.expect("running 2 tools at once");
    app.shown("◆ scan components · Bash(sleep 1.5; echo looked)");
    app.shown("◆ check styles · Bash(sleep 1.5; echo looked)");
    app.expect_all(&[
        "Agent(scan components)",
        "Found it.",
        "Agent(check styles)",
        "Started job 1 in the background",
        "Started the dev server.",
        "ask anything",
    ]);
    let screen = app.all();
    assert!(screen.contains("▶ job 1 · sleep 60"), "the job is listed under the input");
    assert!(screen.contains("· 1 job"), "the footer counts the job");

    app.send("/stats\r");
    app.expect("sleep");
    app.expect("(pid ");
    let job: u32 = app.since().split(')').next().unwrap().trim().parse().expect("the job's pid");
    assert!(Path::new(&format!("/proc/{job}")).exists());

    app.signal("TERM");
    app.exited(128 + 15);
    let end = Instant::now() + WAIT;
    while Path::new(&format!("/proc/{job}")).exists() {
        assert!(Instant::now() < end, "the background job outlived the app");
        sleep(Duration::from_millis(10));
    }
}

#[test]
fn sighup_and_sigint_exit_cleanly() {
    let world = World::new("signals", &[]);
    let mut app = world.start(&["-m", "claude-haiku-5-5", "-e", "low"]);
    app.expect("claude-haiku-5-5 low");
    app.signal("HUP");
    app.exited(128 + 1);

    let app = world.start(&[]);
    app.signal("INT");
    app.exited(128 + 2);
}

/// Removes the folders that earlier runs of these tests left behind: their
/// names start with `prefix` and the id of a process that is gone.
fn sweep_old_runs(prefix: &str) {
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
