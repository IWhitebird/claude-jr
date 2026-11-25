// Tests of the Chat Completions translation: fixed cases, then random
// conversations and random streams.

use super::*;
use std::io::Read;

#[test]
fn messages_translate_block_by_block() {
    let user = json!({ "role": "user", "content": [{ "type": "text", "text": "hi" }] });
    assert_eq!(messages(&user), [json!({ "role": "user", "content": "hi" })]);
    assert_eq!(
        messages(&json!({ "role": "user", "content": "plain" })),
        [json!({ "role": "user", "content": "plain" })]
    );

    // A reply with thinking, text and two calls: the thinking stays behind.
    let reply = json!({ "role": "assistant", "content": [
        { "type": "thinking", "thinking": "hm", "signature": "s" },
        { "type": "text", "text": "Looking." },
        { "type": "tool_use", "id": "a", "name": "read", "input": { "path": "x.rs" } },
        { "type": "tool_use", "id": "b", "name": "bash", "input": {} },
    ]});
    let out = messages(&reply);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["content"], "Looking.");
    assert_eq!(
        out[0]["tool_calls"][0],
        json!({ "id": "a", "type": "function", "function": { "name": "read", "arguments": "{\"path\":\"x.rs\"}" } })
    );
    assert_eq!(out[0]["tool_calls"][1]["function"]["arguments"], "{}");
    // Calls alone: content is still a string, as strict servers want.
    let only_calls =
        json!({ "role": "assistant", "content": [{ "type": "tool_use", "id": "a", "name": "x", "input": {} }] });
    assert_eq!(messages(&only_calls)[0]["content"], "");

    // Results become tool messages, ahead of anything the user added.
    let results = json!({ "role": "user", "content": [
        { "type": "text", "text": "and also this" },
        { "type": "tool_result", "tool_use_id": "a", "content": "file text", "is_error": false },
        { "type": "tool_result", "tool_use_id": "b", "content": [{ "type": "text", "text": "one" }, { "type": "image", "source": {} }, { "type": "text", "text": "two" }] },
    ]});
    let out = messages(&results);
    assert_eq!(out[0], json!({ "role": "tool", "tool_call_id": "a", "content": "file text" }));
    assert_eq!(out[1]["tool_call_id"], "b");
    assert!(
        out[1]["content"].as_str().unwrap().starts_with("one\n[an image")
            && out[1]["content"].as_str().unwrap().ends_with("two")
    );
    assert_eq!(out[2], json!({ "role": "user", "content": "and also this" }));

    // An image goes as a data URL beside the text.
    let image = json!({ "role": "user", "content": [
        { "type": "text", "text": "what is this" },
        { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "QUJD" } },
    ]});
    let out = messages(&image);
    assert_eq!(out[0]["content"][0], json!({ "type": "text", "text": "what is this" }));
    assert_eq!(out[0]["content"][1]["image_url"]["url"], "data:image/png;base64,QUJD");
    assert!(messages(&json!({ "role": "user", "content": [] })).is_empty());
}

#[test]
fn a_stored_image_is_read_from_its_file() {
    crate::tools::test_home();
    let png = crate::tools::test_dir("openai-image").join("x.png");
    std::fs::write(&png, b"\x89PNG\r\n\x1a\nABC").unwrap();
    let block = crate::store::images::block(&png).unwrap();
    let out = messages(&json!({ "role": "user", "content": [block] }));
    let url = out[0]["content"][0]["image_url"]["url"].as_str().unwrap();
    assert_eq!(url, "data:image/png;base64,iVBORw0KGgpBQkM=");
    // Text that merely looks like a stored image's marker can't pull a file in.
    assert_eq!(crate::store::images::inline("@@claude-jr-image:/etc/passwd@@"), "@@claude-jr-image:/etc/passwd@@");
}

fn provider() -> Provider {
    crate::api::provider::resolve("ollama/m", &Default::default()).unwrap().0
}

