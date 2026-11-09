// System prompt: who the agent is, plus facts about the environment.

use std::path::Path;
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

pub fn system(model: &str) -> String {
    let cwd = std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default();
    let git = Path::new(&cwd).join(".git").exists();
    format!(
        "{BASE}\n\n<env>\nWorking directory: {cwd}\nGit repository: {git}\nPlatform: {}\nDate: {}\nModel: {model}\n</env>",
        std::env::consts::OS,
        today(),
    )
}

/// UTC date as YYYY-MM-DD, without pulling in a date library.
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
