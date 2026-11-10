// bash, in the foreground or as a background job, and `jobs` to watch those.

use super::{MAX_BYTES, Output, Tools, def};
use serde_json::value::RawValue;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, LazyLock, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

/// Recent output kept per background job.
const JOB_TAIL: usize = 64 * 1024;
/// Finished jobs kept around (with their output) for the jobs tool.
const KEEP_FINISHED: usize = 8;

pub static BASH: LazyLock<Box<RawValue>> = LazyLock::new(|| {
    def(json!({
        "name": "bash",
        "description": "Run a bash command. stdout and stderr are combined. The working directory carries over \
            between calls. Output over 50 KB keeps the start and the end. \
            Set background to true for servers, watchers and long builds: it returns a job id at once, \
            and you're told when the job ends. Use the jobs tool to read its output or wait for it.",
        "input_schema": {
            "type": "object",
            "properties": {
                "command": { "type": "string" },
                "timeout": { "type": "integer", "description": "Seconds, default 120, max 600 (foreground only)" },
                "background": { "type": "boolean" }
            },
            "required": ["command"]
        }
    }))
});

pub static JOBS: LazyLock<Box<RawValue>> = LazyLock::new(|| {
    def(json!({
        "name": "jobs",
        "description": "Check on background commands. list: all jobs. output: new output since the last check. \
            wait: block until the job ends, its output contains `until`, or `timeout` seconds pass \
            (default 60, max 600); use it to wait for a server to start or a build to finish. kill: stop it.",
        "input_schema": {
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["list", "output", "wait", "kill"] },
                "id": { "type": "integer" },
                "until": { "type": "string" },
                "timeout": { "type": "integer" }
            },
            "required": ["action"]
        }
    }))
});

pub fn run(t: &Tools, input: &Value, cancel: &AtomicBool) -> Output {
    let cmd = input["command"].as_str().ok_or("missing command")?;
    let cwd = t.cwd.lock().unwrap().clone();
    if input["background"] == true {
        return t.jobs.start(cmd, &cwd);
    }
    let timeout = Duration::from_secs(input["timeout"].as_u64().unwrap_or(120).clamp(1, 600));

    // The script reports its final directory so `cd` carries over.
    static CALLS: AtomicU64 = AtomicU64::new(0);
    let n = CALLS.fetch_add(1, Ordering::Relaxed);
    let cwd_file = std::env::temp_dir().join(format!("claude-jr-{}-{n}.cwd", std::process::id()));
    let script = format!("exec 2>&1\n{{\n{cmd}\n}}\n__rc=$?\npwd > '{}'\nexit $__rc", cwd_file.display());

    let mut child = spawn(&script, &cwd)?;
    let pid = child.id();
    t.running.lock().unwrap().push(pid);
    let out = Arc::new(Mutex::new(Capped::new(MAX_BYTES)));
    let done = read_into(&mut child, out.clone());

    // Wait for the output pipe to close, which wakes us the moment the command
    // is done, checking for cancel and timeout every 100 ms. A process left
    // running in the background can keep the pipe open; then the exit status
    // tells us instead.
    let start = Instant::now();
    let mut stopped = None;
    let mut pipe_closed = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {}
            Err(e) => return Err(e.to_string()),
        }
        if cancel.load(Ordering::Relaxed) {
            stopped = Some("Cancelled by the user".to_string());
        } else if start.elapsed() > timeout {
            stopped = Some(format!(
                "Timed out after {}s and was stopped. Run it again with a longer timeout (up to 600) or with background: true.",
                timeout.as_secs()
            ));
        }
        if stopped.is_some() {
            kill_group(child.id());
            let _ = child.wait();
            break None;
        }
        if pipe_closed {
            // Output is done but the process is still exiting: briefly.
            thread::sleep(Duration::from_millis(2));
        } else {
            pipe_closed = !matches!(done.recv_timeout(Duration::from_millis(100)), Err(RecvTimeoutError::Timeout));
        }
    };

    t.running.lock().unwrap().retain(|&p| p != pid);
    if !pipe_closed {
        let _ = done.recv_timeout(Duration::from_millis(300));
    }
    let mut text = out.lock().unwrap().text();

    if let Ok(dir) = std::fs::read_to_string(&cwd_file) {
        let dir = PathBuf::from(dir.trim_end());
        if dir.is_dir() {
            // Said out loud, so the next command isn't written for the old place.
            let mut cwd = t.cwd.lock().unwrap();
            if dir != *cwd {
                text = join(&text, &format!("(working directory is now {})", dir.display()));
            }
            *cwd = dir;
        }
        let _ = std::fs::remove_file(&cwd_file);
    }

    match (status, stopped) {
        (_, Some(why)) => Err(join(&text, &why)),
        (Some(s), _) if s.success() => Ok(if text.is_empty() { "(no output)".into() } else { text }),
        (Some(s), _) => Err(join(&text, &format!("Exit code {}", s.code().unwrap_or(-1)))),
        (None, None) => unreachable!(),
    }
}

