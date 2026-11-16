// Everything that asks the user something: approvals, questions, pickers, the key
// prompt, trust; and the modes that decide when to ask.

use super::common::*;

#[test]
fn approvals_take_yes_always_no_esc_and_ctrl_c() {
    let world = World::new("approve", &[]);
    let mut app = world.start(&[]);

    // First conversation: yes, always, a mode change in the middle, no.
    app.send("go\r");
    app.expect("Allow Bash(head -c 1024");
    // Not answers: another key, and a paste.
    app.send("x\x1b[200~y\x1b[201~");
    app.approve("xy");
    app.expect("Allow Write(demo.txt)?");
    app.approve("a");
    app.expect("Allow Edit(demo.txt)?");
    app.approve(SHIFT_TAB);
    app.expect("accept edits");
    app.send("1");
    app.expect("Round 2: checking the output.");
    app.expect("Allow Bash(head -c 1024");
    app.approve("n");
    app.expect_all(&["did not allow", "Done. Here's what happened:", "ask anything"]);
    assert_eq!(std::fs::read_to_string(world.proj().join("demo.txt")).unwrap(), "one\nTWO\nthree\n");

    // Second: esc refuses; edits no longer ask; ctrl-c refuses and stops the turn.
    app.send("/clear\r");
    app.expect("/help for commands");
    app.send("again\r");
    app.expect("Allow Bash(head -c 1024");
    app.send(ESC);
    app.expect_all(&["did not allow", "Write(demo.txt)", "Edit(demo.txt)", "Allow Bash(head -c 1024"]);
    app.send(CTRL_C);
    app.expect_all(&["Stopped.", "ask anything"]);

    // Third: "always" for a command covers the same command later.
    app.send("/clear\r");
    app.expect("/help for commands");
    app.send("third\r");
    app.expect("Allow Bash(head -c 1024");
    app.approve("A");
    app.expect("Round 2: checking the output.");
    let mark = app.pos;
    app.expect_all(&["Done. Here's what happened:", "ask anything"]);
    assert!(!app.all_from(mark).contains("Allow "), "nothing more to approve");
    app.exit();
}

#[test]
fn questions_subagents_jobs_skills_and_mcp() {
    let world = World::new("features", &[("MOCK_SCENARIO", "features")]);
    world.with_skill();
    world.with_mcp();
    world.write("notes.txt", "a\nb\n");
    world.write("build/out.o", "x");
    let mut app = world.launch(&["-y"], Some("mock"));
    app.expect("Loaded 1 skill · MCP: demo (local, 1 tools).");
    app.expect("ask anything");

    // Answer by number.
    app.send("check everything\r");
    app.expect_all(&["Todos", "look around", "Which color?", "1. red", "2. blue", "type an answer"]);
    app.send("2\r");
    app.expect_all(&[
        "⎿ blue",
        "Agent(check notes)",
        "Subagent report: notes.txt has 2 lines.",
        "Started job 1 in the background",
        "served",
        "Skill(demo)",
        "Say hello from the demo skill.",
        "demo: echo(text: ping)",
        "echoed ping",
        "reviewed-and-allowed",
        "Bash(rm -rf ./build)",
        "All features checked.",
        "ask anything",
    ]);
    assert!(!world.proj().join("build").exists(), "skip-permissions mode ran the rm");
    let screen = app.all();
    assert!(screen.contains("▶ job 1 · sleep 0.3; echo served; sleep 30"), "the job is listed under the input");
    assert!(screen.contains("· 1 job"), "the footer counts the job");

    app.send("/jobs\r");
    app.expect_all(&["Background jobs (stop one with /kill N):", "1. sleep 0.3; echo served; sleep 30  (running)"]);
    app.send("/mcp\r");
    app.expect_all(&["MCP servers:", "demo (local, 1 tools)"]);
    app.send("/skills\r");
    app.expect_all(&["Skills (run with /name):", "demo"]);
    app.send("/kill 1\r");
    app.expect("Stopped job 1.");
    app.send("/kill 7\r");
    app.expect("No such job. /jobs lists them.");

    // Answer in words.
    app.send("/clear\r");
    app.expect("/help for commands");
    app.send("once more\r");
    app.expect("Which color?");
    app.send("purple\r");
    app.expect_all(&["⎿ purple", "All features checked.", "ask anything"]);
    let log = world.log();
    assert!(log.contains("blue") && log.contains("purple"), "both answers went to the model");

    // Esc at a question stops the turn.
    app.send("/clear\r");
    app.expect("/help for commands");
    app.send("and again\r");
    app.expect("Which color?");
    app.send(ESC);
    app.expect_all(&["Stopped.", "ask anything"]);
    app.exit();
}

