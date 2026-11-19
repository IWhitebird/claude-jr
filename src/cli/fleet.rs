// `claudejr fleet`: a list of tasks in, a table of branches out. It runs
// them a few at a time, each as a background task in its own worktree, waits,
// and prints what each one did.

use super::task::table;
use crate::work::task::{self, Start, find, first_line, merge, state};
use serde_json::{Value, json};
use std::fs;
use std::io::Read;
use std::time::Duration;

pub const USAGE: &str = "usage: claudejr fleet [options] TASK... | --file FILE

Runs every task in the background, a few at a time, each in its own worktree
and branch, and prints what each one did.

  TASK...              the tasks, each one argument (in quotes)
  --file FILE          the tasks, one per line (- for standard input; # starts a comment)
  --max N              how many run at once (default 4)
  --budget USD         stop them all once they have spent this much together
  --merge              merge each finished branch into this one, in order
  --json               print the results as JSON
  -m MODEL             the model for every task (default: the worker model,
                       claude-haiku-5-5 unless \"worker_model\" is set in the config)
  -y, --sandbox, -m MODEL, -e EFFORT, --max-cost USD, --no-worktree   as for task start";

/// What `claudejr fleet` was asked for.
struct Fleet {
    tasks: Vec<String>,
    /// How to start each (the prompt is filled in per task).
    start: Start,
    max: usize,
    merge: bool,
    json: bool,
}

fn parse_fleet(args: &[String]) -> Result<Fleet, String> {
    let mut fleet = Fleet {
        tasks: Vec::new(),
        start: Start { worktree: true, ..Start::default() },
        max: 4,
        merge: false,
        json: false,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().ok_or_else(|| format!("{a} needs a value"));
        match a.as_str() {
            "-y" | "--yes" | "--dangerously-skip-permissions" | "--sandbox" => fleet.start.pass.push(a.clone()),
            "-m" | "--model" | "-e" | "--effort" | "--max-cost" => {
                let v = value()?;
                fleet.start.pass.extend([a.clone(), v]);
            }
            "--no-worktree" => fleet.start.worktree = false,
            "--merge" => fleet.merge = true,
            "--json" => fleet.json = true,
            "--max" => {
                let v = value()?;
                fleet.max = v
                    .parse()
                    .ok()
                    .filter(|n| *n >= 1)
                    .ok_or_else(|| format!("--max needs a number from 1 up, got {v}"))?;
            }
            "--budget" => {
                let v = value()?;
                let usd = v.trim_start_matches('$').parse::<f64>().ok().filter(|usd| *usd > 0.0);
                fleet.start.budget = Some(usd.ok_or_else(|| format!("--budget needs an amount in dollars, got {v}"))?);
            }
            "--file" => {
                let path = value()?;
                let text = if path == "-" {
                    let mut text = String::new();
                    std::io::stdin().read_to_string(&mut text).map_err(|e| e.to_string())?;
                    text
                } else {
                    fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?
                };
                let lines = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#'));
                fleet.tasks.extend(lines.map(String::from));
            }
            "-h" | "--help" => return Err(USAGE.into()),
            flag if flag.starts_with('-') && flag.len() > 1 => return Err(format!("unknown option for fleet: {flag}")),
            _ => fleet.tasks.push(a.clone()),
        }
    }
    if fleet.tasks.is_empty() {
        return Err(USAGE.into());
    }
    if fleet.merge && !fleet.start.worktree {
        return Err("--merge needs the tasks in worktrees; leave out --no-worktree".into());
    }
    Ok(fleet)
}

/// Runs `claudejr fleet ...`. Progress goes to stderr; returns the final
/// table (or JSON) and whether every task finished well.
pub fn run(args: &[String]) -> Result<(String, bool), String> {
    let mut fleet = parse_fleet(args)?;
    // The budget is one group's, named after this run.
    if fleet.start.budget.is_some() {
        fleet.start.group = Some(format!("fleet-{}", crate::work::worktree::new_id()));
    }
    let total = fleet.tasks.len();
    let mut waiting = fleet.tasks.clone().into_iter().enumerate();
    let mut running: Vec<(usize, String)> = Vec::new();
    let mut done: Vec<(usize, Value)> = Vec::new();
    loop {
        while running.len() < fleet.max
            && let Some((n, prompt)) = waiting.next()
        {
            // Only the first sets the budget; the rest join the group.
            let start = Start {
                prompt,
                pass: fleet.start.pass.clone(),
                worktree: fleet.start.worktree,
                group: fleet.start.group.clone(),
                budget: fleet.start.budget.take(),
            };
            let id = task::start(&start)?;
            note!("[{}/{total}] started {id}: {}", n + 1, first_line(&start.prompt, 60));
            running.push((n, id));
        }
        if running.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
        let mut still = Vec::new();
        for (n, id) in running {
            let state = state(&find(&id)?);
            if state["status"] == "running" {
                still.push((n, id));
            } else {
                let cost = state["result"]["cost_usd"].as_f64().map_or(String::new(), |c| format!(", ${c:.2}"));
                note!("[{}/{total}] {} {id}{cost}", done.len() + 1, state["status"].as_str().unwrap_or(""));
                done.push((n, state));
            }
        }
        running = still;
    }
    done.sort_by_key(|(n, _)| *n);
    let mut states: Vec<Value> = done.into_iter().map(|(_, s)| s).collect();

    if fleet.merge {
        for s in &mut states {
            if s["status"] != "done" || s["result"]["worktree"].is_null() {
                continue;
            }
            let outcome = merge(s["id"].as_str().unwrap_or(""));
            s["merged"] = json!(outcome.is_ok());
            s["merge"] = json!(outcome.unwrap_or_else(|why| why));
        }
    }
    let ok = states.iter().all(|s| s["status"] == "done" && s["merged"] != false);
    if fleet.json {
        return Ok((json!(states).to_string(), ok));
    }
    let mut out = table(&states);
    let spent: f64 = states.iter().filter_map(|s| s["result"]["cost_usd"].as_f64()).sum();
    let finished = states.iter().filter(|s| s["status"] == "done").count();
    out += &format!("\n\n{finished} of {total} finished, ${spent:.2} spent.");
    for s in states.iter().filter(|s| s.get("merge").is_some()) {
        out += &format!("\n{}: {}", s["id"].as_str().unwrap_or(""), s["merge"].as_str().unwrap_or(""));
    }
    Ok((out, ok))
}
