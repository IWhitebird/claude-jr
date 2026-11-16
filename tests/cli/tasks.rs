// claudejr task: background runs, their branches, cancelling, budgets.

use super::common::*;

fn task_json(e: &Env, args: &[&str]) -> serde_json::Value {
    let o = task(e, args);
    serde_json::from_str(o.out.trim()).unwrap_or_else(|err| panic!("{err}: {}{}", o.out, o.err))
}

/// Waits until `check` holds for the task's state.
fn until(e: &Env, id: &str, what: &str, check: impl Fn(&serde_json::Value) -> bool) -> serde_json::Value {
    let start = Instant::now();
    loop {
        let state = task_json(e, &["result", id]);
        if check(&state) {
            return state;
        }
        assert!(start.elapsed() < TIMEOUT, "{what} never happened: {state:#}");
        std::thread::sleep(Duration::from_millis(30));
    }
}

#[test]
fn a_task_runs_in_the_background_and_its_branch_can_be_merged() {
    let e = Env::new();
    let git = repository(&e);
    let o = task(&e, &["start", "-y", "add the demo file"]);
    ok(&o);
    let id = o.out.trim().to_string();
    assert!(!id.is_empty() && !id.contains(char::is_whitespace), "just the id: {:?}", o.out);

    let o = task(&e, &["wait", &id]);
    ok(&o);
    let state: serde_json::Value = serde_json::from_str(o.out.trim()).unwrap();
    assert_eq!(state["status"], "done", "{state:#}");
    assert_eq!(state["prompt"], "add the demo file");
    let result = &state["result"];
    assert!(result["result"].as_str().unwrap().starts_with("Done. Here's what happened:"));
    assert_eq!(result["files"][0]["path"], "demo.txt");
    assert!(result["cost_usd"].as_f64().unwrap() > 0.0);
    // The task's id names its branch and its worktree.
    assert_eq!(result["worktree"]["branch"], format!("claudejr/{id}"));
    assert_eq!(result["worktree"]["commits"], 1);
    assert!(!e.proj.join("demo.txt").exists(), "the folder it was started from is untouched");

    // The same through `result`, by the start of the id; and it is in the list.
    assert_eq!(task_json(&e, &["result", &id[..4]])["status"], "done");
    let listed = task(&e, &["list"]);
    assert!(
        listed.out.contains(&id) && listed.out.contains("done") && listed.out.contains("add the demo file"),
        "{}",
        listed.out
    );
    assert_eq!(task_json(&e, &["list", "--json"])[0]["id"], id.as_str());

    // Its log has every step, and what allowed each.
    let log = json_lines(&task(&e, &["log", &id]).out);
    let tools: Vec<&str> = log.iter().filter(|l| l["type"] == "step").map(|l| l["tool"].as_str().unwrap()).collect();
    assert_eq!(tools, ["bash", "write", "read", "edit", "bash"]);
    assert_eq!(log.last().unwrap()["type"], "result");

    // Merging brings the work in and clears the worktree and the branch away.
    let o = task(&e, &["merge", &id]);
    ok(&o);
    assert_eq!(std::fs::read_to_string(e.proj.join("demo.txt")).unwrap(), "one\nTWO\nthree\n");
    assert_eq!(git(&["branch", "--list", "claudejr/*"]), "");
    assert_eq!(git(&["worktree", "list"]).lines().count(), 1);

    // Dropping it forgets it.
    ok(&task(&e, &["drop", &id]));
    assert!(task(&e, &["list"]).out.contains("No tasks here"));
    let gone = task(&e, &["result", &id]);
    assert_eq!(gone.code, 2);
    assert!(gone.err.contains("no task"), "{}", gone.err);
}