#[test]
fn the_head_has_tools_as_functions_and_only_what_was_asked_for() {
    let defs = r#"[{"name":"read","description":"Read a file","input_schema":{"type":"object","properties":{"path":{"type":"string"}}}},
                   {"type":"web_search_20260209","name":"web_search","max_uses":5}]"#;
    let tools = serde_json::value::RawValue::from_string(defs.into()).unwrap();
    let req = Request::new("ollama/m", None, "SYS", &tools);
    let h = head(&req, "m", &provider());
    assert_eq!(h["model"], "m");
    assert_eq!(h["stream_options"]["include_usage"], true);
    assert_eq!(h["tools"].as_array().unwrap().len(), 1, "the server-side tool is left out");
    assert_eq!(h["tools"][0]["function"]["parameters"]["properties"]["path"]["type"], "string");
    assert!(h.get("max_tokens").is_none() && h.get("reasoning_effort").is_none());
    for field in ["thinking", "cache_control", "system", "context_management"] {
        assert!(h.get(field).is_none(), "{field}");
    }

    let none = serde_json::value::RawValue::from_string("[]".into()).unwrap();
    let req = Request::new("ollama/m", Some("xhigh"), "SYS", &none);
    let h = head(&req, "m", &Provider { max_tokens: Some(4096), ..provider() });
    assert!(h.get("tools").is_none(), "no empty tool list");
    assert_eq!((h["max_tokens"].as_u64(), h["reasoning_effort"].as_str()), (Some(4096), Some("high")));
}

#[test]
fn the_body_is_one_valid_request_of_exactly_the_length_promised() {
    let head = serde_json::to_vec(&json!({ "model": "m", "stream": true })).unwrap();
    let history = [
        r#"{"role":"user","content":[{"type":"text","text":"go \u00e9"}]}"#,
        r#"{"role":"assistant","content":[{"type":"thinking","thinking":"x"},{"type":"tool_use","id":"t1","name":"bash","input":{"command":"ls"}}]}"#,
        r#"{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"a\nb"}]}"#,
        r#"{"role":"user","content":[]}"#,
        r#"{"role":"assistant","content":[{"type":"text","text":"done"}]}"#,
    ];
    let kept = Mutex::new(Translated::default());
    for (system, count) in [("SYS", 5), ("", 4)] {
        let mut body = body(&head, system, &history, &kept).unwrap();
        let mut out = Vec::new();
        // Read through a tiny buffer, as a slow connection would.
        let mut buf = [0u8; 7];
        loop {
            let n = body.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        assert_eq!(out.len(), body.len, "system {system:?}");
        let v: Value = serde_json::from_slice(&out).unwrap();
        let roles: Vec<&str> = v["messages"].as_array().unwrap().iter().map(|m| m["role"].as_str().unwrap()).collect();
        assert_eq!(roles.len(), count);
        assert_eq!(&roles[roles.len() - 4..], ["user", "assistant", "tool", "assistant"]);
        assert_eq!((v["model"].as_str(), v["stream"].as_bool()), (Some("m"), Some(true)));
    }
    // No messages at all is still a valid request.
    let mut empty = Vec::new();
    body(&head, "", &[] as &[&str], &kept).unwrap().read_to_end(&mut empty).unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&empty).unwrap()["messages"], json!([]));
    // A message that isn't JSON is an error, not a broken request.
    assert!(body(&head, "", &["not json"], &kept).is_err());
}

