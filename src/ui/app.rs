// The app's state, and what it shows: the scrollback (what has been said and
// done, kept so it can be drawn again at a new width) and the live area at
// the bottom (the reply as it streams, prompts, the input box, the footer).

use super::commands::{MENU_ROWS, menu_matches};
use super::history::{Block, History};
use super::input::Input;
use super::keys::{QUIT_WINDOW, SETTINGS, current_setting};
use super::md::Markdown;
use super::term::Term;
use super::text::{BOLD_ORANGE, CYAN, DIM, GREEN, ORANGE, RED, RESET, span};
use super::{HISTORY_LINES, paste, table, text, view};
use crate::agent::{self, Answer, Mode};
use std::collections::VecDeque;
use std::sync::mpsc::Sender;
use std::time::Instant;

/// Text streaming in, not yet complete lines.
#[derive(Default)]
pub(super) struct Stream {
    pub(super) text: String,
    pub(super) thinking: String,
    pub(super) md: Markdown,
    /// Rows of a table still coming in; drawn when the table ends.
    pub(super) table: Vec<String>,
    /// The next line starts a new block and gets the ● marker.
    pub(super) block_start: bool,
}

/// The turn in progress.
pub(super) struct Work {
    pub(super) since: Instant,
    pub(super) activity: String,
    /// Characters of reply streamed so far, for a rough token count.
    pub(super) streamed: usize,
    /// The tool call being written, and how much of its input has arrived.
    pub(super) tool: String,
    pub(super) tool_bytes: usize,
}

impl Work {
    pub(super) fn new() -> Work {
        Work { since: Instant::now(), activity: "listening".into(), streamed: 0, tool: String::new(), tool_bytes: 0 }
    }
}

/// Something waiting on the user. Several can queue up when subagents work
/// in parallel; they're shown one at a time.
pub(super) enum Prompt {
    Approve {
        label: String,
        reply: Sender<Answer>,
    },
    Question {
        question: String,
        options: Vec<String>,
        reply: Sender<String>,
    },
    /// The API key, typed or pasted; shown masked.
    Key,
    /// /rewind: (turn, what you typed), newest first.
    Rewind(Vec<(usize, String)>),
    /// /resume: session ids and their descriptions, newest first.
    Resume(Vec<(String, String)>),
    /// /model with no name: the catalogue.
    Model,
    /// /settings.
    Settings,
}

pub(super) struct App {
    pub(super) agent: agent::Handle,
    pub(super) model: String,
    /// Providers from the config, to know each model's context size.
    pub(super) providers: crate::api::provider::Providers,
    pub(super) input: Input,
    pub(super) stream: Stream,
    /// Lines waiting to be written into scrollback.
    pub(super) commits: Vec<String>,
    /// The last screens' worth of what was written, to redraw after a resize.
    pub(super) history: History,
    /// Whether the last line written to scrollback was blank.
    pub(super) last_blank: bool,
    pub(super) work: Option<Work>,
    pub(super) prompts: VecDeque<Prompt>,
    /// The row picked in the `/` command list.
    pub(super) menu_at: usize,
    /// When the front prompt became the front one.
    pub(super) prompt_since: Instant,
    /// Messages typed while the agent was busy (or before a key), sent in order.
    pub(super) queued: VecDeque<String>,
    /// What ctrl+s put aside, to be typed again once the next message is sent.
    pub(super) stash: Option<String>,
    /// Big pastes, shown in the input as short labels.
    pub(super) pastes: paste::Pastes,
    pub(super) cost: f64,
    pub(super) context: u64,
    /// The session's name, set with /rename.
    pub(super) name: Option<String>,
    pub(super) skills: Vec<String>,
    pub(super) mcp: Vec<String>,
    /// Whether the agent has an API key (from the environment or /login).
    pub(super) has_key: bool,
    /// Effort level chosen with /effort or --effort; None is the model's default.
    pub(super) effort: Option<String>,
    /// Subagents running now: (id, task, latest step). Shown under the input.
    pub(super) subagents: Vec<(usize, String, String)>,
    /// /clear asked for the terminal to be wiped before the next draw.
    pub(super) clear_screen: bool,
    pub(super) started: Instant,
    pub(super) quit_armed: Option<Instant>,
    pub(super) quit: bool,
    pub(super) cols: usize,
    pub(super) rows: usize,
}

