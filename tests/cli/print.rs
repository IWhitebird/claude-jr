// -p mode: a run with tools, JSON and streamed output, the run log, cost limits,
// permissions, config, and every kind of API failure.

use super::common::*;

#[test]
fn json_output_is_one_object_with_the_final_answer() {
    let e = Env::new();
    let o = e.run(&["-p", "do the demo", "-y", "--output-format", "json"]);
    ok(&o);
    // stdout is exactly one JSON object; progress and the reply as it comes are on stderr.
    let r: serde_json::Value = serde_json::from_str(o.out.trim()).unwrap_or_else(|e| panic!("{e}: {}", o.out));
    assert_eq!(r["ok"], true);
    assert_eq!(r["error"], serde_json::Value::Null);
    assert_eq!(r["requests"], 3);
    assert!(r["cost_usd"].as_f64().unwrap() > 0.0);
    assert!(r.get("duration_ms").is_some() && r.get("output_tokens").is_some());
    let result = r["result"].as_str().unwrap();
    assert!(result.starts_with("Done. Here's what happened:") && result.contains("| file | change"), "{result}");
    assert!(o.err.contains("● ") && o.err.contains("Done. Here's what happened:"), "{}", o.err);

    // A failed run is still one JSON object, with the reason.
    let closed = Env::new();
    let mut cmd = closed.command(&["-p", "hi", "-y", "--output-format", "json"]);
    cmd.env("ANTHROPIC_BASE_URL", "http://127.0.0.1:9").env("CLAUDE_JR_OFFLINE_SECS", "1");
    let o = exec(cmd);
    assert_ne!(o.code, 0);
    let r: serde_json::Value = serde_json::from_str(o.out.trim()).unwrap_or_else(|e| panic!("{e}: {}", o.out));
    assert_eq!(r["ok"], false);
    assert!(r["error"].as_str().is_some_and(|e| !e.is_empty()), "{r}");

    let o = e.run(&["-p", "x", "--output-format", "xml"]);
    assert_eq!(o.code, 2);
    assert!(o.err.contains("unknown output format: xml"), "{}", o.err);
}

#[test]
fn stream_json_and_the_log_file_record_every_step() {
    let e = Env::new();
    let log = e.root.join("run.jsonl");
    let o = e.run(&["-p", "do the demo", "-y", "--output-format", "stream-json", "--log", log.to_str().unwrap()]);
    ok(&o);
    // stdout is nothing but the log, a JSON object per line.
    let lines = json_lines(&o.out);
    let kinds: Vec<&str> = lines.iter().map(|l| l["type"].as_str().unwrap()).collect();
    assert_eq!(kinds.first(), Some(&"start"));
    assert_eq!(kinds.last(), Some(&"result"));
    assert_eq!(lines[0]["prompt"], "do the demo");
    let steps: Vec<&serde_json::Value> = lines.iter().filter(|l| l["type"] == "step").collect();
    let tools: Vec<&str> = steps.iter().map(|s| s["tool"].as_str().unwrap()).collect();
    assert_eq!(tools, ["bash", "write", "read", "edit", "bash"]);
    assert!(steps.iter().all(|s| s["ok"] == true && s["ms"].is_u64()), "{steps:?}");
    assert_eq!(steps[0]["allowed_by"], "skip-permissions mode");
    assert_eq!(steps[2]["allowed_by"], "needs no approval");
    assert_eq!(steps[1]["input"]["path"], "demo.txt");
    assert_eq!(steps[3]["input"]["new_string"], "TWO");
    assert_eq!(kinds.iter().filter(|k| **k == "usage").count(), 3, "one per request");
    assert!(kinds.contains(&"text"));

    let result = lines.last().unwrap();
    assert_eq!(result["ok"], true);
    assert!(result["result"].as_str().unwrap().starts_with("Done. Here's what happened:"));
    assert_eq!(result["files"][0]["path"], "demo.txt");
    assert!(result["cost_usd"].as_f64().unwrap() > 0.0);

    // The file holds the same lines.
    let filed = json_lines(&std::fs::read_to_string(&log).unwrap());
    assert_eq!(filed.len(), lines.len());
    assert_eq!(filed.last().unwrap()["result"], result["result"]);
}

