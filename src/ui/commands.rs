// What is typed and sent: messages, pastes, and the slash commands.

use super::app::{App, Prompt, Stream, Work, ago};
use super::history::Block;
use super::text::{DIM, RED};
use super::{paste, stats, text};
use crate::agent::Command;

pub(super) const HELP: &str = "/clear, /new    start a new conversation on a clean screen
/model [name]   pick a model from the list, or set one by id
/effort [level] low, medium, high, xhigh, max, or default
/rename [name]  name this session (resume with claudejr -r NAME)
/rewind         go back to before an earlier message, undoing file changes
/jobs, /kill N  background jobs, and stopping one
/resume         switch to an earlier conversation in this folder
/compact [focus] replace the conversation with a summary, to free up context
/memory         instruction files in use
/skills         available skills (run one with /name)
/mcp            connected MCP servers
/stats          memory and CPU used by claude-jr and what it started
/settings       layout, default model, effort, mode, web tools, sandbox
/login          add or change your API key; /logout removes it
/update         replace claudejr with the newest release (also: claudejr --update)
/cost           tokens and cost so far
/exit           quit
#note           save a note to ./AGENTS.md (used from now on)
!command        run a shell command yourself; the agent sees the output
images          drag a file in (its path is attached), or ctrl+v a screenshot
paste           ctrl+v pastes text too; a long paste shows as a short [Pasted #N] label

type / to list commands: up/down pick, tab completes, enter runs
enter sends · ctrl-j, shift-enter or \\ then enter for a new line
shift+tab: normal → accept edits → plan → auto → skip permissions
esc stops · ctrl-c clears the input, twice quits · up/down for history
ctrl-s stashes what you've typed; it comes back after your next message";

/// The commands, shown in a list as soon as `/` is typed.
pub(super) const COMMANDS: &[(&str, &str)] = &[
    ("help", "commands and keys"),
    ("clear", "start a new conversation on a clean screen"),
    ("new", "start a new conversation on a clean screen"),
    ("model", "pick a model, or set one by id"),
    ("effort", "low, medium, high, xhigh, max, or default"),
    ("resume", "switch to an earlier conversation in this folder"),
    ("rename", "name this session"),
    ("rewind", "go back to before an earlier message, undoing file changes"),
    ("compact", "replace the conversation with a summary"),
    ("jobs", "background jobs"),
    ("kill", "stop a background job: /kill N"),
    ("memory", "instruction files in use"),
    ("skills", "available skills"),
    ("mcp", "connected MCP servers"),
    ("stats", "memory and CPU used"),
    ("cost", "tokens and cost so far"),
    ("settings", "layout, default model, effort, mode, web tools, sandbox"),
    ("login", "add or change your API key"),
    ("logout", "remove your API key"),
    ("update", "replace claudejr with the newest release"),
    ("exit", "quit"),
];
/// How many of them are shown at once.
pub(super) const MENU_ROWS: usize = 8;

/// The commands and skills that start with what's typed after the `/`, an
/// exact match first. Empty unless `input` is a `/` and part of one word.
pub(super) fn menu_matches<'a>(input: &str, skills: &'a [String]) -> Vec<(&'a str, &'a str)> {
    let Some(typed) = input.strip_prefix('/') else { return Vec::new() };
    if typed.contains(char::is_whitespace) {
        return Vec::new();
    }
    let all = COMMANDS.iter().copied().chain(skills.iter().map(|s| (s.as_str(), "skill")));
    let mut found: Vec<(&str, &str)> = all.filter(|(name, _)| name.starts_with(typed)).collect();
    found.sort_by_key(|(name, _)| *name != typed);
    found
}

/// Whether what follows a leading `/` is a file path (a dragged or pasted
/// file such as /tmp/shot.png) and not a command: commands have no `/` or `.`
/// in their name.
pub(super) fn starts_with_path(after_slash: &str) -> bool {
    after_slash.split_whitespace().next().is_some_and(|word| word.contains(['/', '.']))
}

impl App {
    /// Puts pasted text into the input. A big paste goes in as a short label
    /// that is swapped for the text when the message is sent.
    pub(super) fn paste(&mut self, text: &str) {
        let text = if text.contains('\r') { text.replace("\r\n", "\n").replace('\r', "\n") } else { text.to_string() };
        if self.prompts.is_empty() && paste::is_big(&text) {
            let label = self.pastes.add(text);
            self.input.insert(&label);
        } else {
            self.input.insert(&text);
        }
    }

