// System prompt: behavior, environment facts, and project instructions.
//
// The prompt is built once per session and saved with it. It never changes
// mid-session (that would invalidate the prompt cache and earlier thinking),
// so it holds nothing that changes, like the model name.

use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const BASE: &str = "You are claude-jr, a coding agent running in the user's terminal. \
You help with software tasks: reading code, editing files, running commands. \
Be direct and brief. Your output is shown in a terminal, so use plain text or simple markdown.

Tools:
- Read a file before editing it. Prefer edit over write for existing files.
- Use bash for searching (rg, find, ls), git, building and running tests.
- Relative paths are relative to the working directory below.
- Independent tool calls can go in the same reply.
- If a tool call is refused, don't retry it; explain what you wanted to do.
- After making changes, check them (build, test, or read back) when you can.";

const INSTRUCTION_FILES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];
const MAX_FILE: usize = 32 * 1024;
const MAX_GIT_LINES: usize = 30;

pub fn system() -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut s = String::from(BASE);
    let _ = write!(
        s,
        "\n\n<env>\nWorking directory: {}\nPlatform: {}\nDate: {}\n",
        cwd.display(),
        std::env::consts::OS,
        today()
    );
    if let Some(git) = git_status(&cwd) {
        let _ = write!(s, "Git status at session start:\n{git}");
    }
    s.push_str("</env>");

    for (path, text) in instructions(&cwd) {
        let _ = write!(s, "\n\n<instructions file=\"{}\">\n{}\n</instructions>", path.display(), text.trim());
    }
    s
}

/// AGENTS.md / CLAUDE.md from the config dir, then from the outermost parent
/// directory down to the working directory, so the nearest file comes last.
fn instructions(cwd: &Path) -> Vec<(std::path::PathBuf, String)> {
    let mut dirs: Vec<&Path> = cwd.ancestors().collect();
    dirs.reverse();
    let global = crate::config::config_dir();
    std::iter::once(global.as_path())
        .chain(dirs)
        .flat_map(|d| INSTRUCTION_FILES.iter().map(move |f| d.join(f)))
        .filter_map(|p| {
            let mut text = std::fs::read_to_string(&p).ok()?;
            if text.len() > MAX_FILE {
                let cut = (0..=MAX_FILE).rev().find(|&i| text.is_char_boundary(i)).unwrap_or(0);
                text.truncate(cut);
            }
            Some((p, text))
        })
        .collect()
}

fn git_status(cwd: &Path) -> Option<String> {
    let out = Command::new("git").args(["status", "--short", "--branch"]).current_dir(cwd).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut s: String = text.lines().take(MAX_GIT_LINES).flat_map(|l| [l, "\n"]).collect();
    let more = text.lines().count().saturating_sub(MAX_GIT_LINES);
    if more > 0 {
        let _ = writeln!(s, "... {more} more");
    }
    Some(s)
}

/// UTC date as YYYY-MM-DD, without a date library.
fn today() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

// Howard Hinnant's days-to-civil algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    #[test]
    fn dates() {
        assert_eq!(super::civil_from_days(0), (1970, 1, 1));
        assert_eq!(super::civil_from_days(20_766), (2026, 11, 9));
        assert_eq!(super::civil_from_days(11_016), (2000, 2, 29));
    }
}
