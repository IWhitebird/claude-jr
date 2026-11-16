// Models of other providers: Chat Completions, another vendor's Messages API,
// keys, and mistakes.

use super::common::*;

#[test]
fn a_whole_run_with_tools_through_chat_completions() {
    let e = Env::new();
    let o = ollama(&e, &["-y", "-p", "do the demo", "-m", "ollama/qwen3:8b", "--output-format", "json"]);
    ok(&o);
    nothing_refused(&e);
    let r: serde_json::Value = serde_json::from_str(o.out.trim()).unwrap_or_else(|err| panic!("{err}: {}", o.out));
    assert_eq!(r["ok"], true);
    assert!(r["result"].as_str().unwrap().starts_with("Done. Here's what happened:"), "{r}");
    assert_eq!(std::fs::read_to_string(e.proj.join("demo.txt")).unwrap(), "one\nTWO\nthree\n");
    assert_eq!(r["requests"], 3);
    assert!(
        r["input_tokens"].as_u64().unwrap() > 0 && r["cache_read_tokens"].as_u64().unwrap() > 0,
        "usage is read: {r}"
    );
    assert_eq!(r["cost_usd"], 0.0, "no price is known for it");
    assert!(o.err.contains("Round 1: checking the output."), "the model's thinking is shown: {}", o.err);

    // What was sent is Chat Completions, to the right place, with nothing of Anthropic's in it.
    let sent = e.requests();
    let heads = headers(&e);
    assert!(heads.iter().all(|h| h["path"] == "/v1/chat/completions"), "{heads:?}");
    assert!(heads.iter().all(|h| h.get("x-api-key").is_none()
        && h.get("authorization").is_none()
        && h.get("anthropic-version").is_none()));
    let first = &sent[0];
    assert_eq!(first["model"], "qwen3:8b");
    assert_eq!(first["messages"][0]["role"], "system");
    assert!(first["messages"][0]["content"].as_str().unwrap().contains("claude-jr"));
    assert_eq!(first["messages"][1], serde_json::json!({ "role": "user", "content": "do the demo" }));
    let tools: Vec<&str> =
        first["tools"].as_array().unwrap().iter().map(|t| t["function"]["name"].as_str().unwrap()).collect();
    assert!(tools.contains(&"bash") && tools.contains(&"edit") && !tools.contains(&"web_search"), "{tools:?}");
    for field in ["thinking", "cache_control", "system", "context_management", "max_tokens"] {
        assert!(first.get(field).is_none(), "{field} was sent");
    }
    // The second request carries the calls and their results back.
    let second = sent[1]["messages"].as_array().unwrap();
    let calls = second[2]["tool_calls"].as_array().unwrap();
    assert_eq!(calls.len(), 4);
    assert!(calls[0]["function"]["arguments"].is_string());
    let roles: Vec<&str> = second.iter().map(|m| m["role"].as_str().unwrap()).collect();
    assert_eq!(roles, ["system", "user", "assistant", "tool", "tool", "tool", "tool"]);
    assert_eq!(second[3]["tool_call_id"], calls[0]["id"]);
}

#[test]
fn subagents_skills_and_jobs_work_through_chat_completions() {
    let e = Env::with(&[("MOCK_SCENARIO", "features")]);
    e.write_config(&mcp_config(&e));
    let o = ollama(&e, &["-y", "-p", "check", "-m", "ollama/test"]);
    ok(&o);
    nothing_refused(&e);
    for want in ["Todos", "agent:", "Started job", "All features checked"] {
        assert!(o.out.contains(want) || o.err.contains(want), "{want} is missing:\n{}\n{}", o.out, o.err);
    }
}

#[test]
fn a_conversation_carries_on_across_providers() {
    // Started with Claude (whose replies have thinking in them), continued
    // with a local model, then back: each gets the history in its own format.
    let e = Env::new();
    ok(&e.run(&["-y", "-p", "do the demo"]));
    let o = ollama(&e, &["-y", "-c", "-p", "carry on", "-m", "ollama/test"]);
    ok(&o);
    assert!(o.out.contains("Done."), "{}", o.out);
    let o = e.run(&["-y", "-c", "-p", "and again"]);
    ok(&o);
    nothing_refused(&e);
    let sent = e.requests();
    let chat = sent.iter().find(|r| r["model"] == "test").unwrap();
    let text = chat.to_string();
    assert!(!text.contains("thinking") && !text.contains("signature"), "Claude's thinking isn't sent to another model");
    assert!(chat["messages"].as_array().unwrap().iter().any(|m| m["role"] == "tool"));
    // Back with Claude, the local model's turns are ordinary messages.
    let last = sent.last().unwrap();
    assert_eq!(last["model"], "claude-opus-5-5");
    let history = last["messages"].to_string();
    assert!(history.contains("do the demo") && history.contains("carry on") && history.contains("and again"));
    assert_eq!(history.matches("\"tool_use\"").count(), history.matches("\"tool_result\"").count());
}