    pub(super) fn submit(&mut self, text: String) {
        let trimmed = text.trim();
        if let Some(cmd) = trimmed.strip_prefix('/').filter(|c| !starts_with_path(c)) {
            return self.command(cmd);
        }
        if let Some(note) = trimmed.strip_prefix('#').map(str::trim).filter(|n| !n.is_empty()) {
            return self.remember(&self.pastes.expand(note));
        }
        let shell = trimmed.strip_prefix('!').map(|c| c.trim().to_string());
        if self.work.is_some() {
            self.queued.push_back(text);
            return;
        }
        // Shell commands don't need the API, so they run without a key.
        if !self.has_key && shell.is_none() {
            self.queued.push_back(text);
            if !self.prompts.iter().any(|p| matches!(p, Prompt::Key)) {
                self.prompts.push_back(Prompt::Key);
            }
            return;
        }
        self.gap();
        self.add(Block::User(text::clean(&text)));
        self.stream.block_start = true;
        self.work = Some(Work::new());
        // What is shown keeps the labels of big pastes; what is sent has the text.
        let cmd = match shell {
            Some(shell) => Command::Shell(self.pastes.expand(&shell)),
            None => Command::Send(self.pastes.expand(&text)),
        };
        let _ = self.agent.commands.send(cmd);
    }

    pub(super) fn remember(&mut self, note: &str) {
        match crate::prompt::remember(note) {
            Ok(path) => {
                let _ = self.agent.commands.send(Command::Remember(note.to_string()));
                self.note(&format!("Saved to {}.", path.display()), DIM);
            }
            Err(e) => self.note(&format!("Could not save the note: {e}"), RED),
        }
    }

