// Claude through Bedrock and Vertex: what goes out to each, checked against a
// scripted server. (The addresses, logins and the signature itself are tested
// where they are made, in bedrock.rs and vertex.rs.)

use super::models::{BETA_COMPACT, BETA_FALLBACK, BETA_UPDATES};
use super::tests::{JSON, SSE, ask, hello_sse, scripted};
use super::*;
use serde_json::value::RawValue;

fn env(pairs: Vec<(&'static str, String)>) -> impl Fn(&str) -> Option<String> + Send + Sync + 'static {
    move |name| pairs.iter().find(|p| p.0 == name).map(|p| p.1.clone())
}

/// The JSON a request carried, from what the scripted server recorded.
fn body(head: &str) -> Value {
    serde_json::from_str(head.split_once("\r\n\r\n").unwrap().1).unwrap()
}

fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().find_map(|l| l.strip_prefix(name)?.strip_prefix(": "))
}

#[test]
fn a_cloud_model_is_the_same_model_without_what_the_cloud_lacks() {
    for model in ["bedrock/claude-opus-5-5", "bedrock/anthropic.claude-opus-5-5", "vertex/claude-opus-5-5"] {
        let caps = Caps::of(model);
        assert!(caps.updates && caps.compact && !caps.fallback && !caps.web, "{model}");
        assert_eq!(caps.betas(), format!("{BETA_UPDATES},{BETA_COMPACT}"));
        assert_eq!(caps.window, 1_000_000);
        assert_eq!(cost(model, &Usage { input_tokens: 1_000_000, ..Default::default() }), Some(4.0));
        assert_eq!(describe(model), "$4/$20 · 1M context");
        assert_eq!(window(model, &Providers::new()), 1_000_000);
    }
    assert_eq!(describe("vertex/claude-haiku-4-5@20251001"), "$1/$5 · 200k context");
    assert!(Caps::of("claude-opus-5-5").fallback && Caps::of("claude-opus-5-5").web, "directly, nothing changes");
    assert!(!Client::new(String::new(), None).needs_login("bedrock/claude-opus-5-5"), "its login is not Anthropic's");
}

#[test]
fn vertex_gets_the_model_in_the_address_and_the_version_in_the_body() {
    let (base, heads) = scripted(vec![(200, SSE, hello_sse())]);
    let client = Client::new(String::new(), None).with_env(env(vec![
        ("ANTHROPIC_VERTEX_BASE_URL", format!("{base}/v1")),
        ("ANTHROPIC_VERTEX_PROJECT_ID", "proj".into()),
        ("VERTEX_ACCESS_TOKEN", "ya29.token".into()),
    ]));
    let mcp = RawValue::from_string(r#"[{"type":"url","url":"https://x.test","name":"x"}]"#.into()).unwrap();
    let tools = RawValue::from_string("[]".into()).unwrap();
    let mut req = Request::new("vertex/claude-opus-5-5", Some("high"), "sys", &tools);
    req.mcp_servers = Some(&mcp);
    let reply = client.stream(&req, &[r#"{"role":"user","content":"hi"}"#], &AtomicBool::new(false), |_| {}).unwrap();
    assert_eq!(reply.content[0]["text"], "hi");

    let heads = heads.lock().unwrap();
    let head = &heads[0];
    assert!(
        head.starts_with(
            "post /v1/projects/proj/locations/global/publishers/anthropic/models/claude-opus-5-5:streamrawpredict "
        ),
        "{head}"
    );
    assert_eq!(header(head, "authorization"), Some("bearer ya29.token"));
    assert_eq!(header(head, "anthropic-beta"), Some(format!("{BETA_UPDATES},{BETA_COMPACT}").as_str()));
    assert!(header(head, "x-api-key").is_none() && header(head, "anthropic-version").is_none(), "{head}");
    let sent = body(head);
    assert_eq!(sent["anthropic_version"], VERSION);
    for left_out in ["model", "fallbacks", "mcp_servers"] {
        assert!(sent.get(left_out).is_none(), "{left_out} was sent");
    }
    // Everything else is Anthropic's request as it always is.
    assert_eq!(sent["messages"][0]["content"], "hi");
    assert_eq!(sent["system"], "sys");
    assert_eq!(sent["stream"], true);
    assert_eq!(sent["cache_control"]["type"], "ephemeral");
    assert_eq!(sent["thinking"]["type"], "adaptive");
    assert_eq!(sent["output_config"]["effort"], "high");
    assert_eq!(sent["context_management"]["edits"][0]["type"], "compact_20260112");
}

#[test]
fn vertex_says_what_is_missing_and_forgets_a_token_that_was_refused() {
    let (base, heads) = scripted(vec![(401, JSON, r#"{"error":{"code":401,"message":"token expired"}}"#.into())]);
    let client = Client::new("sk-anthropic".into(), None).with_env(env(vec![
        ("ANTHROPIC_VERTEX_BASE_URL", base.clone()),
        ("CLAUDE_JR_GCLOUD", "/no/such/gcloud".into()),
    ]));
    let err = ask(&client, "vertex/claude-opus-5-5").err().unwrap().to_string();
    assert!(err.contains("ANTHROPIC_VERTEX_PROJECT_ID"), "{err}");
    let client = client.with_env(env(vec![
        ("ANTHROPIC_VERTEX_BASE_URL", base),
        ("GOOGLE_CLOUD_PROJECT", "p".into()),
        ("CLAUDE_JR_GCLOUD", "/no/such/gcloud".into()),
    ]));
    let err = ask(&client, "vertex/claude-opus-5-5").err().unwrap().to_string();
    assert!(err.starts_with("vertex needs a login"), "{err}");
    assert!(heads.lock().unwrap().is_empty(), "nothing was sent, and Anthropic's key is not used for it");

    *client.token.lock().unwrap() = Some(("old".into(), Instant::now()));
    let err = ask(&client, "vertex/claude-opus-5-5").err().unwrap().to_string();
    assert_eq!(err, "API error 401: token expired");
    assert!(heads.lock().unwrap()[0].contains("authorization: bearer old"));
    assert!(client.token.lock().unwrap().is_none());
}

#[test]
fn bedrock_with_an_api_key_is_anthropics_request_with_its_own_model_id() {
    let (base, heads) = scripted(vec![(200, SSE, hello_sse())]);
    let client = Client::new("sk-anthropic".into(), None).with_env(env(vec![
        ("ANTHROPIC_BEDROCK_BASE_URL", format!("{base}/anthropic")),
        ("AWS_BEARER_TOKEN_BEDROCK", "bedrock-key".into()),
        ("AWS_ACCESS_KEY_ID", "unused".into()),
        ("AWS_SECRET_ACCESS_KEY", "unused".into()),
    ]));
    ask(&client, "bedrock/claude-opus-5-5").unwrap();
    let heads = heads.lock().unwrap();
    let head = &heads[0];
    assert!(head.starts_with("post /anthropic/v1/messages "), "{head}");
    assert_eq!(header(head, "x-api-key"), Some("bedrock-key"));
    assert_eq!(header(head, "anthropic-version"), Some(VERSION));
    assert_eq!(header(head, "anthropic-beta"), Some(format!("{BETA_UPDATES},{BETA_COMPACT}").as_str()));
    assert!(!head.contains(BETA_FALLBACK));
    assert!(header(head, "authorization").is_none() && header(head, "x-amz-date").is_none(), "{head}");
    let sent = body(head);
    assert_eq!(sent["model"], "anthropic.claude-opus-5-5");
    assert!(sent.get("fallbacks").is_none() && sent.get("anthropic_version").is_none());
}

#[test]
fn bedrock_with_access_keys_signs_every_attempt() {
    let busy = r#"{"error":{"message":"slow down"}}"#.to_string();
    let (base, heads) = scripted(vec![(429, "retry-after: 0\r\n", busy), (200, SSE, hello_sse())]);
    let client = Client::new(String::new(), None).with_env(env(vec![
        ("ANTHROPIC_BEDROCK_BASE_URL", format!("{base}/anthropic")),
        ("AWS_ACCESS_KEY_ID", "AKIDEXAMPLE".into()),
        ("AWS_SECRET_ACCESS_KEY", "secret".into()),
        ("AWS_SESSION_TOKEN", "session".into()),
        ("AWS_REGION", "eu-west-1".into()),
    ]));
    ask(&client, "bedrock/anthropic.claude-haiku-5-5").unwrap();
    let heads = heads.lock().unwrap();
    assert_eq!(heads.len(), 2);
    for head in heads.iter() {
        assert!(header(head, "x-api-key").is_none(), "{head}");
        assert_eq!(header(head, "x-amz-security-token"), Some("session"));
        let date = header(head, "x-amz-date").unwrap();
        assert!(date.len() == 16 && date.ends_with('z'), "{date}");
        let auth = header(head, "authorization").unwrap();
        let scope = format!("credential=akidexample/{}/eu-west-1/bedrock-mantle/aws4_request", &date[..8]);
        assert!(auth.starts_with("aws4-hmac-sha256 ") && auth.contains(&scope), "{auth}");
        assert!(auth.contains("signedheaders=host;x-amz-date;x-amz-security-token, signature="), "{auth}");
        assert_eq!(body(head)["model"], "anthropic.claude-haiku-5-5");
    }
}

#[test]
fn bedrock_without_a_login_sends_nothing() {
    let (base, heads) = scripted(vec![]);
    let client = Client::new("sk-anthropic".into(), None)
        .with_env(env(vec![("ANTHROPIC_BEDROCK_BASE_URL", base), ("AWS_ACCESS_KEY_ID", "only-half".into())]));
    let err = ask(&client, "bedrock/claude-opus-5-5").err().unwrap().to_string();
    assert!(err.starts_with("bedrock needs a login: set AWS_BEARER_TOKEN_BEDROCK"), "{err}");
    assert!(heads.lock().unwrap().is_empty());
}
