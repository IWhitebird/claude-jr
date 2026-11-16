// A whole turn on screen: thinking, tools, a diff, markdown, and what a resize does to it.

use super::common::*;

#[test]
fn a_turn_shows_thinking_tools_a_diff_and_markdown() {
    let world = World::new("turn", &[("MOCK_DELAY_MS", "8")]);
    let mut app = world.launch(&["-y"], Some("mock"));
    app.expect_all(&[
        "claude-jr",
        "claude-opus-5-5 · ~/proj",
        "/help for commands",
        "ask anything",
        "skip permissions",
    ]);

    app.send("hello\r");
    app.expect_all(&[
        "> hello",
        "Round 1: checking the output.",
        "Bash(head -c 1024",
        "Write(demo.txt)",
        "Read(demo.txt)",
        "Edit(demo.txt)",
        "- two",
        "+ TWO",
        "Round 2: checking the output.",
        "Done. Here's what happened:",
        "wrote demo.txt",
        "│ file",
        "cat demo.txt",
        "ask anything",
    ]);
    assert_eq!(std::fs::read_to_string(world.proj().join("demo.txt")).unwrap(), "one\nTWO\nthree\n");
    let saved = sessions(&world);
    assert_eq!(saved.len(), 1, "one session file");
    let saved = std::fs::read_to_string(&saved[0]).unwrap();
    assert!(saved.contains("hello") && saved.contains("tool_result"), "the session holds the turn");

    // Too narrow for the table, its rows are shown as text; wide again, as a table.
    let mark = app.pos;
    app.resize(40, 20);
    let narrow = app.all_from(mark);
    assert!(narrow.contains("notes.md") && !narrow.contains('┌'), "no table at 20 columns:\n{narrow}");
    let mark = app.pos;
    app.resize(40, 100);
    assert!(app.all_from(mark).contains("┌──────────┬"), "the table is back");

    app.send("/cost\r");
    app.expect("so far ·");
    app.expect("tokens in context");
    assert!(!app.all().contains("$0.0000 so far"), "the turn cost something");
    app.send("/stats\r");
    app.expect_all(&["claude-jr", "memory (peak", "threads"]);
    app.exit();
}

#[test]
fn web_searches_are_listed() {
    let world = World::new("web", &[("MOCK_SCENARIO", "web")]);
    let mut app = world.start(&[]);
    app.send("how does vite find its config\r");
    app.expect_all(&["Search(vite config)", "3 results", "Vite reads vite.config.ts.", "ask anything"]);
    app.exit();
}

#[test]
fn the_memory_tool_asks_and_saves_a_note() {
    let world = World::new("memory", &[("MOCK_SCENARIO", "memory")]);
    let mut app = world.start(&[]);
    app.send("remember what I use\r");
    app.expect("Allow Memory(add: uses pnpm, not npm)?");
    app.approve("y");
    app.expect_all(&["Saved. 1 notes in memory.", "Noted.", "ask anything"]);
    let mut notes = Vec::new();
    for project in std::fs::read_dir(world.data_dir().join("memory")).unwrap().flatten() {
        notes.push(std::fs::read_to_string(project.path().join("MEMORY.md")).unwrap());
    }
    assert_eq!(notes, ["- uses pnpm, not npm\n"]);
    app.exit();
}
