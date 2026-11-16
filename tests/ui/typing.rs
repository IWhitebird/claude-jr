// The input box: editing keys, history, pastes, images, the stash; and long
// replies and long sessions through resizes.

use super::common::*;

#[test]
fn editing_keys_history_paste_and_ctrl_c() {
    let world = World::new("editing", &[("MOCK_ROUNDS", "0")]);
    world.config(r#"{"layout": "inline"}"#);
    let mut app = world.start(&["-y"]);
    let sent = |text: &str| world.sent(&format!("\"text\": \"{text}\""));

    // Word and line deletes, both ends, arrows, delete and backspace.
    app.send("one two three");
    app.send(CTRL_W);
    app.send("four");
    app.send(CTRL_A);
    app.send("A");
    app.send(CTRL_E);
    app.send("Z");
    app.send(&format!("{LEFT}{LEFT}{CTRL_K}{BACKSPACE}{HOME}{RIGHT}{DELETE}{PAGE_UP}{END}!"));
    app.expect("> Ane two fo!");
    app.turn("");
    sent("Ane two fo!");

    // History: up and down, and what was being typed comes back.
    app.turn("second entry");
    app.send(&format!("{UP}{UP}{DOWN}{DOWN}draft{UP}{DOWN}ed"));
    app.expect("> drafted");
    app.turn("");
    sent("drafted");
    app.send(&format!("{UP}{UP} again"));
    app.expect("> second entry again");
    app.turn("");
    sent("second entry again");

    // Several lines: ctrl-j, a backslash before enter, alt-enter; up and down move between them.
    app.send(&format!("line1{CTRL_J}line2{UP}X{DOWN}\\\rline3{ALT_ENTER}line4"));
    app.expect("line4");
    app.turn("");
    sent("line1X\\nline2\\nline3\\nline4");

    // Alt-backspace takes a word, ctrl-u the line.
    app.send(&format!("alpha beta{ALT_BACKSPACE}gamma"));
    app.expect("> alpha gamma");
    app.send(CTRL_U);
    app.turn("delta");
    sent("delta");

    // A line as wide as the box, then wider: it wraps, and up and down move between its rows.
    let wide = "w".repeat(94);
    app.send(&wide);
    app.expect(&wide);
    app.send(&format!("123456{UP}^{DOWN}$"));
    app.expect("123456$");
    app.turn("");
    sent(&format!("wwwwww^{}123456$", "w".repeat(88)));

    // A paste keeps its line breaks, whatever kind they were.
    app.send("\x1b[200~p1\r\np2\rp3\x1b[201~");
    app.expect("p3");
    app.turn("");
    sent("p1\\np2\\np3");

    // No clipboard here (and no display to find one on).
    app.send(CTRL_V);
    app.expect("Nothing to paste: the clipboard is empty, or wl-paste / xclip isn't installed.");

    // Ctrl-c clears what's typed, then asks, then quits.
    app.send("never sent");
    app.expect("> never sent");
    app.send(CTRL_C);
    app.expect("ask anything");
    app.send(CTRL_C);
    app.expect("press ctrl-c again to quit");
    app.send(CTRL_C);
    app.exited(0);
    assert!(!world.log().contains("never sent"));
}

#[test]
fn typing_while_busy_queues_and_esc_stops_the_turn() {
    let world = World::new("busy", &[("MOCK_SCENARIO", "bench"), ("MOCK_DELAY_MS", "20"), ("MOCK_TEXT_CHARS", "6000")]);
    world.with_skill();
    let mut app = world.start(&["-y"]);
    app.send("go\r");
    app.expect_all(&["type to queue a message", "esc to stop", "writing"]);
    app.send("second\r");
    app.expect("queued: second");
    app.send("third\r");
    app.expect("queued: second (+1 more)");
    for command in ["/clear", "/compact", "/resume", "/rewind", "/demo"] {
        app.send(&format!("{command}\r"));
        app.expect("Wait for the current turn to finish.");
    }

    // Each stop lets the next queued message go.
    app.send(ESC);
    app.expect_all(&["Stopped.", "> second", "writing"]);
    app.send(CTRL_C);
    app.expect_all(&["Stopped.", "> third", "writing"]);
    app.send(ESC);
    app.expect_all(&["Stopped.", "ask anything"]);
    let log = world.log();
    assert!(log.contains("second") && log.contains("third"), "the queued messages were sent");
    app.exit();
}

/// A reply long enough to be written out row by row while it streams, with
/// the terminal resized on the way and made tiny afterwards.
fn stream_and_resize(name: &str, layout: &str) {
    let world = World::new(
        name,
        &[("MOCK_SCENARIO", "bench"), ("MOCK_DELAY_MS", "3"), ("MOCK_TEXT_CHARS", "3000"), ("MOCK_ROUNDS", "2")],
    );
    world.config(&format!(r#"{{"layout": "{layout}"}}"#));
    let mut app = world.start(&["-y"]);
    app.send("go\r");
    app.expect("The build output looks fine");
    app.resize(30, 80);
    app.resize(24, 120);
    app.expect("ask anything");
    assert_eq!(world.log().lines().count(), 3, "two rounds of tools, then the answer");
    assert!(app.all().contains("Bash(head -c 1024"));

    // Smaller than the input box and its surroundings.
    app.resize(4, 30);
    app.send("/jobs\r");
    app.expect("No background jobs.");
    app.resize(40, 100);
    app.send("/help\r");
    app.expect("type / to list commands");
    app.exit();
}

#[test]
fn a_long_reply_streams_through_resizes_at_the_bottom() {
    stream_and_resize("stream-bottom", "bottom");
}

#[test]
fn a_long_reply_streams_through_resizes_inline() {
    stream_and_resize("stream-inline", "inline");
}

/// Zooming or resizing in a long session draws all of it again, the header
/// and the first message included, not just the most recent screens.
#[test]
fn a_resize_redraws_all_of_a_long_session() {
    let world = World::new("long-resize", &[("MOCK_ROUNDS", "150")]);
    let mut app = world.start(&["-y"]);
    app.send("the first message\r");
    app.expect("Round 150");
    app.expect("Done. Here's what happened:");
    app.expect("ask anything");
    assert!(app.all().lines().count() > 1200, "long enough to be more than a few screens");

    let whole = |redrawn: &str| {
        assert_eq!(redrawn.matches("/help for commands").count(), 1, "the header, once");
        let order =
            ["/help for commands", "> the first message", "Write(demo.txt)", "Round 2:", "Round 75:", "Round 150:"];
        let mut from = 0;
        for part in order {
            let at = redrawn[from..].find(part).unwrap_or_else(|| panic!("{part:?} is missing or out of order"));
            from += at + part.len();
        }
    };
    for (rows, cols) in [(30, 80), (20, 60), (40, 100)] {
        let mark = app.pos;
        app.resize(rows, cols);
        whole(&app.all_from(mark));
    }
    app.exit();

    // The same after coming back to it.
    let mut app = world.start(&["-y", "-c"]);
    app.shown("Done. Here's what happened:");
    let mark = app.pos;
    app.resize(30, 80);
    let redrawn = app.all_from(mark);
    assert_eq!(redrawn.matches("/help for commands").count(), 1, "the header, once");
    assert!(redrawn.contains("> the first message") && redrawn.contains("Write(demo.txt)"), "the start of the session");
    app.exit();
}

/// After /clear the page starts again with the header, and a resize must
/// bring that header back too, with what came after it.
#[test]
fn the_header_survives_a_resize_after_clear() {
    let world = World::new("clear-resize", &[("MOCK_ROUNDS", "1")]);
    let mut app = world.start(&["-y"]);
    app.turn("before the clear");
    app.send("/clear\r");
    app.expect("/help for commands");
    app.turn("after the clear");
    for (rows, cols) in [(30, 80), (40, 100)] {
        let mark = app.pos;
        app.resize(rows, cols);
        let redrawn = app.all_from(mark);
        assert_eq!(redrawn.matches("/help for commands").count(), 1, "the header, once:\n{redrawn}");
        assert!(redrawn.contains("> after the clear"), "what came after the clear");
        assert!(!redrawn.contains("before the clear"), "nothing from before it");
    }
    app.exit();
}

/// A model of another provider, set in the config: no Anthropic key is
/// asked for, a turn with tools works, and the footer shows its context size.
#[test]
fn another_providers_model_works_without_an_anthropic_key() {
    let world = World::new("provider-ui", &[]);
    world.config(&format!(
        r#"{{"model": "local/test-model", "providers": {{"local": {{"base_url": "http://127.0.0.1:{}/v1", "context": 32000}}}}}}"#,
        world.port
    ));
    let mut app = world.launch(&["-y"], None);
    app.expect("ask anything");
    assert!(!app.all().contains("API key"), "no key prompt:\n{}", app.all());
    app.shown("local/test-model");
    app.turn("do the demo");
    app.shown("Edit(demo.txt)");
    app.shown("Done. Here's what happened:");
    app.shown("/32k");
    assert_eq!(std::fs::read_to_string(world.proj().join("demo.txt")).unwrap(), "one\nTWO\nthree\n");
    assert!(world.log().lines().all(|l| l.contains("\"model\": \"test-model\"")), "every request went to it");

    // Compacting asks the same model for the summary.
    app.send("/compact\r");
    app.expect("Compacted");
    // Switching to a Claude model now needs Anthropic's key, and says so.
    app.send("/model claude-opus-5-5\r");
    app.expect("claude-opus-5-5");
    app.send("hello\r");
    app.expect("no API key yet: type /login");
    app.exit();
}

/// An interactive session in a worktree: the folder it was started from is
/// left alone, and on the way out it says where the work is. (Nothing is
/// committed for you when you were there to do it yourself.)
#[test]
fn a_session_in_a_worktree_says_where_its_work_is() {
    let world = World::new("worktree-ui", &[]);
    let git = |args: &[&str]| {
        let out = Command::new("git").args(args).current_dir(world.proj()).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(world.proj().join("kept.txt"), "x\n").unwrap();
    git(&["add", "-A"]);
    git(&[
        "-c",
        "user.name=T",
        "-c",
        "user.email=t@example.com",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "-q",
        "-m",
        "first",
    ]);

    let mut app = world.launch(&["-y", "--worktree"], Some("mock"));
    app.expect("ask anything");
    app.shown("worktrees/proj-");
    app.turn("do the demo");
    app.shown("Edit(demo.txt)");
    app.send("/exit\r");
    app.expect("The work is on branch claudejr/");
    app.expect("changes that aren't committed");
    app.expect("git merge claudejr/");
    app.exited(0);
    assert!(!world.proj().join("demo.txt").exists(), "the folder it was started from is untouched");
    let branch = git(&["branch", "--list", "claudejr/*"]).trim_start_matches(['+', ' ']).to_string();
    let path = git(&["worktree", "list", "--porcelain"]);
    let worktree =
        path.lines().filter_map(|l| l.strip_prefix("worktree ")).nth(1).expect("the worktree is still there");
    assert_eq!(std::fs::read_to_string(format!("{worktree}/demo.txt")).unwrap(), "one\nTWO\nthree\n");
    assert!(branch.starts_with("claudejr/"), "{branch}");

    // A session that changes nothing leaves no worktree or branch behind.
    let before = git(&["worktree", "list"]).lines().count();
    let mut app = world.launch(&["-y", "--worktree"], Some("mock"));
    app.expect("ask anything");
    app.send("/exit\r");
    app.expect("Nothing was changed, so the worktree was removed.");
    app.exited(0);
    assert_eq!(git(&["worktree", "list"]).lines().count(), before);
}