    pub(super) fn command(&mut self, cmd: &str) {
        let (name, arg) = cmd.split_once(' ').map_or((cmd, ""), |(n, a)| (n, a.trim()));
        match name {
            "help" => self.note(HELP, ""),
            "exit" | "quit" => self.quit = true,
            "login" => {
                self.input.clear();
                if !self.prompts.iter().any(|p| matches!(p, Prompt::Key)) {
                    self.prompts.push_front(Prompt::Key);
                }
            }
            "compact" if self.work.is_some() => self.note("Wait for the current turn to finish.", RED),
            "compact" => {
                self.work = Some(Work::new());
                self.set_activity("summarizing the conversation".into());
                let _ = self.agent.commands.send(Command::Compact(arg.to_string()));
            }
            "resume" if self.work.is_some() => self.note("Wait for the current turn to finish.", RED),
            "resume" => {
                let now = std::time::SystemTime::now();
                let sessions: Vec<(String, String)> = crate::store::session::Session::list()
                    .into_iter()
                    .rev()
                    .filter(|s| s.messages > 0)
                    .take(9)
                    .map(|s| {
                        let age = now.duration_since(s.modified).map_or(0, |d| d.as_secs());
                        let title = s.name.unwrap_or_else(|| s.first_prompt.lines().next().unwrap_or("").to_string());
                        let desc = format!("{title}  ({}, {} messages)", ago(age), s.messages);
                        (s.id, desc)
                    })
                    .collect();
                if sessions.is_empty() {
                    self.note("No saved conversations in this folder yet.", DIM);
                } else {
                    self.prompts.push_front(Prompt::Resume(sessions));
                }
            }
            "rewind" if self.work.is_some() => self.note("Wait for the current turn to finish.", RED),
            "rewind" => {
                let _ = self.agent.commands.send(Command::Rewind(None));
            }
            "logout" => {
                let _ = crate::store::config::delete_api_key();
                let _ = self.agent.commands.send(Command::SetKey(String::new()));
                self.has_key = false;
                let env = if std::env::var_os("ANTHROPIC_API_KEY").is_some() {
                    " ANTHROPIC_API_KEY is still set in your environment and will be used next time."
                } else {
                    ""
                };
                self.note(&format!("Removed the saved API key.{env}"), DIM);
            }
            "memory" => {
                let files = crate::prompt::memory_files();
                let list: Vec<String> = files.iter().map(|f| format!("  {}", f.display())).collect();
                let list = if list.is_empty() { "  none yet".to_string() } else { list.join("\n") };
                let msg = format!(
                    "Instruction files (AGENTS.md / CLAUDE.md):\n{list}\nStart a message with # to add a note."
                );
                self.note(&msg, "");
            }
            "skills" => {
                let msg = if self.skills.is_empty() {
                    "No skills found. Put them in .claude/skills/<name>/SKILL.md.".to_string()
                } else {
                    format!("Skills (run with /name):\n  {}", self.skills.join("\n  "))
                };
                self.note(&msg, "");
            }
            "mcp" => {
                let msg = if self.mcp.is_empty() {
                    "No MCP servers. Add them to .mcp.json or \"mcp_servers\" in the config.".to_string()
                } else {
                    format!("MCP servers:\n  {}", self.mcp.join("\n  "))
                };
                self.note(&msg, "");
            }
            "stats" => {
                let children = self.agent.tools.get().map(|t| t.child_pids()).unwrap_or_default();
                let report = stats::report(self.started.elapsed(), &children);
                self.note(&report, "");
            }
            "update" => match crate::cli::update::run() {
                Ok(msg) => self.note(&msg, ""),
                Err(e) => self.note(&format!("Update failed: {e}"), RED),
            },
            "cost" => {
                let msg = format!("${:.4} so far · {} tokens in context", self.cost, self.context);
                self.note(&msg, "");
            }
            "model" if arg.is_empty() => self.prompts.push_front(Prompt::Model),
            "model" => self.set_model(arg),
            "effort" => self.set_effort(arg),
            "settings" => self.prompts.push_front(Prompt::Settings),
            "jobs" => {
                let jobs = self.agent.tools.get().map(|t| t.jobs()).unwrap_or_default();
                let msg = if jobs.is_empty() {
                    "No background jobs.".to_string()
                } else {
                    let lines: Vec<String> = jobs
                        .iter()
                        .map(|(id, cmd, running)| {
                            format!("  {id}. {cmd}  ({})", if *running { "running" } else { "done" })
                        })
                        .collect();
                    format!("Background jobs (stop one with /kill N):\n{}", lines.join("\n"))
                };
                self.note(&msg, "");
            }
            "kill" => {
                let killed = arg.parse().ok().and_then(|id| self.agent.tools.get().map(|t| t.kill_job(id)));
                match killed {
                    Some(true) => self.note(&format!("Stopped job {arg}."), DIM),
                    _ => self.note("No such job. /jobs lists them.", RED),
                }
            }
            "rename" if arg.is_empty() => {
                let msg = match &self.name {
                    Some(n) => format!("This session is \"{n}\". /rename NAME to change it."),
                    None => "This session has no name yet. /rename NAME to set one.".to_string(),
                };
                self.note(&msg, "");
            }
            "rename" => {
                let _ = self.agent.commands.send(Command::Rename(arg.to_string()));
                self.name = Some(arg.to_string());
                self.note(&format!("Renamed to \"{arg}\". Resume it later with: claudejr -r \"{arg}\""), DIM);
            }
            "clear" | "new" if self.work.is_some() => self.note("Wait for the current turn to finish.", RED),
            "clear" | "new" => {
                let _ = self.agent.commands.send(Command::Clear);
                // A clean slate on screen too: the loop wipes the terminal before
                // the next draw, and the header starts the page again.
                self.clear_screen = true;
                self.commits.clear();
                self.history.clear();
                self.stream = Stream { block_start: true, ..Stream::default() };
                (self.context, self.name) = (0, None);
                self.queued.clear();
                self.subagents.clear();
                self.header();
            }
            _ if self.skills.iter().any(|s| s == name) => {
                if self.work.is_some() {
                    return self.note("Wait for the current turn to finish.", RED);
                }
                self.gap();
                self.add(Block::User(format!("/{cmd}")));
                self.stream.block_start = true;
                self.work = Some(Work::new());
                let skill = Command::Skill { name: name.to_string(), args: arg.to_string() };
                let _ = self.agent.commands.send(skill);
            }
            _ => self.note(&format!("Unknown command /{name}. Try /help."), RED),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_is_not_a_command() {
        assert!(starts_with_path("tmp/shot.png what is this"));
        assert!(starts_with_path("home/me/a b.txt"));
        assert!(starts_with_path("etc/"));
        assert!(!starts_with_path("model"));
        assert!(!starts_with_path("model claude-opus-5-5"));
        assert!(!starts_with_path("kill 2"));
        assert!(!starts_with_path(""));
    }

    #[test]
    fn slash_lists_matching_commands() {
        let skills = vec!["review".to_string(), "model-card".to_string()];
        let names = |input: &str| menu_matches(input, &skills).iter().map(|(n, _)| *n).collect::<Vec<_>>();
        assert_eq!(names("/").len(), COMMANDS.len() + 2);
        assert_eq!(names("/re"), ["resume", "rename", "rewind", "review"]);
        assert_eq!(names("/model"), ["model", "model-card"], "an exact match comes first");
        assert!(names("/model x").is_empty(), "arguments are being typed");
        assert!(names("hello").is_empty());
        assert!(names("/nope").is_empty());
    }
}