#[test]
fn saved_messages_are_translated_once_and_then_read_from_disk() {
    crate::tools::test_home();
    let dir = crate::tools::test_dir("openai-kept");
    let lines = [
        r#"{"role":"user","content":[{"type":"text","text":"first"}]}"#,
        r#"{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"bash","input":{"command":"ls"}}]}"#,
        r#"{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"out"}]}"#,
        r#"{"role":"user","content":[]}"#,
    ];
    let path = dir.join("session.jsonl");
    std::fs::write(&path, lines.join("\n")).unwrap();
    let file = File::open(&path).unwrap();
    let mut at = 0;
    let mut saved: Vec<Text> = Vec::new();
    for line in lines {
        saved.push(Text::Saved { file: &file, at, len: line.len() });
        at += line.len() as u64 + 1;
    }
    let head = serde_json::to_vec(&json!({ "model": "m" })).unwrap();
    let kept = Mutex::new(Translated::default());
    let read = |source: &[Text]| {
        let mut out = Vec::new();
        let mut b = body(&head, "", source, &kept).unwrap();
        b.read_to_end(&mut out).unwrap();
        assert_eq!(out.len(), b.len);
        serde_json::from_slice::<Value>(&out).unwrap()
    };
    let first = read(&saved);
    let roles: Vec<&str> = first["messages"].as_array().unwrap().iter().map(|m| m["role"].as_str().unwrap()).collect();
    assert_eq!(roles, ["user", "assistant", "tool"], "the empty message adds nothing, and no stray comma");
    assert_eq!(kept.lock().unwrap().at.len(), 4, "each saved message is kept, the empty one too");
    let size = kept.lock().unwrap().end;

    // Asked again, with a newer unsaved message after them: nothing is translated twice.
    let mut more = saved.clone();
    more.push(Text::Memory(r#"{"role":"assistant","content":[{"type":"text","text":"newest"}]}"#));
    let second = read(&more);
    assert_eq!(second["messages"].as_array().unwrap()[..3], first["messages"].as_array().unwrap()[..]);
    assert_eq!(second["messages"][3]["content"], "newest");
    // The four saved ones were reused; only the new one was added, and a
    // message in memory is kept too (a subagent's whole history is).
    // (One lock at a time: two in one expression would wait on each other.)
    let entries = kept.lock().unwrap().at.len();
    let grown = kept.lock().unwrap().end;
    assert_eq!(entries, 5);
    assert!(grown > size);
    read(&more);
    assert_eq!(kept.lock().unwrap().end, grown, "asked a third time, nothing is translated");

    // With nowhere to keep them it still works, translating each time.
    let nowhere = Mutex::new(Translated { unusable: true, ..Translated::default() });
    let mut out = Vec::new();
    body(&head, "", &saved, &nowhere).unwrap().read_to_end(&mut out).unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&out).unwrap(), first);
    let _ = std::fs::remove_dir_all(dir);
}

/// xorshift: the same on every run.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }

    fn text(&mut self) -> String {
        const BITS: &[&str] = &["", "plain", "two\nlines", "\"quoted\" \\ back", "é漢👍", "{\"json\":1}", "x"];
        let long = "y".repeat(self.below(3) * 5000);
        format!("{}{long}", BITS[self.below(BITS.len())])
    }
}

