// claudejr fleet: a list of tasks run side by side.

use super::common::*;

#[test]
fn a_fleet_runs_a_list_a_few_at_a_time_and_reports_each() {
    let e = Env::with(&[("MOCK_CMD", "sleep 0.4"), ("MOCK_ROUNDS", "1")]);
    let git = repository(&e);
    let o =
        e.run(&["fleet", "-y", "--max", "2", "first task", "second task", "third task", "fourth task", "fifth task"]);
    ok(&o);
    // The table: every task, in the order given, each with its own branch.
    let rows: Vec<&str> = o.out.lines().filter(|l| l.contains(" task")).collect();
    assert_eq!(rows.len(), 5, "{}", o.out);
    for (row, name) in rows.iter().zip(["first", "second", "third", "fourth", "fifth"]) {
        assert!(row.contains("done") && row.contains("claudejr/") && row.ends_with(&format!("{name} task")), "{row}");
    }
    assert!(o.out.contains("5 of 5 finished, $"), "{}", o.out);
    assert_eq!(git(&["branch", "--list", "claudejr/*"]).lines().count(), 5);
    // Only two ran at once: the third was started after something had finished.
    let progress: Vec<&str> = o.err.lines().collect();
    let third = progress.iter().position(|l| l.starts_with("[3/5] started")).expect("the third was started");
    let first_done = progress.iter().position(|l| l.contains("] done ")).expect("something finished");
    assert!(first_done < third, "{}", o.err);
    assert_eq!(progress.iter().filter(|l| l.contains("] done ")).count(), 5);

    // As JSON, for a program: the same tasks with their results.
    let e = Env::new();
    let _git = repository(&e);
    let o = e.run(&["fleet", "-y", "--json", "one", "two"]);
    ok(&o);
    let states: serde_json::Value = serde_json::from_str(o.out.trim()).unwrap_or_else(|err| panic!("{err}: {}", o.out));
    assert_eq!(states.as_array().unwrap().len(), 2);
    assert_eq!((states[0]["prompt"].as_str(), states[1]["prompt"].as_str()), (Some("one"), Some("two")));
    assert!(
        states.as_array().unwrap().iter().all(|s| s["status"] == "done" && s["result"]["worktree"]["commits"] == 1)
    );
}

#[test]
fn a_fleet_reads_its_list_from_a_file_merges_and_keeps_to_a_budget() {
    let e = Env::new();
    let git = repository(&e);
    std::fs::write(e.root.join("tasks.txt"), "# the night's work\n\nadd the demo file\n   tidy the notes   \n# done\n")
        .unwrap();
    let list = e.root.join("tasks.txt");
    let o = e.run(&["fleet", "-y", "--merge", "--file", list.to_str().unwrap()]);
    ok(&o);
    assert!(o.out.contains("2 of 2 finished"), "{}", o.out);
    assert_eq!(o.out.matches(": merged claudejr/").count(), 2, "{}", o.out);
    assert_eq!(std::fs::read_to_string(e.proj.join("demo.txt")).unwrap(), "one\nTWO\nthree\n");
    assert_eq!(git(&["branch", "--list", "claudejr/*"]), "", "merged branches are cleaned up");
    assert_eq!(git(&["worktree", "list"]).lines().count(), 1);

    // From standard input too.
    let cmd = e.command(&["fleet", "-y", "--json", "--file", "-", "extra one"]);
    std::fs::write(e.root.join("stdin.txt"), "from a pipe\n").unwrap();
    let o = exec_with(cmd, std::fs::File::open(e.root.join("stdin.txt")).unwrap().into());
    ok(&o);
    let states: serde_json::Value = serde_json::from_str(o.out.trim()).unwrap();
    let prompts: Vec<&str> = states.as_array().unwrap().iter().map(|s| s["prompt"].as_str().unwrap()).collect();
    assert_eq!(prompts, ["from a pipe", "extra one"]);

    // A budget for the whole fleet: once it is spent, they all stop, and the run says it failed.
    let long = Env::with(&[("MOCK_ROUNDS", "40")]);
    let _git = repository(&long);
    let o = long.run(&["fleet", "-y", "-m", "claude-opus-5-5", "--budget", "0.01", "--json", "a", "b", "c"]);
    assert_eq!(o.code, 1, "{}", o.out);
    let states: serde_json::Value = serde_json::from_str(o.out.trim()).unwrap();
    let spent: f64 = states.as_array().unwrap().iter().map(|s| s["result"]["cost_usd"].as_f64().unwrap()).sum();
    assert!((0.01..0.06).contains(&spent), "spent {spent}");
    assert!(
        states
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["status"] == "failed" && s["result"]["error"].as_str().unwrap().contains("the group's budget"))
    );
}

#[test]
fn a_fleet_given_nonsense_says_so() {
    let e = Env::new();
    for (args, want) in [
        (&["fleet"][..], "usage: claudejr fleet"),
        (&["fleet", "--max", "0", "x"][..], "--max needs a number from 1 up"),
        (&["fleet", "--max"][..], "--max needs a value"),
        (&["fleet", "--budget", "free", "x"][..], "--budget needs an amount"),
        (&["fleet", "--merge", "--no-worktree", "x"][..], "--merge needs the tasks in worktrees"),
        (&["fleet", "--wat", "x"][..], "unknown option for fleet: --wat"),
        (&["fleet", "--file", "/no/such/list.txt"][..], "/no/such/list.txt"),
    ] {
        let o = e.run(args);
        assert_eq!(o.code, 2, "{args:?}: {}", o.out);
        assert!(o.err.contains(want), "{args:?}: {}", o.err);
    }
    let o = e.run(&["fleet", "--help"]);
    ok(&o);
    assert!(o.out.contains("--merge") && o.out.contains("--max N"), "{}", o.out);
    assert!(e.run(&["task", "--help"]).out.contains("claudejr task <command>"));
    assert!(e.requests().is_empty());
}