#[test]
fn the_log_says_what_was_refused_and_what_a_rule_allowed() {
    let e = Env::with(&[("MOCK_CMD", "echo hi")]);
    e.write_config(r#"{"allow": ["bash:echo *"]}"#);
    let log = e.root.join("run.jsonl");
    let o = e.run(&["-p", "do the demo", "--log", log.to_str().unwrap()]);
    ok(&o);
    assert!(o.out.contains("Done."), "text output is unchanged by --log: {}", o.out);
    let lines = json_lines(&std::fs::read_to_string(&log).unwrap());
    let step = |tool: &str| lines.iter().find(|l| l["type"] == "step" && l["tool"] == tool).unwrap().clone();
    assert_eq!(step("bash")["allowed_by"], "allow rule");
    let write = step("write");
    assert!(write["allowed_by"].is_null() && write["ok"] == false, "{write}");
    assert!(write["result"].as_str().unwrap().contains("Not allowed"), "{write}");
    assert!(lines.last().unwrap()["files"].as_array().unwrap().is_empty(), "nothing was changed");

    // In a git repository the files come from git, so what commands change counts too.
    let e = Env::with(&[("MOCK_CMD", "echo made > by-a-command.txt"), ("MOCK_ROUNDS", "1")]);
    let git =
        |args: &[&str]| assert!(Command::new("git").args(args).current_dir(&e.proj).output().unwrap().status.success());
    git(&["init", "-q"]);
    let o = e.run(&["-p", "go", "-y", "--output-format", "json"]);
    ok(&o);
    let r: serde_json::Value = serde_json::from_str(o.out.trim()).unwrap();
    let files: Vec<&str> = r["files"].as_array().unwrap().iter().map(|f| f["path"].as_str().unwrap()).collect();
    assert_eq!(files, ["by-a-command.txt", "demo.txt"]);
    assert_eq!(r["files"][0]["status"], "new");

    // A log that can't be written is said up front, before anything is sent.
    let o = e.run(&["-p", "go", "--log", "/nonexistent-dir/run.jsonl"]);
    assert_eq!(o.code, 2);
    assert!(o.err.contains("can't write the log"), "{}", o.err);
}

#[test]
fn max_cost_stops_the_run() {
    let e = Env::new();
    let report = e.root.join("report.json");
    let mut cmd = e.command(&["-p", "do the demo", "-y", "--max-cost", "0.000001"]);
    cmd.env("CLAUDE_JR_REPORT", &report);
    let o = exec(cmd);
    assert_eq!(o.code, 1, "{}", o.err);
    assert!(o.err.contains("stopped: the cost limit of $0.00 was reached"), "{}", o.err);
    let r: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
    assert_eq!(r["ok"], false);
    assert!(r["error"].as_str().unwrap().starts_with("stopped: the cost limit"));
    assert!(r["requests"].as_u64().unwrap() < 3, "it stopped before the end: {r}");

    // A generous limit changes nothing.
    let o = e.run(&["-p", "do the demo", "-y", "--max-cost", "$5"]);
    ok(&o);
    assert!(o.out.contains("Done. Here's what happened:"), "{}", o.out);

    for bad in ["abc", "0", "-1"] {
        let o = e.run(&["-p", "x", "--max-cost", bad]);
        assert_eq!(o.code, 2, "{bad}");
        assert!(o.err.contains("--max-cost needs an amount in dollars"), "{}", o.err);
    }
}

#[test]
fn default_scenario_runs_tools() {
    let e = Env::new();
    let report = e.root.join("report.json");
    let mut cmd = e.command(&["-p", "do the demo", "-y"]);
    cmd.env("CLAUDE_JR_REPORT", &report);
    let o = exec(cmd);
    ok(&o);
    assert!(o.out.contains("Done. Here's what happened:") && o.out.contains("| file | change"), "{}", o.out);
    assert!(o.err.contains("● ") && o.err.contains("demo.txt"), "{}", o.err);
    assert!(o.err.contains("Round 1: checking the output."), "thinking goes to stderr: {}", o.err);
    assert_eq!(std::fs::read_to_string(e.proj.join("demo.txt")).unwrap(), "one\nTWO\nthree\n");

    let r: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
    assert_eq!(r["ok"], true);
    assert_eq!(r["requests"], 3);
    assert!(r["input_tokens"].as_u64().unwrap() >= 150);
    assert!(r["output_tokens"].as_u64().unwrap() >= 120);
    assert!(r["cache_read_tokens"].as_u64().unwrap() > 0);
    assert!(r["cost_usd"].as_f64().unwrap() > 0.0);
    assert!(r.get("cache_write_tokens").is_some());

    let reqs = e.requests();
    assert_eq!(reqs.len(), 3);
    assert_eq!(reqs[0]["model"], "claude-opus-5-5");
    let tools: Vec<&str> = reqs[0]["tools"].as_array().unwrap().iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(!tools.contains(&"ask_user"), "nobody is there to ask in -p mode: {tools:?}");
    for t in ["bash", "read", "write", "edit"] {
        assert!(tools.contains(&t), "{tools:?}");
    }
}

/// Each way a request can go wrong, at each point of a run with tools: the
/// run either recovers or says what happened, never sends a conversation the
/// API would refuse, and the saved session can always be continued.
#[test]
fn every_kind_of_api_failure_is_survived() {
    // (what goes wrong, whether the run should still succeed)
    let kinds = [
        ("500", true),
        ("529", true),
        ("429", true),
        ("early", true),
        ("400", false),
        ("401", false),
        ("garbage", false),
        ("cut", false),
        ("overloaded", false),
        ("half-tool", false),
    ];
    let runs: Vec<_> = kinds
        .iter()
        .flat_map(|&(kind, recovers)| (1..=3).map(move |at| (kind, recovers, at)))
        // Through each door: Anthropic's own, Chat Completions, Bedrock (signed), Vertex.
        .flat_map(|run| (0..4).map(move |door| (run, door)))
        .map(|((kind, recovers, at), door)| {
            std::thread::spawn(move || {
                let fail = format!("{at}:{kind}");
                let e = Env::with(&[("MOCK_FAIL", &fail), ("MOCK_ROUNDS", "2")]);
                let model =
                    ["claude-opus-5-5", "ollama/test-model", "bedrock/claude-opus-5-5", "vertex/claude-opus-5-5"][door];
                let run = |args: &[&str]| {
                    let mut cmd = e.command(args);
                    cmd.args(["-m", model]).env("OLLAMA_HOST", format!("127.0.0.1:{}", e.port));
                    if door >= 2 {
                        through_cloud(&e, &mut cmd);
                    }
                    exec(cmd)
                };
                let fail = format!("{fail} ({model})");
                let o = run(&["-y", "-p", "do the work"]);
                let what = format!("{fail}: exit {}\n{}\n{}", o.code, o.out, o.err);
                if recovers {
                    assert_eq!(o.code, 0, "{what}");
                    assert!(o.out.contains("Done."), "{what}");
                } else {
                    assert_eq!(o.code, 1, "{what}");
                    assert!(o.err.contains("error:"), "{what}");
                    if kind == "garbage" {
                        assert!(
                            o.err.contains("not as the API does (text/html)") && o.err.contains("502 from some proxy"),
                            "{what}"
                        );
                    }
                }
                assert!(!o.err.contains("panicked"), "{what}");
                // Whatever happened, the conversation goes on from there.
                let o = run(&["-y", "-c", "-p", "carry on"]);
                assert_eq!(o.code, 0, "{fail}, continued: exit {}\n{}\n{}", o.code, o.out, o.err);
                assert!(o.out.contains("Done."), "{fail}, continued:\n{}", o.out);
                let refused = std::fs::read_to_string(e.root.join("requests.jsonl.invalid")).unwrap_or_default();
                assert!(refused.is_empty(), "{fail}: a malformed conversation was sent: {refused}");
            })
        })
        .collect();
    let failed: Vec<String> = runs
        .into_iter()
        .filter_map(|r| r.join().err())
        .map(|e| e.downcast_ref::<String>().cloned().unwrap_or_else(|| "a run panicked".into()))
        .collect();
    assert!(failed.is_empty(), "{} of {} failed:\n\n{}", failed.len(), kinds.len() * 12, failed.join("\n\n"));
}

/// A failure that doesn't go away is given up on in reasonable time, with
/// what the API said.
#[test]
fn a_failure_that_persists_is_reported_with_its_cause() {
    for (kind, want) in
        [("500", "API error 500: the fake API was told to answer 500"), ("early", "the stream ended before any reply")]
    {
        let fail: Vec<String> = (1..=12).map(|n| format!("{n}:{kind}")).collect();
        let e = Env::with(&[("MOCK_FAIL", &fail.join(","))]);
        let start = Instant::now();
        let o = e.run(&["-y", "-p", "hi"]);
        assert_eq!(o.code, 1, "{kind}: {}", o.err);
        assert!(o.err.contains(want), "{kind}: {}", o.err);
        assert_eq!(e.requests().len(), 5, "{kind}: five tries, then it stops");
        assert!(start.elapsed() < Duration::from_secs(30), "{kind}: {:?}", start.elapsed());
    }
}

#[test]
fn api_failure_is_an_error() {
    let e = Env::new();
    // Port 1: nothing listens there, and no test can take it (a port that
    // was merely free a moment ago could be another test's fake API by now).
    let mut cmd = e.command(&["-p", "hi", "-y"]);
    cmd.env("ANTHROPIC_BASE_URL", "http://127.0.0.1:1");
    let report = e.root.join("report.json");
    cmd.env("CLAUDE_JR_REPORT", &report);
    let o = exec(cmd);
    assert_eq!(o.code, 1, "{}{}", o.out, o.err);
    assert!(o.err.contains("error:"), "{}", o.err);
    let r: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
    assert_eq!(r["ok"], false);
    assert_eq!(r["requests"], 0);
}

#[test]
fn model_and_effort_precedence() {
    let e = Env::with(&[("MOCK_ROUNDS", "0")]);
    e.write_config(r#"{"model":"from-config","effort":"low"}"#);
    let run = |args: &[&str], env_model: Option<&str>| {
        let mut cmd = e.command(args);
        if let Some(m) = env_model {
            cmd.env("CLAUDE_JR_MODEL", m);
        }
        ok(&exec(cmd));
    };
    run(&["-p", "aaa", "-y"], None);
    run(&["-p", "bbb", "-y"], Some("from-env"));
    run(&["-p", "ccc", "-y", "-m", "from-flag", "-e", "high"], Some("from-env"));
    let reqs = e.requests();
    let first_of = |prompt: &str| {
        reqs.iter()
            .find(|r| r["messages"][0]["content"].to_string().contains(prompt))
            .unwrap_or_else(|| panic!("no request for {prompt}"))
            .clone()
    };
    assert_eq!(first_of("aaa")["model"], "from-config");
    assert_eq!(first_of("bbb")["model"], "from-env");
    assert_eq!(first_of("ccc")["model"], "from-flag");
    assert!(first_of("aaa")["output_config"].to_string().contains("low"), "{}", first_of("aaa"));
    assert!(first_of("ccc")["output_config"].to_string().contains("high"), "effort from the flag wins");
}

#[test]
fn both_yes_flags_skip_permissions() {
    for flag in ["-y", "--yes", "--dangerously-skip-permissions"] {
        let e = Env::new();
        let o = e.run(&["-p", "go", flag]);
        ok(&o);
        assert!(e.proj.join("demo.txt").exists(), "{flag}: {}", o.err);
    }
}

#[test]
fn print_mode_refuses_without_yes() {
    let e = Env::new();
    let o = e.run(&["-p", "go"]);
    ok(&o); // the turn finishes; the tools were refused
    assert!(o.err.contains("nobody is here to approve"), "{}", o.err);
    assert!(!e.proj.join("demo.txt").exists());
    assert!(e.requests_text().contains("nobody is here to approve"), "the model is told");
}

#[test]
fn allow_rules_let_tools_run() {
    let e = Env::new();
    e.write_config(r#"{"allow":["write","edit","bash"]}"#);
    let o = e.run(&["-p", "go"]);
    ok(&o);
    assert!(!o.err.contains("nobody is here"), "{}", o.err);
    assert_eq!(std::fs::read_to_string(e.proj.join("demo.txt")).unwrap(), "one\nTWO\nthree\n");
}

#[test]
fn partial_allow_rule() {
    let e = Env::with(&[("MOCK_ROUNDS", "1"), ("MOCK_CMD", "echo hello-from-bash")]);
    e.write_config(r#"{"allow":["bash:echo *"]}"#);
    let o = e.run(&["-p", "go"]);
    ok(&o);
    assert!(o.err.contains("hello-from-bash"), "bash ran: {}", o.err);
    assert!(o.err.contains("nobody is here"), "write was refused: {}", o.err);
    assert!(!e.proj.join("demo.txt").exists());
}

#[test]
fn config_mode_from_file() {
    let e = Env::new();
    e.write_config(r#"{"mode":"skip-permissions"}"#);
    ok(&e.run(&["-p", "go"]));
    assert!(e.proj.join("demo.txt").exists());
}

#[test]
fn bad_config_names_the_file() {
    let e = Env::new();
    e.write_config("{ not json");
    let o = e.run(&["-p", "hi"]);
    assert_eq!(o.code, 2);
    assert!(o.err.contains("config.json"), "{}", o.err);
    let e = Env::new();
    e.write_config(r#"{"bogus_key": 1}"#);
    let o = e.run(&["-p", "hi"]);
    assert_eq!(o.code, 2);
    assert!(o.err.contains("config.json") && o.err.contains("bogus_key"), "{}", o.err);
}

#[test]
fn web_tools_setting() {
    let has_web = |r: &serde_json::Value| r["tools"].to_string().contains("web_search");
    let e = Env::with(&[("MOCK_SCENARIO", "web")]);
    let o = e.run(&["-p", "how do I configure vite?", "-y"]);
    ok(&o);
    assert!(o.out.contains("Vite reads vite.config.ts."), "{}", o.out);
    assert!(has_web(&e.requests()[0]));

    let e = Env::with(&[("MOCK_SCENARIO", "web")]);
    e.write_config(r#"{"web_tools":false}"#);
    ok(&e.run(&["-p", "q", "-y"]));
    assert!(!has_web(&e.requests()[0]));
}

/// Output nobody can read is no reason to crash (this build aborts on a
/// panic, and Rust's print macros panic on a failed write): a reader that has
/// gone, as in `claudejr -p ... 2>&1 | head -1`, is fine; a full disk fails
/// the run and says so.
#[test]
fn output_that_cannot_be_written_never_crashes_it() {
    let e = Env::new();
    for format in ["text", "json", "stream-json"] {
        // Both streams into a pipe that is already closed.
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let mut cmd = e.command(&["-p", "do the demo", "-y", "--output-format", format]);
        cmd.stdout(writer.try_clone().unwrap()).stderr(writer).stdin(Stdio::null());
        let status = cmd.status().unwrap();
        assert!(status.success(), "{format}, read by no one: {status:?}");
    }
    #[cfg(target_os = "linux")]
    for (format, stdout) in [("text", true), ("json", true), ("text", false)] {
        let full = std::fs::OpenOptions::new().write(true).open("/dev/full").unwrap();
        let mut cmd = e.command(&["-p", "do the demo", "-y", "--output-format", format]);
        if stdout {
            cmd.stdout(full).stderr(Stdio::piped());
        } else {
            cmd.stderr(full).stdout(Stdio::piped());
        }
        let o = cmd.stdin(Stdio::null()).output().unwrap();
        let err = String::from_utf8_lossy(&o.stderr);
        if stdout {
            assert_eq!(o.status.code(), Some(1), "{format}: the reply was lost, so the run failed: {err}");
            assert!(err.contains("No space left on device"), "{format}: {err}");
        } else {
            assert!(o.status.success(), "progress lost to a full disk is no failure: {:?}", o.status);
            assert!(String::from_utf8_lossy(&o.stdout).contains("Done."));
        }
    }
}

/// What a command prints reaches the model as a terminal would show it:
/// no colour codes, a progress bar only as it ended.
#[test]
fn command_output_reaches_the_model_as_a_terminal_shows_it() {
    let cmd = r"printf '\033[1;32mPASSED\033[0m 3 tests\n'; printf '10%%\r60%%\r100%%\n'; printf 'tail\n'";
    let e = Env::with(&[("MOCK_CMD", cmd), ("MOCK_ROUNDS", "1")]);
    ok(&e.run(&["-p", "run it", "-y"]));
    let last = e.requests().pop().unwrap();
    let result = last["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| m["content"].as_array().cloned().unwrap_or_default())
        .find(|c| c["tool_use_id"] == "b0")
        .unwrap_or_else(|| panic!("no result for the command: {last}"));
    assert_eq!(result["content"], "PASSED 3 tests\n100%\ntail\n");
}