/// A conversation as claude-jr keeps it: turns alternate, every call is
/// answered in the next message, and a user may add text beside results.
fn conversation(rng: &mut Rng) -> Vec<String> {
    let mut out = Vec::new();
    let mut calls: Vec<String> = Vec::new();
    for turn in 0..1 + rng.below(12) {
        let mut user = Vec::new();
        for id in calls.drain(..) {
            let content = match rng.below(3) {
                0 => json!(rng.text()),
                1 => json!([{ "type": "text", "text": rng.text() }, { "type": "image", "source": {} }]),
                _ => json!([]),
            };
            user.push(
                json!({ "type": "tool_result", "tool_use_id": id, "content": content, "is_error": rng.below(4) == 0 }),
            );
        }
        for _ in 0..rng.below(3) {
            user.push(json!({ "type": "text", "text": rng.text() }));
        }
        if rng.below(6) == 0 {
            user.push(
                json!({ "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "QUJD" } }),
            );
        }
        out.push(json!({ "role": "user", "content": user }).to_string());

        let mut reply = Vec::new();
        if rng.below(2) == 0 {
            reply.push(json!({ "type": "thinking", "thinking": rng.text(), "signature": "s" }));
        }
        if rng.below(3) != 0 {
            reply.push(json!({ "type": "text", "text": rng.text() }));
        }
        for n in 0..rng.below(4) {
            let id = format!("t{turn}_{n}");
            reply.push(json!({ "type": "tool_use", "id": id, "name": "bash", "input": { "command": rng.text() } }));
            calls.push(id);
        }
        out.push(json!({ "role": "assistant", "content": reply }).to_string());
    }
    if !calls.is_empty() {
        let results: Vec<Value> =
            calls.iter().map(|id| json!({ "type": "tool_result", "tool_use_id": id, "content": "x" })).collect();
        out.push(json!({ "role": "user", "content": results }).to_string());
    }
    out
}

/// What a Chat Completions server insists on.
fn check(request: &Value, source: &[String]) {
    let messages = request["messages"].as_array().unwrap();
    let mut waiting: Vec<String> = Vec::new();
    for (i, m) in messages.iter().enumerate() {
        match m["role"].as_str().unwrap() {
            "system" => assert_eq!(i, 0, "the system message comes first"),
            "tool" => {
                let id = m["tool_call_id"].as_str().unwrap().to_string();
                assert!(m["content"].is_string(), "a tool message's content is a string");
                let at = waiting
                    .iter()
                    .position(|w| *w == id)
                    .unwrap_or_else(|| panic!("result {id} answers no call\n{source:?}"));
                waiting.remove(at);
            }
            role => {
                assert!(
                    waiting.is_empty(),
                    "calls {waiting:?} were not answered before the next {role} message\n{source:?}"
                );
                if role == "assistant" {
                    assert!(m["content"].is_string());
                    for call in m["tool_calls"].as_array().into_iter().flatten() {
                        assert_eq!(call["type"], "function");
                        let args = call["function"]["arguments"].as_str().expect("arguments are a string");
                        assert!(serde_json::from_str::<Value>(args).is_ok(), "and that string is JSON");
                        waiting.push(call["id"].as_str().unwrap().to_string());
                    }
                    assert!(
                        m.get("tool_calls").is_none_or(|c| !c.as_array().unwrap().is_empty()),
                        "no empty list of calls"
                    );
                } else {
                    assert_eq!(role, "user");
                    assert!(m["content"].is_string() || m["content"].as_array().is_some_and(|parts| !parts.is_empty()));
                }
            }
        }
    }
    assert!(waiting.is_empty(), "the last calls were never answered");
}

#[test]
fn any_conversation_becomes_a_request_a_server_accepts() {
    let mut rng = Rng(0x9E3779B97F4A7C15);
    let head = serde_json::to_vec(&json!({ "model": "m", "stream": true })).unwrap();
    for _ in 0..400 {
        let source = conversation(&mut rng);
        let kept = Mutex::new(Translated { unusable: rng.below(2) == 0, ..Translated::default() });
        let texts: Vec<&str> = source.iter().map(String::as_str).collect();
        // Sent as it grows, the way a real session sends it.
        for upto in (1..=source.len()).step_by(1 + rng.below(3)) {
            let mut out = Vec::new();
            let mut b = body(&head, "SYS", &texts[..upto], &kept).unwrap();
            b.read_to_end(&mut out).unwrap();
            assert_eq!(out.len(), b.len);
            let request: Value = serde_json::from_slice(&out).unwrap_or_else(|e| panic!("not JSON ({e})\n{source:?}"));
            // (Part-way through a turn the newest calls have no results yet.)
            if upto == source.len() {
                check(&request, &source);
            }
        }
    }
}

#[test]
fn no_stream_makes_the_reader_panic() {
    let mut rng = Rng(0xD1B54A32D192ED03);
    let pieces = [
        r#"{"choices":[{"delta":{"content":"hi"}}]}"#,
        r#"{"choices":[{"delta":{"content":null,"tool_calls":[{"index":3,"function":{"arguments":"{"}}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"id":"","function":{"name":"x","arguments":null}}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":"nope"}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":-1}]}}]}"#,
        r#"{"choices":[{"finish_reason":"stop"}]}"#,
        r#"{"choices":[{"finish_reason":"tool_calls","delta":{}}]}"#,
        r#"{"choices":[]}"#,
        r#"{"choices":"x"}"#,
        r#"{"usage":{"prompt_tokens":"many","completion_tokens":-4}}"#,
        r#"{"usage":{"prompt_tokens":5,"prompt_tokens_details":{"cached_tokens":99}}}"#,
        r#"{"error":{"message":"bad"}}"#,
        r#"{"error":"flat"}"#,
        "[DONE]",
        "{}",
        "[]",
        "null",
        "7",
        "not json",
        "",
    ];
    for _ in 0..5000 {
        let mut stream = String::new();
        for _ in 0..rng.below(9) {
            match rng.below(8) {
                0 => stream.push_str(": a comment\n"),
                1 => stream.push_str("event: something\n"),
                2 => stream.push_str(&format!("data:{}", pieces[rng.below(pieces.len())])),
                _ => stream.push_str(&format!("data: {}\n\n", pieces[rng.below(pieces.len())])),
            }
        }
        if let (Ok(reply), _) = parse(&stream) {
            assert!(matches!(reply.stop_reason.as_str(), "end_turn" | "tool_use" | "max_tokens" | "refusal"));
            for block in &reply.content {
                assert!(block["type"] == "text" || (block["id"].is_string() && block["input"].is_object()), "{block}");
            }
        }
    }
}

fn parse(stream: &str) -> (Result<Reply>, Vec<String>) {
    let mut seen = Vec::new();
    let reply = read(stream.as_bytes(), &AtomicBool::new(false), Duration::from_secs(5), &mut |c| {
        seen.push(match c {
            Chunk::Text(t) => format!("text:{t}"),
            Chunk::Thinking(t) => format!("think:{t}"),
            Chunk::ToolStart(n) => format!("tool:{n}"),
            Chunk::ToolInput(n) => format!("input:{n}"),
            Chunk::BlockEnd => "end".into(),
            Chunk::ServerTool(_) => "server".into(),
        })
    });
    (reply, seen)
}

fn lines(chunks: &[Value]) -> String {
    chunks.iter().map(|c| format!("data: {c}\n\n")).collect::<String>() + "data: [DONE]\n\n"
}

fn delta(d: Value, finish: Option<&str>) -> Value {
    json!({ "choices": [{ "index": 0, "delta": d, "finish_reason": finish }] })
}

#[test]
fn a_reply_with_thinking_text_and_two_calls_is_read() {
    let stream = lines(&[
        delta(json!({ "role": "assistant", "content": "" }), None),
        delta(json!({ "reasoning_content": "let me see" }), None),
        delta(json!({ "content": "I'll " }), None),
        delta(json!({ "content": "check." }), None),
        delta(
            json!({ "tool_calls": [{ "index": 0, "id": "c1", "type": "function", "function": { "name": "read", "arguments": "" } }] }),
            None,
        ),
        delta(json!({ "tool_calls": [{ "index": 0, "function": { "arguments": "{\"path\":" } }] }), None),
        delta(
            json!({ "tool_calls": [{ "index": 1, "id": "c2", "function": { "name": "bash", "arguments": "{\"command\":\"ls\"}" } }] }),
            None,
        ),
        delta(json!({ "tool_calls": [{ "index": 0, "function": { "arguments": "\"a.rs\"}" } }] }), None),
        delta(json!({}), Some("tool_calls")),
        json!({ "choices": [], "usage": { "prompt_tokens": 120, "completion_tokens": 30, "prompt_tokens_details": { "cached_tokens": 100 } } }),
    ]);
    let (reply, seen) = parse(&stream);
    let reply = reply.unwrap();
    assert_eq!(reply.stop_reason, "tool_use");
    assert_eq!(reply.content[0], json!({ "type": "text", "text": "I'll check." }));
    assert_eq!(
        reply.content[1],
        json!({ "type": "tool_use", "id": "c1", "name": "read", "input": { "path": "a.rs" } })
    );
    assert_eq!(reply.content[2]["input"]["command"], "ls");
    assert_eq!(reply.content.len(), 3, "thinking isn't kept: it can't be sent back");
    assert_eq!(
        (reply.usage.input_tokens, reply.usage.cache_read_input_tokens, reply.usage.output_tokens),
        (20, 100, 30)
    );
    assert_eq!(seen[..4], ["think:let me see", "text:I'll ", "text:check.", "end"]);
    assert!(seen.contains(&"tool:read".to_string()) && seen.contains(&"tool:bash".to_string()));
    assert_eq!(seen.last().unwrap(), "end");
}

#[test]
fn odd_servers_are_understood() {
    // Whole calls with no index and no id; "reasoning" for thinking; no usage; no [DONE].
    let stream = format!(
        "data: {}\n\ndata: {}\n\ndata: {}\n\n",
        delta(json!({ "reasoning": "hm" }), None),
        delta(
            json!({ "tool_calls": [{ "function": { "name": "bash", "arguments": "{\"command\":\"pwd\"}" } }, { "function": { "name": "read", "arguments": "" } }] }),
            None
        ),
        delta(json!({}), Some("stop")),
    );
    let (reply, _) = parse(&stream);
    let reply = reply.unwrap();
    assert_eq!(reply.stop_reason, "tool_use", "calls were made, whatever the server calls the ending");
    assert_eq!(
        (reply.content[0]["id"].as_str(), reply.content[0]["input"]["command"].as_str()),
        (Some("call_0"), Some("pwd"))
    );
    assert_eq!((reply.content[1]["id"].as_str(), &reply.content[1]["input"]), (Some("call_1"), &json!({})));

    // Input that isn't JSON goes back to the model to try again.
    let stream = lines(&[delta(
        json!({ "tool_calls": [{ "index": 0, "id": "x", "function": { "name": "bash", "arguments": "{oops" } }] }),
        Some("tool_calls"),
    )]);
    assert!(parse(&stream).0.unwrap().content[0]["input"]["_invalid_json"].is_string());

    // Plain text, cut off by the length limit, and a refusal.
    for (finish, want) in [("stop", "end_turn"), ("length", "max_tokens"), ("content_filter", "refusal")] {
        let stream = lines(&[delta(json!({ "content": "Hello" }), None), delta(json!({}), Some(finish))]);
        let (reply, seen) = parse(&stream);
        assert_eq!(reply.unwrap().stop_reason, want);
        assert_eq!(seen, ["text:Hello", "end"]);
    }
}

#[test]
fn broken_streams_are_errors_and_retried_only_before_anything_was_shown() {
    let retryable = |stream: &str| parse(stream).0.err().is_some_and(|e| e.is::<Retryable>());
    assert!(retryable(""), "nothing at all");
    assert!(retryable("data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n\n"));
    assert!(retryable("data: {\"error\":{\"message\":\"overloaded\"}}\n\n"));
    // Once text has been shown, asking again would show it twice.
    let shown = format!("data: {}\n\n", delta(json!({ "content": "Hel" }), None));
    let (reply, _) = parse(&shown);
    let err = reply.err().unwrap();
    assert!(!err.is::<Retryable>() && err.to_string().contains("ended early"), "{err}");
    let (reply, _) = parse(&format!("{shown}data: {{\"error\":{{\"message\":\"boom\"}}}}\n\n"));
    assert_eq!(reply.err().unwrap().to_string(), "API error: boom");
    assert!(parse("data: not json\n\n").0.is_err());
}

/// A server that numbers a tool call in the millions (seen from a fuzzer, not
/// yet from a server) asked for 4.8 GB of room for the calls before it.
#[test]
fn a_tool_call_numbered_absurdly_is_an_error_not_gigabytes() {
    for index in ["256", "99999999", "18446744073709551615"] {
        let stream = format!(
            "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":{index},\"id\":\"far\"}}]}}}}]}}\n\ndata: [DONE]\n\n"
        );
        let err = read(stream.as_bytes(), &AtomicBool::new(false), Duration::from_secs(5), &mut |_| {})
            .err()
            .expect("an error");
        assert!(format!("{err:#}").contains("the reply was malformed"), "{index}: {err:#}");
    }
}
