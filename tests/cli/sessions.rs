// Continuing and resuming a saved conversation.

use super::common::*;

/// A run that is killed part-way keeps the steps it finished: -c continues from them.
#[test]
fn a_killed_run_keeps_the_steps_it_finished() {
    // Round one prints and leaves a marker; with the marker there, round two hangs.
    let cmd = "test -f marker && sleep 30; touch marker; echo first-round-output";
    let e = Env::with(&[("MOCK_ROUNDS", "2"), ("MOCK_CMD", cmd)]);
    let mut child =
        e.command(&["-p", "do two rounds", "-y"]).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    // Wait for the second request: by then round one's results are in the conversation.
    let end = Instant::now() + Duration::from_secs(20);
    while e.requests().len() < 2 {
        assert!(Instant::now() < end, "the second request never came");
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(300));
    child.kill().unwrap(); // SIGKILL: no chance to save on the way out
    child.wait().unwrap();
    let before = e.requests().len();

    std::fs::remove_file(e.proj.join("marker")).unwrap();
    let o = e.run(&["-c", "-p", "carry on", "-y"]);
    ok(&o);
    let reqs = e.requests();
    let resumed = reqs[before]["messages"].to_string();
    assert!(resumed.contains("do two rounds"), "the first message was kept: {resumed}");
    assert!(resumed.contains("first-round-output"), "round one's result was kept: {resumed}");
    assert!(resumed.contains("carry on"));
}

#[test]
fn continue_sends_the_earlier_messages_again() {
    let e = Env::with(&[("MOCK_ROUNDS", "0")]);
    ok(&e.run(&["-p", "remember-the-walrus", "-y"]));
    let o = e.run(&["-c", "-p", "and now the second thing", "-y"]);
    ok(&o);
    let reqs = e.requests();
    let last = reqs.last().unwrap();
    let text = last["messages"].to_string();
    assert!(text.contains("remember-the-walrus") && text.contains("and now the second thing"), "{text}");
    assert!(last["messages"].as_array().unwrap().len() >= 3);
    assert!(!reqs[0]["messages"].to_string().contains("second thing"));
}

/// A session file with nothing in it, as a full disk can leave (empty, or a
/// header and no messages), is newer than the real conversation but isn't
/// what -c continues, and --sessions doesn't list it.
#[test]
fn continue_skips_sessions_with_nothing_in_them() {
    let e = Env::with(&[("MOCK_ROUNDS", "0")]);
    ok(&e.run(&["-p", "remember-the-walrus", "-y"]));
    // One folder per project, holding its sessions.
    let dir = std::fs::read_dir(e.data.join("claude-jr/sessions")).unwrap().next().unwrap().unwrap().path();
    std::fs::write(dir.join("9999999999999-1.jsonl"), "").unwrap();
    std::fs::write(dir.join("9999999999999-2.jsonl"), "{\"system\":\"x\"}\n").unwrap();
    let listed = e.run(&["--sessions"]);
    ok(&listed);
    assert!(listed.out.contains("remember-the-walrus") && !listed.out.contains("9999999999999"), "{}", listed.out);
    ok(&e.run(&["-c", "-p", "and now the second thing", "-y"]));
    let text = e.requests().last().unwrap()["messages"].to_string();
    assert!(text.contains("remember-the-walrus"), "-c continued an empty session: {text}");
}

#[test]
fn resume_by_id_and_unknown_name() {
    let e = Env::with(&[("MOCK_ROUNDS", "0")]);
    ok(&e.run(&["-p", "the-first-topic", "-y"]));
    let list = e.run(&["--sessions"]).out;
    let id = list.lines().next().unwrap().split_whitespace().next().unwrap().to_string();
    std::thread::sleep(Duration::from_millis(1100));
    ok(&e.run(&["-p", "an unrelated topic", "-y"])); // a newer session

    let o = e.run(&["-r", &id, "-p", "continuing", "-y"]);
    ok(&o);
    let reqs = e.requests();
    let last = reqs.last().unwrap()["messages"].to_string();
    assert!(last.contains("the-first-topic") && last.contains("continuing"), "{last}");
    assert!(!last.contains("unrelated"), "resumed the right one");

    let before = e.requests().len();
    // An unknown name is an error in -p mode: a typo must not lose the conversation.
    let o = e.run(&["-r", "no-such-session", "-p", "fresh-start", "-y"]);
    assert_ne!(o.code, 0, "{}", o.err);
    assert!(o.err.contains("no saved session named \"no-such-session\""), "{}", o.err);
    assert_eq!(e.requests().len(), before, "nothing was sent");
}
