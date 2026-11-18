// Tasks: runs that carry on in the background, each with an id. This is how
// another program, or another agent, hands work to claude-jr without waiting
// for it: start several, do something else, collect them. (The command line
// for them is in cli/task.rs.)
//
// In a git repository each task works in a worktree of its own (see
// worktree.rs) unless told otherwise, so they can't collide.
//
// A task is a folder in the data folder, tasks/ID/, holding:
//   task.json     what was asked, where, when, and the worker's process id
//   log.jsonl     the run log, written as it happens
//   output.txt    what the worker printed for people
//   result.json   the outcome, once there is one

use super::budget;
use crate::store::config::data_dir;
use crate::sys::os::{self, alive};
use serde_json::{Value, json};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Set for a worker: its task folder, where it leaves the result.
pub const ENV: &str = "CLAUDE_JR_TASK";

fn tasks_dir() -> PathBuf {
    data_dir().join("tasks")
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

pub(super) fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

/// Written whole or not at all, so a reader never finds half a file.
pub fn write_json(path: &Path, value: &Value) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, format!("{value:#}\n"))?;
    fs::rename(tmp, path)
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

/// The model a worker started with these flags uses, and whether that model
/// is asked through Anthropic's own API (which needs Anthropic's key). It is
/// the last -m among the flags; else the one set for workers; else the usual
/// choice beside the main model (see `api::worker`).
pub fn worker_model(pass: &[String]) -> Result<(String, bool), String> {
    let file = crate::store::config::Config::load()?;
    let named = pass.windows(2).rev().find(|w| matches!(w[0].as_str(), "-m" | "--model")).map(|w| w[1].clone());
    let model =
        named.or_else(|| file.worker_setting()).unwrap_or_else(|| crate::api::worker(&file.main_model()).to_string());
    let direct =
        crate::api::provider::resolve(&model, &file.providers).is_none() && crate::api::cloud::of(&model).is_none();
    Ok((model, direct))
}

/// Starts a worker in the background and returns the task's id.
pub fn start(start: &Start) -> Result<String, String> {
    if start.prompt.trim().is_empty() {
        return Err("the task needs a prompt".into());
    }
    let (model, direct) = worker_model(&start.pass)?;
    if direct && crate::store::config::api_key().unwrap_or_default().is_empty() {
        return Err("no API key: run claudejr and type /login, or set ANTHROPIC_API_KEY".into());
    }
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let in_git = Command::new("git")
        .args(["rev-parse", "--verify", "-q", "HEAD"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    let id = crate::work::worktree::new_id();
    let dir = tasks_dir().join(&id);
    fs::create_dir_all(&dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
    let budget = match &start.group {
        Some(group) => Some(budget::open(group, start.budget)?),
        None if start.budget.is_some() => {
            return Err("--budget needs --group NAME: a budget is shared by a group".into());
        }
        None => None,
    };

    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let output = File::create(dir.join("output.txt")).map_err(|e| e.to_string())?;
    let mut cmd = Command::new(exe);
    // Named last, so the worker uses the model that is recorded for it.
    cmd.args(&start.pass).args(["-m", &model]);
    if start.worktree && in_git {
        cmd.arg("--worktree");
    }
    cmd.arg("--log").arg(dir.join("log.jsonl")).arg("-p").arg(&start.prompt);
    cmd.env(ENV, &dir).env("CLAUDE_JR_ID", &id);
    if let Some(budget) = &budget {
        cmd.env(budget::ENV, budget);
    }
    // Its own process group, and nothing tied to this terminal: it carries
    // on when the program that started it has gone.
    cmd.stdin(Stdio::null()).stdout(output.try_clone().map_err(|e| e.to_string())?).stderr(output);
    os::apart(&mut cmd, true);
    let child = cmd.spawn().map_err(|e| format!("couldn't start the worker: {e}"))?;
    let task = json!({
        "id": id,
        "prompt": start.prompt,
        "cwd": cwd,
        "started_ms": now_ms(),
        "pid": child.id(),
        "model": model,
        "group": start.group,
    });
    write_json(&dir.join("task.json"), &task).map_err(|e| e.to_string())?;
    Ok(id)
}

/// The folder of the task `id` names: the whole id, or the start of it if
/// only one task begins that way.
pub fn find(id: &str) -> Result<PathBuf, String> {
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
    // Asked first, so it can commit what it has: a signal, or on Windows
    // (which has none to send) a file it watches for.
    #[cfg(unix)]
    os::terminate(pid);
    #[cfg(windows)]
    let _ = fs::write(dir.join("stop"), "");
    let start = Instant::now();
    while alive(pid) && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(50));
    }
    if alive(pid) {
        os::kill_tree(pid as u32);
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
    let _one_at_a_time = crate::work::worktree::lock();
    let _ = git(&repo, &["worktree", "remove", "--force", &path]);
    let _ = git(&repo, &["branch", "-q", "-d", &branch]);
    Ok(format!("merged {branch}"))
}

/// What a finished task changed, as a diff against the commit it started from.
pub fn diff(id: &str) -> Result<String, String> {
    let dir = find(id)?;
    let task = state(&dir);
    if task["status"] == "running" {
        return Err(format!("{} is still running; its diff is there when it has finished", name(&dir)));
    }
    let w = &task["result"]["worktree"];
    let (Some(repo), Some(base), Some(branch)) = (w["repository"].as_str(), w["base"].as_str(), w["branch"].as_str())
    else {
        return Err(format!("{} left no branch (it changed nothing, or didn't run in a worktree)", name(&dir)));
    };
    let diff = git(repo, &["diff", base, branch])?;
    Ok(if diff.is_empty() { "(no differences)".into() } else { diff })
}

/// The last `last` steps of a task, a line each: what was called, whether
/// it was allowed, how it went.
pub fn steps(id: &str, last: usize) -> Result<String, String> {
    let dir = find(id)?;
    let log = fs::read_to_string(dir.join("log.jsonl")).unwrap_or_default();
    let steps: Vec<Value> =
        log.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter(|l| l["type"] == "step").collect();
    if steps.is_empty() {
        return Ok("(no steps yet)".into());
    }
    let lines: Vec<String> = steps[steps.len().saturating_sub(last.max(1))..]
        .iter()
        .map(|s| {
            let input: String = s["input"].to_string().chars().take(160).collect();
            let how = match (s["allowed_by"].is_null(), s["ok"].as_bool()) {
                (true, _) => "refused",
                (_, Some(true)) => "ok",
                _ => "failed",
            };
            let result: String =
                s["result"].as_str().unwrap_or("").lines().next().unwrap_or("").chars().take(120).collect();
            let agent = s["agent"].as_str().map(|a| format!("[{a}] ")).unwrap_or_default();
            format!("{agent}{} {input} -> {how}: {result}", s["tool"].as_str().unwrap_or(""))
        })
        .collect();
    Ok(format!("{} steps in all; the last {}:\n{}", steps.len(), lines.len(), lines.join("\n")))
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
        let _one_at_a_time = crate::work::worktree::lock();
        let _ = git(&repo, &["worktree", "remove", "--force", &path]);
        let _ = git(&repo, &["branch", "-q", "-D", &branch]);
    }
    fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(format!("dropped {}", name(&dir)))
}

pub fn first_line(text: &str, max: usize) -> String {
    text.lines().next().unwrap_or("").chars().take(max).collect()
}