impl App {
    pub(super) fn new(agent: agent::Handle, model: String) -> App {
        let (cols, rows) = Term::size();
        App {
            agent,
            model,
            providers: Default::default(),
            input: Input::default(),
            stream: Stream { block_start: true, ..Stream::default() },
            commits: Vec::new(),
            history: History::default(),
            last_blank: true,
            work: None,
            prompts: VecDeque::new(),
            menu_at: 0,
            prompt_since: Instant::now(),
            queued: VecDeque::new(),
            stash: None,
            pastes: paste::Pastes::default(),
            cost: 0.0,
            context: 0,
            name: None,
            skills: Vec::new(),
            mcp: Vec::new(),
            has_key: true,
            effort: None,
            subagents: Vec::new(),
            clear_screen: false,
            started: Instant::now(),
            quit_armed: None,
            quit: false,
            cols,
            rows,
        }
    }

    pub(super) fn header(&mut self) {
        let cwd = std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default();
        let home = crate::store::config::home().display().to_string();
        let cwd = match cwd.strip_prefix(&home) {
            Some(rest) if !home.is_empty() => format!("~{rest}"),
            _ => cwd,
        };
        let version = env!("CARGO_PKG_VERSION");
        let sandbox = if crate::sys::sandbox::active() {
            format!("{ORANGE}sandboxed:{RESET} {DIM}writes only inside {cwd}{RESET}")
        } else {
            String::new()
        };
        let text = [
            format!("{BOLD_ORANGE}claude-jr{RESET} {DIM}{version}{RESET}"),
            format!("{DIM}{} · {cwd}{RESET}", self.model),
            format!("{DIM}/help for commands · shift+tab to change mode{RESET}"),
            sandbox,
        ];
        // A blank line between the shell prompt and the mascot, then the
        // mascot with the text beside its middle rows.
        let art = view::mascot();
        let offset = art.len().saturating_sub(text.len()) / 2;
        let mut lines = vec![String::new()];
        for (i, row) in art.iter().enumerate() {
            let words = i.checked_sub(offset).and_then(|k| text.get(k)).map_or("", String::as_str);
            lines.push(format!(" {row}   {words}"));
        }
        lines.push(String::new());
        self.add(Block::Lines(lines));
    }

    // ---- scrollback -----------------------------------------------------

    /// Draws a block into scrollback and remembers it for redrawing after a
    /// resize.
    pub(super) fn add(&mut self, block: Block) {
        let before = self.commits.len();
        block.render(self.cols, &mut self.commits);
        let lines = self.commits.len() - before;
        if let Some(last) = self.commits[before..].last() {
            self.last_blank = text::is_blank(last);
        }
        self.history.push(block, lines, HISTORY_LINES.max(self.rows * 2));
    }

    /// A blank line before a new block, unless there already is one.
    pub(super) fn gap(&mut self) {
        if !self.last_blank {
            self.add(Block::Blank);
        }
    }

    /// Whether the next line starts a block (and so gets the ● marker, after
    /// a blank line).
    pub(super) fn take_lead(&mut self) -> bool {
        let start = std::mem::take(&mut self.stream.block_start);
        if start {
            self.gap();
        }
        start
    }

    pub(super) fn commit_text_line(&mut self, line: &str) {
        if !self.stream.md.in_code() && table::is_row(line) {
            return self.stream.table.push(line.to_string());
        }
        self.commit_table();
        let lead = self.take_lead();
        let md = self.stream.md;
        let line = text::clean(line);
        self.stream.md = md.after(&line);
        self.add(Block::Text { line, lead, md, more: false });
    }

