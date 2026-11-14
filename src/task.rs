// Tasks: runs that carry on in the background, each with an id.
//
//   claudejr task start -y "fix the flaky test"     prints the task's id at once
//   claudejr task list                              what is running, what finished
//   claudejr task wait [ID...]                      blocks until they finish
//   claudejr task result ID                         the outcome as JSON (or how far it is)
//   claudejr task log ID [-f]                       every step it took (see log.rs)
//   claudejr task cancel ID
//   claudejr task merge ID                          takes its branch into yours
//   claudejr task drop ID                           throws it away, branch and all
//
// This is how another program, or another agent, hands work to claude-jr
// without waiting for it: start several, do something else, collect them.
// In a git repository each task works in a worktree of its own (see
// worktree.rs) unless --no-worktree is given, so they can't collide.
//
// A task is a folder in the data folder, tasks/ID/, holding:
//   task.json     what was asked, where, when, and the worker's process id
//   log.jsonl     the run log, written as it happens
//   output.txt    what the worker printed for people
//   result.json   the outcome, once there is one
//
// A group of tasks can share a budget (--group NAME --budget USD): every
// worker adds what it spends to one file, and all stop when it is used up.

use crate::config::data_dir;
use serde_json::{Value, json};
use std::fs::{self, File};
use std::io::{Read, Seek, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Set for a worker: its task folder, where it leaves the result.
pub const ENV: &str = "CLAUDE_JR_TASK";
/// Set for a worker in a group: the file the group's spending is kept in.
pub const BUDGET_ENV: &str = "CLAUDE_JR_BUDGET";

fn tasks_dir() -> PathBuf {
    data_dir().join("tasks")
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

/// Written whole or not at all, so a reader never finds half a file.
pub fn write_json(path: &Path, value: &Value) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, format!("{value:#}\n"))?;
    fs::rename(tmp, path)
}

fn alive(pid: u64) -> bool {
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    // SAFETY: signal 0 sends nothing; it only asks whether the process exists.
    pid > 0 && unsafe { kill(pid as i32, 0) } == 0
}

/// What `task start` was asked for.
#[derive(Default)]
pub struct Start {
    pub prompt: String,
    /// Flags handed on to the worker as they are (-y, --sandbox, -m MODEL, ...).
    pub pass: Vec<String>,
    pub worktree: bool,
    pub group: Option<String>,
    pub budget: Option<f64>,
}