#[test]
fn several_tasks_run_at_once_each_on_its_own_branch() {
    let e = Env::new();
    let git = repository(&e);
    let ids: Vec<String> =
        (0..5).map(|n| task(&e, &["start", "-y", &format!("task {n}")]).out.trim().to_string()).collect();
    // With no ids, wait takes every running task started here.
    let o = task(&e, &["wait"]);
    ok(&o);
    let states = json_lines(&o.out);
    assert!(states.len() <= 5, "{}", o.out);
    for id in &ids {
        let state = task_json(&e, &["result", id]);
        assert_eq!(state["status"], "done", "{state:#}");
        assert_eq!(git(&["show", &format!("claudejr/{id}:demo.txt")]), "one\nTWO\nthree");
    }
    // The first merges; the second touched the same lines the same way, so it
    // merges too or is reported cleanly, never left half-merged.
    ok(&task(&e, &["merge", &ids[0]]));
    let second = task(&e, &["merge", &ids[1]]);
    assert!(second.code == 0 || second.err.contains("conflicts with the current branch"), "{}", second.err);
    assert_eq!(git(&["status", "--porcelain"]), "", "no merge left open");
    // Dropping the rest removes their branches.
    for id in &ids[2..] {
        ok(&task(&e, &["drop", id]));
    }
    let left = git(&["branch", "--list", "claudejr/*"]);
    assert!(left.lines().count() <= 1, "{left}");
}

#[test]
fn a_cancelled_task_stops_its_commands_and_keeps_what_it_did() {
    let e = Env::with(&[("MOCK_CMD", "echo early > early.txt; sleep 300")]);
    let git = repository(&e);
    let id = task(&e, &["start", "-y", "do something slow"]).out.trim().to_string();
    let running = until(&e, &id, "the command starting", |s| {
        s["status"] == "running" && git(&["worktree", "list"]).lines().count() == 2
    });
    assert_eq!(running["status"], "running");
    let worktree = e.data.join("claude-jr/worktrees").join(format!("proj-{id}"));
    let start = Instant::now();
    while !worktree.join("early.txt").exists() {
        assert!(start.elapsed() < TIMEOUT, "the command never ran");
        std::thread::sleep(Duration::from_millis(20));
    }

    let o = task(&e, &["cancel", &id]);
    ok(&o);
    assert!(o.out.contains("cancelled"), "{}", o.out);
    let state = task_json(&e, &["result", &id]);
    assert_eq!(state["status"], "cancelled", "{state:#}");
    assert!(!process_in(&worktree), "the sleep was stopped");
    // What it had done is on its branch.
    assert_eq!(state["result"]["worktree"]["commits"], 1, "{state:#}");
    assert_eq!(git(&["show", &format!("claudejr/{id}:early.txt")]), "early");
    // Waiting for it returns at once and says it didn't finish.
    let waited = task(&e, &["wait", &id]);
    assert_eq!(waited.code, 1);
    assert!(task(&e, &["cancel", &id]).out.contains("already finished"));
    ok(&task(&e, &["drop", &id]));
    assert_eq!(git(&["branch", "--list", "claudejr/*"]), "");
}