pub fn jobs(t: &Tools, input: &Value, cancel: &AtomicBool) -> Output {
    let action = input["action"].as_str().unwrap_or("list");
    if action == "list" {
        return Ok(t.jobs.list());
    }
    let id = input["id"].as_u64().ok_or("missing id")? as usize;
    let job = t.jobs.get(id).ok_or_else(|| format!("no job {id}"))?;
    match action {
        "output" => Ok(job.report()),
        "kill" => {
            kill_group(job.pid);
            job.wait_done(Duration::from_secs(2), None, cancel);
            Ok(job.report())
        }
        "wait" => {
            let timeout = Duration::from_secs(input["timeout"].as_u64().unwrap_or(60).clamp(1, 600));
            job.wait_done(timeout, input["until"].as_str(), cancel);
            Ok(job.report())
        }
        _ => Err(format!("unknown action: {action}")),
    }
}

fn spawn(script: &str, cwd: &std::path::Path) -> Result<Child, String> {
    Command::new("bash")
        .arg("-c")
        .arg(script)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("could not start bash: {e}"))
}

/// Copies the child's stdout into `sink` on a side thread. The returned
/// channel fires when the pipe closes.
fn read_into(child: &mut Child, sink: Arc<Mutex<Capped>>) -> mpsc::Receiver<()> {
    let (tx, rx) = mpsc::channel();
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let _ = thread::Builder::new().name("bash-out".into()).spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n @ 1..) = stdout.read(&mut buf) {
            sink.lock().unwrap().push(&buf[..n]);
        }
        let _ = tx.send(());
    });
    rx
}

/// Kills a process and everything it started (they share its process group).
pub fn kill_group(pid: u32) {
    let _ = Command::new("kill").args(["-KILL", "--", &format!("-{pid}")]).status();
}

fn join(text: &str, note: &str) -> String {
    if text.is_empty() { note.to_string() } else { format!("{}\n{note}", text.trim_end()) }
}

// ---- background jobs ----------------------------------------------------

/// A finished background job, reported to the model with the next message.
pub struct JobEvent {
    pub id: usize,
    pub command: String,
    pub status: String,
}

#[derive(Default)]
pub struct Jobs(Arc<JobsInner>);

/// Called with a one-line message when a job ends.
pub type Notify = Box<dyn Fn(String) + Send + Sync>;

#[derive(Default)]
struct JobsInner {
    list: Mutex<Vec<Arc<Job>>>,
    finished: Mutex<Vec<JobEvent>>,
    notify: Mutex<Option<Notify>>,
}

struct Job {
    id: usize,
    command: String,
    pid: u32,
    started: Instant,
    out: Mutex<Tail>,
    /// How much output the model has already been shown.
    shown: Mutex<usize>,
    /// "exit 0", "exit 1", "killed"; None while running.
    status: Mutex<Option<String>>,
}

