// Todos, subagents, background jobs, skills, MCP tools, memory and the auto-mode
// reviewer; and an MCP server that won't start.

use super::common::*;

#[test]
fn features_scenario() {
    let e = Env::with(&[("MOCK_SCENARIO", "features")]);
    e.write_config(&mcp_config(&e));
    std::fs::write(e.proj.join("notes.txt"), "line one\nline two\n").unwrap();
    let skill = e.proj.join(".claude/skills/demo");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: demo\ndescription: A demo skill for tests\n---\nSKILL-BODY-MARKER: do the demo thing.\n",
    )
    .unwrap();

    let report = e.root.join("report.json");
    let mut cmd = e.command(&["-p", "exercise everything", "--dangerously-skip-permissions"]);
    cmd.env("CLAUDE_JR_REPORT", &report);
    let o = exec(cmd);
    ok(&o);
    assert!(o.out.contains("All features checked."), "{}", o.out);
    assert!(o.err.contains("● agent: check notes"), "{}", o.err);
    assert!(o.err.contains("● agent: check notes again"), "{}", o.err);
    let sent = e.requests_text();
    assert!(sent.contains("The user isn't available"), "ask_user result missing");
    assert!(sent.contains("Subagent report: notes.txt has 2 lines."), "subagent report not returned");
    assert!(sent.contains("served"), "jobs wait did not see the output");
    assert!(sent.contains("SKILL-BODY-MARKER"), "skill body not returned");
    assert!(sent.contains("echoed: ping"), "MCP tool result missing");
    assert!(sent.contains("mcp__demo__echo"), "MCP tool was not offered");
    assert!(sent.contains("reviewed-and-allowed"));
    let r: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
    assert_eq!(r["ok"], true);
    assert!(r["requests"].as_u64().unwrap() >= 8);
    assert!(!process_in(&e.proj), "background job left running");
}

#[test]
fn features_with_the_auto_mode_reviewer() {
    let e = Env::with(&[("MOCK_SCENARIO", "features")]);
    std::fs::write(e.proj.join("notes.txt"), "a\nb\n").unwrap();
    e.write_config(r#"{"mode":"auto"}"#);
    let o = e.run(&["-p", "exercise everything"]);
    ok(&o);
    let reviews: Vec<_> =
        e.requests().into_iter().filter(|r| r["system"].to_string().contains("You review actions")).collect();
    assert!(!reviews.is_empty(), "the reviewer was never asked; stderr: {}", o.err);
    let text = reviews.iter().map(|r| r.to_string()).collect::<String>();
    assert!(text.contains("rm -rf ./build"), "{text}");
    assert!(e.requests_text().contains("deletes files"), "{}", o.err);
}

#[test]
fn agents_scenario_stops_background_jobs() {
    let e = Env::with(&[("MOCK_SCENARIO", "agents")]);
    let o = e.run(&["-p", "start the dev server", "-y"]);
    ok(&o);
    assert!(o.out.contains("Started the dev server."), "{}", o.out);
    assert!(o.err.contains("● agent: scan components") && o.err.contains("● agent: check styles"), "{}", o.err);
    assert!(e.requests_text().matches("Found it.").count() >= 2, "both subagents reported");
    assert!(!process_in(&e.proj), "the background server was left running");
}

#[test]
fn memory_scenario_writes_the_note() {
    let e = Env::with(&[("MOCK_SCENARIO", "memory")]);
    let o = e.run(&["-p", "remember that we use pnpm", "-y"]);
    ok(&o);
    assert!(o.out.contains("Noted."), "{}", o.out);
    let mut found = false;
    let mut stack = vec![e.data.clone()];
    while let Some(d) = stack.pop() {
        for ent in std::fs::read_dir(&d).unwrap().flatten() {
            let p = ent.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_none_or(|x| x != "jsonl")
                && std::fs::read_to_string(&p).is_ok_and(|t| t.contains("uses pnpm, not npm"))
            {
                found = true;
            }
        }
    }
    assert!(found, "no memory file under {}", e.data.display());
    ok(&e.run(&["-p", "what do you know", "-y"]));
    let last = e.requests().pop().unwrap();
    assert!(last["system"].to_string().contains("uses pnpm, not npm"), "memory not in the system prompt");
}

#[test]
fn mcp_server_that_cannot_start_is_reported() {
    let e = Env::with(&[("MOCK_ROUNDS", "0")]);
    e.write_config(
        r#"{"mcp_servers":{"ghost":{"command":"/nonexistent/binary"},"empty":{},"remote":{"url":"https://example.invalid/mcp","authorization_token":"tok"}}}"#,
    );
    let o = e.run(&["-p", "hi", "-y"]);
    ok(&o);
    assert!(o.err.contains("MCP server ghost"), "{}", o.err);
    assert!(o.err.contains("MCP server empty: needs a command or a url"), "{}", o.err);
    let req = &e.requests()[0];
    assert_eq!(req["mcp_servers"][0]["url"], "https://example.invalid/mcp");
    assert_eq!(req["mcp_servers"][0]["authorization_token"], "tok");
}
