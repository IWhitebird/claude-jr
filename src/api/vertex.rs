// Google Cloud's Vertex AI: where a request for a Claude model goes, and the
// access token it carries. The model is named in the address, not the body.
//
//   ANTHROPIC_VERTEX_PROJECT_ID (or GOOGLE_CLOUD_PROJECT)   the project, required
//   CLOUD_ML_REGION                                         "global" if unset
//   VERTEX_ACCESS_TOKEN                                     else `gcloud auth print-access-token`

use super::cloud::{Env, set};
use anyhow::{Result, bail};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Google's tokens last an hour; one from gcloud is asked for again before that.
const KEEP: Duration = Duration::from_secs(45 * 60);

/// A token from gcloud and when it was fetched.
pub(super) type Kept = Mutex<Option<(String, Instant)>>;

pub(super) fn project(env: Env) -> Result<String> {
    match set(env, "ANTHROPIC_VERTEX_PROJECT_ID").or_else(|| set(env, "GOOGLE_CLOUD_PROJECT")) {
        Some(p) => Ok(p),
        None => bail!("vertex needs a Google Cloud project: set ANTHROPIC_VERTEX_PROJECT_ID"),
    }
}

/// One host for everywhere, one per continent, one per region.
fn host(region: &str) -> String {
    match region {
        "global" => "aiplatform.googleapis.com".into(),
        "us" | "eu" => format!("aiplatform.{region}.rep.googleapis.com"),
        _ => format!("{region}-aiplatform.googleapis.com"),
    }
}

/// The address that streams a reply from `model`.
pub(super) fn address(env: Env, model: &str) -> Result<String> {
    let project = project(env)?;
    let region = set(env, "CLOUD_ML_REGION").unwrap_or_else(|| "global".into());
    // ANTHROPIC_VERTEX_BASE_URL: a gateway in front of Vertex, as Anthropic's own tools read it.
    let base = match set(env, "ANTHROPIC_VERTEX_BASE_URL") {
        Some(base) => base.trim_end_matches('/').to_string(),
        None => format!("https://{}/v1", host(&region)),
    };
    Ok(format!("{base}/projects/{project}/locations/{region}/publishers/anthropic/models/{model}:streamRawPredict"))
}

/// The token to send: the one given, or gcloud's, asked for once and kept.
pub(super) fn token(env: Env, kept: &Kept) -> Result<String> {
    if let Some(token) = set(env, "VERTEX_ACCESS_TOKEN") {
        return Ok(token);
    }
    let mut kept = kept.lock().unwrap();
    if let Some((token, at)) = &*kept
        && at.elapsed() < KEEP
    {
        return Ok(token.clone());
    }
    let how = "set VERTEX_ACCESS_TOKEN, or install gcloud and run `gcloud auth login`";
    let out = std::process::Command::new(set(env, "CLAUDE_JR_GCLOUD").unwrap_or_else(|| "gcloud".into()))
        .args(["auth", "print-access-token"])
        .stdin(std::process::Stdio::null())
        .output();
    let out = match out {
        Ok(out) => out,
        Err(_) => bail!("vertex needs a login: {how}"),
    };
    let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || token.is_empty() {
        let said = String::from_utf8_lossy(&out.stderr);
        let said = said.lines().find(|l| !l.trim().is_empty()).unwrap_or("it printed no token").trim().to_string();
        bail!("vertex: gcloud gave no access token ({said}); {how}");
    }
    *kept = Some((token.clone(), Instant::now()));
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> + Send + Sync {
        move |name| pairs.iter().find(|p| p.0 == name).map(|p| p.1.to_string())
    }

    #[test]
    fn the_address_names_the_project_the_region_and_the_model() {
        let e = env(&[("ANTHROPIC_VERTEX_PROJECT_ID", "proj-1")]);
        assert_eq!(
            address(&e, "claude-opus-5-5").unwrap(),
            "https://aiplatform.googleapis.com/v1/projects/proj-1/locations/global/publishers/anthropic/models/claude-opus-5-5:streamRawPredict"
        );
        let e = env(&[("GOOGLE_CLOUD_PROJECT", "p"), ("CLOUD_ML_REGION", "eu")]);
        assert!(
            address(&e, "m")
                .unwrap()
                .starts_with("https://aiplatform.eu.rep.googleapis.com/v1/projects/p/locations/eu/")
        );
        let e = env(&[("GOOGLE_CLOUD_PROJECT", "p"), ("CLOUD_ML_REGION", "us-east5")]);
        assert!(address(&e, "m").unwrap().starts_with("https://us-east5-aiplatform.googleapis.com/v1/projects/p/"));
        let e = env(&[("GOOGLE_CLOUD_PROJECT", "p"), ("ANTHROPIC_VERTEX_BASE_URL", "http://gw:8080/v1/")]);
        assert!(address(&e, "m").unwrap().starts_with("http://gw:8080/v1/projects/p/locations/global/"));
        let none = env(&[("ANTHROPIC_VERTEX_PROJECT_ID", " ")]);
        assert!(address(&none, "m").unwrap_err().to_string().contains("ANTHROPIC_VERTEX_PROJECT_ID"));
    }

    #[test]
    fn a_given_token_is_used_and_gclouds_is_asked_for_once() {
        let kept = Kept::default();
        assert_eq!(token(&env(&[("VERTEX_ACCESS_TOKEN", " tok \n")]), &kept).unwrap(), "tok");
        assert!(kept.lock().unwrap().is_none());

        // A stand-in for gcloud that counts how often it runs, and does what `mode` says.
        let dir = std::env::temp_dir().join(format!("cjr-gcloud-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (script, count, mode) = (dir.join("gcloud"), dir.join("count"), dir.join("mode"));
        let text = format!(
            "#!/bin/sh\necho run >> {}\n[ -f {} ] && {{ echo 'ERROR: not logged in' >&2; exit 1; }}\necho from-gcloud\n",
            count.display(),
            mode.display()
        );
        std::fs::write(&script, text).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = script.to_str().unwrap().to_string();
        let e = move |name: &str| (name == "CLAUDE_JR_GCLOUD").then(|| path.clone());
        // A file just written can be briefly busy while another test starts a program.
        let mut first = token(&e, &kept);
        for _ in 0..20 {
            if first.is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
            first = token(&e, &kept);
        }
        assert_eq!(first.unwrap(), "from-gcloud");
        let runs = || std::fs::read_to_string(&count).unwrap().lines().count();
        assert_eq!(token(&e, &kept).unwrap(), "from-gcloud");
        assert_eq!(runs(), 1);
        // An old one is replaced.
        if let Some(long_ago) = Instant::now().checked_sub(KEEP) {
            kept.lock().unwrap().as_mut().unwrap().1 = long_ago;
            token(&e, &kept).unwrap();
            assert_eq!(runs(), 2);
        }

        // gcloud that fails, and no gcloud at all.
        std::fs::write(&mode, "fail").unwrap();
        *kept.lock().unwrap() = None;
        let err = token(&e, &kept).unwrap_err().to_string();
        assert!(err.contains("not logged in") && err.contains("VERTEX_ACCESS_TOKEN"), "{err}");
        let gone = |name: &str| (name == "CLAUDE_JR_GCLOUD").then(|| "/no/such/gcloud".to_string());
        assert!(token(&gone, &kept).unwrap_err().to_string().starts_with("vertex needs a login"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