    pub(super) fn commit_thinking_line(&mut self, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        if std::mem::take(&mut self.stream.block_start) {
            self.gap();
        }
        self.add(Block::Thinking(text::clean(line)));
    }

    /// The terminal changed size: the screen is cleared and the kept history
    /// drawn again at the new width.
    pub(super) fn redraw_history(&mut self) {
        // The rows of a streaming paragraph that were already written out
        // were cut for the old width: lay the paragraph out again whole.
        if !self.stream.text.is_empty()
            && let Some((line, lead, md)) = self.history.take_partial()
        {
            self.stream.text = format!("{line} {}", self.stream.text);
            self.stream.block_start |= lead;
            self.stream.md = md;
        }
        self.commits = self.history.render(self.cols);
        self.last_blank = self.commits.last().is_none_or(|l| text::is_blank(l));
    }

    /// A long paragraph that's still streaming would be re-wrapped and
    /// redrawn in full on every new piece. Rows that can't change any more go
    /// to scrollback instead, leaving the last two live. Only done when the
    /// result looks exactly the same as rendering the whole paragraph at once.
    pub(super) fn commit_finished_rows(&mut self) {
        const KEEP: usize = 2;
        let text = &self.stream.text;
        if text.len() < self.cols * (KEEP + 1) || !self.stream.md.is_paragraph(text) {
            return;
        }
        let lead = if self.stream.block_start { span("● ", ORANGE) } else { span("  ", "") };
        let render = |s: &str| {
            let mut out = Vec::new();
            let mut md = self.stream.md;
            md.line(&text::clean(s), self.cols, lead, &mut out);
            out
        };
        let full = render(text);
        if full.len() <= KEEP {
            return;
        }
        let target = full.len() - KEEP;

        // The longest prefix, cut at a space, that renders to `target` rows.
        let spaces: Vec<usize> = text.match_indices(' ').map(|(i, _)| i).collect();
        let fits = |&i: &usize| render(&text[..i]).len() <= target;
        let Some(&cut) = spaces.get(spaces.partition_point(fits).wrapping_sub(1)) else { return };
        let head = &text[..cut];
        // An unclosed `code` or **bold** would render differently in two halves.
        if head.matches('`').count() % 2 == 1 || head.matches("**").count() % 2 == 1 {
            return;
        }
        let rows = render(head);
        if rows.len() != target || rows[..] != full[..target] {
            return;
        }

        let (head, rest) = (head.to_string(), text[cut..].trim_start().to_string());
        let md = self.stream.md;
        let lead = self.take_lead(); // the blank line and ● marker, as for any committed line
        self.add(Block::Text { line: text::clean(&head), lead, md, more: true });
        self.stream.text = rest;
    }

    /// Draws a finished table into scrollback (as plain lines if it can't fit).
    pub(super) fn commit_table(&mut self) {
        if self.stream.table.is_empty() {
            return;
        }
        let rows = std::mem::take(&mut self.stream.table);
        let lead = self.take_lead();
        self.add(Block::Table { rows, lead, md: self.stream.md });
    }

    /// Commits any partial line still being streamed.
    pub(super) fn flush(&mut self) {
        let thinking = std::mem::take(&mut self.stream.thinking);
        if !thinking.is_empty() {
            self.commit_thinking_line(&thinking);
        }
        let text = std::mem::take(&mut self.stream.text);
        if !text.is_empty() {
            self.commit_text_line(&text);
        }
        self.commit_table();
    }

    pub(super) fn note(&mut self, msg: &str, style: &'static str) {
        self.gap();
        self.add(Block::Note { msg: text::clean(msg), style });
    }

    pub(super) fn set_activity(&mut self, activity: String) {
        if let Some(w) = &mut self.work {
            w.activity = activity;
        }
    }

    // ---- live area ------------------------------------------------------

