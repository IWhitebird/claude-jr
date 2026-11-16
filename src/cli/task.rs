// `claudejr task ...`: background tasks from the command line.
//
//   claudejr task start -y "fix the flaky test"     prints the task's id at once
//   claudejr task list                              what is running, what finished
//   claudejr task wait [ID...]                      blocks until they finish
//   claudejr task result ID                         the outcome as JSON (or how far it is)
//   claudejr task log ID [-f]                       every step it took (see work/log.rs)
//   claudejr task cancel ID
//   claudejr task merge ID                          takes its branch into yours
//   claudejr task drop ID                           throws it away, branch and all

use crate::work::task::{Start, cancel, drop, find, first_line, list, merge, result, start, state, wait};
use serde_json::{Value, json};
use std::fs::File;
use std::io::{Read, Seek, Write};
use std::path::Path;
use std::time::Duration;

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
pub fn run(args: &[String]) -> Result<(String, bool), String> {
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
pub(super) fn table(tasks: &[Value]) -> String {
    if tasks.is_empty() {
        return "No tasks here. Start one with: claudejr task start \"...\"".into();
    }
    let mut out = format!("{:<12} {:<10} {:>7}  {:<24} {}", "ID", "STATUS", "COST", "BRANCH", "TASK");
    for t in tasks {
        let cost = t["result"]["cost_usd"].as_f64().map_or(String::new(), |c| format!("${c:.2}"));
        let branch = t["result"]["worktree"]["branch"].as_str().unwrap_or("");
        let prompt = first_line(t["prompt"].as_str().unwrap_or(""), 50);
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