#[test]
fn a_merge_that_conflicts_is_undone_and_says_where() {
    let e = Env::new();
    let git = repository(&e);
    let id = task(&e, &["start", "-y", "add the demo file"]).out.trim().to_string();
    ok(&task(&e, &["wait", &id]));
    // Meanwhile the same file was written differently on the main branch.
    std::fs::write(e.proj.join("demo.txt"), "something else entirely\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "ours"]);
    let head = git(&["rev-parse", "HEAD"]);

    let o = task(&e, &["merge", &id]);
    assert_eq!(o.code, 2);
    assert!(o.err.contains("conflicts with the current branch in: demo.txt. Nothing was merged."), "{}", o.err);
    // Nothing is left half-merged, and the task's work is still there to look at.
    assert_eq!(git(&["status", "--porcelain"]), "");
    assert_eq!(git(&["rev-parse", "HEAD"]), head);
    assert_eq!(std::fs::read_to_string(e.proj.join("demo.txt")).unwrap(), "something else entirely\n");
    assert_eq!(git(&["show", &format!("claudejr/{id}:demo.txt")]), "one\nTWO\nthree");
    assert_eq!(task_json(&e, &["result", &id])["status"], "done");
    ok(&task(&e, &["drop", &id]));
}

#[test]
fn a_running_task_cannot_be_merged_and_its_log_can_be_followed() {
    let e = Env::with(&[("MOCK_CMD", "sleep 1.5"), ("MOCK_ROUNDS", "1")]);
    let _git = repository(&e);
    let id = task(&e, &["start", "-y", "-m", "claude-haiku-5-5", "slow one"]).out.trim().to_string();
    until(&e, &id, "the worker starting", |s| s["status"] == "running");
    let o = task(&e, &["merge", &id]);
    assert_eq!(o.code, 2);
    assert!(o.err.contains("is still running"), "{}", o.err);

    // Following the log returns when the task has finished, with all of it.
    let start = Instant::now();
    let followed = task(&e, &["log", &id, "-f"]);
    ok(&followed);
    assert!(start.elapsed() > Duration::from_millis(500), "it waited for the task");
    let lines = json_lines(&followed.out);
    assert_eq!(lines.first().unwrap()["type"], "start");
    assert_eq!(lines.last().unwrap()["type"], "result");
    assert_eq!(lines[0]["model"], "claude-haiku-5-5", "options are handed on to the worker");
    assert_eq!(e.requests()[0]["model"], "claude-haiku-5-5");
    assert_eq!(task_json(&e, &["result", &id])["status"], "done");

    // Two tasks whose ids begin alike can't be told apart by that beginning.
    let other = task(&e, &["start", "-y", "another"]).out.trim().to_string();
    ok(&task(&e, &["wait", &other]));
    let shared: String = id.chars().zip(other.chars()).take_while(|(a, b)| a == b).map(|(a, _)| a).collect();
    if !shared.is_empty() {
        let o = task(&e, &["result", &shared]);
        assert_eq!(o.code, 2);
        assert!(o.err.contains("is the start of 2 tasks' ids"), "{}", o.err);
    }
}

/// A task outlives whatever started it: the shell that ran `task start` can
/// be killed, hang-up signal and all, and the worker carries on to the end.
#[test]
fn a_task_carries_on_after_its_starter_is_gone() {
    let e = Env::with(&[("MOCK_CMD", "sleep 1"), ("MOCK_ROUNDS", "2")]);
    let _git = repository(&e);
    // Started from a shell of its own session, which is then hung up on and killed.
    let mut cmd = Command::new("setsid");
    cmd.arg("sh").arg("-c").arg(format!("'{BIN}' task start -y 'slow work' > id.txt; kill -HUP 0; sleep 30"));
    cmd.current_dir(&e.proj)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &e.home)
        .env("XDG_CONFIG_HOME", &e.config)
        .env("XDG_DATA_HOME", &e.data)
        .env("ANTHROPIC_API_KEY", "mock")
        .env("ANTHROPIC_BASE_URL", format!("http://127.0.0.1:{}", e.port));
    let status = cmd.status().unwrap();
    assert!(!status.success(), "the starting shell was hung up on");
    let id = std::fs::read_to_string(e.proj.join("id.txt")).unwrap().trim().to_string();
    assert!(!id.is_empty());

    assert_eq!(task_json(&e, &["result", &id])["status"], "running", "still going after its starter died");
    let o = task(&e, &["wait", &id]);
    ok(&o);
    let state = task_json(&e, &["result", &id]);
    assert_eq!(state["status"], "done", "{state:#}");
    assert_eq!(state["result"]["worktree"]["commits"], 1);
}

#[test]
fn a_group_of_tasks_shares_one_budget() {
    let e = Env::with(&[("MOCK_ROUNDS", "30")]);
    let _git = repository(&e);
    let first = task(&e, &["start", "-y", "--group", "night", "--budget", "0.01", "one"]).out.trim().to_string();
    // The group exists now, so the second needs no budget of its own.
    let second = task(&e, &["start", "-y", "--group", "night", "two"]).out.trim().to_string();
    let o = task(&e, &["wait", &first, &second]);
    assert_eq!(o.code, 1, "they were stopped, not finished: {}", o.out);
    let states = json_lines(&o.out);
    let spent: f64 = states.iter().map(|s| s["result"]["cost_usd"].as_f64().unwrap()).sum();
    for s in &states {
        assert_eq!(s["status"], "failed", "{s:#}");
        assert!(s["result"]["error"].as_str().unwrap().contains("the group's budget of $0.01 is used up"), "{s:#}");
        assert!(s["result"]["requests"].as_u64().unwrap() < 30, "{s:#}");
    }
    // Together they went past the budget by no more than the request each
    // had under way when it ran out (a request's cost is known only when it
    // comes back), and nowhere near the forty rounds they'd have run.
    assert!((0.01..0.05).contains(&spent), "spent {spent}");

    for (args, want) in [
        (&["start", "--budget", "1", "x"][..], "--budget needs --group"),
        (&["start", "--group", "fresh", "x"][..], "has no budget yet"),
        (&["start", "--group", "a/b", "--budget", "1", "x"][..], "letters, digits"),
        (&["start", "--group", "g", "--budget", "lots", "x"][..], "--budget needs an amount"),
    ] {
        let o = task(&e, args);
        assert_eq!(o.code, 2, "{args:?}");
        assert!(o.err.contains(want), "{args:?}: {}", o.err);
    }
}

