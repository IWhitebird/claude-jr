// --worktree: do the work in a git worktree of its own, on its own branch, so
// several agents can work on one repository without touching each other's
// files or yours. The worktree starts from the current commit (changes you
// haven't committed stay where they are) and lives in claude-jr's data
// folder, outside the repository.
//
// When the run ends, a headless run commits what it changed, so the branch
// carries the work and can be merged or thrown away. A worktree in which
// nothing changed is removed again, branch and all.

use crate::config::data_dir;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Set once the worktree exists: "repository\tworktree\tbranch\tbase commit".
/// A process started again inside the sandbox finds it here.
pub const ENV: &str = "CLAUDE_JR_WORKTREE";

pub struct Worktree {
    /// The repository the worktree belongs to.
    pub repo: PathBuf,
    pub path: PathBuf,
    pub branch: String,
    /// The commit it started from.
    pub base: String,
}

/// Runs git in `dir` and returns what it printed.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        // Nobody may be there to type a passphrase or a password.
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| format!("couldn't run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

impl Worktree {
    /// The worktree this process is working in, if it was started with one.
    pub fn current() -> Option<Worktree> {
        let value = std::env::var(ENV).ok()?;
        let mut parts = value.split('\t');
        Some(Worktree {
            repo: parts.next()?.into(),
            path: parts.next()?.into(),
            branch: parts.next()?.into(),
            base: parts.next()?.into(),
        })
    }

    /// Makes a worktree of the repository the current directory is in, named
    /// after `id`, and moves into it (into the same subfolder, if we were in one).
    pub fn create(id: &str) -> Result<Worktree, String> {
        let here = std::env::current_dir().map_err(|e| e.to_string())?;
        let not_git = |_| "--worktree needs a git repository; this folder isn't in one".to_string();
        let repo = PathBuf::from(git(&here, &["rev-parse", "--show-toplevel"]).map_err(not_git)?);
        let subfolder = git(&here, &["rev-parse", "--show-prefix"])?;
        let base = git(&repo, &["rev-parse", "HEAD"])
            .map_err(|_| "--worktree needs at least one commit to start from".to_string())?;
        let name = repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let branch = format!("claudejr/{id}");
        let path = data_dir().join("worktrees").join(format!("{name}-{id}"));
        std::fs::create_dir_all(data_dir().join("worktrees")).map_err(|e| e.to_string())?;
        let path_text = path.to_string_lossy().into_owned();
        git(&repo, &["worktree", "add", "-q", "-b", &branch, &path_text, &base])
            .map_err(|e| format!("couldn't make the worktree: {e}"))?;
        std::env::set_current_dir(path.join(&subfolder)).map_err(|e| e.to_string())?;
        let worktree = Worktree { repo, path, branch, base };
        let value =
            format!("{}\t{}\t{}\t{}", worktree.repo.display(), worktree.path.display(), worktree.branch, worktree.base);
        // SAFETY: called at start-up, before any other thread exists.
        unsafe { std::env::set_var(ENV, value) };
        Ok(worktree)
    }

    /// Where the repository keeps its own data (`.git`), which a sandboxed
    /// run must be able to write to for commits in the worktree to work.
    pub fn git_dir(&self) -> PathBuf {
        git(&self.repo, &["rev-parse", "--path-format=absolute", "--git-common-dir"])
            .map_or_else(|_| self.repo.join(".git"), PathBuf::from)
    }

    /// Wraps the run up. With a `message`, what changed is committed under
    /// it. A worktree with nothing in it is removed. Returns what is left,
    /// for the run's result: the branch, where it is and what it holds.
    pub fn finish(&self, message: Option<&str>) -> Value {
        let dirty = || git(&self.path, &["status", "--porcelain"]).is_ok_and(|s| !s.is_empty());
        let mut problem = None;
        if let (true, Some(message)) = (dirty(), message) {
            let first_line: String = message.lines().next().unwrap_or("").chars().take(72).collect();
            let subject = if first_line.trim().is_empty() { "claude-jr" } else { first_line.trim() };
            let committed =
                git(&self.path, &["add", "-A"]).and_then(|_| git(&self.path, &["commit", "-q", "-m", subject]));
            problem = committed.err().map(|e| format!("the changes are not committed: {e}"));
        }
        let commits = git(&self.path, &["rev-list", "--count", &format!("{}..HEAD", self.base)])
            .ok()
            .and_then(|n| n.parse::<u64>().ok())
            .unwrap_or(0);
        let uncommitted = dirty();
        if commits == 0 && !uncommitted {
            // Not from inside the folder that is about to go.
            let _ = std::env::set_current_dir(&self.repo);
            let path = self.path.to_string_lossy();
            let _ = git(&self.repo, &["worktree", "remove", "--force", &path]);
            let _ = git(&self.repo, &["branch", "-q", "-D", &self.branch]);
            return Value::Null;
        }
        let commit = git(&self.path, &["rev-parse", "--short", "HEAD"]).unwrap_or_default();
        json!({
            "branch": self.branch,
            "path": self.path,
            "repository": self.repo,
            "base": self.base,
            "commit": commit,
            "commits": commits,
            "uncommitted": uncommitted,
            "problem": problem,
        })
    }
}

/// What to tell a person about a finished worktree.
pub fn summary(left: &Value) -> String {
    if left.is_null() {
        return "Nothing was changed, so the worktree was removed.".into();
    }
    let (branch, path) = (left["branch"].as_str().unwrap_or(""), left["path"].as_str().unwrap_or(""));
    let state = match (left["commits"].as_u64().unwrap_or(0), left["uncommitted"].as_bool().unwrap_or(false)) {
        (0, _) => "changes that aren't committed".to_string(),
        (n, false) => format!("{n} commit{}", if n == 1 { "" } else { "s" }),
        (n, true) => format!("{n} commit{} and changes that aren't committed", if n == 1 { "" } else { "s" }),
    };
    let problem = left["problem"].as_str().map(|p| format!("\n{p}")).unwrap_or_default();
    format!(
        "The work is on branch {branch} ({state}), in {path}{problem}\n\
         Take it:    git merge {branch}\n\
         Drop it:    git worktree remove --force {path} && git branch -D {branch}"
    )
}

/// A short id for a worktree or a task: the time and the process, so two
/// started together differ.
pub fn new_id() -> String {
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    format!("{:x}{:x}", ms % 0xff_ffff, std::process::id() % 0xfff)
}
