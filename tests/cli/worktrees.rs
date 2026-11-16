// --worktree: a run in its own git worktree and branch.

use super::common::*;

#[test]
fn a_worktree_run_leaves_its_work_on_a_branch_and_the_folder_alone() {
    let e = Env::new();
    let git = repository(&e);
    std::fs::write(e.proj.join("kept.txt"), "edited but not committed\n").unwrap();

    let o = e.run(&["--worktree", "-y", "-p", "add the demo file\nwith details", "--output-format", "json"]);
    ok(&o);
    let r: serde_json::Value = serde_json::from_str(o.out.trim()).unwrap_or_else(|e| panic!("{e}: {}", o.out));
    let w = &r["worktree"];
    let (branch, path) = (w["branch"].as_str().unwrap(), PathBuf::from(w["path"].as_str().unwrap()));
    assert!(branch.starts_with("claudejr/"), "{w}");
    assert_eq!((w["commits"].as_u64(), w["uncommitted"].as_bool()), (Some(1), Some(false)), "{w}");
    assert_eq!(r["files"][0]["path"], "demo.txt", "the files are listed as they were before the commit");

    // The work is on the branch, as one commit named after the task.
    assert_eq!(git(&["show", &format!("{branch}:demo.txt")]), "one\nTWO\nthree");
    assert_eq!(git(&["log", "-1", "--format=%s", branch]), "add the demo file");
    assert_eq!(git(&["rev-list", "--count", &format!("main..{branch}")]), "1");
    // The folder it was started from is exactly as it was.
    assert!(!e.proj.join("demo.txt").exists());
    assert_eq!(std::fs::read_to_string(e.proj.join("kept.txt")).unwrap(), "edited but not committed\n");
    assert_eq!(git(&["rev-parse", "--abbrev-ref", "HEAD"]), "main");
    // The worktree started from the commit, and lives outside the repository.
    assert_eq!(std::fs::read_to_string(path.join("kept.txt")).unwrap(), "as committed\n");
    assert!(path.starts_with(&e.data) && !path.starts_with(&e.proj), "{}", path.display());
    assert!(o.err.contains(&format!("git merge {branch}")), "{}", o.err);

    // It merges cleanly.
    git(&["stash", "-q"]);
    git(&["merge", "-q", branch]);
    assert!(e.proj.join("demo.txt").exists());
}

#[test]
fn a_worktree_with_nothing_in_it_is_removed_and_bad_places_are_refused() {
    let e = Env::with(&[("MOCK_ROUNDS", "0")]);
    let git = repository(&e);
    let o = e.run(&["--worktree", "-p", "just answer", "--output-format", "json"]);
    ok(&o);
    let r: serde_json::Value = serde_json::from_str(o.out.trim()).unwrap();
    assert!(r["worktree"].is_null(), "{r}");
    assert_eq!(git(&["branch", "--list", "claudejr/*"]), "", "the branch is gone");
    assert_eq!(git(&["worktree", "list"]).lines().count(), 1, "and so is the worktree");
    assert!(o.err.contains("Nothing was changed"), "{}", o.err);

    // From a subfolder, the run works in the same subfolder of the worktree.
    let e = Env::with(&[("MOCK_ROUNDS", "1"), ("MOCK_CMD", "pwd; ls")]);
    let git = repository(&e);
    let mut cmd = e.command(&["--worktree", "-y", "-p", "look around", "--output-format", "json"]);
    cmd.current_dir(e.proj.join("sub"));
    let o = exec(cmd);
    ok(&o);
    let r: serde_json::Value = serde_json::from_str(o.out.trim()).unwrap();
    assert_eq!(r["files"][0]["path"], "sub/demo.txt", "paths are from the top of the repository: {r}");
    let branch = r["worktree"]["branch"].as_str().unwrap();
    assert_eq!(git(&["show", &format!("{branch}:sub/demo.txt")]), "one\nTWO\nthree");

    // Not a repository, or one with no commit to start from.
    let e = Env::new();
    let o = e.run(&["--worktree", "-y", "-p", "x"]);
    assert_eq!(o.code, 2);
    assert!(o.err.contains("a worktree needs a git repository"), "{}", o.err);
    assert!(Command::new("git").args(["init", "-q"]).current_dir(&e.proj).status().unwrap().success());
    let o = e.run(&["--worktree", "-y", "-p", "x"]);
    assert_eq!(o.code, 2);
    assert!(o.err.contains("needs at least one commit"), "{}", o.err);
    assert!(e.requests().is_empty());
}

#[test]
fn worktree_runs_side_by_side_do_not_meet() {
    let e = Env::new();
    let git = repository(&e);
    let runs: Vec<_> = (0..16)
        .map(|_| {
            let cmd = e.command(&["--worktree", "-y", "-p", "add the demo file", "--output-format", "json"]);
            std::thread::spawn(move || exec(cmd))
        })
        .collect();
    let mut branches = std::collections::BTreeSet::new();
    for run in runs {
        let o = run.join().unwrap();
        ok(&o);
        let r: serde_json::Value = serde_json::from_str(o.out.trim()).unwrap();
        assert_eq!(r["worktree"]["commits"], 1, "{r}");
        branches.insert(r["worktree"]["branch"].as_str().unwrap().to_string());
    }
    assert_eq!(branches.len(), 16, "each has its own branch");
    for branch in &branches {
        assert_eq!(git(&["show", &format!("{branch}:demo.txt")]), "one\nTWO\nthree");
    }
    assert!(!e.proj.join("demo.txt").exists());
}

#[test]
fn a_sandboxed_worktree_run_can_commit_but_not_plant_a_hook() {
    if !bwrap_works() {
        eprintln!("bwrap is not usable here");
        return;
    }
    let e = Env::with(&[("MOCK_ROUNDS", "1"), ("MOCK_CMD", "echo made > made.txt")]);
    let git = repository(&e);
    let o = e.run(&["--worktree", "--sandbox", "-y", "-p", "make a file", "--output-format", "json"]);
    ok(&o);
    let r: serde_json::Value = serde_json::from_str(o.out.trim()).unwrap_or_else(|e| panic!("{e}: {}{}", o.out, o.err));
    let w = &r["worktree"];
    assert_eq!((w["commits"].as_u64(), w["problem"].is_null()), (Some(1), true), "{w} {}", o.err);
    let branch = w["branch"].as_str().unwrap();
    assert_eq!(git(&["show", &format!("{branch}:made.txt")]), "made");
    assert!(!e.proj.join("made.txt").exists());
}