#[test]
fn tasks_outside_git_failures_and_bad_commands() {
    // Not a repository: the task works in the folder itself.
    let e = Env::new();
    let id = task(&e, &["start", "-y", "add the demo file"]).out.trim().to_string();
    ok(&task(&e, &["wait", &id]));
    let state = task_json(&e, &["result", &id]);
    assert!(state["result"].get("worktree").is_none(), "{state:#}");
    assert_eq!(state["result"]["files"][0], serde_json::json!({ "path": "demo.txt", "status": "edited" }));
    assert!(e.proj.join("demo.txt").exists());
    // Nothing to merge, and it says why.
    let o = task(&e, &["merge", &id]);
    assert_eq!(o.code, 2);
    assert!(o.err.contains("left no branch to merge"), "{}", o.err);

    // Without -y its edits are refused, and the log says so.
    let id = task(&e, &["start", "edit without asking"]).out.trim().to_string();
    ok(&task(&e, &["wait", &id]));
    let log = json_lines(&task(&e, &["log", &id]).out);
    assert!(log.iter().any(|l| l["type"] == "step" && l["allowed_by"].is_null()), "a refused step is in the log");

    // A run that fails says so in its result.
    let failing = Env::with(&[("MOCK_FAIL", "1:400")]);
    let id = task(&failing, &["start", "-y", "x"]).out.trim().to_string();
    let o = task(&failing, &["wait", &id]);
    assert_eq!(o.code, 1);
    let state = task_json(&failing, &["result", &id]);
    assert_eq!(state["status"], "failed");
    assert!(state["result"]["error"].as_str().unwrap().contains("API error 400"), "{state:#}");

    // A worker that is killed outright leaves no result: the task died.
    let slow = Env::with(&[("MOCK_CMD", "sleep 300")]);
    let id = task(&slow, &["start", "-y", "--no-worktree", "x"]).out.trim().to_string();
    let pid = until(&slow, &id, "the worker starting", |s| s["status"] == "running")["pid"].as_u64().unwrap();
    assert!(Command::new("kill").args(["-KILL", &pid.to_string()]).status().unwrap().success());
    assert_eq!(until(&slow, &id, "the worker dying", |s| s["status"] != "running")["status"], "died");
    // (The command it was running lost its minder; stop it here.)
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        if std::fs::read_link(entry.path().join("cwd")).is_ok_and(|c| c == slow.proj) {
            let _ = Command::new("kill").arg("-KILL").arg(entry.file_name()).status();
        }
    }

    // Commands that can't be right.
    for (args, want) in [
        (&[][..], "usage: claudejr task <command>"),
        (&["frobnicate"][..], "unknown task command: frobnicate"),
        (&["start"][..], "usage: claudejr task start"),
        (&["start", "one", "two"][..], "as one argument"),
        (&["start", "--wat", "x"][..], "unknown option for task start: --wat"),
        (&["start", "-m"][..], "-m needs a value"),
        (&["start", "  "][..], "needs a prompt"),
        (&["result"][..], "usage: claudejr task result ID"),
        (&["result", "nope"][..], "no task \"nope\""),
        (&["wait", "--timeout", "soon"][..], "--timeout needs a number"),
        (&["log"][..], "usage: claudejr task log ID"),
    ] {
        let o = task(&e, args);
        assert_eq!(o.code, 2, "{args:?}: {}", o.out);
        assert!(o.err.contains(want), "{args:?}: {}", o.err);
    }
    assert!(task(&e, &["--help"]).out.contains("merge ID"));
    let mut no_key = e.command(&["task", "start", "x"]);
    no_key.env_remove("ANTHROPIC_API_KEY");
    let o = exec(no_key);
    assert_eq!(o.code, 2);
    assert!(o.err.contains("no API key"), "{}", o.err);
    // A wait that runs out of time says where things stand instead of hanging.
    let id = task(&slow, &["start", "-y", "--no-worktree", "again"]).out.trim().to_string();
    let o = task(&slow, &["wait", &id, "--timeout", "0.3"]);
    assert_eq!(o.code, 1);
    assert!(o.out.contains("\"status\":\"running\""), "{}", o.out);
    ok(&task(&slow, &["drop", &id]));
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