impl Jobs {
    fn start(&self, cmd: &str, cwd: &std::path::Path) -> Output {
        let mut child = spawn(&format!("exec 2>&1\n{cmd}"), cwd)?;
        let mut list = self.0.list.lock().unwrap();
        // Drop the oldest finished jobs and their output; running ones stay.
        let finished = list.iter().filter(|j| j.status.lock().unwrap().is_some()).count();
        let mut extra = finished.saturating_sub(KEEP_FINISHED - 1);
        list.retain(|j| {
            let done = j.status.lock().unwrap().is_some();
            let drop = done && extra > 0;
            extra -= usize::from(drop);
            !drop
        });
        let id = list.last().map_or(1, |j| j.id + 1);
        let job = Arc::new(Job {
            id,
            command: cmd.to_string(),
            pid: child.id(),
            started: Instant::now(),
            out: Mutex::new(Tail::new(JOB_TAIL)),
            shown: Mutex::new(0),
            status: Mutex::new(None),
        });
        list.push(job.clone());
        drop(list);

        let reader_job = job.clone();
        let mut stdout = child.stdout.take().expect("stdout is piped");
        let _ = thread::Builder::new().name("job-out".into()).spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n @ 1..) = stdout.read(&mut buf) {
                reader_job.out.lock().unwrap().push(&buf[..n]);
            }
        });

        let inner = self.0.clone();
        let reaper_job = job.clone();
        let _ = thread::Builder::new().name("job-wait".into()).spawn(move || {
            let status = match child.wait() {
                Ok(s) => match s.code() {
                    Some(c) => format!("exit {c}"),
                    None => "killed".to_string(),
                },
                Err(e) => format!("lost ({e})"),
            };
            *reaper_job.status.lock().unwrap() = Some(status.clone());
            if let Some(notify) = &*inner.notify.lock().unwrap() {
                notify(format!("Background job {} finished ({status}): {}", reaper_job.id, reaper_job.command));
            }
            inner.finished.lock().unwrap().push(JobEvent {
                id: reaper_job.id,
                command: reaper_job.command.clone(),
                status,
            });
        });

        Ok(format!("Started job {} in the background. Use the jobs tool to check on it.", job.id))
    }

    fn get(&self, id: usize) -> Option<Arc<Job>> {
        self.0.list.lock().unwrap().iter().find(|j| j.id == id).cloned()
    }

    fn list(&self) -> String {
        let list = self.0.list.lock().unwrap();
        if list.is_empty() {
            return "No background jobs.".into();
        }
        list.iter().map(|j| format!("{}: {} ({})", j.id, j.command, j.state())).collect::<Vec<_>>().join("\n")
    }

    /// (id, command, running) for every job, oldest first.
    pub fn summary(&self) -> Vec<(usize, String, bool)> {
        let list = self.0.list.lock().unwrap();
        list.iter().map(|j| (j.id, j.command.clone(), j.status.lock().unwrap().is_none())).collect()
    }

    /// Stops a job. Returns false if there's no such job.
    pub fn kill(&self, id: usize) -> bool {
        let Some(job) = self.get(id) else { return false };
        kill_group(job.pid);
        true
    }

    pub fn on_finish(&self, f: Notify) {
        *self.0.notify.lock().unwrap() = Some(f);
    }

    pub fn take_finished(&self) -> Vec<JobEvent> {
        std::mem::take(&mut self.0.finished.lock().unwrap())
    }

    pub fn running(&self) -> usize {
        self.0.list.lock().unwrap().iter().filter(|j| j.status.lock().unwrap().is_none()).count()
    }

    pub fn pids(&self) -> Vec<u32> {
        let list = self.0.list.lock().unwrap();
        list.iter().filter(|j| j.status.lock().unwrap().is_none()).map(|j| j.pid).collect()
    }
}

impl Jobs {
    pub fn kill_all(&self) {
        for pid in self.pids() {
            kill_group(pid);
        }
    }
}

