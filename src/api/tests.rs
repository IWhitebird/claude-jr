// Tests of the client against small servers of its own, and of the pieces
// around it.

use super::anthropic::*;
use super::body::*;
use super::models::*;
use super::*;
use serde_json::value::RawValue;
use std::io::BufRead;

#[test]
fn parses_text_thinking_and_tool_use() {
    let sse = r#"event: message_start
data: {"type":"message_start","message":{"usage":{"input_tokens":10,"cache_read_input_tokens":4}}}

data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}
data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Checking a.rs"}}
data: {"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig"}}
data: {"type":"content_block_stop","index":0}
data: {"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}
data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Hel"}}
data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"lo"}}
data: {"type":"content_block_stop","index":1}
data: {"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"t1","name":"read","input":{}}}
data: {"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}
data: {"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"\"a.rs\"}"}}
data: {"type":"content_block_stop","index":2}
data: {"type":"content_block_start","index":3,"content_block":{"type":"compaction","content":""}}
data: {"type":"content_block_delta","index":3,"delta":{"type":"compaction_delta","content":"sum"}}
data: {"type":"content_block_delta","index":3,"delta":{"type":"compaction_delta","content":"mary"}}
data: {"type":"content_block_stop","index":3}
event: ping
data: {"type":"ping"}
data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":7}}
data: {"type":"message_stop"}
"#;
    let (mut text, mut thinking, mut tools, mut ends) = (String::new(), String::new(), Vec::new(), 0);
    let reply = read_sse(sse.as_bytes(), &AtomicBool::new(false), SILENCE_LIMIT, &mut |c| match c {
        Chunk::Text(t) => text.push_str(t),
        Chunk::Thinking(t) => thinking.push_str(t),
        Chunk::ToolStart(n) => tools.push(n.to_string()),
        Chunk::ToolInput(_) | Chunk::ServerTool(_) => {}
        Chunk::BlockEnd => ends += 1,
    })
    .unwrap();

    assert_eq!((text.as_str(), thinking.as_str(), ends), ("Hello", "Checking a.rs", 4));
    assert_eq!(tools, ["read"]);
    assert_eq!(reply.stop_reason, "tool_use");
    assert_eq!(reply.usage.context(), 21);
    assert_eq!(reply.content[0]["signature"], "sig");
    assert_eq!(reply.content[1]["text"], "Hello");
    assert_eq!(reply.content[2]["input"]["path"], "a.rs");
    assert_eq!(reply.content[3]["content"], "summary");
}

