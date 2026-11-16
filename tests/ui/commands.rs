// Slash commands.

use super::common::*;

#[test]
fn every_command_answers() {
    let world = World::new("commands", &[("MOCK_ROUNDS", "0")]);
    let mut app = world.start(&["-y"]);
    let mut run = |command: &str, shown: &[&str]| {
        app.send(&format!("{command}\r"));
        app.expect_all(shown);
    };
    run("/help", &["/clear, /new", "type / to list commands", "up/down for history"]);
    run("/cost", &["$0.0000 so far · 0 tokens in context"]);
    run("/stats", &["memory (peak", "threads"]);
    run("/memory", &["Instruction files (AGENTS.md / CLAUDE.md):", "Start a message with # to add a note."]);
    run("/skills", &["No skills found. Put them in .claude/skills/<name>/SKILL.md."]);
    run("/mcp", &["No MCP servers."]);
    run("/jobs", &["No background jobs."]);
    run("/kill 3", &["No such job. /jobs lists them."]);
    run("/kill", &["No such job. /jobs lists them."]);
    run("/effort", &["Effort: the model's default. Choose with /effort low|medium|high|xhigh|max or default."]);
    run("/effort high", &["Effort set to high.", "claude-opus-5-5 high"]);
    run("/effort", &["Effort: high."]);
    run("/effort silly", &["Unknown effort \"silly\". Use low, medium, high, xhigh, max or default."]);
    run("/effort default", &["Effort set to the model's default."]);
    run("/rename", &["This session has no name yet. /rename NAME to set one."]);
    run("/rename night shift", &["Renamed to \"night shift\"."]);
    run("/rename", &["This session is \"night shift\". /rename NAME to change it."]);
    run("/model claude-sonnet-5-5", &["Model set to claude-sonnet-5-5."]);
    run("/model", &["Choose a model", "1. claude-opus-5-5", "2. claude-sonnet-5-5  (current)", "press a number"]);
    run("3", &["Model set to claude-haiku-5-5."]);
    run("/model", &["Choose a model", "3. claude-haiku-5-5  (current)"]);
    app.send("x0");
    app.send(ESC);
    app.expect("ask anything");
    let mut run = |command: &str, shown: &[&str]| {
        app.send(&format!("{command}\r"));
        app.expect_all(shown);
    };
    run("/nonsense", &["Unknown command /nonsense. Try /help."]);
    run("/rewind", &["Nothing to rewind to yet."]);
    run("/resume", &["No saved conversations in this folder yet."]);
    run("/compact", &["Error: nothing to compact yet"]);

    // A note, a shell command, and a message that carries both to the model.
    run("#use tabs here", &["Saved to AGENTS.md."]);
    assert_eq!(std::fs::read_to_string(world.proj().join("AGENTS.md")).unwrap(), "- use tabs here\n");
    run("/memory", &["Instruction files (AGENTS.md / CLAUDE.md):", "AGENTS.md"]);
    run("!echo from-the-shell", &["! echo from-the-shell", "⎿ from-the-shell", "ask anything"]);
    run("what now", &["> what now", "Done. Here's what happened:", "ask anything"]);
    let log = world.log();
    assert!(log.contains("The user saved this to memory: use tabs here"));
    assert!(log.contains("<user-shell>") && log.contains("from-the-shell"));
    assert!(log.contains("\"model\": \"claude-haiku-5-5\""), "the picked model is the one used");

    run(
        "/compact keep the table",
        &["Compacted ", "-word summary. /rewind can't go back past this point.", "ask anything"],
    );
    assert!(world.log().contains("Pay particular attention to: keep the table"));
    run("/new", &["/help for commands", "ask anything"]);
    run("/rename", &["This session has no name yet."]);
    run("/logout", &["Removed the saved API key. ANTHROPIC_API_KEY is still set in your environment"]);
    app.send("/quit\r");
    app.exited(0);
}

