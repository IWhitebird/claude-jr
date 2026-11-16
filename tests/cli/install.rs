// claudejr install and uninstall, for each agent.

use super::common::*;

fn read_json(path: &std::path::Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))).unwrap()
}

#[test]
fn installing_for_claude_code_writes_a_server_entry_and_a_skill_and_can_be_undone() {
    let e = Env::new();
    // Someone else's server is already configured here.
    std::fs::write(e.proj.join(".mcp.json"), r#"{"mcpServers": {"other": {"command": "x"}}, "kept": true}"#).unwrap();
    let o = e.run(&["install", "claude-code", "-y", "-m", "claude-haiku-5-5"]);
    ok(&o);
    assert!(
        o.out.contains(".mcp.json") && o.out.contains("SKILL.md") && o.out.contains("Restart the agent"),
        "{}",
        o.out
    );
    let config = read_json(&e.proj.join(".mcp.json"));
    assert_eq!(config["mcpServers"]["other"]["command"], "x", "what was there stays");
    assert_eq!(config["kept"], true);
    let ours = &config["mcpServers"]["claude-jr"];
    assert_eq!(ours["args"], serde_json::json!(["mcp", "-y", "-m", "claude-haiku-5-5"]));
    let skill = std::fs::read_to_string(e.proj.join(".claude/skills/claude-jr/SKILL.md")).unwrap();
    assert!(skill.starts_with("---\nname: claude-jr\ndescription: ") && skill.contains("start_task"), "{skill}");

    // What was written starts a server that answers.
    let mut cmd = Command::new(ours["command"].as_str().unwrap());
    cmd.args(ours["args"].as_array().unwrap().iter().map(|a| a.as_str().unwrap()))
        .current_dir(&e.proj)
        .env("ANTHROPIC_API_KEY", "x");
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    let mut server = cmd.spawn().unwrap();
    writeln!(server.stdin.as_mut().unwrap(), r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{}}}}"#)
        .unwrap();
    let mut line = String::new();
    std::io::BufRead::read_line(&mut std::io::BufReader::new(server.stdout.as_mut().unwrap()), &mut line).unwrap();
    assert!(line.contains("\"name\":\"claude-jr\""), "{line}");
    drop(server.stdin.take());
    assert!(server.wait().unwrap().success(), "it ends when its input closes");

    // Again: nothing to do. With other options: only the entry changes.
    assert!(e.run(&["install", "claude-code", "-y", "-m", "claude-haiku-5-5"]).out.contains("Already set up"));
    ok(&e.run(&["install", "claude-code", "-y", "--budget", "3"]));
    assert_eq!(
        read_json(&e.proj.join(".mcp.json"))["mcpServers"]["claude-jr"]["args"],
        serde_json::json!(["mcp", "-y", "--budget", "3"])
    );

    // Taken out: ours goes, theirs stays; a second time there is nothing to take.
    let o = e.run(&["uninstall", "claude-code"]);
    ok(&o);
    let config = read_json(&e.proj.join(".mcp.json"));
    assert!(config["mcpServers"].get("claude-jr").is_none() && config["mcpServers"]["other"]["command"] == "x");
    assert!(!e.proj.join(".claude/skills/claude-jr").exists());
    assert!(e.run(&["uninstall", "claude-code"]).out.contains("Nothing of claude-jr's was there"));

    // A file made only for us is removed with us.
    let fresh = Env::new();
    ok(&fresh.run(&["install", "claude-code", "-y"]));
    ok(&fresh.run(&["uninstall", "claude-code"]));
    assert!(!fresh.proj.join(".mcp.json").exists() && !fresh.proj.join(".claude/skills").join("claude-jr").exists());

    // A config that isn't valid is left exactly as it is.
    std::fs::write(fresh.proj.join(".mcp.json"), "{ not json").unwrap();
    let o = fresh.run(&["install", "claude-code", "-y"]);
    assert_eq!(o.code, 2);
    assert!(o.err.contains(".mcp.json isn't a JSON object"), "{}", o.err);
    assert_eq!(std::fs::read_to_string(fresh.proj.join(".mcp.json")).unwrap(), "{ not json");
}