#[test]
fn providers_from_the_config_keys_and_mistakes() {
    let e = Env::new();
    let base = format!("http://127.0.0.1:{}", e.port);
    e.write_config(&format!(
        r#"{{"providers": {{
            "work": {{"base_url": "{base}/v1/", "key_env": "WORK_KEY", "max_tokens": 4096}},
            "other": {{"base_url": "{base}", "api": "anthropic", "key_env": "OTHER_KEY"}}
        }}}}"#
    ));
    // A key that isn't set is said before anything is sent, by name.
    let o = e.run(&["-y", "-p", "x", "-m", "work/big"]);
    assert_eq!(o.code, 2);
    assert!(o.err.contains("work needs an API key: set WORK_KEY"), "{}", o.err);
    assert!(e.requests().is_empty());

    // With it, the key goes as a bearer token, and nothing of Anthropic's key does.
    let mut cmd = e.command(&["-y", "-p", "do the demo", "-m", "work/big"]);
    cmd.env("WORK_KEY", "sk-work").env_remove("ANTHROPIC_API_KEY");
    ok(&exec(cmd));
    let h = &headers(&e)[0];
    assert_eq!((h["authorization"].as_str(), h.get("x-api-key")), (Some("Bearer sk-work"), None), "{h}");
    assert_eq!(h["path"], "/v1/chat/completions");
    assert_eq!((e.requests()[0]["model"].as_str(), e.requests()[0]["max_tokens"].as_u64()), (Some("big"), Some(4096)));

    // Another vendor serving the Messages format gets the plain request.
    let before = e.requests().len();
    let mut cmd = e.command(&["-y", "-p", "do the demo", "-m", "other/their-model", "-e", "high"]);
    cmd.env("OTHER_KEY", "sk-other").env_remove("ANTHROPIC_API_KEY");
    let o = exec(cmd);
    ok(&o);
    nothing_refused(&e);
    let (req, h) = (&e.requests()[before], &headers(&e)[before]);
    assert_eq!(h["path"], "/v1/messages");
    assert_eq!((h["x-api-key"].as_str(), h["authorization"].as_str()), (Some("sk-other"), Some("Bearer sk-other")));
    assert!(h.get("anthropic-beta").is_none(), "{h}");
    assert_eq!((req["model"].as_str(), req["max_tokens"].as_u64()), (Some("their-model"), Some(16384)));
    for field in ["thinking", "cache_control", "context_management", "fallbacks", "output_config"] {
        assert!(req.get(field).is_none(), "{field} was sent to another vendor");
    }
    assert!(req["tools"].as_array().unwrap().iter().all(|t| t.get("input_schema").is_some()), "no server-side tools");

    // A name that is no provider is said, with the ones there are.
    let o = e.run(&["-y", "-p", "x", "-m", "nope/model"]);
    assert_eq!(o.code, 2);
    assert!(
        o.err.contains("nope/model: no provider by that name") && o.err.contains("work") && o.err.contains("ollama"),
        "{}",
        o.err
    );
    // A built-in one without its key.
    let o = e.run(&["-y", "-p", "x", "-m", "openai/gpt-5"]);
    assert_eq!(o.code, 2);
    assert!(o.err.contains("openai needs an API key: set OPENAI_API_KEY"), "{}", o.err);
    // A server that isn't running is an ordinary connection error.
    let mut cmd = e.command(&["-y", "-p", "x", "-m", "ollama/x"]);
    cmd.env("OLLAMA_HOST", "127.0.0.1:9").env("CLAUDE_JR_OFFLINE_SECS", "1");
    let o = exec(cmd);
    assert_eq!(o.code, 1);
    assert!(o.err.contains("can't reach the API"), "{}", o.err);
    // A config with a mistake in a provider names the file.
    e.write_config(r#"{"providers": {"x": {"base_url": "u", "api": "soap"}}}"#);
    let o = e.run(&["-p", "x"]);
    assert_eq!(o.code, 2);
    assert!(o.err.contains("config.json"), "{}", o.err);
}
