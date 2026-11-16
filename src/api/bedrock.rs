// Amazon Bedrock: where a request for a Claude model goes and how it is
// signed. This is Bedrock's Messages API address ("bedrock-mantle"), which
// takes Anthropic's request as it is; model ids carry an "anthropic." prefix.
//
//   AWS_REGION (or AWS_DEFAULT_REGION)             "us-east-1" if unset
//   AWS_BEARER_TOKEN_BEDROCK                       a Bedrock API key, or:
//   AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY, AWS_SESSION_TOKEN
//
// With access keys every request is signed (AWS Signature Version 4): the
// signature covers the address, the time and a hash of the body.

use super::cloud::{Env, set};
use anyhow::{Result, bail};
use ring::{digest, hmac};
use std::time::{SystemTime, UNIX_EPOCH};

const SERVICE: &str = "bedrock-mantle";

pub(super) enum Login {
    /// A Bedrock API key, sent like Anthropic's own.
    Key(String),
    Keys(Keys),
}

pub(super) struct Keys {
    pub id: String,
    pub secret: String,
    pub session: Option<String>,
    pub region: String,
}

fn region(env: Env) -> String {
    set(env, "AWS_REGION").or_else(|| set(env, "AWS_DEFAULT_REGION")).unwrap_or_else(|| "us-east-1".into())
}

pub(super) fn address(env: Env) -> String {
    // ANTHROPIC_BEDROCK_BASE_URL: a gateway in front of Bedrock.
    let base = match set(env, "ANTHROPIC_BEDROCK_BASE_URL") {
        Some(base) => base.trim_end_matches('/').to_string(),
        None => format!("https://{SERVICE}.{}.api.aws/anthropic", region(env)),
    };
    format!("{base}/v1/messages")
}

pub(super) fn login(env: Env) -> Result<Login> {
    if let Some(key) = set(env, "AWS_BEARER_TOKEN_BEDROCK") {
        return Ok(Login::Key(key));
    }
    match (set(env, "AWS_ACCESS_KEY_ID"), set(env, "AWS_SECRET_ACCESS_KEY")) {
        (Some(id), Some(secret)) => {
            Ok(Login::Keys(Keys { id, secret, session: set(env, "AWS_SESSION_TOKEN"), region: region(env) }))
        }
        _ => bail!(
            "bedrock needs a login: set AWS_BEARER_TOKEN_BEDROCK (a Bedrock API key), \
             or AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY"
        ),
    }
}

/// The id Bedrock knows a model by.
pub(super) fn model(id: &str) -> String {
    if id.contains("anthropic.") { id.to_string() } else { format!("anthropic.{id}") }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// The SHA-256 of everything `body` holds, read through once.
pub(super) fn hash(mut body: impl std::io::Read) -> std::io::Result<String> {
    let mut sum = digest::Context::new(&digest::SHA256);
    let mut buf = [0u8; 16 * 1024];
    loop {
        match body.read(&mut buf)? {
            0 => return Ok(hex(sum.finish().as_ref())),
            n => sum.update(&buf[..n]),
        }
    }
}

fn mac(key: &[u8], text: &str) -> hmac::Tag {
    hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), text.as_bytes())
}

/// "20150830T123600Z".
fn timestamp(now: SystemTime) -> String {
    let secs = now.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (y, m, d) = crate::prompt::civil_from_days((secs / 86_400) as i64);
    let s = secs % 86_400;
    format!("{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z", s / 3600, s % 3600 / 60, s % 60)
}

impl Keys {
    /// The headers that sign a POST of a body with this hash to `url`.
    pub(super) fn sign(&self, url: &str, body_hash: &str, now: SystemTime) -> Vec<(&'static str, String)> {
        self.sign_as(SERVICE, url, body_hash, now)
    }

    fn sign_as(&self, service: &str, url: &str, body_hash: &str, now: SystemTime) -> Vec<(&'static str, String)> {
        let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
        let (host, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let time = timestamp(now);
        let date = &time[..8];

        // What is signed: the method, the path, no query, these headers, the body's hash.
        let mut headers = format!("host:{host}\nx-amz-date:{time}\n");
        let mut names = "host;x-amz-date".to_string();
        if let Some(session) = &self.session {
            headers.push_str(&format!("x-amz-security-token:{session}\n"));
            names.push_str(";x-amz-security-token");
        }
        let request = format!("POST\n{path}\n\n{headers}\n{names}\n{body_hash}");
        let scope = format!("{date}/{}/{service}/aws4_request", self.region);
        let to_sign = format!(
            "AWS4-HMAC-SHA256\n{time}\n{scope}\n{}",
            hex(digest::digest(&digest::SHA256, request.as_bytes()).as_ref())
        );
        // The key is made for this day, region and service, one step at a time.
        let mut key = mac(format!("AWS4{}", self.secret).as_bytes(), date);
        for part in [self.region.as_str(), service, "aws4_request"] {
            key = mac(key.as_ref(), part);
        }
        let signature = hex(mac(key.as_ref(), &to_sign).as_ref());

        let mut out = vec![
            ("x-amz-date", time),
            (
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={names}, Signature={signature}",
                    self.id
                ),
            ),
        ];
        if let Some(session) = &self.session {
            out.push(("x-amz-security-token", session.clone()));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    fn example(session: Option<&str>) -> Keys {
        Keys {
            id: "AKIDEXAMPLE".into(),
            secret: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            session: session.map(String::from),
            region: "us-east-1".into(),
        }
    }

    /// 2015-08-30 12:36:00 UTC, the time AWS's examples use.
    fn then() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_440_938_160)
    }

