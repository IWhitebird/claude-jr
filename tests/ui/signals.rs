// Signals: what is stopped and tidied when the app is told to go.

use super::common::*;

#[test]
fn subagents_and_jobs_are_listed_and_sigterm_stops_the_jobs() {
    let world = World::new("agents", &[("MOCK_SCENARIO", "agents")]);
    let mut app = world.start(&["-y"]);
    app.send("start the app\r");
    // Both subagents run at once, each with its latest step beside it
    // (which of the two gets there first varies).
    app.expect("running 2 tools at once");
    app.shown("◆ scan components · Bash(sleep 1.5; echo looked)");
    app.shown("◆ check styles · Bash(sleep 1.5; echo looked)");
    app.expect_all(&[
        "Agent(scan components)",
        "Found it.",
        "Agent(check styles)",
        "Started job 1 in the background",
        "Started the dev server.",
        "ask anything",
    ]);
    let screen = app.all();
    assert!(screen.contains("▶ job 1 · sleep 60"), "the job is listed under the input");
    assert!(screen.contains("· 1 job"), "the footer counts the job");

    app.send("/stats\r");
    app.expect("sleep");
    app.expect("(pid ");
    let job: u32 = app.since().split(')').next().unwrap().trim().parse().expect("the job's pid");
    assert!(Path::new(&format!("/proc/{job}")).exists());

    app.signal("TERM");
    app.exited(128 + 15);
    let end = Instant::now() + WAIT;
    while Path::new(&format!("/proc/{job}")).exists() {
        assert!(Instant::now() < end, "the background job outlived the app");
        sleep(Duration::from_millis(10));
    }
}

#[test]
fn sighup_and_sigint_exit_cleanly() {
    let world = World::new("signals", &[]);
    let mut app = world.start(&["-m", "claude-haiku-5-5", "-e", "low"]);
    app.expect("claude-haiku-5-5 low");
    app.signal("HUP");
    app.exited(128 + 1);

    let app = world.start(&[]);
    app.signal("INT");
    app.exited(128 + 2);
}

/// ctrl+z hands the terminal back to the shell as other terminal programs do,
/// and `fg` brings the app back as it was, half-typed message included.
#[test]
fn ctrl_z_suspends_it_and_fg_brings_it_back() {
    let world = World::new("suspend", &[]);
    let mut app = world.launch_as_job();
    app.expect("ask anything");
    app.send("half typed");
    app.expect("half typed");
    app.send("\x1a");
    // Stopped by SIGTSTP (128 + 20), with the terminal back in its usual mode.
    app.expect("STOPPED-148");
    app.expect(" icanon");
    let mark = app.pos;
    app.expect("half typed");
    assert!(!app.all_from(mark).contains("-icanon"), "the shell got a raw terminal");
    app.send(" and done");
    app.expect("half typed and done");
    app.send("\x15");
    app.exit();
}