/// Starts a worker in the background and returns the task's id.
pub fn start(start: &Start) -> Result<String, String> {
    if start.prompt.trim().is_empty() {
        return Err("the task needs a prompt".into());
    }
    if crate::config::api_key().unwrap_or_default().is_empty() {
        return Err("no API key: run claudejr and type /login, or set ANTHROPIC_API_KEY".into());
    }
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let in_git = Command::new("git")
        .args(["rev-parse", "--verify", "-q", "HEAD"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    let id = crate::worktree::new_id();
    let dir = tasks_dir().join(&id);
    fs::create_dir_all(&dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
    let budget = match &start.group {
        Some(group) => Some(open_budget(group, start.budget)?),
        None if start.budget.is_some() => {
            return Err("--budget needs --group NAME: a budget is shared by a group".into());
        }
        None => None,
    };

    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let output = File::create(dir.join("output.txt")).map_err(|e| e.to_string())?;
    let mut cmd = Command::new(exe);
    cmd.args(&start.pass);
    if start.worktree && in_git {
        cmd.arg("--worktree");
    }
    cmd.arg("--log").arg(dir.join("log.jsonl")).arg("-p").arg(&start.prompt);
    cmd.env(ENV, &dir).env("CLAUDE_JR_ID", &id);
    if let Some(budget) = &budget {
        cmd.env(BUDGET_ENV, budget);
    }
    // Its own process group, and nothing tied to this terminal: it carries
    // on when the program that started it has gone.
    cmd.stdin(Stdio::null()).stdout(output.try_clone().map_err(|e| e.to_string())?).stderr(output).process_group(0);
    let child = cmd.spawn().map_err(|e| format!("couldn't start the worker: {e}"))?;
    let task = json!({
        "id": id,
        "prompt": start.prompt,
        "cwd": cwd,
        "started_ms": now_ms(),
        "pid": child.id(),
        "group": start.group,
    });
    write_json(&dir.join("task.json"), &task).map_err(|e| e.to_string())?;
    Ok(id)
}

/// The folder of the task `id` names: the whole id, or the start of it if
/// only one task begins that way.
fn find(id: &str) -> Result<PathBuf, String> {
    let exact = tasks_dir().join(id);
    if !id.is_empty() && exact.join("task.json").is_file() {
        return Ok(exact);
    }
    let matches: Vec<PathBuf> = all().into_iter().filter(|d| name(d).starts_with(id) && !id.is_empty()).collect();
    match matches.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(format!("no task {id:?} (see claudejr task list)")),
        _ => Err(format!("{id:?} is the start of {} tasks' ids; give more of it", matches.len())),
    }
}

fn name(dir: &Path) -> String {
    dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Every task's folder, oldest first.
fn all() -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(tasks_dir()) else { return Vec::new() };
    let mut dirs: Vec<(u64, PathBuf)> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter_map(|d| Some((read_json(&d.join("task.json"))?["started_ms"].as_u64()?, d)))
        .collect();
    dirs.sort();
    dirs.into_iter().map(|(_, d)| d).collect()
}

/// Where a task stands: what was asked, plus the result if it has finished,
/// or its latest step if it is still running. `status` is one of running,
/// done, failed, cancelled, died (the worker is gone and left no result).
pub fn state(dir: &Path) -> Value {
    let mut task = read_json(&dir.join("task.json")).unwrap_or_else(|| json!({ "id": name(dir) }));
    if let Some(result) = read_json(&dir.join("result.json")) {
        let status = match (result["ok"].as_bool(), result["cancelled"].as_bool()) {
            (_, Some(true)) => "cancelled",
            (Some(true), _) => "done",
            _ => "failed",
        };
        task["status"] = json!(status);
        task["result"] = result;
        return task;
    }
    if alive(task["pid"].as_u64().unwrap_or(0)) {
        task["status"] = json!("running");
        let log = fs::read_to_string(dir.join("log.jsonl")).unwrap_or_default();
        let steps: Vec<Value> = log.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
        task["steps"] = json!(steps.iter().filter(|l: &&Value| l["type"] == "step").count());
        if let Some(last) = steps.iter().rev().find(|l| l["type"] == "step") {
            task["last_step"] = json!({ "tool": last["tool"], "input": last["input"], "ok": last["ok"] });
        }
    } else {
        task["status"] = json!("died");
        let output = fs::read_to_string(dir.join("output.txt")).unwrap_or_default();
        let tail: Vec<&str> = output.lines().rev().take(5).collect();
        task["error"] = json!(tail.into_iter().rev().collect::<Vec<_>>().join("\n"));
    }
    task
}

pub fn result(id: &str) -> Result<Value, String> {
    Ok(state(&find(id)?))
}

/// The tasks started from this folder (or all of them), oldest first.
pub fn list(everywhere: bool) -> Vec<Value> {
    let here = std::env::current_dir().ok();
    all()
        .iter()
        .map(|d| state(d))
        .filter(|t| everywhere || here.as_deref().is_some_and(|h| t["cwd"].as_str() == h.to_str()))
        .collect()
}

/// Blocks until every task named has finished (all of this folder's running
/// ones if none is named), or `timeout` passes. Returns where each stands.
pub fn wait(ids: &[String], timeout: Option<Duration>) -> Result<Vec<Value>, String> {
    let dirs: Vec<PathBuf> = if ids.is_empty() {
        let running = list(false).into_iter().filter(|t| t["status"] == "running");
        running.filter_map(|t| find(t["id"].as_str()?).ok()).collect()
    } else {
        ids.iter().map(|id| find(id)).collect::<Result<_, _>>()?
    };
    let start = Instant::now();
    loop {
        let states: Vec<Value> = dirs.iter().map(|d| state(d)).collect();
        if states.iter().all(|s| s["status"] != "running") || timeout.is_some_and(|t| start.elapsed() >= t) {
            return Ok(states);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Stops a running task. Its worker stops the commands it started and, in a
/// worktree, still commits what it had done.
pub fn cancel(id: &str) -> Result<String, String> {
    let dir = find(id)?;
    let task = state(&dir);
    if task["status"] != "running" {
        return Ok(format!("{} had already finished ({})", name(&dir), task["status"].as_str().unwrap_or("")));
    }
    let pid = task["pid"].as_u64().unwrap_or(0);
    let _ = Command::new("kill").args(["-TERM", &pid.to_string()]).status();
    let start = Instant::now();
    while alive(pid) && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(50));
    }
    if alive(pid) {
        crate::tools::kill_group(pid as u32);
    }
    if !dir.join("result.json").exists() {
        let _ = write_json(&dir.join("result.json"), &json!({ "ok": false, "cancelled": true, "error": "cancelled" }));
    }
    Ok(format!("cancelled {}", name(&dir)))
}

fn git(dir: &str, args: &[&str]) -> Result<String, String> {
    let out =
        Command::new("git").args(args).current_dir(dir).stdin(Stdio::null()).output().map_err(|e| e.to_string())?;
    let text = |b: &[u8]| String::from_utf8_lossy(b).trim().to_string();
    if out.status.success() {
        Ok(text(&out.stdout))
    } else {
        Err(format!("{}\n{}", text(&out.stdout), text(&out.stderr)).trim().to_string())
    }
}

/// The worktree a finished task left: (repository, path, branch).
fn worktree_of(task: &Value) -> Option<(String, String, String)> {
    let w = &task["result"]["worktree"];
    Some((w["repository"].as_str()?.into(), w["path"].as_str()?.into(), w["branch"].as_str()?.into()))
}

/// Merges a finished task's branch into the branch its repository is on,
/// then removes the task's worktree and branch. A merge that conflicts is
/// undone and reported; nothing is left half-merged.
pub fn merge(id: &str) -> Result<String, String> {
    let dir = find(id)?;
    let task = state(&dir);
    if task["status"] == "running" {
        return Err(format!("{} is still running; wait for it or cancel it", name(&dir)));
    }
    let Some((repo, path, branch)) = worktree_of(&task) else {
        return Err(format!(
            "{} left no branch to merge (it changed nothing, or didn't run in a worktree)",
            name(&dir)
        ));
    };
    if task["result"]["worktree"]["uncommitted"] == true {
        return Err(format!("{branch} has changes that were never committed; look in {path}"));
    }
    if let Err(e) = git(&repo, &["merge", "--no-edit", "-q", &branch]) {
        let conflicts = git(&repo, &["diff", "--name-only", "--diff-filter=U"]).unwrap_or_default();
        let _ = git(&repo, &["merge", "--abort"]);
        return Err(match conflicts.is_empty() {
            true => format!("couldn't merge {branch}: {e}"),
            false => format!(
                "{branch} conflicts with the current branch in: {}. Nothing was merged.",
                conflicts.replace('\n', ", ")
            ),
        });
    }
    let _ = git(&repo, &["worktree", "remove", "--force", &path]);
    let _ = git(&repo, &["branch", "-q", "-d", &branch]);
    Ok(format!("merged {branch}"))
}

/// Forgets a task: stops it if it is running, removes its worktree, its
/// branch and its folder.
pub fn drop(id: &str) -> Result<String, String> {
    let dir = find(id)?;
    if state(&dir)["status"] == "running" {
        cancel(&name(&dir))?;
    }
    // (Read again: a cancelled worker may just have left its worktree behind.)
    if let Some((repo, path, branch)) = worktree_of(&state(&dir)) {
        let _ = git(&repo, &["worktree", "remove", "--force", &path]);
        let _ = git(&repo, &["branch", "-q", "-D", &branch]);
    }
    fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(format!("dropped {}", name(&dir)))
}

// ---- a group's shared budget ---------------------------------------------

/// The file for group `name`'s spending. A budget given now replaces the one
/// it had; without one the group must already exist.
fn open_budget(name: &str, budget: Option<f64>) -> Result<PathBuf, String> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) {
        return Err(format!("a group's name is letters, digits, - _ and . only (got {name:?})"));
    }
    let dir = data_dir().join("groups");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{name}.json"));
    match (budget, path.exists()) {
        (Some(usd), _) => {
            let spent = read_json(&path).and_then(|v| v["spent_usd"].as_f64()).unwrap_or(0.0);
            write_json(&path, &json!({ "budget_usd": usd, "spent_usd": spent })).map_err(|e| e.to_string())?;
        }
        (None, false) => return Err(format!("group {name:?} has no budget yet: give --budget USD")),
        (None, true) => {}
    }
    Ok(path)
}

/// Adds `usd` to what the group has spent and returns (spent, budget). The
/// file is locked meanwhile, so workers adding at once don't lose a sum.
pub fn spend(file: &Path, usd: f64) -> Option<(f64, f64)> {
    let mut f = fs::OpenOptions::new().read(true).write(true).open(file).ok()?;
    f.lock().ok()?;
    let mut text = String::new();
    f.read_to_string(&mut text).ok()?;
    let mut v: Value = serde_json::from_str(&text).ok()?;
    let spent = v["spent_usd"].as_f64().unwrap_or(0.0) + usd;
    v["spent_usd"] = json!(spent);
    f.rewind().ok()?;
    f.set_len(0).ok()?;
    f.write_all(format!("{v:#}\n").as_bytes()).ok()?;
    Some((spent, v["budget_usd"].as_f64().unwrap_or(f64::MAX)))
}

// ---- the command line ------------------------------------------------------

pub const USAGE: &str = "usage: claudejr task <command>

  start [options] PROMPT   start a task in the background; prints its id
        -y, --sandbox, -m MODEL, -e EFFORT, --max-cost USD   as for claudejr -p
        --no-worktree        work in this folder, not in a worktree of its own
        --group NAME         put the task in a group that shares a budget
        --budget USD         the group's budget (with --group)
  list [--all] [--json]    tasks started from this folder (or everywhere)
  wait [ID...] [--timeout SECONDS]   wait for them (all running ones if none is named)
  result ID                where the task stands, as JSON
  log ID [-f]              every step it took, as JSON lines (-f: keep following)
  cancel ID                stop it
  merge ID                 merge its branch into the current one and clean up
  drop ID                  throw it away: worktree, branch and all";

/// Runs `claudejr task ...`. Returns what to print and whether it went well.
pub fn cli(args: &[String]) -> Result<(String, bool), String> {
    let (verb, rest) = args.split_first().ok_or(USAGE)?;
    let one_id = || match rest {
        [id] => Ok(id.as_str()),
        _ => Err(format!("usage: claudejr task {verb} ID")),
    };
    let done = |text: String| Ok((text, true));
    match verb.as_str() {
        "start" => done(start(&parse_start(rest)?)?),
        "list" => {
            let tasks = list(rest.iter().any(|a| a == "--all"));
            done(if rest.iter().any(|a| a == "--json") { json!(tasks).to_string() } else { table(&tasks) })
        }
        "wait" => {
            let mut ids = Vec::new();
            let mut timeout = None;
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                if a == "--timeout" {
                    let secs = it.next().and_then(|s| s.parse::<f64>().ok()).filter(|s| *s >= 0.0);
                    timeout = Some(Duration::from_secs_f64(secs.ok_or("--timeout needs a number of seconds")?));
                } else {
                    ids.push(a.clone());
                }
            }
            let states = wait(&ids, timeout)?;
            let ok = states.iter().all(|s| s["status"] == "done");
            Ok((states.iter().map(Value::to_string).collect::<Vec<_>>().join("\n"), ok))
        }
        "result" => {
            let state = result(one_id()?)?;
            let ok = matches!(state["status"].as_str(), Some("done" | "running"));
            Ok((format!("{state:#}"), ok))
        }
        "log" => {
            let follow = rest.iter().any(|a| a == "-f");
            let ids: Vec<&String> = rest.iter().filter(|a| *a != "-f").collect();
            let [id] = ids.as_slice() else { return Err("usage: claudejr task log ID [-f]".into()) };
            show_log(&find(id)?, follow);
            done(String::new())
        }
        "cancel" => done(cancel(one_id()?)?),
        "merge" => done(merge(one_id()?)?),
        "drop" => done(drop(one_id()?)?),
        "-h" | "--help" | "help" => done(USAGE.into()),
        other => Err(format!("unknown task command: {other}\n\n{USAGE}")),
    }
}

