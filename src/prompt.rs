// System prompt: behavior, environment facts, and project instructions.
//
// The prompt is built once per session and saved with it. It never changes
// mid-session (that would invalidate the prompt cache and earlier thinking),
// so it holds nothing that changes, like the model name.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const BASE: &str = "You are claude-jr, a coding agent running in the user's terminal. \
You help with software tasks: reading code, editing files, running commands. \
Be direct and brief. Your output is shown in a terminal, so use plain text or simple markdown.

Tools:
- Change existing files with edit; use write for new files or whole rewrites.
- Use bash for searching (rg, find, ls), git, building and running tests.
- Long command output is shortened for you with its start and end kept, so run tests and builds \
without `| tail` or `| grep`; a filter can hide the failure.
- Relative paths are relative to the working directory below.
- Independent tool calls can go in the same reply.
- If a tool call is refused, don't retry it; explain what you wanted to do.

Changing code:
- For a bug, reproduce it before you change anything, and run the same reproduction afterwards.
- Fix the cause, in every place it occurs, not only where the symptom shows. Keep the change small \
and in the style of the code around it.
- Run the existing tests for what you changed, not the whole suite when that is large: pick the \
test files or names that cover it. If one fails, find out whether your change caused it before \
deciding it doesn't matter.
- Finish the task before you stop, and say what you checked and what you could not.";

/// Whether a program is installed.
fn on_path(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(name).is_file()))
}

const INSTRUCTION_FILES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];
const MAX_FILE: usize = 32 * 1024;
const MAX_GIT_LINES: usize = 30;

pub fn system() -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut s = String::from(BASE);
    // Name a search tool that is there: without ripgrep the model's first
    // search would fail.
    if !on_path("rg") {
        s = s.replace("(rg, find, ls)", "(grep -rn, find, ls)");
    }
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

    if let Some(notes) = crate::tools::memory::load() {
        let _ = write!(
            s,
            "\n\n<memory>\nYour notes from earlier sessions in this project (update them with the memory tool):\n{}\n</memory>",
            notes.trim()
        );
    }
    for (path, text) in instructions(&cwd) {
        let _ = write!(s, "\n\n<instructions file=\"{}\">\n{}\n</instructions>", path.display(), text.trim());
    }
    s
}

/// Where instruction files are looked for: the config dir, then every parent
/// directory from the outermost down to `cwd`, so the nearest file comes last.
fn candidates(cwd: &Path) -> impl Iterator<Item = PathBuf> {
    let mut dirs: Vec<PathBuf> = cwd.ancestors().map(Path::to_path_buf).collect();
    dirs.push(crate::config::config_dir());
    dirs.into_iter().rev().flat_map(|d| INSTRUCTION_FILES.map(|f| d.join(f)))
}

/// Instruction files that exist right now.
pub fn memory_files() -> Vec<PathBuf> {
    let cwd = std::env::current_dir().unwrap_or_default();
    candidates(&cwd).filter(|p| p.is_file()).collect()
}

