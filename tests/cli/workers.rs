// Which model does handed-off work. Tasks, fleets, the MCP server's tasks
// and subagents use the worker model: Haiku beside a Claude model unless one
// is set or named, and the main model itself beside any other.

use super::common::*;

/// Starts a task with `args` and `env`, waits for it, and returns its state.
fn run_task(e: &Env, args: &[&str], env: &[(&str, &str)]) -> serde_json::Value {
    let mut all = vec!["task", "start", "-y"];
    all.extend_from_slice(args);
    let mut cmd = e.command(&all);
    cmd.envs(env.iter().copied());
    let o = exec(cmd);
    ok(&o);
    let o = task(e, &["wait", o.out.trim()]);
    ok(&o);
    serde_json::from_str(o.out.trim()).unwrap_or_else(|err| panic!("{err}: {}", o.out))
}

/// The model the most recent request asked for.
fn last_asked(e: &Env) -> String {
    e.requests().last().unwrap()["model"].as_str().unwrap().to_string()
}

#[test]
fn a_task_uses_the_worker_model_and_says_which() {
    let e = Env::new();
    let _git = repository(&e);
    // Nothing set: Haiku does the work, though the main model is Opus.
    let state = run_task(&e, &["first"], &[]);
    assert_eq!((state["status"].as_str(), state["model"].as_str()), (Some("done"), Some("claude-haiku-5-5")));
    assert!(e.requests().iter().all(|r| r["model"] == "claude-haiku-5-5"), "what is recorded is what was asked");
    // Named for this task.
    assert_eq!(run_task(&e, &["-m", "claude-sonnet-5-5", "second"], &[])["model"], "claude-sonnet-5-5");
    assert_eq!(last_asked(&e), "claude-sonnet-5-5");
    // Set in the config, then in the environment, which wins; a name still beats both.
    e.write_config(r#"{"worker_model": "claude-opus-5"}"#);
    assert_eq!(run_task(&e, &["third"], &[])["model"], "claude-opus-5");
    assert_eq!(last_asked(&e), "claude-opus-5");
    let env = [("CLAUDE_JR_WORKER_MODEL", "claude-sonnet-5")];
    assert_eq!(run_task(&e, &["fourth"], &env)["model"], "claude-sonnet-5");
    assert_eq!(run_task(&e, &["-m", "claude-opus-4-8", "fifth"], &env)["model"], "claude-opus-4-8");
    assert_eq!(last_asked(&e), "claude-opus-4-8");

    // The list says what did each piece of work.
    let o = task(&e, &["list"]);
    let header = o.out.lines().next().unwrap();
    assert!(header.contains("COST") && header.contains("MODEL") && header.contains("BRANCH"), "{header}");
    let row = |name: &str| o.out.lines().find(|l| l.ends_with(name)).unwrap_or_else(|| panic!("{name}: {}", o.out));
    assert!(row("first").contains(" claude-haiku-5-5 ") && row("fifth").contains(" claude-opus-4-8 "), "{}", o.out);
}

#[test]
fn beside_another_providers_model_the_worker_is_that_model_and_needs_only_its_key() {
    let e = Env::new();
    let _git = repository(&e);
    e.write_config(r#"{"model": "ollama/test-model"}"#);
    let mut cmd = e.command(&["task", "start", "-y", "do it"]);
    cmd.env("OLLAMA_HOST", format!("127.0.0.1:{}", e.port)).env_remove("ANTHROPIC_API_KEY");
    let o = exec(cmd);
    ok(&o); // no Anthropic key anywhere, and none is needed
    let mut cmd = e.command(&["task", "wait", o.out.trim()]);
    cmd.env_remove("ANTHROPIC_API_KEY");
    let state: serde_json::Value = serde_json::from_str(exec(cmd).out.trim()).unwrap();
    assert_eq!((state["status"].as_str(), state["model"].as_str()), (Some("done"), Some("ollama/test-model")));
    assert_eq!(last_asked(&e), "test-model");
    nothing_refused(&e);

    // Anthropic's own model without Anthropic's key is still refused before anything starts.
    let mut cmd = e.command(&["task", "start", "-y", "-m", "claude-haiku-5-5", "do it"]);
    cmd.env_remove("ANTHROPIC_API_KEY");
    let o = exec(cmd);
    assert!(o.code != 0 && o.err.contains("no API key"), "{}{}", o.out, o.err);
}

#[test]
fn a_fleet_and_an_install_say_which_model_works() {
    let e = Env::new();
    let _git = repository(&e);
    let o = e.run(&["fleet", "-y", "--json", "one", "two"]);
    ok(&o);
    let states: serde_json::Value = serde_json::from_str(o.out.trim()).unwrap();
    assert!(states.as_array().unwrap().iter().all(|s| s["model"] == "claude-haiku-5-5"), "{states:#}");
    let o = e.run(&["fleet", "-y", "-m", "claude-sonnet-5-5", "three"]);
    ok(&o);
    assert!(o.out.contains("MODEL") && o.out.contains(" claude-sonnet-5-5 "), "{}", o.out);
    assert_eq!(last_asked(&e), "claude-sonnet-5-5");

    let o = e.run(&["install", "claude-code"]);
    ok(&o);
    assert!(o.out.contains("Workers use claude-haiku-5-5 unless a task names another"), "{}", o.out);
    assert!(o.out.contains("\"worker_model\""), "and how to change it: {}", o.out);
    let o = e.run(&["install", "cursor", "-m", "claude-sonnet-5-5"]);
    assert!(o.out.contains("Workers use claude-sonnet-5-5 unless"), "{}", o.out);
    e.write_config(r#"{"worker_model": "claude-opus-5"}"#);
    let o = e.run(&["install", "claude-code"]);
    assert!(o.out.contains("Workers use claude-opus-5 unless"), "{}", o.out);
}

#[test]
fn subagents_run_on_the_worker_model() {
    // Who asked for what: the main agent's requests and its subagents'.
    let asked = |e: &Env| -> (Vec<String>, Vec<String>) {
        let model = |r: &serde_json::Value| r["model"].as_str().unwrap().to_string();
        let sub = |r: &&serde_json::Value| r["system"].as_str().is_some_and(|s| s.contains("You are a subagent"));
        let all = e.requests();
        (all.iter().filter(|r| !sub(r)).map(model).collect(), all.iter().filter(sub).map(model).collect())
    };
    let e = Env::with(&[("MOCK_SCENARIO", "agents"), ("MOCK_CMD", "true")]);
    ok(&e.run(&["-y", "-p", "look around"]));
    nothing_refused(&e);
    let (main, subs) = asked(&e);
    assert!(!main.is_empty() && main.iter().all(|m| m == "claude-opus-5-5"), "{main:?}");
    assert!(subs.len() >= 4 && subs.iter().all(|m| m == "claude-haiku-5-5"), "two subagents, on Haiku: {subs:?}");

    // One that is set is used instead.
    let e = Env::with(&[("MOCK_SCENARIO", "agents"), ("MOCK_CMD", "true")]);
    let mut cmd = e.command(&["-y", "-p", "look around", "-m", "claude-sonnet-5-5"]);
    cmd.env("CLAUDE_JR_WORKER_MODEL", "claude-sonnet-5");
    ok(&exec(cmd));
    let (main, subs) = asked(&e);
    assert!(main.iter().all(|m| m == "claude-sonnet-5-5") && subs.iter().all(|m| m == "claude-sonnet-5"));
    assert!(!subs.is_empty());
}
