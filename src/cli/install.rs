// `claudejr install AGENT`: sets claude-jr up as a helper for another coding
// agent, so that agent can hand work to claude-jr workers (see mcp.rs).
// It registers the MCP server with the agent and, where the agent has such a
// thing, adds a short skill or rule saying when to use it.
//
//   claudejr install claude-code     .mcp.json and .claude/skills/claude-jr/ in this project
//   claudejr install cursor          .cursor/mcp.json and .cursor/rules/claude-jr.mdc in this project
//   claudejr install codex           ~/.codex/config.toml
//   claudejr install claude-code --user     for every project (through `claude mcp add`)
//   claudejr uninstall AGENT [--user]       takes it out again
//
// Options after the agent are the server's, and decide what the workers may
// do: -y, --sandbox, -m MODEL, --budget USD (see `claudejr mcp --help`). With
// none given, workers get to edit and run commands only where the sandbox
// can confine them to their worktree; elsewhere you choose -y yourself.
//
// Only claude-jr's own entries are written or removed; whatever else is in
// those files stays as it is.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const USAGE: &str = "usage: claudejr install AGENT [--user] [-y] [--sandbox] [-m MODEL] [--budget USD]
       claudejr uninstall AGENT [--user]

Lets another coding agent hand work to claude-jr workers.

  AGENT      claude-code, cursor or codex
  --user     for all your projects, not just this folder (codex is always this)
  -y, --sandbox, -m MODEL, --budget USD   what the workers may do and spend";

const NAME: &str = "claude-jr";

/// The skill: when another agent should reach for claude-jr, and how.
const SKILL: &str = "---
name: claude-jr
description: Run independent coding tasks in parallel with claude-jr workers. Use when work splits into pieces that don't depend on each other (several unrelated bugs, the same change across separate modules, tests for different files), or when a long task should carry on in the background while you do something else.
---

# Working with claude-jr workers

claude-jr is a small coding agent. Through the `claude-jr` MCP tools you can start
several of them at once. Each works in its own git worktree and branch, so they
can't disturb each other's files or yours, and each uses about half a megabyte.

## When to use it

- The work splits into pieces that can be done without knowing about each other.
- A piece is long and you have other things to do meanwhile.

Don't use it for one small change (do it yourself), or for pieces that must
agree on a shared design as they go (do those in order, yourself).

## How

1. Commit first. A worker starts from the last commit and sees nothing newer.
2. `start_task` once per piece. Write each prompt so it stands alone: the goal,
   the files involved, what done looks like, and how to check it (the test
   command). The worker can't see this conversation or ask you anything.
3. Carry on with your own work. `wait_tasks` when you need the results; it
   returns after a while even if some are still running, so call it again.
4. For each finished task read `task_result`, and `task_diff` for what it
   changed. `task_log` shows every step it took if something looks off.
5. `merge_task` to take a task's branch, `drop_task` to discard it. A merge
   that conflicts is undone and reported: resolve it by merging the branch by
   hand, or drop the task and redo that piece yourself.
6. Run the project's tests after merging. Workers checked their own piece, not
   the combination.

A task that fails or is refused permission says why in its result and log.
Workers' permissions and budget were set by the user when this was installed;
you can't change them, so tell the user if a task needed more than it had.
";

/// The same guidance as a Cursor rule.
fn cursor_rule() -> String {
    let body = SKILL.split_once("\n---\n").map_or(SKILL, |(_, body)| body);
    format!(
        "---\ndescription: Hand independent coding tasks to claude-jr workers through its MCP tools (start_task, wait_tasks, task_result, merge_task)\nalwaysApply: false\n---\n{body}"
    )
}

#[derive(Clone, Copy, PartialEq)]
enum Agent {
    ClaudeCode,
    Cursor,
    Codex,
}

struct Request {
    agent: Agent,
    user: bool,
    /// The server's options, as given.
    options: Vec<String>,
}