#[test]
fn installing_for_cursor_codex_and_user_wide() {
    let e = Env::new();
    ok(&e.run(&["install", "cursor", "-y"]));
    assert_eq!(
        read_json(&e.proj.join(".cursor/mcp.json"))["mcpServers"]["claude-jr"]["args"],
        serde_json::json!(["mcp", "-y"])
    );
    let rule = std::fs::read_to_string(e.proj.join(".cursor/rules/claude-jr.mdc")).unwrap();
    assert!(rule.starts_with("---\ndescription: ") && rule.contains("merge_task"), "{rule}");
    ok(&e.run(&["uninstall", "cursor"]));
    assert!(!e.proj.join(".cursor/mcp.json").exists() && !e.proj.join(".cursor/rules/claude-jr.mdc").exists());

    // Codex keeps its servers in one file in the home folder; the rest of it is left alone.
    let codex = e.home.join(".codex/config.toml");
    std::fs::create_dir_all(codex.parent().unwrap()).unwrap();
    std::fs::write(&codex, "model = \"o\"\n\n[mcp_servers.docs]\ncommand = \"d\"\n").unwrap();
    ok(&e.run(&["install", "codex", "-y"]));
    let text = std::fs::read_to_string(&codex).unwrap();
    assert!(
        text.starts_with(
            "model = \"o\"\n\n[mcp_servers.docs]\ncommand = \"d\"\n\n[mcp_servers.claude-jr]\ncommand = \""
        ),
        "{text}"
    );
    assert!(text.ends_with("args = [\"mcp\",\"-y\"]\n"), "{text}");
    assert!(e.run(&["install", "codex", "-y"]).out.contains("Already set up"));
    ok(&e.run(&["uninstall", "codex"]));
    assert_eq!(std::fs::read_to_string(&codex).unwrap(), "model = \"o\"\n\n[mcp_servers.docs]\ncommand = \"d\"\n");

    // For every project: Claude Code's own command is used for its user-wide list.
    let bin = e.root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let said = e.root.join("claude-args.txt");
    std::fs::write(bin.join("claude"), format!("#!/bin/sh\necho \"$@\" >> '{}'\n", said.display())).unwrap();
    std::fs::set_permissions(bin.join("claude"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let with_claude = |args: &[&str]| {
        let mut cmd = e.command(args);
        cmd.env("PATH", format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()));
        exec(cmd)
    };
    let o = with_claude(&["install", "claude-code", "--user", "-y"]);
    ok(&o);
    let ran = std::fs::read_to_string(&said).unwrap();
    assert!(ran.starts_with("mcp add --scope user claude-jr -- ") && ran.trim_end().ends_with(" mcp -y"), "{ran}");
    assert!(e.home.join(".claude/skills/claude-jr/SKILL.md").exists(), "the skill is in the home folder");
    assert!(!e.proj.join(".mcp.json").exists(), "nothing was written in the project");
    ok(&with_claude(&["uninstall", "claude-code", "--user"]));
    assert!(std::fs::read_to_string(&said).unwrap().contains("mcp remove --scope user claude-jr"));
    assert!(!e.home.join(".claude/skills/claude-jr").exists());
    // Without Claude Code installed, the command to run is given instead.
    let mut cmd = e.command(&["install", "claude-code", "--user", "-y"]);
    cmd.env("PATH", "/nonexistent-bin");
    let o = exec(cmd);
    ok(&o);
    assert!(o.out.contains("run this yourself: claude mcp add --scope user claude-jr -- "), "{}", o.out);

    for (args, want) in [
        (&["install"][..], "usage: claudejr install"),
        (&["install", "emacs"][..], "emacs: not an agent"),
        (&["install", "cursor", "--budget", "x"][..], "--budget needs an amount"),
        (&["uninstall"][..], "usage: claudejr install"),
    ] {
        let o = e.run(args);
        assert_eq!(o.code, 2, "{args:?}");
        assert!(o.err.contains(want), "{args:?}: {}", o.err);
    }
    assert!(e.run(&["install", "--help"]).out.contains("claude-code, cursor or codex"));
}