    /// The bottom area: partial streaming lines, status, prompt or input box,
    /// footer. Returns its lines and the cursor position.
    pub(super) fn live(&self) -> (Vec<String>, Option<(usize, usize)>) {
        let cols = self.cols;
        let mut out = Vec::new();
        if !self.stream.thinking.is_empty() {
            view::thinking(&text::clean(&self.stream.thinking), cols, &mut out);
        }
        if !self.stream.table.is_empty() {
            let lead = if self.stream.block_start { span("● ", ORANGE) } else { span("  ", "") };
            table::render(&self.stream.table, cols, lead, &mut out);
        }
        if !self.stream.text.is_empty() {
            let lead = if self.stream.block_start && self.stream.table.is_empty() {
                span("● ", ORANGE)
            } else {
                span("  ", "")
            };
            let mut md = self.stream.md;
            md.line(&text::clean(&self.stream.text), cols, lead, &mut out);
        }
        if let Some(w) = self.work.as_ref().filter(|_| self.prompts.is_empty()) {
            if !out.is_empty() || !self.last_blank {
                out.push(String::new());
            }
            out.push(view::status(&w.activity, w.since.elapsed(), w.streamed / 4, cols));
        }
        out.push(String::new());

        let cursor = match self.prompts.front() {
            Some(Prompt::Approve { label, .. }) => {
                view::approve(label, cols, &mut out);
                None
            }
            Some(Prompt::Question { question, options, .. }) => {
                view::question(question, options, cols, &mut out);
                Some(view::input_box(&self.input, "type an answer, or an option's number, then enter", cols, &mut out))
            }
            Some(Prompt::Rewind(points)) => {
                let items: Vec<String> =
                    points.iter().map(|(_, p)| p.lines().next().unwrap_or("").to_string()).collect();
                let hint = "press a number to go back to before that message · esc to cancel";
                view::picker("Rewind: restores the files the agent changed since then", &items, hint, cols, &mut out);
                None
            }
            Some(Prompt::Resume(sessions)) => {
                let items: Vec<String> = sessions.iter().map(|(_, d)| d.clone()).collect();
                view::picker("Resume a conversation", &items, "press a number · esc to cancel", cols, &mut out);
                None
            }
            Some(Prompt::Model) => {
                let items: Vec<String> = crate::api::CATALOGUE
                    .iter()
                    .map(|(m, what)| {
                        let m = crate::api::cloud::same_door(&self.model, m);
                        let here = if m == self.model { "  (current)" } else { "" };
                        format!("{m}{here} · {} · {what}", crate::api::describe(&m))
                    })
                    .collect();
                let hint = "price per million tokens in/out · press a number · /model <id> for others · esc to cancel";
                view::picker("Choose a model", &items, hint, cols, &mut out);
                None
            }
            Some(Prompt::Settings) => {
                let items: Vec<String> =
                    SETTINGS.iter().map(|s| format!("{}: {}", s.label, current_setting(s))).collect();
                let hint =
                    "press a number to change it · effort applies now, the rest from the next start · esc to close";
                view::picker("Settings (~/.config/claude-jr/config.json)", &items, hint, cols, &mut out);
                None
            }
            Some(Prompt::Key) => {
                view::key_prompt(cols, &mut out);
                Some(view::input_box(&self.input.masked(), "sk-ant-…", cols, &mut out))
            }
            None => {
                let hint = if self.stash.is_some() {
                    "ctrl+s brings back what you stashed"
                } else if self.work.is_some() {
                    "type to queue a message"
                } else {
                    "ask anything, or /help"
                };
                Some(view::input_box(&self.input, hint, cols, &mut out))
            }
        };
        if self.prompts.is_empty() {
            view::menu(&menu_matches(self.input.text(), &self.skills), self.menu_at, MENU_ROWS, cols, &mut out);
        }
        self.panel(&mut out);
        out.push(self.footer());

        // Never taller than the screen, or the cursor math breaks.
        let max = self.rows.max(2) - 1;
        let extra = out.len().saturating_sub(max);
        out.drain(..extra);
        (out, cursor.map(|(r, c)| (r.saturating_sub(extra), c)))
    }