fn parse(args: &[String]) -> Result<Request, String> {
    let mut agent = None;
    let mut user = false;
    let mut options = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "claude-code" | "claude" => agent = Some(Agent::ClaudeCode),
            "cursor" => agent = Some(Agent::Cursor),
            "codex" => agent = Some(Agent::Codex),
            "--user" => user = true,
            "-y" | "--yes" | "--sandbox" | "--no-worktree" => options.push(a.clone()),
            "-m" | "--model" | "-e" | "--effort" | "--max-cost" | "--budget" => {
                let v = it.next().ok_or_else(|| format!("{a} needs a value"))?;
                options.extend([a.clone(), v.clone()]);
            }
            "-h" | "--help" => return Err(USAGE.into()),
            other => return Err(format!("{other}: not an agent or an option claude-jr knows\n\n{USAGE}")),
        }
    }
    let agent = agent.ok_or(USAGE)?;
    // The options are checked the way the server will read them.
    crate::cli::mcp::parse(&options)?;
    Ok(Request { agent, user: user || agent == Agent::Codex, options })
}

fn home() -> Result<PathBuf, String> {
    Some(crate::store::config::home()).filter(|h| h.is_absolute()).ok_or_else(|| "HOME isn't set".to_string())
}

/// The command another program should run to start claude-jr: its plain name
/// if that finds this binary, otherwise where this binary is.
fn command() -> String {
    let me = std::env::current_exe().and_then(|p| p.canonicalize()).ok();
    let on_path = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|d| d.join("claudejr"))
            .find(|p| p.is_file())
            .and_then(|p| p.canonicalize().ok())
    });
    match (me, on_path) {
        (Some(me), Some(found)) if me == found => "claudejr".into(),
        (Some(me), _) => me.display().to_string(),
        _ => "claudejr".into(),
    }
}

/// The server's arguments: what was asked for, or a default that is safe.
/// Returns them with a note for the user about what the workers can do.
fn server_args(options: &[String]) -> (Vec<String>, String) {
    let mut args = vec!["mcp".to_string()];
    // Which model does the work is the first thing its cost depends on.
    let model = match crate::work::task::worker_model(options) {
        Ok((model, _)) => format!(
            "Workers use {model} unless a task names another (to change it: install again with -m MODEL, or set \
             \"worker_model\" in claude-jr's config)."
        ),
        Err(_) => String::new(),
    };
    if options.iter().any(|o| matches!(o.as_str(), "-y" | "--yes" | "--sandbox")) {
        args.extend(options.iter().cloned());
        return (args, model);
    }
    if crate::sys::sandbox::available() {
        args.extend(["-y".to_string(), "--sandbox".to_string()]);
        args.extend(options.iter().cloned());
        return (args, format!("Workers can edit and run commands, sandboxed to their own worktree.\n{model}"));
    }
    args.extend(options.iter().cloned());
    let note = "Workers can read but not edit or run commands: there is no sandbox on this system to confine them. \
                To let them, install again with -y.";
    (args, format!("{note}\n{model}"))
}

/// Reads a JSON file that should hold an object ({} if it isn't there).
fn read_object(path: &Path) -> Result<Value, String> {
    match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(format!("{}: {e}", path.display())),
        Ok(text) if text.trim().is_empty() => Ok(json!({})),
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(v) if v.is_object() => Ok(v),
            _ => Err(format!("{} isn't a JSON object; fix or move it and try again", path.display())),
        },
    }
}

