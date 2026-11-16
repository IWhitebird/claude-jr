// Claude through Amazon Bedrock and Google's Vertex AI: whole runs against
// the fake API, which checks each request the way that cloud documents it
// and works out a signed request's signature again for itself.

use super::common::*;

fn result(o: &Out) -> serde_json::Value {
    serde_json::from_str(o.out.trim()).unwrap_or_else(|err| panic!("{err}: {}\n{}", o.out, o.err))
}

#[test]
fn a_whole_run_with_tools_through_vertex() {
    let e = Env::new();
    let o = cloud(&e, &["-y", "-p", "do the demo", "-m", "vertex/claude-opus-5-5", "--output-format", "json"]);
    ok(&o);
    nothing_refused(&e);
    let r = result(&o);
    assert_eq!(r["ok"], true);
    assert_eq!(std::fs::read_to_string(e.proj.join("demo.txt")).unwrap(), "one\nTWO\nthree\n");
    assert_eq!(r["requests"], 3);
    assert!(r["cost_usd"].as_f64().unwrap() > 0.0, "the model's price is known through any door: {r}");

    let heads = headers(&e);
    let path =
        "/v1/projects/test-project/locations/global/publishers/anthropic/models/claude-opus-5-5:streamRawPredict";
    assert!(heads.iter().all(|h| h["path"] == path), "{heads:?}");
    assert!(heads.iter().all(|h| h["authorization"] == "Bearer ya29.test" && h.get("x-api-key").is_none()));
    let sent = e.requests();
    assert!(sent.iter().all(|r| r.get("model").is_none() && r["anthropic_version"] == "2026-10-21"));
    let tools: Vec<&str> = sent[0]["tools"].as_array().unwrap().iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(tools.contains(&"bash") && !tools.contains(&"web_search") && !tools.contains(&"web_fetch"), "{tools:?}");
}

#[test]
fn vertex_asks_gcloud_for_its_token_once_per_run() {
    let e = Env::new();
    let count = e.root.join("gcloud-runs");
    let gcloud = e.root.join("gcloud");
    std::fs::write(&gcloud, format!("#!/bin/sh\necho \"$*\" >> {}\necho ya29.from-gcloud\n", count.display())).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&gcloud, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut cmd = e.command(&["-y", "-p", "do the demo", "-m", "vertex/claude-sonnet-5-5"]);
    through_cloud(&e, &mut cmd);
    cmd.env_remove("VERTEX_ACCESS_TOKEN").env("CLAUDE_JR_GCLOUD", &gcloud).env("CLOUD_ML_REGION", "eu");
    let o = exec(cmd);
    ok(&o);
    nothing_refused(&e);
    assert_eq!(std::fs::read_to_string(&count).unwrap(), "auth print-access-token\n", "asked once for three requests");
    let heads = headers(&e);
    assert_eq!(heads.len(), 3);
    assert!(heads.iter().all(|h| h["authorization"] == "Bearer ya29.from-gcloud"));
    assert!(
        heads[0]["path"].as_str().unwrap().contains("/locations/eu/publishers/anthropic/models/claude-sonnet-5-5:")
    );
}

#[test]
fn a_whole_run_through_bedrock_with_an_api_key() {
    let e = Env::new();
    let mut cmd = e.command(&["-y", "-p", "do the demo", "-m", "bedrock/claude-opus-5-5", "--output-format", "json"]);
    through_cloud(&e, &mut cmd);
    cmd.env("AWS_BEARER_TOKEN_BEDROCK", "bedrock-api-key");
    let o = exec(cmd);
    ok(&o);
    nothing_refused(&e);
    let r = result(&o);
    assert_eq!((&r["ok"], &r["requests"]), (&serde_json::json!(true), &serde_json::json!(3)));
    assert!(r["cost_usd"].as_f64().unwrap() > 0.0);
    let heads = headers(&e);
    assert!(heads.iter().all(|h| h["path"] == "/anthropic/v1/messages"
        && h["x-api-key"] == "bedrock-api-key"
        && h["anthropic-version"] == "2026-10-21"
        && h.get("authorization").is_none()));
    assert!(heads[0]["anthropic-beta"].as_str().unwrap().contains("compact"));
    let sent = e.requests();
    assert!(sent.iter().all(|r| r["model"] == "anthropic.claude-opus-5-5" && r.get("fallbacks").is_none()));
    assert_eq!(sent[0]["thinking"]["type"], "adaptive");
}