#[test]
fn auto_mode_asks_only_about_what_the_reviewer_flags() {
    let world = World::new("auto", &[("MOCK_SCENARIO", "features")]);
    world.config(r#"{"mode": "auto"}"#);
    world.with_skill();
    world.write("notes.txt", "a\nb\n");
    world.write("build/out.o", "x");
    let mut app = world.start(&[]);
    app.expect("◆ auto mode");

    app.send("check everything\r");
    app.expect("Which color?");
    app.send("1\r");
    app.expect_all(&["⎿ red", "Started job 1 in the background", "reviewed-and-allowed"]);
    app.expect("Allow Bash(rm -rf ./build) — flagged: deletes files?");
    app.approve("n");
    app.expect_all(&["did not allow", "All features checked.", "ask anything"]);
    assert!(world.proj().join("build/out.o").exists(), "the flagged command did not run");
    assert!(world.log().contains("You review actions"), "the reviewer was asked");
    app.exit();
}

#[test]
fn shift_tab_cycles_modes_and_plan_mode_refuses_edits() {
    let world = World::new("modes", &[("MOCK_ROUNDS", "1")]);
    let mut app = world.start(&[]);
    for footer in ["⏵⏵ accept edits", "⏸ plan mode", "◆ auto mode", "⏵⏵ skip permissions"] {
        app.send(SHIFT_TAB);
        app.expect(footer);
    }
    // Round to normal again, then on to plan.
    app.send(SHIFT_TAB);
    app.send(SHIFT_TAB);
    app.expect("⏵⏵ accept edits");
    app.send(SHIFT_TAB);
    app.expect("⏸ plan mode");

    app.send("make the change\r");
    app.expect("Allow Bash(head -c 1024");
    app.approve("y");
    app.expect_all(&["Plan mode is on", "Done. Here's what happened:", "ask anything"]);
    assert!(!world.proj().join("demo.txt").exists(), "nothing was written in plan mode");
    world.sent("Plan mode is on. Explore and explain");

    for footer in ["◆ auto mode", "⏵⏵ skip permissions"] {
        app.send(SHIFT_TAB);
        app.expect(footer);
    }
    app.send(SHIFT_TAB);
    app.turn("carry on");
    world.sent("Plan mode is off.");
    app.exit();
}

#[test]
fn the_key_prompt_takes_a_pasted_key() {
    let world = World::new("key", &[("MOCK_ROUNDS", "0")]);
    let credentials = world.config_dir().join("credentials");
    let mut app = world.launch(&[], None);
    app.expect_all(&["Paste your Anthropic API key", "esc to skip", "sk-ant-"]);
    app.send(ESC);
    app.expect("No key set. Type /login when you're ready.");

    // Shell commands need no key; a message waits for one.
    app.send("!echo no-key-needed\r");
    app.expect_all(&["! echo no-key-needed", "⎿ no-key-needed", "ask anything"]);
    app.send("hello there\r");
    app.expect_all(&["Paste your Anthropic API key", "queued: hello there"]);
    app.send("\r"); // an empty key is not saved
    app.send("\x1b[200~sk-ant-test-0123\x1b[201~");
    app.expect("••••••••••••••••");
    assert!(!app.all().contains("sk-ant-test-0123"), "the key is never drawn");
    app.send("\r");
    app.expect_all(&["Saved your API key to", "> hello there", "Done. Here's what happened:", "ask anything"]);
    assert_eq!(std::fs::read_to_string(&credentials).unwrap(), "sk-ant-test-0123");
    assert_eq!(std::fs::metadata(&credentials).unwrap().permissions().mode() & 0o777, 0o600);

    app.send("/login\r");
    app.expect("Paste your Anthropic API key");
    app.send("wrong-key\r");
    app.expect_all(&["Saved your API key to", "That doesn't look like an Anthropic key"]);
    assert_eq!(std::fs::read_to_string(&credentials).unwrap(), "wrong-key");

    app.send("/logout\r");
    app.expect("Removed the saved API key.");
    assert!(!credentials.exists());
    assert!(!app.all().contains("is still set in your environment"));
    app.exit();
}

#[test]
fn continuing_shows_the_whole_earlier_conversation() {
    let world = World::new("replay", &[("MOCK_ROUNDS", "1")]);
    let mut app = world.start(&["-y"]);
    app.turn("first question");
    app.turn("second question");
    app.exit();

    // A new start with -c: everything from before is on screen again, in order.
    let mut app = world.launch(&["-c", "-y"], Some("mock"));
    app.expect_all(&[
        "Resumed the last session",
        "> first question",
        "Write(demo.txt)",
        "Wrote 3 lines",
        "Edit(demo.txt)",
        "Edited ",
        "Done. Here's what happened:",
        "> second question",
        "Done. Here's what happened:",
        "ask anything",
    ]);
    let screen = app.all();
    assert_eq!(screen.matches("> first question").count(), 1, "shown once");
    assert!(screen.find("> first question") < screen.find("> second question"));
    assert!(screen.contains("│ demo.txt │"), "the reply's table is drawn: {screen}");

    // /resume replays the picked conversation too.
    app.send("/clear\r");
    app.expect("/help for commands");
    app.send("/resume\r");
    app.expect("Resume a conversation");
    app.send("1");
    app.expect_all(&["> first question", "> second question", "ask anything"]);
    app.exit();
}

#[test]
fn ctrl_s_stashes_the_input_until_the_next_message_is_sent() {
    let world = World::new("stash", &[("MOCK_ROUNDS", "0")]);
    let mut app = world.start(&["-y"]);
    app.send("a long question I'm halfway through");
    app.expect("> a long question I'm halfway through");
    // Put it aside: the box is empty and says how to get it back.
    app.send(CTRL_S);
    app.expect("ctrl+s brings back what you stashed");
    // Ctrl+s on an empty box brings it straight back, and again puts it aside.
    app.send(CTRL_S);
    app.expect("> a long question I'm halfway through");
    app.send(CTRL_S);
    app.expect("ctrl+s brings back what you stashed");
    // Send something else first: the stashed text comes back afterwards, unsent.
    app.send("quick one\r");
    app.expect_all(&["> quick one", "Done. Here's what happened:", "> a long question I'm halfway through"]);
    world.sent("quick one");
    assert!(!world.log().contains("halfway through"), "the stashed text was not sent");
    app.send("\r");
    app.expect_all(&["Done. Here's what happened:", "ask anything"]);
    world.sent("halfway through");
    app.exit();
}

#[test]
fn rewind_works_before_any_file_was_changed() {
    let world = World::new("rewind-nofiles", &[("MOCK_ROUNDS", "0")]);
    let mut app = world.start(&["-y"]);
    app.turn("just a question");
    app.send("/rewind\r");
    app.expect("1. just a question");
    app.send("1");
    app.expect_all(&["Rewound to before \"just a question\". No files needed restoring;", "> just a question"]);
    app.send(CTRL_U);
    app.exit();
}

#[test]
fn rewind_and_resume_pick_from_a_list() {
    let world = World::new("rewind", &[("MOCK_ROUNDS", "1")]);
    let demo = world.proj().join("demo.txt");
    let mut app = world.start(&["-y"]);
    app.turn("first message");
    assert!(demo.exists());
    app.send("/rename alpha\r");
    app.expect("Renamed to \"alpha\". Resume it later with: claudejr -r \"alpha\"");

    // The picker: other keys do nothing, esc closes it.
    app.send("/rewind\r");
    app.expect_all(&["Rewind: restores the files the agent changed since then", "1. first message", "esc to cancel"]);
    app.send("x9");
    app.send(ESC);
    app.expect("ask anything");
    app.send("/rewind\r");
    app.expect("1. first message");
    app.send("1");
    app.expect("Rewound to before \"first message\". Restored 1 file;");
    assert!(!demo.exists(), "the file the agent wrote is gone again");
    // The message is back in the input box.
    app.expect("> first message");
    app.send(" again\r");
    app.expect_all(&["> first message again", "Done. Here's what happened:", "ask anything"]);
    assert!(demo.exists());

    // A turn that changed no files: nothing to restore, the newest message first in the list.
    app.turn("just talk");
    app.send("/rewind\r");
    app.expect_all(&["1. just talk", "2. first message again"]);
    app.send("1");
    app.expect_all(&["Rewound to before \"just talk\". No files needed restoring;", "> just talk"]);
    app.send(CTRL_U);
    app.expect("ask anything");
    assert!(demo.exists());

    app.send("/clear\r");
    app.expect("/help for commands");
    app.turn("other topic");
    let saved = sessions(&world);
    assert_eq!(saved.len(), 2);
    assert!(Command::new("touch").args(["-d", "2 hours ago"]).arg(&saved[0]).status().unwrap().success());
    app.send("/resume\r");
    app.expect_all(&["Resume a conversation", "1. other topic  (just now, ", "2. alpha  (2 h ago, ", "press a number"]);
    app.send("x9");
    app.send(ESC);
    app.expect("ask anything");
    app.send("/resume\r");
    app.expect("2. alpha");
    app.send("2");
    app.expect_all(&["Resumed \"alpha\" (", "> first message again", "Done. Here's what happened:"]);
    app.send("/rename\r");
    app.expect("This session is \"alpha\".");
    app.exit();

    // From the command line: the newest, one by name, one that isn't there.
    let mut app = world.launch(&["-y", "-c"], Some("mock"));
    app.expect("Resumed the last session (");
    app.expect("ask anything");
    app.exit();
    let mut app = world.launch(&["-y", "-r", "alpha"], Some("mock"));
    app.expect("Resumed \"alpha\" (");
    app.expect("ask anything");
    app.exit();
    let mut app = world.launch(&["-r", "nope"], Some("mock"));
    app.expect("No saved session named \"nope\" here; starting a new one.");
    app.exit();
}

#[test]
fn the_settings_picker_saves_to_the_config_file() {
    let world = World::new("settings", &[]);
    let mut app = world.start(&[]);
    app.send("/settings\r");
    app.expect_all(&[
        "Settings (~/.config/claude-jr/config.json)",
        "1. Input at the bottom of the screen: bottom",
        "2. Model for new sessions: claude-opus-5-5",
        "3. Effort: default",
        "4. Start in mode: normal",
        "5. Web search and fetch: on",
        "6. Sandbox (writes only in the project): off",
    ]);
    app.send("1");
    app.expect("1. Input at the bottom of the screen: inline");
    app.send("2");
    app.expect("2. Model for new sessions: claude-sonnet-5-5");
    app.send("3");
    app.expect_all(&["Effort set to low.", "3. Effort: low", "claude-opus-5-5 low"]);
    app.send("4");
    app.expect("4. Start in mode: accept-edits");
    app.send("5");
    app.expect("5. Web search and fetch: off");
    app.send("6");
    app.expect("6. Sandbox (writes only in the project): on");
    app.send("6");
    app.expect("6. Sandbox (writes only in the project): off");
    app.send("8x"); // not settings
    let config = world.read_config();
    for line in [
        "\"layout\": \"inline\"",
        "\"model\": \"claude-sonnet-5-5\"",
        "\"effort\": \"low\"",
        "\"mode\": \"accept-edits\"",
        "\"web_tools\": false",
        "\"enabled\": false",
    ] {
        assert!(config.contains(line), "{line} in {config}");
    }
    // Effort goes round to the default, which is no value at all.
    for level in ["medium", "high", "xhigh", "max"] {
        app.send("3");
        app.expect(&format!("Effort set to {level}."));
    }
    app.send("3");
    app.expect("Effort set to the model's default.");
    assert!(world.read_config().contains("\"effort\": null"));
    app.send(ESC);
    app.expect("ask anything");
    app.exit();

    // The next start uses them.
    let mut app = world.start(&[]);
    assert!(app.all().contains("claude-sonnet-5-5 · "), "the saved model is used");
    app.expect("⏵⏵ accept edits");
    app.exit();
}

#[test]
fn a_project_config_is_used_once_the_folder_is_trusted() {
    let world = World::new("trust", &[]);
    world.write(".claude-jr.json", r#"{"mode": "plan"}"#);
    let mut app = world.launch(&[], Some("mock"));
    app.expect("Trust this folder? [y/N]");
    app.send("n\r");
    app.expect("ask anything");
    assert!(!app.all().contains("plan mode"));
    app.exit();

    let mut app = world.launch(&[], Some("mock"));
    app.expect_all(&["This folder has .claude-jr.json", "Trust this folder? [y/N]"]);
    app.send("y\r");
    app.expect_all(&["ask anything", "⏸ plan mode"]);
    app.exit();
    assert!(world.config_dir().join("trusted").exists());

    let mut app = world.start(&[]);
    app.expect("⏸ plan mode");
    assert!(!app.all().contains("Trust this folder?"), "asked only once");
    app.exit();
}

#[test]
fn failed_saves_are_reported() {
    let world = World::new("unsaved", &[("MOCK_ROUNDS", "0")]);
    // Folders where the note, the key and the settings should go.
    for dir in
        [world.proj().join("AGENTS.md"), world.config_dir().join("credentials"), world.config_dir().join("config.json")]
    {
        std::fs::create_dir_all(dir).unwrap();
    }
    let mut app = world.launch(&[], None);
    app.expect("Paste your Anthropic API key");
    app.send("sk-ant-for-now\r");
    app.expect_all(&["Couldn't save the key (", "it's used for this session only."]);
    app.turn("hello");
    world.sent("\"text\": \"hello\"");

    app.send("#a note\r");
    app.expect("Could not save the note:");
    app.send("/settings\r");
    app.expect("1. Input at the bottom of the screen: bottom");
    app.send("1");
    app.expect("Couldn't save the setting:");
    app.send(ESC);
    app.expect("ask anything");
    app.exit();
}

#[test]
fn a_big_paste_shows_as_a_label_and_is_sent_in_full() {
    let world = World::new("bigpaste", &[("MOCK_ROUNDS", "0")]);
    // A clipboard tool with text only: asking it for an image fails.
    let tool = world.root.join("bin/wl-paste");
    let script = "#!/bin/sh\ncase \"$*\" in *image*) exit 1;; esac\nprintf 'from the clipboard'\n";
    std::fs::write(&tool, script).unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut app = world.start(&["-y"]);

    // Ten pasted lines appear as one short label, not as ten rows.
    let lines: Vec<String> = (1..=10).map(|i| format!("pasted-line-{i}")).collect();
    app.send("explain ");
    app.send(&format!("\x1b[200~{}\x1b[201~", lines.join("\r")));
    app.expect("> explain [Pasted #1: 10 lines, 140 chars]");
    assert!(!app.all().contains("pasted-line-3"), "the pasted text itself is not drawn");
    // Backspace removes the whole label; pasting again gives the next one.
    app.send(BACKSPACE);
    app.expect("> explain ");
    app.send(&format!("\x1b[200~{}\x1b[201~", lines.join("\n")));
    app.expect("[Pasted #2: 10 lines, 140 chars]");
    app.send("\r");
    app.expect_all(&["> explain [Pasted #2: 10 lines, 140 chars]", "Done. Here's what happened:", "ask anything"]);
    // The API got the text, not the label.
    world.sent("pasted-line-1\\npasted-line-2");
    world.sent("pasted-line-10");
    assert!(!world.log().contains("[Pasted #"), "{}", world.log());

    // A short paste goes in as it is, and ctrl+v pastes text when there is no image.
    app.send("\x1b[200~short paste\x1b[201~ and ");
    app.send(CTRL_V);
    app.expect("> short paste and from the clipboard");
    app.send("\r");
    app.expect_all(&["Done. Here's what happened:", "ask anything"]);
    world.sent("short paste and from the clipboard");
    app.exit();
}

#[test]
fn images_come_from_the_clipboard_or_a_dragged_file() {
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
    let world = World::new("images", &[("MOCK_ROUNDS", "0")]);
    // A clipboard tool that always has a (very small) PNG.
    let tool = world.root.join("bin/wl-paste");
    std::fs::write(&tool, "#!/bin/sh\nprintf '\\211PNG\\r\\n\\032\\n'\n").unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    let shot = world.proj().join("shot.png");
    std::fs::write(&shot, [PNG, b"pixels".as_slice()].concat()).unwrap();
    let mut app = world.start(&["-y"]);

    // Ctrl-v saves the image and puts its path in the input.
    app.send("what is this ");
    app.send(CTRL_V);
    app.expect("paste-");
    app.send("\r");
    app.expect_all(&["Attached ", "Done. Here's what happened:", "ask anything"]);
    let images = world.data_dir().join("images");
    let pasted: Vec<PathBuf> = std::fs::read_dir(&images).unwrap().flatten().map(|f| f.path()).collect();
    assert!(pasted.iter().any(|p| std::fs::read(p).unwrap() == PNG), "the pasted image is saved: {pasted:?}");
    world.sent("\"media_type\": \"image/png\"");

    // A dragged file arrives as a pasted path.
    app.send(&format!("and this \x1b[200~{}\x1b[201~\r", shot.display()));
    app.expect_all(&["Attached ", "Done. Here's what happened:", "ask anything"]);
    // A message that starts with the path is still a message, not a command.
    app.send(&format!("\x1b[200~{}\x1b[201~ what is in it\r", shot.display()));
    app.expect_all(&["Attached ", "Done. Here's what happened:", "ask anything"]);
    let stored = std::fs::read_dir(&images).unwrap().flatten().map(|f| f.path());
    assert!(stored.into_iter().any(|p| std::fs::read(p).unwrap().ends_with(b"pixels")), "the dragged image is stored");
    app.exit();
}

#[test]
fn a_terminal_with_the_kitty_keyboard_protocol_and_24_bit_color() {
    let mut world = World::new("kitty", &[("MOCK_ROUNDS", "0")]);
    (world.enhanced_keys, world.truecolor) = (true, true);
    let mut app = world.start(&["-y"]);
    let codes = app.codes.clone();
    assert!(codes.lock().unwrap().contains(&">1u".to_string()), "the protocol is turned on: {codes:?}");
    assert!(codes.lock().unwrap().contains(&"truecolor".to_string()), "the mascot is in 24-bit color: {codes:?}");

    // Shift+enter is a new line; a key going up is nothing.
    app.send(&format!("one{SHIFT_ENTER}two{A_RELEASED}"));
    app.expect("two");
    app.turn("");
    world.sent("\"text\": \"one\\ntwo\"");
    app.exit();
    assert!(codes.lock().unwrap().contains(&"<1u".to_string()), "and off again on the way out: {codes:?}");
}