#[test]
fn body_reads_saved_messages_from_their_file() {
    let dir = crate::tools::test_dir("body-file");
    let path = dir.join("session.jsonl");
    let (first, second) = (r#"{"role":"user","content":"hi"}"#, r#"{"role":"assistant","content":"yo"}"#);
    std::fs::write(&path, format!("header\n{first}\n{second}\n")).unwrap();
    let file = std::fs::File::open(&path).unwrap();
    let head = br#"{"model":"m"}"#;
    let saved = |at: usize, len: usize| Text::Saved { file: &file, at: at as u64, len };
    let msgs = [saved(7, first.len()), Text::Memory(second), saved(7 + first.len() + 1, second.len())];
    let mut body = Body::new(head, &msgs);
    let mut out = String::new();
    std::io::Read::read_to_string(&mut body, &mut out).unwrap();
    assert_eq!(out.len(), body.len);
    assert_eq!(out, format!(r#"{{"messages":[{first},{second},{second}],"model":"m"}}"#));

    // A file that ends before the message does is an error, not a short body.
    let msgs = [saved(7, 4096)];
    let mut out = Vec::new();
    assert!(std::io::Read::read_to_end(&mut Body::new(head, &msgs), &mut out).is_err());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn each_providers_error_is_read() {
    for (body, want) in [
        (r#"{"type":"error","error":{"type":"x","message":"prompt is too long"}}"#, "prompt is too long"),
        (
            r#"[{"error":{"code":400,"message":"Please pass a valid API key","status":"INVALID_ARGUMENT"}}]"#,
            "Please pass a valid API key",
        ),
        (r#"{"code":"invalid-argument","error":"Incorrect API key provided."}"#, "Incorrect API key provided."),
        (r#"{"detail":"Invalid API Key"}"#, "Invalid API Key"),
        (r#"{"message":"no such model"}"#, "no such model"),
        ("  <html>bad gateway</html>\n", "<html>bad gateway</html>"),
        (r#"{"error":{"message":""},"detail":"the real reason"}"#, "the real reason"),
        ("", ""),
    ] {
        assert_eq!(error_message(body), want, "{body}");
    }
    assert_eq!(error_message(&"x".repeat(900)).len(), 500, "an odd body is cut short");
}

#[test]
fn body_splices_messages() {
    let head = br#"{"model":"m","stream":true}"#;
    let msgs = [r#"{"role":"user","content":"hi"}"#, r#"{"role":"assistant","content":"yo"}"#];
    let mut body = Body::new(head, &msgs);
    let mut out = String::new();
    std::io::Read::read_to_string(&mut body, &mut out).unwrap();
    assert_eq!(out.len(), body.len);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["messages"][1]["content"], "yo");
    assert_eq!(v["model"], "m");
}

#[test]
fn newer_versions_use_their_family() {
    let caps = Caps::of("claude-fable-5-3");
    assert!(caps.updates && caps.compact && !caps.fallback && !caps.web);
    assert_eq!(cost("claude-fable-5-3", &Usage { input_tokens: 1_000_000, ..Default::default() }), Some(8.0));
    assert!(Caps::of("claude-sonnet-5-7").web);
    assert!(!Caps::of("claude-haiku-5-6").updates);
    assert_eq!(cost("gpt-9", &Usage::default()), None);
    assert_eq!(family("claude-opus-4-5"), None);
}

#[test]
fn errors_before_any_content_can_be_retried() {
    let overloaded = "data: {\"type\":\"error\",\"error\":{\"message\":\"Overloaded\"}}\n";
    let err =
        read_sse(overloaded.as_bytes(), &AtomicBool::new(false), SILENCE_LIMIT, &mut |_| {}).err().expect("an error");
    assert!(err.is::<Retryable>());
    let late =
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n"
            .to_string()
            + overloaded;
    let err = read_sse(late.as_bytes(), &AtomicBool::new(false), SILENCE_LIMIT, &mut |_| {}).err().expect("an error");
    assert!(!err.is::<Retryable>(), "text was already shown");
}

#[test]
fn caps_and_cost() {
    assert_eq!(Caps::of("claude-opus-5-5").betas(), [BETA_FALLBACK, BETA_UPDATES, BETA_COMPACT].join(","));
    assert_eq!(Caps::of("something-else").betas(), "");
    let u = Usage { input_tokens: 1_000_000, output_tokens: 1_000_000, ..Default::default() };
    assert_eq!(cost("claude-opus-5-5", &u), Some(24.0));
}

/// Serves one SSE event, then goes quiet with the connection open.
fn stalled_server() -> (String, std::thread::JoinHandle<()>) {
    use std::io::{Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let h = std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let mut buf = [0u8; 8192];
        read_request(&mut s);
        let ev = "data: {\"type\":\"message_start\",\"message\":{\"usage\":{}}}\n\n";
        let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n";
        s.write_all(format!("{head}{:x}\r\n{ev}\r\n", ev.len()).as_bytes()).unwrap();
        // hold the socket open until the client hangs up
        while s.read(&mut buf).map(|n| n > 0).unwrap_or(false) {}
    });
    (base, h)
}

/// Esc stops a request at once even while it waits for the reply to begin
/// (no read timeout can be picked up again there), and the next request works.
#[test]
fn cancel_works_before_the_reply_begins() {
    use std::io::{Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        // First request: read it, then say nothing until the client hangs up.
        let (mut s, _) = l.accept().unwrap();
        let mut buf = [0u8; 8192];
        read_request(&mut s);
        while s.read(&mut buf).map(|n| n > 0).unwrap_or(false) {}
        // Second request: a normal reply.
        let (mut s, _) = l.accept().unwrap();
        read_request(&mut s);
        let body = "data: {\"type\":\"message_start\",\"message\":{\"usage\":{}}}\n\n\
            data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
            data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"again\"}}\n\n\
            data: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
            data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n\
            data: {\"type\":\"message_stop\"}\n\n";
        let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n";
        let _ = s.write_all(format!("{head}content-length: {}\r\n\r\n{body}", body.len()).as_bytes());
    });
    // The normal client: a 60 s wait for the reply to begin.
    let client = Client::new("k".into(), Some(base));
    let tools = RawValue::from_string("[]".into()).unwrap();
    let req = Request::new("claude-opus-5-5", None, "", &tools);
    let cancel = AtomicBool::new(false);
    let started = Instant::now();
    let err = std::thread::scope(|sc| {
        sc.spawn(|| {
            sleep(Duration::from_millis(400));
            cancel.store(true, Ordering::Relaxed);
        });
        client.stream(&req, &[] as &[&str], &cancel, |_| {}).err().expect("an error")
    });
    assert!(err.is::<Cancelled>(), "{err}");
    assert!(started.elapsed() < Duration::from_secs(2), "took {:?}", started.elapsed());

    // The next request goes through on a fresh connection.
    let mut text = String::new();
    let reply = client.stream(&req, &[] as &[&str], &AtomicBool::new(false), |c| {
        if let Chunk::Text(t) = c {
            text.push_str(t);
        }
    });
    assert!(reply.is_ok(), "{:?}", reply.err());
    assert_eq!(text, "again");
    server.join().unwrap();
}

/// A network that is down for a while (longer than the few quick retries
/// other failures get) is waited out, and nothing is sent until it is back.
// 127.0.0.2 is loopback on Linux; elsewhere it may not exist.
#[test]
#[cfg_attr(not(target_os = "linux"), ignore)]
fn a_network_outage_is_waited_out() {
    use std::io::Write;
    // A port with nothing behind it yet: connections are refused. On an
    // address no other test uses, so none can be given the port while it
    // is free and answer in the server's place.
    let l = std::net::TcpListener::bind("127.0.0.2:0").unwrap();
    let addr = l.local_addr().unwrap();
    drop(l);
    let server = std::thread::spawn(move || {
        sleep(Duration::from_millis(8500)); // longer than five quick retries take
        let l = std::net::TcpListener::bind(addr).unwrap();
        let (mut s, _) = l.accept().unwrap();
        read_request(&mut s);
        let body = "data: {\"type\":\"message_start\",\"message\":{\"usage\":{}}}\n\n\
            data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
            data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"back\"}}\n\n\
            data: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
            data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n\
            data: {\"type\":\"message_stop\"}\n\n";
        let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n";
        let _ = s.write_all(format!("{head}content-length: {}\r\n\r\n{body}", body.len()).as_bytes());
    });
    let mut client = Client::new("k".into(), Some(format!("http://{addr}")));
    client.offline = Duration::from_secs(60);
    let tools = RawValue::from_string("[]".into()).unwrap();
    let req = Request::new("claude-opus-5-5", None, "", &tools);
    let mut text = String::new();
    let started = Instant::now();
    let reply = client.stream(&req, &[] as &[&str], &AtomicBool::new(false), |c| {
        if let Chunk::Text(t) = c {
            text.push_str(t);
        }
    });
    assert!(reply.is_ok(), "{:?}", reply.err());
    assert_eq!(text, "back");
    assert!(started.elapsed() >= Duration::from_secs(8));
    server.join().unwrap();

    // With no patience left it gives up, and says how long it tried.
    let mut client = Client::new("k".into(), Some("http://127.0.0.1:9".into()));
    client.offline = Duration::from_millis(600);
    let err = client.stream(&req, &[] as &[&str], &AtomicBool::new(false), |_| {}).err().unwrap();
    assert!(err.to_string().starts_with("can't reach the API after"), "{err}");
}

/// The model can take a while to begin its reply. Giving up on that wait
/// re-sends the request, which is slow and is billed again.
#[test]
fn a_slow_start_is_waited_for_not_sent_again() {
    use std::io::Write;
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = requests.clone();
    std::thread::spawn(move || {
        for mut s in l.incoming().flatten() {
            seen.fetch_add(1, Ordering::Relaxed);
            read_request(&mut s);
            sleep(Duration::from_millis(3500));
            let body = "data: {\"type\":\"message_start\",\"message\":{\"usage\":{}}}\n\n\
                data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
                data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"late\"}}\n\n\
                data: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
                data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n\
                data: {\"type\":\"message_stop\"}\n\n";
            let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n";
            let _ = s.write_all(format!("{head}content-length: {}\r\n\r\n{body}", body.len()).as_bytes());
        }
    });
    let client = Client::new("k".into(), Some(base));
    let tools = RawValue::from_string("[]".into()).unwrap();
    let req = Request::new("claude-opus-5-5", None, "", &tools);
    let mut text = String::new();
    let reply = client
        .stream(&req, &[] as &[&str], &AtomicBool::new(false), |c| {
            if let Chunk::Text(t) = c {
                text.push_str(t);
            }
        })
        .expect("the reply arrives");
    assert_eq!(text, "late");
    assert_eq!(reply.stop_reason, "end_turn");
    assert_eq!(requests.load(Ordering::Relaxed), 1, "the request was sent once");
}

#[test]
fn cancel_works_while_the_stream_stalls() {
    let (base, server) = stalled_server();
    let client = Client::with_read_timeout("k".into(), Some(base), Duration::from_secs(1));
    let tools = RawValue::from_string("[]".into()).unwrap();
    let req = Request::new("claude-opus-5-5", None, "", &tools);
    let cancel = AtomicBool::new(false);
    let start = Instant::now();
    let err = std::thread::scope(|sc| {
        sc.spawn(|| {
            sleep(Duration::from_millis(500));
            cancel.store(true, Ordering::Relaxed);
        });
        client.stream(&req, &[] as &[&str], &cancel, |_| {}).err().expect("an error")
    });
    assert!(err.is::<Cancelled>(), "{err}");
    assert!(start.elapsed() < Duration::from_secs(5), "took {:?}", start.elapsed());
    drop(client);
    server.join().unwrap();
}

#[test]
fn a_silent_stream_times_out_as_retryable() {
    use std::io::Write;
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        s.write_all(b"data: {\"type\":\"ping\"}\n").unwrap();
        sleep(Duration::from_secs(3));
    });
    let sock = std::net::TcpStream::connect(addr).unwrap();
    sock.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
    let start = Instant::now();
    let err = read_sse(BufReader::new(sock), &AtomicBool::new(false), Duration::from_millis(700), &mut |_| {})
        .err()
        .expect("an error");
    assert!(err.is::<Retryable>(), "{err}");
    assert!(start.elapsed() < Duration::from_secs(3));
    server.join().unwrap();
}

#[test]
fn a_broken_stream_is_retryable_only_before_output() {
    struct Broken(&'static [u8]);
    impl std::io::Read for Broken {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.0.is_empty() {
                return Err(std::io::ErrorKind::ConnectionReset.into());
            }
            let n = self.0.len().min(buf.len());
            buf[..n].copy_from_slice(&self.0[..n]);
            self.0 = &self.0[n..];
            Ok(n)
        }
    }
    let run = |data: &'static [u8]| {
        read_sse(BufReader::new(Broken(data)), &AtomicBool::new(false), SILENCE_LIMIT, &mut |_| {})
            .err()
            .expect("an error")
    };
    assert!(run(b"data: {\"type\":\"ping\"}\n").is::<Retryable>());
    let text = b"data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"name\":\"x\"}}\n";
    assert!(!run(text).is::<Retryable>());
}

#[test]
fn reading_resumes_after_read_timeouts() {
    use std::io::Write;
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        read_request(&mut s);
        let chunk = |s: &mut std::net::TcpStream, d: &str| {
            s.write_all(format!("{:x}\r\n{d}\r\n", d.len()).as_bytes()).unwrap();
            s.flush().unwrap();
        };
        s.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n")
            .unwrap();
        chunk(&mut s, "data: {\"type\":\"message_start\",\"message\":{\"usage\":{}}}\n\n");
        sleep(Duration::from_secs(4));
        chunk(
            &mut s,
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hel",
        );
        sleep(Duration::from_millis(3500));
        chunk(
            &mut s,
            "lo\"}}\n\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\ndata: {\"type\":\"message_stop\"}\n\n",
        );
        s.write_all(b"0\r\n\r\n").unwrap();
    });
    let client = Client::with_read_timeout("k".into(), Some(base), Duration::from_secs(1));
    let tools = RawValue::from_string("[]".into()).unwrap();
    let req = Request::new("claude-opus-5-5", None, "", &tools);
    let mut text = String::new();
    let reply = client
        .stream(&req, &[] as &[&str], &AtomicBool::new(false), |c| {
            if let Chunk::Text(t) = c {
                text.push_str(t)
            }
        })
        .expect("the stream should survive read timeouts");
    assert_eq!(text, "hello");
    assert_eq!(reply.stop_reason, "end_turn");
    server.join().unwrap();
}

use std::sync::{Arc, Mutex};

/// What the scripted server saw: the head of each request, lower-cased.
type Heads = Arc<Mutex<Vec<String>>>;

/// Reads one whole request, body included. A test server that answers
/// and closes with part of the request unread resets the connection.
fn read_request(s: &mut std::net::TcpStream) {
    use std::io::Read;
    let (mut seen, mut buf) = (Vec::new(), [0u8; 8192]);
    loop {
        let n = s.read(&mut buf).unwrap_or(0);
        seen.extend_from_slice(&buf[..n]);
        let text = String::from_utf8_lossy(&seen).to_ascii_lowercase();
        let Some(head) = text.find("\r\n\r\n") else {
            if n == 0 {
                return;
            }
            continue;
        };
        let len: usize = text[..head]
            .lines()
            .find_map(|l| l.strip_prefix("content-length:"))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0);
        if n == 0 || seen.len() >= head + 4 + len {
            return;
        }
    }
}

/// A server that answers request n with `responses[n]` (status, extra
/// header lines, body) and records each request's head.
fn scripted(responses: Vec<(u16, &'static str, String)>) -> (String, Heads) {
    use std::io::{Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let heads: Heads = Arc::default();
    let seen = heads.clone();
    std::thread::spawn(move || {
        let mut responses = responses.into_iter();
        for stream in l.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut out = stream;
            'requests: loop {
                let (mut head, mut len) = (String::new(), 0);
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break 'requests;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                    let end = line == "\r\n";
                    head.push_str(&line.to_ascii_lowercase());
                    if end {
                        break;
                    }
                }
                let mut body = vec![0; len];
                let _ = reader.read_exact(&mut body);
                head.push_str(&String::from_utf8_lossy(&body));
                seen.lock().unwrap().push(head);
                let Some((status, extra, body)) = responses.next() else { return };
                let reply = format!("HTTP/1.1 {status} X\r\n{extra}content-length: {}\r\n\r\n{body}", body.len());
                let _ = out.write_all(reply.as_bytes());
            }
        }
    });
    (base, heads)
}

fn hello_sse() -> String {
    let events = [
        r#"{"type":"message_start","message":{"usage":{"input_tokens":1}}}"#,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#,
        r#"{"type":"message_stop"}"#,
    ];
    events.iter().map(|e| format!("data: {e}\n\n")).collect()
}

const SSE: &str = "content-type: text/event-stream\r\n";
const JSON: &str = "content-type: application/json\r\n";

fn ask(client: &Client, model: &str) -> Result<Reply> {
    let tools = RawValue::from_string("[]".into()).unwrap();
    let req = Request::new(model, None, "sys", &tools);
    client.stream(&req, &[r#"{"role":"user","content":"hi"}"#], &AtomicBool::new(false), |_| {})
}

#[test]
fn a_missing_key_is_reported_before_anything_is_sent() {
    let (base, heads) = scripted(vec![]);
    let client = Client::new(String::new(), Some(base));
    assert!(!client.has_key());
    let err = ask(&client, "claude-opus-5-5").err().unwrap();
    assert_eq!(err.to_string(), "no API key yet: type /login to add one");
    client.set_key("k".into());
    assert!(client.has_key());
    assert!(heads.lock().unwrap().is_empty());
}

#[test]
fn rate_limits_are_retried_after_the_time_the_server_asks_for() {
    let limited = (
        429,
        "retry-after: 0\r\ncontent-type: application/json\r\n",
        r#"{"error":{"message":"slow down"}}"#.to_string(),
    );
    let (base, heads) = scripted(vec![limited, (200, SSE, hello_sse())]);
    let reply = ask(&Client::new("k".into(), Some(base)), "claude-opus-5-5").unwrap();
    assert_eq!(reply.content[0]["text"], "hi");
    assert_eq!(heads.lock().unwrap().len(), 2);
}

#[test]
fn giving_up_after_five_attempts_shows_the_servers_message() {
    let limited = || {
        (
            429,
            "retry-after: 0\r\ncontent-type: application/json\r\n",
            r#"{"error":{"message":"slow down"}}"#.to_string(),
        )
    };
    let (base, heads) = scripted((0..5).map(|_| limited()).collect());
    let err = ask(&Client::new("k".into(), Some(base)), "claude-opus-5-5").err().unwrap();
    assert_eq!(err.to_string(), "API error 429: slow down");
    assert_eq!(heads.lock().unwrap().len(), 5);
}

#[test]
fn other_errors_are_not_retried_and_odd_bodies_are_shortened() {
    let body = "x".repeat(900);
    let (base, heads) = scripted(vec![(400, "", body)]);
    let err = ask(&Client::new("k".into(), Some(base)), "claude-opus-5-5").err().unwrap();
    assert_eq!(err.to_string(), format!("API error 400: {}", "x".repeat(500)));
    assert_eq!(heads.lock().unwrap().len(), 1);
    assert_eq!(error_message(r#"{"error":{"message":"bad"}}"#), "bad");
    assert_eq!(error_message(r#"{"error":{}}"#), r#"{"error":{}}"#);
}

#[test]
fn an_overloaded_stream_before_any_content_is_requested_again() {
    let overloaded = "data: {\"type\":\"error\",\"error\":{\"message\":\"Overloaded\"}}\n\n".to_string();
    let (base, heads) = scripted(vec![(200, SSE, overloaded), (200, SSE, hello_sse())]);
    let start = Instant::now();
    let reply = ask(&Client::new("k".into(), Some(base)), "claude-opus-5-5").unwrap();
    assert_eq!(reply.content[0]["text"], "hi");
    assert_eq!(heads.lock().unwrap().len(), 2);
    assert!(start.elapsed() >= Duration::from_millis(450), "it waited before retrying");
}

#[test]
fn requests_carry_the_key_version_and_the_betas_the_model_has() {
    let (base, heads) = scripted(vec![(200, SSE, hello_sse()), (200, SSE, hello_sse()), (200, JSON, hello_sse())]);
    let client = Client::new("sk-test".into(), Some(format!("{base}/")));
    ask(&client, "claude-opus-5-5").unwrap();
    let mcp = RawValue::from_string(r#"[{"type":"url","url":"https://x.test","name":"x"}]"#.into()).unwrap();
    let tools = RawValue::from_string("[]".into()).unwrap();
    let mut req = Request::new("something-else", None, "", &tools);
    req.mcp_servers = Some(&mcp);
    client.stream(&req, &[] as &[&str], &AtomicBool::new(false), |_| {}).unwrap();
    req.model = "claude-haiku-5-5";
    client.stream(&req, &[] as &[&str], &AtomicBool::new(false), |_| {}).unwrap();

    let heads = heads.lock().unwrap();
    assert!(heads[0].starts_with("post /v1/messages http/1.1"), "{}", heads[0]);
    assert!(heads[0].contains("x-api-key: sk-test"));
    assert!(heads[0].contains(&format!("anthropic-version: {VERSION}")));
    assert!(heads[0].contains(&format!("anthropic-beta: {}", Caps::of("claude-opus-5-5").betas())));
    assert!(!heads[0].contains(BETA_MCP));
    assert!(heads[1].contains(&format!("anthropic-beta: {BETA_MCP}\r\n")), "only the connector beta: {}", heads[1]);
    assert!(heads[1].contains(r#""mcp_servers":[{"type":"url""#));
    assert!(heads[2].contains(&format!("{},{BETA_MCP}", BETA_COMPACT)), "{}", heads[2]);
}

#[test]
fn a_stream_that_ends_early_says_so() {
    let run =
        |data: &str| read_sse(data.as_bytes(), &AtomicBool::new(false), SILENCE_LIMIT, &mut |_| {}).err().unwrap();
    let err = run("");
    assert!(err.is::<Retryable>() && err.to_string() == "the stream ended before any reply");
    let started =
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n";
    let err = run(started);
    assert!(!err.is::<Retryable>() && err.to_string() == "the stream ended early");
    // The connection dropped in the middle of a line.
    let err = run(&format!("{started}data: {{\"type\":\"content_bl"));
    assert_eq!(err.to_string(), "the stream ended early");
    let err = run("data: {not json}\n");
    assert!(err.to_string().contains("key must be a string"), "{err}");
}

#[test]
fn waiting_between_attempts() {
    assert_eq!(backoff(1, None), Duration::from_millis(500));
    assert_eq!(backoff(3, None), Duration::from_secs(2));
    assert_eq!(backoff(7, None), Duration::from_secs(16), "capped");
    assert_eq!(backoff(1, Some(3)), Duration::from_secs(3));
    assert_eq!(backoff(1, Some(900)), Duration::from_secs(60), "a server's long wait is capped too");
    assert!(wait(Duration::ZERO, &AtomicBool::new(false)).is_ok());
    let start = Instant::now();
    assert!(wait(Duration::from_millis(250), &AtomicBool::new(false)).is_ok());
    assert!(start.elapsed() >= Duration::from_millis(250));
    let err = wait(Duration::from_secs(30), &AtomicBool::new(true)).err().unwrap();
    assert!(err.is::<Cancelled>());
    assert_eq!(Cancelled.to_string(), "cancelled");
    assert_eq!(Retryable("x".into()).to_string(), "x");
}

#[test]
fn models_are_described_with_price_and_window() {
    assert_eq!(describe("claude-opus-5-5"), "$4/$20 \u{b7} 1M context");
    assert_eq!(describe("claude-haiku-4-5"), "$1/$5 \u{b7} 200k context");
    assert_eq!(describe("claude-sonnet-5-5"), "$2/$10 \u{b7} 1M context");
    assert_eq!(describe("my-own-model"), "1M context");
    for (id, _) in CATALOGUE {
        assert!(price(id).is_some(), "{id} has a price");
    }
    assert_eq!(family("claude-mythos-5-1"), Some("fable"));
    assert_eq!(family("claude-sonnet-4-5"), None);
    assert_eq!(family("claude-unknown-9"), None);
    assert_eq!(price("claude-unknown-9"), None);
    assert_eq!(price("claude-opus-9-1"), price("claude-opus-5-5"));
}