/// Writes `text` to `path` unless that is what it already holds. Returns
/// whether anything was written.
fn write_new(path: &Path, text: &str) -> Result<bool, String> {
    if std::fs::read_to_string(path).is_ok_and(|old| old == text) {
        return Ok(false);
    }
    write(path, text).map(|()| true)
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Adds (or with None removes) claude-jr under "mcpServers" in a JSON config.
/// Returns whether the file changed.
fn set_json_server(path: &Path, server: Option<Value>) -> Result<bool, String> {
    let mut config = read_object(path)?;
    let before = config.clone();
    match server {
        Some(server) => {
            if !config["mcpServers"].is_object() {
                config["mcpServers"] = json!({});
            }
            config["mcpServers"][NAME] = server;
        }
        None => {
            if let Some(servers) = config["mcpServers"].as_object_mut() {
                servers.remove(NAME);
            }
        }
    }
    if config == before {
        return Ok(false);
    }
    // A file we would leave holding nothing is one we made: take it away.
    if config == json!({ "mcpServers": {} }) {
        return std::fs::remove_file(path).map(|()| true).map_err(|e| format!("{}: {e}", path.display()));
    }
    write(path, &format!("{config:#}\n")).map(|()| true)
}

/// A TOML config with claude-jr's `[mcp_servers.claude-jr]` table replaced by
/// `table` (or removed, with None). Everything else is kept as it is.
fn set_toml_server(text: &str, table: Option<&str>) -> String {
    let header = format!("[mcp_servers.{NAME}]");
    let mut out: Vec<&str> = Vec::new();
    let mut skipping = false;
    for line in text.lines() {
        if line.trim() == header {
            skipping = true;
            continue;
        }
        // Our table ends where the next one begins.
        if skipping && line.trim_start().starts_with('[') {
            skipping = false;
        }
        if !skipping {
            out.push(line);
        }
    }
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    let mut text = out.join("\n");
    if let Some(table) = table {
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(table);
    }
    if !text.is_empty() {
        text.push('\n');
    }
    text
}

fn remove(path: &Path) -> bool {
    let gone = std::fs::remove_file(path).is_ok();
    // The folder too, if that was all it held.
    if let Some(dir) = path.parent() {
        let _ = std::fs::remove_dir(dir);
    }
    gone
}

/// Runs `claudejr install ...` or, with `install` false, `claudejr uninstall ...`.
pub fn run(args: &[String], install: bool) -> Result<String, String> {
    let req = parse(args)?;
    let (server_args, note) = server_args(&req.options);
    let server = json!({ "command": command(), "args": server_args });
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let base = if req.user { home()? } else { cwd };
    let mut did: Vec<String> = Vec::new();

    match req.agent {
        Agent::ClaudeCode => {
            let skill = base.join(".claude/skills").join(NAME).join("SKILL.md");
            if req.user {
                // The user-wide server list is Claude Code's own file; its
                // command line is the supported way to change it.
                let mut cmd = std::process::Command::new("claude");
                if install {
                    cmd.args(["mcp", "add", "--scope", "user", NAME, "--", &command()]).args(&server_args);
                } else {
                    cmd.args(["mcp", "remove", "--scope", "user", NAME]);
                }
                let shown =
                    format!("claude {}", cmd.get_args().map(|a| a.to_string_lossy()).collect::<Vec<_>>().join(" "));
                match cmd.output() {
                    Ok(out) if out.status.success() => did.push(format!("ran: {shown}")),
                    Ok(out) if !install => {
                        did.push(format!("{shown}: {}", String::from_utf8_lossy(&out.stderr).trim()))
                    }
                    Ok(out) => return Err(format!("{shown} failed: {}", String::from_utf8_lossy(&out.stderr).trim())),
                    Err(_) => did.push(format!("`claude` isn't on your PATH; run this yourself: {shown}")),
                }
            } else if set_json_server(&base.join(".mcp.json"), install.then_some(server))? {
                did.push(format!("{} .mcp.json", if install { "wrote" } else { "updated" }));
                if install {
                    did.push(
                        "Claude Code asks you to approve a project's servers the first time it starts there.".into(),
                    );
                }
            }
            if install {
                if write_new(&skill, SKILL)? {
                    did.push(format!("wrote {}", skill.display()));
                }
            } else if remove(&skill) {
                did.push(format!("removed {}", skill.display()));
            }
        }
        Agent::Cursor => {
            let config = base.join(".cursor/mcp.json");
            if set_json_server(&config, install.then_some(server))? {
                did.push(format!("{} {}", if install { "wrote" } else { "updated" }, config.display()));
            }
            // Rules are a project's; there is no user-wide folder for them.
            let rule = base.join(".cursor/rules").join(format!("{NAME}.mdc"));
            if !req.user && install {
                if write_new(&rule, &cursor_rule())? {
                    did.push(format!("wrote {}", rule.display()));
                }
            } else if !install && remove(&rule) {
                did.push(format!("removed {}", rule.display()));
            }
        }
        Agent::Codex => {
            let config = base.join(".codex/config.toml");
            let old = std::fs::read_to_string(&config).unwrap_or_default();
            let table = format!(
                "[mcp_servers.{NAME}]\ncommand = {}\nargs = {}",
                json!(command()),
                json!(server_args) // a JSON list of strings is a TOML one too
            );
            let new = set_toml_server(&old, install.then_some(table.as_str()));
            if new != old {
                write(&config, &new)?;
                did.push(format!("{} {}", if install { "wrote" } else { "updated" }, config.display()));
            }
        }
    }

    if did.is_empty() {
        return Ok(if install {
            // Still worth saying what the workers are: the config may have changed since.
            format!("Already set up; nothing changed.\n{note}").trim_end().to_string()
        } else {
            "Nothing of claude-jr's was there.".into()
        });
    }
    let mut out = did.join("\n");
    if install {
        if !note.is_empty() {
            out += &format!("\n{note}");
        }
        out += "\nRestart the agent (or reload its MCP servers) to pick it up.";
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_toml_table_is_added_replaced_and_removed_without_touching_the_rest() {
        let table = "[mcp_servers.claude-jr]\ncommand = \"claudejr\"\nargs = [\"mcp\"]";
        assert_eq!(set_toml_server("", Some(table)), format!("{table}\n"));
        let mine = "model = \"x\"\n\n[mcp_servers.other]\ncommand = \"o\"\n";
        let added = set_toml_server(mine, Some(table));
        assert_eq!(added, format!("model = \"x\"\n\n[mcp_servers.other]\ncommand = \"o\"\n\n{table}\n"));
        // Installing again changes nothing; other options replace the old table.
        assert_eq!(set_toml_server(&added, Some(table)), added);
        let middle = format!("a = 1\n\n{table}\nextra = true\n\n[later]\nb = 2\n");
        let replaced = set_toml_server(&middle, Some("[mcp_servers.claude-jr]\ncommand = \"new\""));
        assert_eq!(replaced, "a = 1\n\n[later]\nb = 2\n\n[mcp_servers.claude-jr]\ncommand = \"new\"\n");
        assert_eq!(set_toml_server(&added, None), mine);
        assert_eq!(set_toml_server(&format!("{table}\n"), None), "");
    }

    #[test]
    fn arguments_are_checked_and_the_skill_is_well_formed() {
        let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let req = parse(&args(&["cursor", "--user", "-y", "-m", "claude-haiku-5-5"])).ok().unwrap();
        assert!(req.agent == Agent::Cursor && req.user && req.options == ["-y", "-m", "claude-haiku-5-5"]);
        assert!(parse(&args(&["codex"])).ok().unwrap().user, "codex has only a user-wide config");
        assert!(parse(&args(&[])).is_err_and(|e| e.starts_with("usage:")));
        assert!(parse(&args(&["emacs"])).is_err_and(|e| e.contains("emacs: not an agent")));
        assert!(parse(&args(&["cursor", "-m"])).is_err_and(|e| e.contains("-m needs a value")));
        assert!(parse(&args(&["cursor", "--budget", "lots"])).is_err_and(|e| e.contains("--budget needs an amount")));

        // Explicit permissions are used as given; the note is for the default only.
        let (given, note) = server_args(&args(&["-y", "-m", "x"]));
        assert_eq!(given, args(&["mcp", "-y", "-m", "x"]));
        assert!(note.starts_with("Workers use x unless a task names another"), "only which model: {note}");
        let (default, note) = server_args(&args(&["--budget", "2"]));
        assert_eq!(
            default.contains(&"-y".to_string()),
            default.contains(&"--sandbox".to_string()),
            "never -y without the sandbox by default"
        );
        assert!(note.starts_with("Workers can"), "{note}");
        assert!(note.contains("\nWorkers use "), "and which model does the work: {note}");

        let (front, body) = SKILL.strip_prefix("---\n").unwrap().split_once("\n---\n").unwrap();
        assert!(front.lines().any(|l| l == "name: claude-jr") && front.lines().any(|l| l.starts_with("description: ")));
        for tool in ["start_task", "wait_tasks", "task_result", "task_diff", "task_log", "merge_task", "drop_task"] {
            assert!(body.contains(tool), "the skill mentions {tool}");
        }
        let rule = cursor_rule();
        assert!(
            rule.starts_with("---\ndescription: ") && rule.contains("alwaysApply: false") && rule.contains("## How")
        );
    }
}