impl Drop for Jobs {
    /// A subagent's jobs end with it.
    fn drop(&mut self) {
        self.kill_all();
    }
}

impl Job {
    fn state(&self) -> String {
        match &*self.status.lock().unwrap() {
            Some(s) => s.clone(),
            None => format!("running for {}s", self.started.elapsed().as_secs()),
        }
    }

    /// Blocks until the job ends, its new output contains `until`, the
    /// timeout passes, or the user cancels.
    fn wait_done(&self, timeout: Duration, until: Option<&str>, cancel: &AtomicBool) {
        let start = Instant::now();
        let from = *self.shown.lock().unwrap();
        while start.elapsed() < timeout && !cancel.load(Ordering::Relaxed) {
            if self.status.lock().unwrap().is_some() {
                // Let the reader drain the last output.
                thread::sleep(Duration::from_millis(50));
                return;
            }
            if until.is_some_and(|needle| self.out.lock().unwrap().since(from).0.contains(needle)) {
                return;
            }
            thread::sleep(Duration::from_millis(200));
        }
    }

    /// Status plus output the model hasn't seen yet.
    fn report(&self) -> String {
        let mut shown = self.shown.lock().unwrap();
        let (text, skipped, end) = self.out.lock().unwrap().since(*shown);
        *shown = end;
        let mut s = format!("job {} ({}): {}\n", self.id, self.command, self.state());
        if skipped > 0 {
            let _ = writeln!(s, "... [{skipped} earlier bytes not kept] ...");
        }
        let text = tail_str(&text, MAX_BYTES);
        s.push_str(if text.is_empty() { "(no new output)" } else { text });
        s
    }
}

