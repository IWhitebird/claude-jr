// Hostile and malformed input for every tool. A tool may refuse, but it must
// answer: no panic, no hang, nothing written outside where it was pointed,
// and a result small enough to send. Seeds are fixed.

use super::*;
use serde_json::{Value, json};
use std::sync::atomic::AtomicBool;

fn run(t: &Tools, name: &str, input: Value) -> Output {
    t.run(name, &input, &AtomicBool::new(false))
}

struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }

    fn value(&mut self, depth: usize) -> Value {
        const TEXT: &[&str] = &[
            "",
            " ",
            "a.txt",
            "sub/b.txt",
            "missing.txt",
            "../outside.txt",
            "/",
            "/dev/null",
            "/proc/self/mem",
            ".",
            "..",
            "~",
            "sub",
            "a\0b",
            "\n",
            "é漢👍",
            "-rf",
            "list",
            "add",
            "output",
            "kill",
            "wait",
            "echo hi",
            "true",
            "one",
            "two",
            "x",
            "in_progress",
            "pending",
            "completed",
        ];
        match self.below(if depth > 2 { 6 } else { 9 }) {
            0 => Value::Null,
            1 => json!(self.below(2) == 0),
            2 => json!(TEXT[self.below(TEXT.len())]),
            3 => json!([0i64, 1, -1, 2000, i64::MAX, i64::MIN][self.below(6)]),
            4 => json!([0.5, -1e308, 1e308][self.below(3)]),
            5 => json!("y".repeat([1, 100, 70_000][self.below(3)])),
            6 => Value::Array((0..self.below(4)).map(|_| self.value(depth + 1)).collect()),
            _ => {
                const KEYS: &[&str] = &[
                    "path",
                    "offset",
                    "limit",
                    "content",
                    "old_string",
                    "new_string",
                    "replace_all",
                    "command",
                    "timeout",
                    "background",
                    "action",
                    "id",
                    "todos",
                    "status",
                    "name",
                    "note",
                    "extra",
                ];
                let mut map = serde_json::Map::new();
                for _ in 0..self.below(6) {
                    map.insert(KEYS[self.below(KEYS.len())].into(), self.value(depth + 1));
                }
                Value::Object(map)
            }
        }
    }
}

/// The most memory this process has used, in kB.
fn peak_kb() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    status.lines().find_map(|l| l.strip_prefix("VmHWM:")?.split_whitespace().next()?.parse().ok()).unwrap_or(0)
}

