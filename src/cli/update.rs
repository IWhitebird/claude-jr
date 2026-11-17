// `claudejr --update` and /update: replace this binary with the newest
// release. Each release publishes a `version` file and one plain binary per
// platform (claudejr-linux-x86_64, claudejr-macos-aarch64,
// claudejr-windows-x86_64.exe, ...), so nothing has to be unpacked.

use std::io::Read;
use std::path::Path;
use std::time::Duration;

const RELEASES: &str = "https://github.com/IWhitebird/claude-jr/releases/latest/download";
/// No release binary is anywhere near this big.
const MAX_BYTES: u64 = 64 * 1024 * 1024;

/// Where releases are downloaded from; CLAUDE_JR_RELEASES replaces it (tests, mirrors).
fn base() -> String {
    std::env::var("CLAUDE_JR_RELEASES").unwrap_or_else(|_| RELEASES.into()).trim_end_matches('/').to_string()
}

/// The release file for this machine, or None where there are no builds.
fn asset() -> Option<String> {
    let os = match std::env::consts::OS {
        "linux" => "linux",
        "macos" => "macos",
        "windows" if std::env::consts::ARCH == "x86_64" => "windows",
        _ => return None,
    };
    let exe = std::env::consts::EXE_SUFFIX;
    matches!(std::env::consts::ARCH, "x86_64" | "aarch64")
        .then(|| format!("claudejr-{os}-{}{exe}", std::env::consts::ARCH))
}

/// One of a host's addresses can be unreachable from a given network while
/// the others work, and each try may land on a different one: a few tries
/// get past it.
const TRIES: u32 = 5;

fn get(url: &str) -> Result<ureq::Body, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(5)))
        .timeout_recv_response(Some(Duration::from_secs(10)))
        .timeout_per_read(Some(Duration::from_secs(30)))
        .build()
        .into();
    let mut last = String::new();
    for attempt in 1..=TRIES {
        match agent.get(url).header("user-agent", "claudejr").call() {
            Ok(res) => return Ok(res.into_body()),
            // The server answered: asking again won't change a 404.
            Err(e @ ureq::Error::StatusCode(_)) => return Err(format!("{url}: {e}")),
            Err(e) => last = format!("{url}: {e} (tried {attempt} times)"),
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    Err(last)
}

/// "1.12.3" as numbers, so 1.10 is newer than 1.9.
fn parts(version: &str) -> Vec<u64> {
    version.trim().trim_start_matches('v').split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

/// Updates this binary if a newer release exists. Returns what to tell the user.
pub fn run() -> Result<String, String> {
    let current = env!("CARGO_PKG_VERSION");
    let asset = asset().ok_or("there are no release builds for this platform; build it from source")?;
    let base = base();
    let latest = get(&format!("{base}/version"))?.read_to_string().map_err(|e| e.to_string())?;
    let latest = latest.trim().trim_start_matches('v').to_string();
    if latest.is_empty() || latest.len() > 32 || !latest.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return Err("couldn't read the latest version number".into());
    }
    if parts(&latest) <= parts(current) {
        return Ok(format!("claude-jr {current} is the newest version."));
    }

    let exe = std::env::current_exe().and_then(|p| p.canonicalize()).map_err(|e| e.to_string())?;
    // (Windows only runs a file whose name ends in .exe.)
    let new = exe.with_extension(format!("update{}", std::env::consts::EXE_SUFFIX));
    let result = download(&format!("{base}/{asset}"), &new)
        .and_then(|()| check(&new, &latest))
        .and_then(|()| replace(&exe, &new).map_err(|e| format!("couldn't replace {}: {e}", exe.display())));
    if result.is_err() {
        let _ = std::fs::remove_file(&new);
    }
    result.map(|()| format!("Updated claude-jr {current} to {latest}. Start it again to use the new version."))
}

/// Puts `new` where the running program `exe` is.
fn replace(exe: &Path, new: &Path) -> std::io::Result<()> {
    if cfg!(windows) {
        // A running program can't be written over on Windows, but it can be
        // moved aside; the old one is cleared away by the next update.
        let old = exe.with_extension("old.exe");
        let _ = std::fs::remove_file(&old);
        std::fs::rename(exe, &old)?;
        return std::fs::rename(new, exe).inspect_err(|_| {
            let _ = std::fs::rename(&old, exe);
        });
    }
    // Renaming over a running binary is fine: the old one stays open until it exits.
    std::fs::rename(new, exe)
}

fn download(url: &str, to: &Path) -> Result<(), String> {
    let mut body = get(url)?.into_with_config().limit(MAX_BYTES).reader();
    let mut file =
        crate::sys::os::owner_only(std::fs::OpenOptions::new().write(true).create(true).truncate(true), 0o755)
            .open(to)
            .map_err(|e| format!("can't write {} ({e}); install it again with the install script", to.display()))?;
    let mut buf = [0u8; 64 * 1024];
    loop {
        match body.read(&mut buf).map_err(|e| format!("the download failed: {e}"))? {
            0 => return Ok(()),
            n => std::io::Write::write_all(&mut file, &buf[..n]).map_err(|e| e.to_string())?,
        }
    }
}

/// The downloaded file must run and say it is the version we asked for.
fn check(new: &Path, version: &str) -> Result<(), String> {
    // "Busy" means some other thread's child process still has the file open
    // from the moment it was forked, which passes in an instant.
    let mut tries = 0;
    let out = loop {
        match std::process::Command::new(new).arg("--version").output() {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && tries < 20 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(25));
            }
            out => break out.map_err(|e| format!("the download doesn't run: {e}"))?,
        }
    };
    let said = String::from_utf8_lossy(&out.stdout);
    if out.status.success() && said.trim() == format!("claude-jr {version}") {
        Ok(())
    } else {
        Err(format!("the download isn't claude-jr {version} (it says {:?})", said.trim()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_as_numbers() {
        assert!(parts("1.10.0") > parts("1.9.9"));
        assert!(parts("v0.2.0") > parts("0.1.9"));
        assert!(parts("0.1.0") <= parts("0.1.0"));
        assert!(parts("0.1") < parts("0.1.1"));
    }

    #[test]
    fn this_platform_has_a_release_name() {
        let name = asset().expect("linux and macos have builds");
        assert!(name.starts_with("claudejr-") && name.ends_with(std::env::consts::ARCH), "{name}");
    }
}