#[test]
fn bedrock_with_access_keys_signs_what_it_sends() {
    let e = Env::new();
    let o = cloud(&e, &["-y", "-p", "do the demo", "-m", "bedrock/anthropic.claude-haiku-5-5"]);
    ok(&o);
    nothing_refused(&e); // the fake API worked each signature out again from the bytes it got
    let heads = headers(&e);
    assert_eq!(heads.len(), 3);
    for h in &heads {
        let auth = h["authorization"].as_str().unwrap();
        assert!(auth.starts_with("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/"), "{auth}");
        assert!(auth.contains("/us-east-1/bedrock-mantle/aws4_request"), "{auth}");
        assert_eq!(h["x-amz-security-token"], "a-session-token");
        assert!(h.get("x-api-key").is_none());
    }
    assert_eq!(e.requests()[0]["model"], "anthropic.claude-haiku-5-5");

    // The wrong secret is refused, and the reason reaches the person.
    let mut cmd = e.command(&["-p", "hi", "-m", "bedrock/claude-haiku-5-5"]);
    through_cloud(&e, &mut cmd);
    cmd.env("AWS_SECRET_ACCESS_KEY", "not-the-secret").env("AWS_REGION", "eu-west-1");
    let o = exec(cmd);
    assert_eq!(o.code, 1);
    assert!(o.err.contains("API error 403: The request signature we calculated does not match"), "{}", o.err);
}

#[test]
fn a_conversation_carries_on_from_one_door_to_the_next() {
    let e = Env::new();
    ok(&e.run(&["-y", "-p", "do the demo"]));
    ok(&cloud(&e, &["-y", "-c", "-p", "and again", "-m", "bedrock/claude-opus-5-5"]));
    ok(&cloud(&e, &["-y", "-c", "-p", "once more", "-m", "vertex/claude-opus-5-5"]));
    nothing_refused(&e);
    let sent = e.requests();
    let last = sent.last().unwrap()["messages"].to_string();
    let at: Vec<usize> = ["do the demo", "and again", "once more"].iter().filter_map(|said| last.find(said)).collect();
    assert!(at.len() == 3 && at.is_sorted(), "one history, three doors: {at:?}");
}

#[test]
fn subagents_skills_and_jobs_work_through_a_cloud() {
    for model in ["bedrock/claude-opus-5-5", "vertex/claude-opus-5-5"] {
        let e = Env::with(&[("MOCK_SCENARIO", "features")]);
        e.write_config(&mcp_config(&e));
        let o = cloud(&e, &["-y", "-p", "check", "-m", model]);
        ok(&o);
        nothing_refused(&e);
    }
}

#[test]
fn a_cloud_without_its_login_says_what_to_set() {
    let e = Env::new();
    for (model, want) in [
        ("bedrock/claude-opus-5-5", "bedrock needs a login: set AWS_BEARER_TOKEN_BEDROCK"),
        ("vertex/claude-opus-5-5", "vertex needs a Google Cloud project: set ANTHROPIC_VERTEX_PROJECT_ID"),
    ] {
        // Anthropic's own key is there and is not used for it.
        let o = e.run(&["-p", "hi", "-m", model]);
        assert_eq!(o.code, 2, "{model}");
        assert!(o.err.contains(want), "{model}: {}", o.err);
    }
    assert!(e.requests().is_empty());
    // A model the cloud doesn't serve, and a token it refuses, are errors a person can read.
    let o = cloud(&e, &["-p", "hi", "-m", "bedrock/claude-fable-5-2"]);
    assert!(o.err.contains("API error 404: model: anthropic.claude-fable-5-2 is not served"), "{}", o.err);
    let mut cmd = e.command(&["-p", "hi", "-m", "vertex/claude-opus-5-5"]);
    through_cloud(&e, &mut cmd);
    cmd.env("VERTEX_ACCESS_TOKEN", "expired");
    let bad = Env::with(&[("MOCK_BAD_TOKEN", "expired")]);
    cmd.env("ANTHROPIC_VERTEX_BASE_URL", format!("http://127.0.0.1:{}/v1", bad.port));
    let o = exec(cmd);
    assert!(o.err.contains("API error 401: Request had invalid authentication credentials"), "{}", o.err);
}