/// The last `max` bytes of `s` or fewer, starting on a character boundary.
fn tail_str(s: &str, max: usize) -> &str {
    let mut start = s.len().saturating_sub(max);
    while !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

/// The last `cap` bytes of a stream, plus how many bytes came in total.
struct Tail {
    buf: VecDeque<u8>,
    cap: usize,
    total: usize,
}

impl Tail {
    fn new(cap: usize) -> Self {
        Tail { buf: VecDeque::new(), cap, total: 0 }
    }

    fn push(&mut self, b: &[u8]) {
        self.total += b.len();
        let b = &b[b.len().saturating_sub(self.cap)..];
        let overflow = (self.buf.len() + b.len()).saturating_sub(self.cap);
        self.buf.drain(..overflow);
        self.buf.extend(b);
    }

    /// Output after byte `offset`: (text, bytes no longer kept, new offset).
    fn since(&mut self, offset: usize) -> (String, usize, usize) {
        let kept_from = self.total - self.buf.len();
        let start = offset.max(kept_from);
        let bytes = &self.buf.make_contiguous()[start - kept_from..];
        (String::from_utf8_lossy(bytes).into_owned(), start - offset, self.total)
    }
}

/// Keeps the first and last `cap / 2` bytes of a stream and counts the rest.
struct Capped {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    half: usize,
    total: usize,
}

impl Capped {
    fn new(cap: usize) -> Self {
        Capped { head: Vec::new(), tail: VecDeque::new(), half: cap / 2, total: 0 }
    }

    fn push(&mut self, mut b: &[u8]) {
        self.total += b.len();
        if self.head.len() < self.half {
            let n = (self.half - self.head.len()).min(b.len());
            self.head.extend_from_slice(&b[..n]);
            b = &b[n..];
        }
        if b.len() >= self.half {
            self.tail.clear();
            self.tail.extend(&b[b.len() - self.half..]);
        } else {
            let overflow = (self.tail.len() + b.len()).saturating_sub(self.half);
            self.tail.drain(..overflow);
            self.tail.extend(b);
        }
    }

    fn text(&mut self) -> String {
        let mut s = String::from_utf8_lossy(&self.head).into_owned();
        let omitted = self.total - self.head.len() - self.tail.len();
        if omitted > 0 {
            let _ = write!(
                s,
                "\n\n... [{omitted} bytes omitted here; filter the command or send its output to a file to see them] ...\n\n"
            );
        }
        s.push_str(&String::from_utf8_lossy(self.tail.make_contiguous()));
        s
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_tools;
    use super::*;

    #[test]
    fn tail_str_cuts_on_char_boundary() {
        assert_eq!(tail_str("ééé", 3), "é");
        assert_eq!(tail_str("abc", 10), "abc");
        let big = "é".repeat(MAX_BYTES * 3);
        assert!(tail_str(&big, MAX_BYTES).len() <= MAX_BYTES);
    }

    #[test]
    fn keeps_cwd_and_reports_exit_code() {
        let (t, dir) = test_tools("bash");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let no = AtomicBool::new(false);
        // A move is said out loud; staying put is not.
        let moved = t.run("bash", &json!({"command": "cd sub"}), &no).unwrap();
        assert!(moved.starts_with("(working directory is now ") && moved.ends_with("/sub)"), "{moved}");
        assert!(t.run("bash", &json!({"command": "pwd"}), &no).unwrap().ends_with("/sub\n"));
        let err = t.run("bash", &json!({"command": "echo oops >&2; exit 3"}), &no).unwrap_err();
        assert_eq!(err, "oops\nExit code 3");
    }

    #[test]
    fn timeout_kills() {
        let (t, _) = test_tools("timeout");
        let start = Instant::now();
        let err = t.run("bash", &json!({"command": "sleep 30", "timeout": 1}), &AtomicBool::new(false)).unwrap_err();
        assert!(err.contains("Timed out"));
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn background_job_wait_until_and_finish() {
        let (t, _) = test_tools("jobs");
        let no = AtomicBool::new(false);
        let cmd = "echo starting; sleep 0.3; echo ready; sleep 0.3; echo bye";
        let started = t.run("bash", &json!({"command": cmd, "background": true}), &no).unwrap();
        assert!(started.contains("job 1"));

        let ready = t.run("jobs", &json!({"action": "wait", "id": 1, "until": "ready", "timeout": 5}), &no).unwrap();
        assert!(ready.contains("ready") && ready.contains("running"), "{ready}");

        let end = t.run("jobs", &json!({"action": "wait", "id": 1, "timeout": 5}), &no).unwrap();
        // Only output since the last check: "starting" was already shown.
        let lines: Vec<&str> = end.lines().skip(1).collect();
        assert!(end.contains("exit 0") && lines == ["bye"], "{end}");

        let finished = t.finished_jobs();
        assert_eq!((finished.len(), finished[0].status.as_str()), (1, "exit 0"));
    }

    #[test]
    fn background_job_kill() {
        let (t, _) = test_tools("kill");
        let no = AtomicBool::new(false);
        t.run("bash", &json!({"command": "sleep 30", "background": true}), &no).unwrap();
        assert_eq!(t.running_jobs(), 1);
        let out = t.run("jobs", &json!({"action": "kill", "id": 1}), &no).unwrap();
        assert!(out.contains("killed"), "{out}");
        assert_eq!(t.running_jobs(), 0);
    }

    #[test]
    fn capped_and_tail() {
        let mut c = Capped::new(10);
        for chunk in [&b"abc"[..], b"defgh", b"ijklmn", b"opqrst"] {
            c.push(chunk);
        }
        assert_eq!(
            c.text(),
            "abcde\n\n... [10 bytes omitted here; filter the command or send its output to a file to see them] ...\n\npqrst"
        );

        let mut t = Tail::new(4);
        t.push(b"abcdef");
        assert_eq!(t.since(0), ("cdef".into(), 2, 6));
        t.push(b"gh");
        assert_eq!(t.since(6), ("gh".into(), 0, 8));
    }

    fn tool(t: &Tools, name: &str, input: Value) -> Output {
        t.run(name, &input, &AtomicBool::new(false))
    }

    fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
        for _ in 0..200 {
            if f() {
                return;
            }
            thread::sleep(Duration::from_millis(25));
        }
        panic!("timed out waiting for {what}");
    }

    #[test]
    fn the_jobs_tool_lists_reads_waits_and_kills() {
        let (t, _dir) = test_tools("bash-jobs-tool");
        assert_eq!(tool(&t, "jobs", json!({})).unwrap(), "No background jobs.");
        assert_eq!(tool(&t, "jobs", json!({"action": "output"})).unwrap_err(), "missing id");
        assert_eq!(tool(&t, "jobs", json!({"action": "output", "id": 9})).unwrap_err(), "no job 9");

        tool(&t, "bash", json!({"command": "echo ready; sleep 30", "background": true})).unwrap();
        let listed = tool(&t, "jobs", json!({"action": "list"})).unwrap();
        assert!(listed.starts_with("1: echo ready; sleep 30 (running for "), "{listed}");
        assert_eq!(t.jobs(), [(1, "echo ready; sleep 30".to_string(), true)]);
        assert_eq!(t.running_jobs(), 1);

        // Wait until the output has what we're looking for; later calls only show what is new.
        let out = tool(&t, "jobs", json!({"action": "wait", "id": 1, "until": "ready", "timeout": 20})).unwrap();
        assert!(out.starts_with("job 1 (echo ready; sleep 30): running for ") && out.ends_with("\nready\n"), "{out}");
        let again = tool(&t, "jobs", json!({"action": "output", "id": 1})).unwrap();
        assert!(again.ends_with("\n(no new output)"), "{again}");
        assert_eq!(tool(&t, "jobs", json!({"action": "dance", "id": 1})).unwrap_err(), "unknown action: dance");
        assert!(t.child_pids().len() == 1);

        let killed = tool(&t, "jobs", json!({"action": "kill", "id": 1})).unwrap();
        assert!(killed.contains("): killed"), "{killed}");
        assert_eq!(t.jobs(), [(1, "echo ready; sleep 30".to_string(), false)]);
        assert_eq!(t.running_jobs(), 0);
        assert!(!t.kill_job(7));
        assert!(t.child_pids().is_empty());
    }

    #[test]
    fn jobs_can_be_killed_by_the_ui_and_on_shutdown() {
        let (t, _dir) = test_tools("bash-jobs-kill");
        tool(&t, "bash", json!({"command": "sleep 30", "background": true})).unwrap();
        tool(&t, "bash", json!({"command": "sleep 31", "background": true})).unwrap();
        assert_eq!(t.running_jobs(), 2);
        assert!(t.kill_job(1));
        wait_until("job 1 to end", || t.running_jobs() == 1);
        t.shutdown();
        wait_until("shutdown to stop the rest", || t.running_jobs() == 0);
        let status: Vec<bool> = t.jobs().iter().map(|j| j.2).collect();
        assert_eq!(status, [false, false]);
    }

    #[test]
    fn a_job_that_waits_can_be_cancelled() {
        let (t, _dir) = test_tools("bash-jobs-wait");
        tool(&t, "bash", json!({"command": "sleep 30", "background": true})).unwrap();
        let cancel = AtomicBool::new(true);
        let started = Instant::now();
        let out = t.run("jobs", &json!({"action": "wait", "id": 1, "timeout": 60}), &cancel).unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(out.contains("): running for "), "{out}");
        // A finished job is reported at once, with its exit status.
        tool(&t, "bash", json!({"command": "echo done; exit 3", "background": true})).unwrap();
        let out = tool(&t, "jobs", json!({"action": "wait", "id": 2, "timeout": 20})).unwrap();
        assert!(out.contains("): exit 3\ndone"), "{out}");
        t.shutdown();
    }

    #[test]
    fn only_the_newest_finished_jobs_are_kept() {
        let (t, _dir) = test_tools("bash-jobs-prune");
        for i in 1..=12 {
            tool(&t, "bash", json!({"command": format!("echo {i}"), "background": true})).unwrap();
            wait_until("the job to finish", || t.running_jobs() == 0);
        }
        let ids: Vec<usize> = t.jobs().iter().map(|j| j.0).collect();
        assert_eq!(ids, [5, 6, 7, 8, 9, 10, 11, 12]);
        assert_eq!(tool(&t, "jobs", json!({"action": "output", "id": 1})).unwrap_err(), "no job 1");
        // Each one that finished was reported once.
        let mut reported = 0;
        wait_until("all reports", || {
            reported += t.finished_jobs().len();
            reported >= 12
        });
    }

    #[test]
    fn a_job_that_outgrows_its_buffer_says_how_much_was_lost() {
        let (t, _dir) = test_tools("bash-jobs-tail");
        let cmd = "head -c 70000 /dev/zero | tr '\\0' z; echo END; sleep 30";
        tool(&t, "bash", json!({"command": cmd, "background": true})).unwrap();
        // END is the last thing printed, so once it shows, all 70 KB have arrived.
        let out = tool(&t, "jobs", json!({"action": "wait", "id": 1, "until": "END", "timeout": 20})).unwrap();
        assert!(out.contains("\n... [") && out.contains(" earlier bytes not kept] ...\n"), "{}", &out[..200]);
        assert!(out.ends_with("zzzEND\n"));
        t.shutdown();
    }

    #[test]
    fn tail_keeps_the_end_and_counts_what_was_dropped() {
        let mut tail = Tail::new(10);
        tail.push(b"abcdef");
        assert_eq!(tail.since(0), ("abcdef".to_string(), 0, 6));
        tail.push(b"ghijklmn");
        // 14 bytes arrived, the last 10 are kept.
        assert_eq!(tail.since(0), ("efghijklmn".to_string(), 4, 14));
        assert_eq!(tail.since(6), ("ghijklmn".to_string(), 0, 14));
        tail.push(&[b'x'; 30]);
        assert_eq!(tail.since(14), ("xxxxxxxxxx".to_string(), 20, 44));
        assert_eq!(tail.since(44), (String::new(), 0, 44));
    }

    #[test]
    fn capped_output_keeps_both_ends() {
        let mut c = Capped::new(10);
        c.push(b"abc");
        assert_eq!(c.text(), "abc");
        c.push(b"defghijklmnopqrstuvwxyz");
        assert_eq!(
            c.text(),
            "abcde\n\n... [16 bytes omitted here; filter the command or send its output to a file to see them] ...\n\nvwxyz"
        );
        let mut one_big = Capped::new(4);
        one_big.push(b"0123456789");
        assert_eq!(
            one_big.text(),
            "01\n\n... [6 bytes omitted here; filter the command or send its output to a file to see them] ...\n\n89"
        );
    }

    #[test]
    fn long_command_output_is_cut_in_the_middle() {
        let (t, _dir) = test_tools("bash-long-output");
        let out = tool(&t, "bash", json!({"command": "head -c 120000 /dev/zero | tr '\\0' q"})).unwrap();
        assert!(out.contains("bytes omitted"), "{}", &out[..100]);
        assert!(out.len() < MAX_BYTES + 100);
        assert!(out.starts_with("qqq") && out.ends_with("qqq"));
        assert_eq!(tool(&t, "bash", json!({})).unwrap_err(), "missing command");
    }

    #[test]
    fn user_shell_runs_without_a_model() {
        let (t, dir) = test_tools("bash-user-shell");
        let cancel = AtomicBool::new(false);
        assert_eq!(t.user_shell("pwd", &cancel).unwrap().trim_end(), dir.canonicalize().unwrap().to_str().unwrap());
        assert!(t.user_shell("exit 5", &cancel).unwrap_err().contains("Exit code 5"));
        assert_eq!(t.user_shell("true", &cancel).unwrap(), "(no output)");
    }
}