    /// Subagents and background jobs that are running right now.
    pub(super) fn panel(&self, out: &mut Vec<String>) {
        const MAX: usize = 4;
        let jobs = self.agent.tools.get().map(|t| t.jobs()).unwrap_or_default();
        let mut lines: Vec<String> = self
            .subagents
            .iter()
            .map(|(_, task, step)| {
                let (task, step) = (text::clean(task), text::clean(step));
                format!(" {ORANGE}◆{RESET} {task}{DIM} · {step}{RESET}")
            })
            .collect();
        lines.extend(jobs.iter().filter(|j| j.2).map(|(id, cmd, _)| {
            let cmd = text::clean(cmd);
            format!(" {GREEN}▶{RESET} job {id}{DIM} · {}{RESET}", cmd.lines().next().unwrap_or(""))
        }));
        let more = lines.len().saturating_sub(MAX);
        for line in lines.into_iter().take(MAX) {
            // Cut by visible width: escape codes don't take up columns.
            out.push(text::fit_styled(&line, self.cols.saturating_sub(1)));
        }
        if more > 0 {
            out.push(format!("   {DIM}… {more} more (/jobs){RESET}"));
        }
    }

    pub(super) fn footer(&self) -> String {
        let queued;
        let mut left = Vec::new();
        if self.quit_armed.is_some_and(|t| t.elapsed() < QUIT_WINDOW) {
            left.push(span("press ctrl-c again to quit", DIM));
        } else if let Some(q) = self.queued.front() {
            let more =
                if self.queued.len() > 1 { format!(" (+{} more)", self.queued.len() - 1) } else { String::new() };
            queued = format!("queued: {}{more}", q.lines().next().unwrap_or(""));
            left.push(span(&queued, DIM));
        } else {
            match self.agent.mode.get() {
                Mode::Default => {}
                Mode::AcceptEdits => left.push(span("⏵⏵ accept edits", ORANGE)),
                Mode::Plan => left.push(span("⏸ plan mode", CYAN)),
                Mode::Auto => left.push(span("◆ auto mode", GREEN)),
                Mode::Bypass => left.push(span("⏵⏵ skip permissions", RED)),
            }
            let hint = if self.work.is_some() { "esc to stop" } else { "shift+tab to change mode" };
            if !left.is_empty() {
                left.push(span(" · ", DIM));
            }
            left.push(span(hint, DIM));
        }

        let window = crate::api::window(&self.model, &self.providers);
        let mut right = Vec::new();
        if crate::sys::sandbox::active() {
            right.push("sandbox".to_string());
        }
        if let Some(n) = &self.name {
            right.push(n.clone());
        }
        right.push(match &self.effort {
            Some(e) => format!("{} {e}", self.model),
            None => self.model.clone(),
        });
        right.push(format!("{}/{}", tokens(self.context), tokens(window)));
        right.push(format!("${:.2}", self.cost));
        let jobs = self.agent.tools.get().map_or(0, |t| t.running_jobs());
        if jobs > 0 {
            right.push(format!("{jobs} job{}", if jobs == 1 { "" } else { "s" }));
        }
        view::footer(&left, &right.join(" · "), self.cols)
    }
}

pub(super) fn size(bytes: usize) -> String {
    if bytes < 1024 { format!("{bytes} B") } else { format!("{:.1} KB", bytes as f64 / 1024.0) }
}

/// 90 -> "1 min ago", 7200 -> "2 h ago".
pub(super) fn ago(secs: u64) -> String {
    match secs {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86_400 => format!("{} h ago", secs / 3600),
        _ => format!("{} days ago", secs / 86_400),
    }
}

/// 950 -> "950", 12_400 -> "12k", 1_000_000 -> "1M".
pub(super) fn tokens(n: u64) -> String {
    match n {
        0..1000 => n.to_string(),
        1000..1_000_000 => format!("{}k", n / 1000),
        _ => format!("{}M", n / 1_000_000),
    }
}
