// The command line itself: help, version, bad arguments, the key, a piped prompt.

use super::common::*;

#[test]
fn help_and_version() {
    let e = Env::new();
    for flag in ["-h", "--help"] {
        let o = e.run(&[flag]);
        ok(&o);
        assert!(o.out.starts_with("usage: claudejr"), "{}", o.out);
        assert!(o.out.contains("--sandbox") && o.out.contains("CLAUDE_JR_MODEL"));
    }
    for flag in ["-V", "--version"] {
        let o = e.run(&[flag]);
        ok(&o);
        assert_eq!(o.out.trim(), format!("claude-jr {}", env!("CARGO_PKG_VERSION")));
    }
    assert!(e.requests().is_empty());
}

#[test]
fn the_prompt_can_be_piped_in() {
    let e = Env::new();
    let piped = |bytes: &[u8]| {
        let path = e.root.join("prompt.txt");
        std::fs::write(&path, bytes).unwrap();
        exec_with(e.command(&["-y", "-p"]), std::fs::File::open(path).unwrap().into())
    };
    let o = piped(b"summarize this\nplease");
    ok(&o);
    assert!(o.out.contains("Done."), "{}", o.out);
    assert!(e.requests_text().contains("summarize this\\nplease"), "the piped text is the prompt");

    // Bytes that aren't text still make a prompt, and a big one is fine.
    ok(&piped(b"caf\xe9 \xff\xfe"));
    ok(&piped(&vec![b'a'; 2_000_000]));

    // Nothing piped, or only blank space, is no prompt at all.
    let before = e.requests().len();
    for (args, stdin, want) in [
        (&["-y", "-p"][..], &b""[..], "-p needs a value"),
        (&["-y", "-p"][..], &b" \n\t\n"[..], "-p needs a prompt"),
        (&["-y", "-p", ""][..], &b""[..], "-p needs a prompt"),
        (&["-y", "-p", "  \n"][..], &b""[..], "-p needs a prompt"),
    ] {
        let path = e.root.join("prompt.txt");
        std::fs::write(&path, stdin).unwrap();
        let o = exec_with(e.command(args), std::fs::File::open(path).unwrap().into());
        assert_eq!(o.code, 2, "{args:?}: {}", o.err);
        assert!(o.err.contains(want), "{args:?}: {}", o.err);
    }
    assert_eq!(e.requests().len(), before, "nothing was sent for them");
}

#[test]
fn an_api_address_that_cannot_be_one_fails_at_once() {
    let e = Env::new();
    let mut cmd = e.command(&["-y", "-p", "hi"]);
    cmd.env("ANTHROPIC_BASE_URL", "not a url").env("CLAUDE_JR_OFFLINE_SECS", "30");
    let start = Instant::now();
    let o = exec(cmd);
    assert_eq!(o.code, 1, "{}", o.err);
    assert!(o.err.contains("isn't valid") && o.err.contains("ANTHROPIC_BASE_URL"), "{}", o.err);
    assert!(start.elapsed() < Duration::from_secs(10), "it didn't wait as if the network were down");
}

#[test]
fn bad_arguments_are_rejected() {
    let e = Env::new();
    let o = e.run(&["--frobnicate"]);
    assert_eq!(o.code, 2);
    assert!(o.err.contains("unknown argument: --frobnicate") && o.err.contains("usage: claudejr"), "{}", o.err);
    for flag in ["-p", "-m", "-e", "-r", "--model"] {
        let o = e.run(&[flag]);
        assert_eq!(o.code, 2, "{flag}");
        assert!(o.err.contains(&format!("{flag} needs a value")), "{}", o.err);
    }
    assert!(e.requests().is_empty());
}

#[test]
fn sessions_listing() {
    let e = Env::new();
    let o = e.run(&["--sessions"]);
    ok(&o);
    assert!(o.out.contains("No saved conversations in this directory."), "{}", o.out);

    ok(&e.run(&["-p", "first prompt here", "-y"]));
    let o = e.run(&["--sessions"]);
    ok(&o);
    assert!(o.out.contains("first prompt here") && o.out.contains("msgs"), "{}", o.out);
    assert!(o.out.contains("Resume with: claudejr -r NAME_OR_ID"));

    // Another folder has its own list.
    let other = e.root.join("other");
    std::fs::create_dir_all(&other).unwrap();
    let mut cmd = e.command(&["--sessions"]);
    cmd.current_dir(&other);
    let o = exec(cmd);
    assert!(o.out.contains("No saved conversations"), "{}", o.out);
}

#[test]
fn no_api_key_in_print_mode() {
    let e = Env::new();
    let mut cmd = e.command(&["-p", "hi"]);
    cmd.env_remove("ANTHROPIC_API_KEY");
    let o = exec(cmd);
    assert_eq!(o.code, 2);
    assert!(o.err.contains("no API key"), "{}", o.err);
    assert!(e.requests().is_empty());
}

#[test]
fn saved_credentials_are_used() {
    let e = Env::new();
    std::fs::create_dir_all(e.config_dir()).unwrap();
    std::fs::write(e.config_dir().join("credentials"), "from-file\n").unwrap();
    let mut cmd = e.command(&["-p", "hi"]);
    cmd.env_remove("ANTHROPIC_API_KEY");
    let o = exec(cmd);
    ok(&o);
    assert!(o.out.contains("Done."), "{}", o.out);
}

#[test]
fn interactive_needs_a_terminal() {
    let e = Env::new();
    let o = e.run(&[]);
    assert_eq!(o.code, 2);
    assert!(o.err.contains("not a terminal; use -p PROMPT"), "{}", o.err);
}

/// What is piped in goes with a prompt given to -p, before it; with nothing
/// after -p but options, what is piped in is the prompt.
#[test]
fn piped_input_goes_with_the_prompt() {
    let e = Env::with(&[("MOCK_ROUNDS", "0")]);
    let first_text = |args: &[&str], input: &str| {
        let mut child =
            e.command(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        e.requests().last().unwrap()["messages"][0]["content"][0]["text"].as_str().unwrap().to_string()
    };
    assert_eq!(first_text(&["-p", "what is the word"], "the word is plum\n"), "the word is plum\n\nwhat is the word");
    assert_eq!(first_text(&["-p", "only this"], ""), "only this");
    assert_eq!(first_text(&["-p", "-y", "--output-format", "json"], "the pipe's prompt\n"), "the pipe's prompt\n");
}