fn instructions(cwd: &Path) -> Vec<(PathBuf, String)> {
    candidates(cwd)
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

/// Appends a note to the project's instruction file: ./AGENTS.md, or
/// ./CLAUDE.md if that's the one the project already has.
pub fn remember(note: &str) -> std::io::Result<PathBuf> {
    remember_in(Path::new(""), note)
}

fn remember_in(dir: &Path, note: &str) -> std::io::Result<PathBuf> {
    use std::io::Write;
    let path = if !dir.join("AGENTS.md").exists() && dir.join("CLAUDE.md").exists() {
        dir.join("CLAUDE.md")
    } else {
        dir.join("AGENTS.md")
    };
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let sep = if existing.is_empty() || existing.ends_with('\n') { "" } else { "\n" };
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(f, "{sep}- {}", note.trim())?;
    Ok(path)
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
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(super::civil_from_days(0), (1970, 1, 1));
        assert_eq!(super::civil_from_days(20_766), (2026, 11, 9));
        assert_eq!(super::civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        assert_eq!(civil_from_days(19_783), (2024, 3, 1));
    }

    #[test]
    fn today_is_a_utc_date() {
        let d = today();
        let parts: Vec<&str> = d.split('-').collect();
        assert_eq!(parts.iter().map(|p| p.len()).collect::<Vec<_>>(), [4, 2, 2], "{d}");
        assert!(parts.iter().all(|p| p.bytes().all(|b| b.is_ascii_digit())));
        assert!(parts[0] >= "2026");
    }

    #[test]
    fn instruction_files_come_outermost_first() {
        crate::tools::test_home();
        let root = crate::tools::test_dir("prompt-instructions");
        let inner = root.join("a/b");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(root.join("AGENTS.md"), "  outer rules \n").unwrap();
        std::fs::write(root.join("a/CLAUDE.md"), "middle").unwrap();
        std::fs::write(inner.join("AGENTS.md"), "inner").unwrap();
        std::fs::write(inner.join("README.md"), "not an instruction file").unwrap();

        let found: Vec<(PathBuf, String)> =
            instructions(&inner).into_iter().filter(|(p, _)| p.starts_with(&root)).collect();
        assert_eq!(
            found,
            [
                (root.join("AGENTS.md"), "  outer rules \n".to_string()),
                (root.join("a/CLAUDE.md"), "middle".to_string()),
                (inner.join("AGENTS.md"), "inner".to_string()),
            ]
        );
    }

    #[test]
    fn a_huge_instruction_file_is_cut_on_a_character_boundary() {
        crate::tools::test_home();
        let dir = crate::tools::test_dir("prompt-huge");
        // 3-byte characters: the limit falls inside one.
        std::fs::write(dir.join("AGENTS.md"), "\u{20ac}".repeat(12_000)).unwrap();
        let found: Vec<String> =
            instructions(&dir).into_iter().filter(|(p, _)| p.starts_with(&dir)).map(|(_, t)| t).collect();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].len(), 32 * 1024 - 2);
        assert!(found[0].chars().all(|c| c == '\u{20ac}'));
    }

    #[test]
    fn the_config_folder_has_instructions_too() {
        crate::tools::test_home();
        let dir = crate::tools::test_dir("prompt-config-instr");
        std::fs::create_dir_all(crate::config::config_dir()).unwrap();
        let user = crate::config::config_dir().join("AGENTS.md");
        std::fs::write(&user, "my habits").unwrap();
        assert!(candidates(&dir).any(|p| p == user));
        assert!(instructions(&dir).iter().any(|(p, t)| *p == user && t == "my habits"));
        assert!(memory_files().contains(&user));
        let _ = std::fs::remove_file(user);
    }

    #[test]
    fn the_system_prompt_describes_the_environment() {
        crate::tools::test_home();
        let s = system();
        // The search tool named is one that is installed here.
        let search = if on_path("rg") { "(rg, find, ls)" } else { "(grep -rn, find, ls)" };
        assert!(s.starts_with(&BASE.replace("(rg, find, ls)", search)));
        assert!(BASE.contains("(rg, find, ls)"));
        let cwd = std::env::current_dir().unwrap();
        assert!(s.contains(&format!(
            "<env>\nWorking directory: {}\nPlatform: {}\nDate: {}\n",
            cwd.display(),
            std::env::consts::OS,
            today()
        )));
        assert!(s.contains("</env>"));
        // This project's own AGENTS.md is among the instructions.
        assert!(s.contains(&format!("<instructions file=\"{}\">", cwd.join("AGENTS.md").display())));
    }

    #[test]
    fn notes_are_appended_to_the_instruction_file() {
        let dir = crate::tools::test_dir("prompt-remember");
        // No file yet: AGENTS.md is made.
        assert_eq!(remember_in(&dir, "  uses pnpm \n").unwrap(), dir.join("AGENTS.md"));
        assert_eq!(std::fs::read_to_string(dir.join("AGENTS.md")).unwrap(), "- uses pnpm\n");
        remember_in(&dir, "runs on node 22").unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("AGENTS.md")).unwrap(), "- uses pnpm\n- runs on node 22\n");

        // A project that only has CLAUDE.md keeps using it, and a missing newline is added.
        let other = crate::tools::test_dir("prompt-remember-claude");
        std::fs::write(other.join("CLAUDE.md"), "# Notes").unwrap();
        assert_eq!(remember_in(&other, "x").unwrap(), other.join("CLAUDE.md"));
        assert_eq!(std::fs::read_to_string(other.join("CLAUDE.md")).unwrap(), "# Notes\n- x\n");
        assert!(!other.join("AGENTS.md").exists());

        // With both, AGENTS.md wins.
        std::fs::write(other.join("AGENTS.md"), "").unwrap();
        assert_eq!(remember_in(&other, "y").unwrap(), other.join("AGENTS.md"));
        assert_eq!(std::fs::read_to_string(other.join("AGENTS.md")).unwrap(), "- y\n");
    }

    #[test]
    fn git_status_is_brief_and_only_inside_a_repo() {
        let plain = crate::tools::test_dir("prompt-nogit");
        assert_eq!(git_status(&plain), None);

        let repo = crate::tools::test_dir("prompt-git");
        let init = Command::new("git").args(["init", "-q"]).current_dir(&repo).status().unwrap();
        assert!(init.success());
        std::fs::write(repo.join("one.txt"), "1").unwrap();
        let status = git_status(&repo).unwrap();
        assert!(status.starts_with("## No commits yet on"), "{status}");
        assert!(status.contains("?? one.txt\n"));
        assert!(!status.contains("more"));

        for i in 0..40 {
            std::fs::write(repo.join(format!("f{i:02}.txt")), "x").unwrap();
        }
        let status = git_status(&repo).unwrap();
        // The branch line, then files, cut at 30 lines with a count of the rest.
        assert_eq!(status.lines().count(), MAX_GIT_LINES + 1);
        assert_eq!(status.lines().last(), Some("... 12 more"));
    }
}