    #[test]
    fn hashing_and_time_are_as_aws_writes_them() {
        assert_eq!(hash(&b""[..]).unwrap(), EMPTY);
        assert_eq!(hash(&b"abc"[..]).unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(timestamp(then()), "20150830T123600Z");
        assert_eq!(timestamp(UNIX_EPOCH + Duration::from_secs(1_794_873_599)), "20261116T235959Z");
    }

    // AWS's own test suite for Signature Version 4, "post-vanilla": an empty
    // POST to example.amazonaws.com, signed for the service named "service".
    #[test]
    fn the_signature_matches_awss_published_example() {
        let signed = example(None).sign_as("service", "https://example.amazonaws.com/", EMPTY, then());
        assert_eq!(signed[0], ("x-amz-date", "20150830T123600Z".to_string()));
        assert_eq!(
            signed[1].1,
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
             SignedHeaders=host;x-amz-date, \
             Signature=5da7c1a2acd57cee7505fc6676e4e544621c30862966e37dddb68e92efbe5d6b"
        );
        assert_eq!(signed.len(), 2);
    }

    #[test]
    fn a_session_token_is_sent_and_signed() {
        let plain = example(None).sign("https://bedrock-mantle.us-east-1.api.aws/anthropic/v1/messages", EMPTY, then());
        let keys = example(Some("TOKEN"));
        let signed = keys.sign("https://bedrock-mantle.us-east-1.api.aws/anthropic/v1/messages", EMPTY, then());
        assert_eq!(signed[2], ("x-amz-security-token", "TOKEN".to_string()));
        assert!(signed[1].1.contains("/us-east-1/bedrock-mantle/aws4_request"));
        assert!(signed[1].1.contains("SignedHeaders=host;x-amz-date;x-amz-security-token"));
        assert_ne!(signed[1].1.rsplit('=').next(), plain[1].1.rsplit('=').next());
        // Anything that changes, changes the signature: the body, the path, the time.
        let sig = |url: &str, hash: &str, at: SystemTime| keys.sign(url, hash, at)[1].1.clone();
        let url = "https://h/anthropic/v1/messages";
        let base = sig(url, EMPTY, then());
        assert_eq!(base, sig(url, EMPTY, then()));
        assert_ne!(base, sig(url, &hash(&b"x"[..]).unwrap(), then()));
        assert_ne!(base, sig("https://h/other", EMPTY, then()));
        assert_ne!(base, sig(url, EMPTY, then() + Duration::from_secs(1)));
    }

    #[test]
    fn the_address_login_and_model_id() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| pairs.iter().find(|p| p.0 == name).map(|p| p.1.to_string())
        };
        assert_eq!(address(&env(&[])), "https://bedrock-mantle.us-east-1.api.aws/anthropic/v1/messages");
        assert_eq!(
            address(&env(&[("AWS_DEFAULT_REGION", "eu-west-1")])),
            "https://bedrock-mantle.eu-west-1.api.aws/anthropic/v1/messages"
        );
        let both = env(&[("AWS_REGION", "ap-south-1"), ("AWS_DEFAULT_REGION", "eu-west-1")]);
        assert!(address(&both).contains(".ap-south-1."));
        assert_eq!(
            address(&env(&[("ANTHROPIC_BEDROCK_BASE_URL", "http://gw/anthropic/")])),
            "http://gw/anthropic/v1/messages"
        );

        assert!(login(&env(&[])).is_err());
        assert!(login(&env(&[("AWS_ACCESS_KEY_ID", "a")])).is_err(), "half a pair is no login");
        let all = env(&[
            ("AWS_BEARER_TOKEN_BEDROCK", "bk"),
            ("AWS_ACCESS_KEY_ID", "a"),
            ("AWS_SECRET_ACCESS_KEY", "s"),
            ("AWS_SESSION_TOKEN", "t"),
            ("AWS_REGION", "us-west-2"),
        ]);
        assert!(matches!(login(&all).unwrap(), Login::Key(k) if k == "bk"), "the API key wins");
        let pair = env(&[("AWS_ACCESS_KEY_ID", "a"), ("AWS_SECRET_ACCESS_KEY", "s"), ("AWS_SESSION_TOKEN", "t")]);
        let Login::Keys(k) = login(&pair).unwrap() else { panic!() };
        assert_eq!(
            (k.id.as_str(), k.secret.as_str(), k.session.as_deref(), k.region.as_str()),
            ("a", "s", Some("t"), "us-east-1")
        );

        assert_eq!(model("claude-opus-5-5"), "anthropic.claude-opus-5-5");
        assert_eq!(model("anthropic.claude-opus-5-5"), "anthropic.claude-opus-5-5");
        assert_eq!(model("us.anthropic.claude-sonnet-5"), "us.anthropic.claude-sonnet-5");
    }
}
