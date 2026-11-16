// Subagents in worktrees of their own, and subagents of subagents.

use super::common::*;

#[test]
fn subagents_work_in_their_own_worktrees_and_the_main_agent_merges_them() {
    let e = Env::with(&[("MOCK_SCENARIO", "team")]);
    let git = repository(&e);
    let log = e.root.join("run.jsonl");
    let o = e.run(&["-y", "-p", "split the work", "--log", log.to_str().unwrap()]);
    ok(&o);
    nothing_refused(&e);
    assert!(o.out.contains("Merged both."), "{}", o.out);
    // Both files arrived by merging the subagents' branches.
    assert_eq!(std::fs::read_to_string(e.proj.join("alpha.txt")).unwrap(), "alpha.txt\n");
    assert_eq!(std::fs::read_to_string(e.proj.join("beta.txt")).unwrap(), "beta.txt\n");
    let merged = git(&["log", "--format=%s", "main"]);
    assert!(
        merged.contains("write alpha") && merged.contains("write beta"),
        "each one's commit is named after its task:\n{merged}"
    );
    // Their folders are gone; only the (merged) branches remain, to be deleted at leisure.
    assert_eq!(git(&["worktree", "list"]).lines().count(), 1);
    assert_eq!(git(&["branch", "--list", "claudejr/*"]).lines().count(), 2);
    assert_eq!(git(&["status", "--porcelain"]), "");

    // Each was told where it works, and its report came back with its branch.
    let sent = e.requests();
    let subs: Vec<&serde_json::Value> =
        sent.iter().filter(|r| r["system"].as_str().unwrap().contains("You are a subagent")).collect();
    assert_eq!(subs.len(), 4, "two subagents, two requests each");
    let told = subs[0]["messages"][0].to_string();
    assert!(told.contains("a git worktree of the repository on its own branch, claudejr/"), "{told}");
    let main_last = sent.iter().rev().find(|r| !r["system"].as_str().unwrap().contains("You are a subagent")).unwrap();
    let reports = main_last["messages"].to_string();
    assert!(reports.contains("Wrote alpha.txt.") && reports.contains("Take it with: git merge claudejr/"), "{reports}");
    assert!(reports.contains("A alpha.txt") && reports.contains("A beta.txt"), "the files each changed are listed");
    // The log says which subagent did what.
    let steps = json_lines(&std::fs::read_to_string(&log).unwrap());
    let writes: Vec<&str> =
        steps.iter().filter(|l| l["tool"] == "write").map(|l| l["agent"].as_str().unwrap()).collect();
    assert_eq!(writes.len(), 2);
    assert!(writes.contains(&"write alpha") && writes.contains(&"write beta"), "{writes:?}");
}

#[test]
fn a_subagent_worktree_needs_a_repository_and_says_so_to_the_model() {
    let e = Env::with(&[("MOCK_SCENARIO", "team")]);
    let o = e.run(&["-y", "-p", "split the work"]);
    ok(&o);
    nothing_refused(&e);
    // The refusal went back as the tool's result, for the model to act on.
    let said = e.requests().last().unwrap()["messages"].to_string();
    assert!(
        said.contains("a worktree needs a git repository") && said.contains("Start the subagent without worktree"),
        "{said}"
    );
    assert!(!e.proj.join("alpha.txt").exists());
}

#[test]
fn a_subagent_can_start_one_of_its_own_and_no_deeper() {
    let e = Env::with(&[("MOCK_SCENARIO", "nested")]);
    let log = e.root.join("run.jsonl");
    let o = e.run(&["-y", "-p", "go deep", "--log", log.to_str().unwrap(), "--output-format", "json"]);
    ok(&o);
    nothing_refused(&e);
    let r: serde_json::Value = serde_json::from_str(o.out.trim()).unwrap();
    assert_eq!(r["result"], "All levels done.");
    assert_eq!(r["requests"], 6, "two for each of the three levels, all counted");

    let sent = e.requests();
    let tools = |marker: &str| -> Vec<String> {
        let req = sent.iter().find(|r| r["messages"][0].to_string().contains(marker)).unwrap();
        req["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect()
    };
    let has_agent = |marker: &str| tools(marker).contains(&"agent".to_string());
    assert!(has_agent("go deep") && has_agent("middle:"), "the main agent and its subagent can delegate");
    assert!(!has_agent("leaf:"), "the subagent's subagent is not offered the tool");
    assert!(!tools("middle:").contains(&"memory".to_string()) && !tools("middle:").contains(&"ask_user".to_string()));

    // The leaf's step is in the log under the chain of agents that led to it.
    let steps = json_lines(&std::fs::read_to_string(&log).unwrap());
    let leaf = steps.iter().find(|l| l["tool"] == "bash").expect("the leaf ran its command");
    assert_eq!(leaf["agent"], "the middle > the leaf");
    assert!(steps.iter().filter(|l| l["type"] == "agent").count() >= 1);
}