#[test]
fn no_tool_panics_or_hangs_on_malformed_input() {
    let (t, dir) = test_tools("stress-malformed");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    std::fs::write(dir.join("sub/b.txt"), "x").unwrap();
    let mut rng = Rng(0x9E3779B97F4A7C15);
    // Not bash or memory: random text must not be run as a command or kept as a note.
    for name in ["read", "write", "edit", "jobs", "todo", "skill", "nonsense", "mcp__none__tool", ""] {
        for _ in 0..1500 {
            let mut input = rng.value(0);
            // Whatever it writes, it writes here.
            if let Some(path) = input.get("path").and_then(Value::as_str)
                && (path.starts_with('/') || path.contains(".."))
                && name != "read"
            {
                input["path"] = json!("a.txt");
            }
            let out = run(&t, name, input.clone());
            let text = match &out {
                Ok(t) | Err(t) => t,
            };
            assert!(text.len() < 200_000, "{name} answered with {} bytes for {input}", text.len());
            let _ =
                (t.label(name, &input), t.full_label(name, &input), t.subject(name, &input), t.preview(name, &input));
            let _ = summary(name, &out);
        }
    }
    for input in
        [json!({}), json!(null), json!("x"), json!({"command": 5}), json!({"command": ""}), json!({"command": ["ls"]})]
    {
        assert!(run(&t, "bash", input.clone()).is_err(), "bash ran {input}");
        let _ = t.label("bash", &input);
    }
    for input in [json!({}), json!(null), json!({"action": 3}), json!({"action": "explode"}), json!({"action": "add"})]
    {
        assert!(run(&t, "memory", input.clone()).is_err(), "memory took {input}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn reading_odd_files_answers() {
    let (t, dir) = test_tools("stress-read");
    let read = |path: &str| run(&t, "read", json!({ "path": path }));
    std::fs::write(dir.join("empty"), "").unwrap();
    std::fs::write(dir.join("binary"), [0u8, 159, 146, 150, 255, 0, 10, 13]).unwrap();
    std::fs::write(dir.join("latin1.txt"), b"caf\xe9\n").unwrap();
    std::fs::write(dir.join("one-long-line"), "z".repeat(3_000_000)).unwrap();
    std::fs::write(dir.join("many-lines"), "l\n".repeat(200_000)).unwrap();
    std::fs::write(dir.join("crlf.txt"), "a\r\nb\r\n").unwrap();
    std::fs::write(dir.join("no-newline"), "last").unwrap();
    std::os::unix::fs::symlink("loop", dir.join("loop")).unwrap();
    std::os::unix::fs::symlink("nowhere", dir.join("dangling")).unwrap();
    std::fs::create_dir(dir.join("folder")).unwrap();
    for path in [
        "empty",
        "binary",
        "latin1.txt",
        "one-long-line",
        "many-lines",
        "crlf.txt",
        "no-newline",
        "loop",
        "dangling",
        "folder",
        "missing",
        "",
    ] {
        let out = read(path);
        let text = out.clone().unwrap_or_else(|e| e);
        assert!(text.len() < 200_000, "{path}: {} bytes", text.len());
        assert!(!text.is_empty() || path == "empty", "{path}: an empty answer tells the model nothing");
    }
    assert!(read("loop").is_err() && read("dangling").is_err() && read("missing").is_err());
    assert!(read("no-newline").unwrap().contains("last"));
    assert!(read("crlf.txt").unwrap().contains('b'));
    // Devices and sockets are refused by every file tool, never opened.
    for path in ["/dev/zero", "/dev/tty", "/dev/stdin", "/dev/null"] {
        assert!(read(path).unwrap_err().contains("not a file"), "{path}");
        assert!(run(&t, "edit", json!({ "path": path, "old_string": "a", "new_string": "b" })).is_err(), "{path}");
        assert!(t.preview("write", &json!({ "path": path, "content": "x" })).is_none(), "{path}");
    }
    assert!(run(&t, "write", json!({ "path": "/dev/null", "content": "x" })).unwrap_err().contains("not a file"));
    // One enormous line is read without holding it in memory.
    {
        use std::io::Write;
        let mut f = std::fs::File::create(dir.join("huge-line")).unwrap();
        let chunk = vec![b'q'; 1 << 20];
        for _ in 0..300 {
            f.write_all(&chunk).unwrap();
        }
        f.write_all(b"\nsecond line\n").unwrap();
    }
    let before = peak_kb();
    let out = read("huge-line").unwrap();
    assert!(out.contains("second line") && out.len() < 20_000, "{} bytes", out.len());
    assert!(peak_kb() < before + 50_000, "a 300 MB line cost {} MB of memory", (peak_kb() - before) / 1024);
    // Offsets and limits anywhere, including past the end and absurd ones.
    for (offset, limit) in [(0, 0), (1, 1), (3, 1), (4, 5), (1_000_000, 1), (1, u64::MAX), (u64::MAX, u64::MAX)] {
        let out = run(&t, "read", json!({ "path": "many-lines", "offset": offset, "limit": limit }));
        let text = out.unwrap_or_else(|e| e);
        assert!(!text.is_empty() && text.len() < 200_000, "offset {offset} limit {limit}");
    }
    // A pipe nobody writes to must not be waited on forever.
    let fifo = dir.join("fifo");
    if std::process::Command::new("mkfifo").arg(&fifo).status().is_ok_and(|s| s.success()) {
        let (tx, rx) = std::sync::mpsc::channel();
        let t2 = t.child();
        std::thread::spawn(move || tx.send(run(&t2, "read", json!({ "path": "fifo" }))));
        let out = rx.recv_timeout(std::time::Duration::from_secs(5));
        // Unblock the reader either way, so the test's thread can end.
        let _ = std::os::unix::fs::OpenOptionsExt::custom_flags(std::fs::OpenOptions::new().write(true), 0o4000)
            .open(&fifo);
        assert!(out.is_ok_and(|o| o.is_err()), "reading a pipe should be refused, not waited on");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn edits_and_writes_of_odd_content_round_trip() {
    let (t, dir) = test_tools("stress-edit");
    let odd = [
        "",
        "\n",
        "no newline",
        "tabs\tand  spaces  \n",
        "crlf\r\nlines\r\n",
        "é漢👨‍👩‍👧‍👦\u{feff}\u{202e}\n",
        "$1 \\1 {} %s \\n\n",
        "quote \" and ' and `\n",
    ];
    for (i, content) in odd.iter().enumerate() {
        let path = format!("deep/er/{i}.txt");
        run(&t, "write", json!({ "path": path, "content": content }))
            .unwrap_or_else(|e| panic!("write {content:?}: {e}"));
        assert_eq!(std::fs::read_to_string(dir.join(&path)).unwrap(), *content);
    }
    std::fs::write(dir.join("f.txt"), "alpha\nbeta\nalpha\n").unwrap();
    let edit = |old: &str, new: &str, all: bool| {
        run(&t, "edit", json!({ "path": "f.txt", "old_string": old, "new_string": new, "replace_all": all }))
    };
    let now = || std::fs::read_to_string(dir.join("f.txt")).unwrap();
    assert!(edit("alpha", "x", false).is_err(), "two matches and not replace_all");
    assert!(edit("gamma", "x", false).is_err(), "no match");
    assert!(edit("", "x", false).is_err(), "nothing to find");
    assert!(edit("beta", "beta", false).is_err(), "no change");
    assert_eq!(now(), "alpha\nbeta\nalpha\n", "a refused edit changes nothing");
    edit("beta", "$1 \\0 é\r\n", false).unwrap();
    assert_eq!(now(), "alpha\n$1 \\0 é\r\n\nalpha\n");
    edit("alpha", "", true).unwrap();
    assert_eq!(now(), "\n$1 \\0 é\r\n\n\n");
    // A file that isn't text is left alone.
    std::fs::write(dir.join("bin"), [0u8, 255, 254, 0]).unwrap();
    assert!(run(&t, "edit", json!({ "path": "bin", "old_string": "\0", "new_string": "x" })).is_err());
    assert_eq!(std::fs::read(dir.join("bin")).unwrap(), [0u8, 255, 254, 0]);
    // Nothing can be written where a folder is, or through a file as if it were a folder.
    assert!(run(&t, "write", json!({ "path": "deep", "content": "x" })).is_err());
    assert!(run(&t, "write", json!({ "path": "f.txt/under", "content": "x" })).is_err());
    assert!(dir.join("deep").is_dir());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn bash_handles_hostile_commands() {
    let (t, dir) = test_tools("stress-bash");
    let bash = |cmd: &str, timeout: u64| run(&t, "bash", json!({ "command": cmd, "timeout": timeout }));
    let started = std::time::Instant::now();
    // More output than can be kept, on both streams, with no newline at all.
    let out = bash("head -c 30000000 /dev/zero | tr '\\0' x; head -c 3000000 /dev/zero | tr '\\0' e >&2", 60).unwrap();
    assert!(out.len() < 200_000 && out.contains("omitted"), "{} bytes", out.len());
    // Output that isn't text.
    let out = bash("head -c 2000 /dev/urandom", 60).unwrap_or_else(|e| e);
    assert!(out.len() < 200_000);
    // Reading the terminal it doesn't have must not hang.
    bash("read -t 1 line; cat", 60).unwrap_or_else(|e| e);
    // A command that outlives its time limit is stopped, children included.
    let err = bash("sh -c 'sleep 300 & sleep 300'; echo never", 1).unwrap_err();
    assert!(err.contains("Timed out after 1s"), "{err}");
    // Children that keep the output pipe open after the command exits.
    bash("(sleep 300 >/dev/null 2>&1 &) ; echo started", 5).unwrap();
    bash("nohup sleep 300 & disown; echo detached", 5).unwrap_or_else(|e| e);
    // Exit codes and signals are reported, not swallowed.
    assert!(bash("exit 7", 5).unwrap_err().contains('7'));
    let killed = bash("kill -9 $$", 5).unwrap_err();
    assert!(!killed.is_empty());
    // The shell survives all of that and still runs in the project.
    assert_eq!(bash("pwd", 5).unwrap().lines().next().unwrap(), dir.canonicalize().unwrap().to_str().unwrap());
    assert!(started.elapsed() < std::time::Duration::from_secs(40), "took {:?}", started.elapsed());
    t.shutdown();
    let _ = std::fs::remove_dir_all(dir);
}