fn parse_start(args: &[String]) -> Result<Start, String> {
    let mut start = Start { worktree: true, ..Start::default() };
    let mut prompt = None;
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
            "--group" => start.group = Some(value()?),
            "--budget" => {
                let v = value()?;
                let usd = v.trim_start_matches('$').parse::<f64>().ok().filter(|usd| *usd > 0.0);
                start.budget = Some(usd.ok_or_else(|| format!("--budget needs an amount in dollars, got {v}"))?);
            }
            flag if flag.starts_with('-') && flag.len() > 1 => {
                return Err(format!("unknown option for task start: {flag}"));
            }
            _ if prompt.is_some() => return Err("give the task as one argument (put it in quotes)".into()),
            _ => prompt = Some(a.clone()),
        }
    }
    start.prompt = prompt.ok_or("usage: claudejr task start [options] PROMPT")?;
    Ok(start)
}

/// The list as a table for people.
fn table(tasks: &[Value]) -> String {
    if tasks.is_empty() {
        return "No tasks here. Start one with: claudejr task start \"...\"".into();
    }
    let mut out = format!("{:<12} {:<10} {:>7}  {:<24} {}", "ID", "STATUS", "COST", "BRANCH", "TASK");
    for t in tasks {
        let cost = t["result"]["cost_usd"].as_f64().map_or(String::new(), |c| format!("${c:.2}"));
        let branch = t["result"]["worktree"]["branch"].as_str().unwrap_or("");
        let prompt: String = t["prompt"].as_str().unwrap_or("").lines().next().unwrap_or("").chars().take(50).collect();
        let (id, status) = (t["id"].as_str().unwrap_or(""), t["status"].as_str().unwrap_or(""));
        out += &format!("\n{id:<12} {status:<10} {cost:>7}  {branch:<24} {prompt}");
    }
    out
}

/// Prints a task's log; with `follow`, keeps printing what is added until
/// the task has finished.
fn show_log(dir: &Path, follow: bool) {
    let mut out = std::io::stdout();
    let mut shown = 0;
    loop {
        let finished = state(dir)["status"] != "running";
        if let Ok(mut f) = File::open(dir.join("log.jsonl")) {
            let mut new = Vec::new();
            if f.seek(std::io::SeekFrom::Start(shown)).is_ok() && f.read_to_end(&mut new).is_ok() {
                // Whole lines only; the rest is still being written.
                let whole = new.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
                let _ = out.write_all(&new[..whole]).and_then(|()| out.flush());
                shown += whole as u64;
            }
        }
        if !follow || finished {
            return;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}