#[test]
fn the_slash_menu_filters_picks_completes_and_runs() {
    let world = World::new("menu", &[("MOCK_ROUNDS", "0")]);
    world.with_skill();
    let mut app = world.launch(&["-y"], Some("mock"));
    app.expect_all(&["Loaded 1 skill.", "ask anything"]);

    app.send("/");
    app.expect_all(&["› /help", "commands and keys", "14 more · up/down to pick · tab completes · enter runs"]);
    app.send("re");
    app.expect("› /resume");
    assert!(!app.since().contains("more ·"), "three matches fit");
    for (key, picked) in
        [(DOWN, "› /rename"), (DOWN, "› /rewind"), (DOWN, "› /resume"), (UP, "› /rewind"), (UP, "› /rename")]
    {
        app.send(key);
        app.expect(picked);
    }
    app.send("\t");
    app.expect("> /rename ");
    app.send("tabbed\r");
    app.expect("Renamed to \"tabbed\".");

    // Further down than the list is tall.
    app.send("/");
    app.expect("› /help");
    app.send(&DOWN.repeat(9));
    app.expect("› /jobs");
    app.send(CTRL_U);
    app.expect("ask anything");

    // A skill is in the list and runs like a command.
    app.send("/de");
    app.expect_all(&["› /demo", "skill"]);
    app.send("\r");
    app.expect_all(&["> /demo", "Done. Here's what happened:", "ask anything"]);
    world.sent("Say hello from the demo skill.");
    app.turn("/demo with these words");
    world.sent("with these words");

    // The first match runs on enter; alt-enter is a new line instead.
    app.send("/he\r");
    app.expect("type / to list commands");
    app.send("/he");
    app.expect("› /help");
    app.send(ALT_ENTER);
    app.send(CTRL_C);
    app.expect("ask anything");
    app.send(CTRL_D);
    app.exited(0);
}

/// Claude through a cloud, in the app: no Anthropic key is asked for, the
/// token comes from gcloud, and a pick from /model stays on that cloud.
#[test]
fn a_cloud_model_works_in_the_app_and_the_picker_stays_on_that_cloud() {
    let mut world = World::new("cloud", &[("MOCK_ROUNDS", "0")]);
    world.write("../bin/gcloud", "#!/bin/sh\necho ya29.from-gcloud\n");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(world.root.join("bin/gcloud"), std::fs::Permissions::from_mode(0o755)).unwrap();
    world.env = vec![
        ("ANTHROPIC_VERTEX_BASE_URL".into(), format!("http://127.0.0.1:{}/v1", world.port)),
        ("ANTHROPIC_VERTEX_PROJECT_ID".into(), "test-project".into()),
    ];
    let mut app = world.launch(&["-y", "-m", "vertex/claude-opus-5-5"], None);
    app.expect_all(&["vertex/claude-opus-5-5 · ~/proj", "ask anything"]);
    app.turn("hello");
    app.send("/model\r");
    app.expect_all(&[
        "Choose a model",
        "1. vertex/claude-opus-5-5  (current) · $4/$20 · 1M context",
        "3. vertex/claude-haiku-5-5 · $0.1/$0.5",
    ]);
    app.send("3");
    app.expect("Model set to vertex/claude-haiku-5-5.");
    app.turn("and again");
    let heads = std::fs::read_to_string(world.root.join("mock.log.headers")).unwrap();
    let paths: Vec<&str> = heads.lines().filter_map(|l| l.split("models/").nth(1)?.split(':').next()).collect();
    assert_eq!(paths, ["claude-opus-5-5", "claude-haiku-5-5"], "{heads}");
    assert_eq!(heads.matches("Bearer ya29.from-gcloud").count(), 2);
    // (The one refusal on record is the harness asking whether the fake API is up.)
    let refused = std::fs::read_to_string(world.root.join("mock.log.invalid")).unwrap_or_default();
    assert_eq!(refused.lines().count(), 1, "nothing of the app's was refused: {refused}");
}
