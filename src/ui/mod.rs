// The interactive terminal UI.
//
// Two threads feed one channel: the agent (events) and a key reader (terminal
// input). The loop blocks on that channel, so an idle session uses no CPU.
// While the agent works it also wakes every 300 ms for the status dot, and
// the terminal is only written to when the picture actually changed.

mod history;
mod input;
mod md;
mod stats;
mod table;
mod term;
mod text;
mod view;

use crate::agent::{self, Answer, Command, Event, Mode};
use crossterm::event::{self as ct, Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use history::{Block, History};
use input::Input;
use md::Markdown;
use std::collections::VecDeque;
use std::io;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};
use term::Term;
use text::{BOLD_ORANGE, CYAN, DIM, GREEN, ORANGE, RED, RESET, span};

/// How often the status dot moves while the agent works.
const TICK: Duration = Duration::from_millis(300);
/// The fastest the screen is redrawn for streamed text (about 30 frames/s).
/// Key presses are drawn at once.
const FRAME: Duration = Duration::from_millis(33);
const QUIT_WINDOW: Duration = Duration::from_millis(1500);
/// How long an approval prompt ignores answer keys after it appears.
const APPROVE_GUARD: Duration = Duration::from_millis(400);

/// Keys pressed this soon after an approval appears were typed for something else.
fn armed(since: Instant, now: Instant) -> bool {
    now.duration_since(since) >= APPROVE_GUARD
}
/// Lines of history kept to redraw after a resize.
const HISTORY_LINES: usize = 1000;
/// A resize is drawn once the size has stopped changing for this long:
/// dragging a window edge or zooming sends a burst of them.
const RESIZE_SETTLE: Duration = Duration::from_millis(60);

const HELP: &str = "/clear, /new    start a new conversation on a clean screen
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
/cost           tokens and cost so far
/exit           quit
#note           save a note to ./AGENTS.md (used from now on)
!command        run a shell command yourself; the agent sees the output
images          drag a file in (its path is attached), or ctrl+v a screenshot

type / to list commands: up/down pick, tab completes, enter runs
enter sends · ctrl-j, shift-enter or \\ then enter for a new line
shift+tab: normal → accept edits → plan → auto → skip permissions
esc stops · ctrl-c clears the input, twice quits · up/down for history";

/// The commands, shown in a list as soon as `/` is typed.
const COMMANDS: &[(&str, &str)] = &[
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
    ("exit", "quit"),
];
/// How many of them are shown at once.
const MENU_ROWS: usize = 8;

/// The commands and skills that start with what's typed after the `/`, an
/// exact match first. Empty unless `input` is a `/` and part of one word.
fn menu_matches<'a>(input: &str, skills: &'a [String]) -> Vec<(&'a str, &'a str)> {
    let Some(typed) = input.strip_prefix('/') else { return Vec::new() };
    if typed.contains(char::is_whitespace) {
        return Vec::new();
    }
    let all = COMMANDS.iter().copied().chain(skills.iter().map(|s| (s.as_str(), "skill")));
    let mut found: Vec<(&str, &str)> = all.filter(|(name, _)| name.starts_with(typed)).collect();
    found.sort_by_key(|(name, _)| *name != typed);
    found
}

enum Msg {
    Agent(Event),
    Term(TermEvent),
}

/// `bottom`: keep the input on the last rows of the screen.
pub fn run(cfg: agent::Config, bottom: bool) -> io::Result<()> {
    term::restore_on_panic();
    let model = cfg.model.clone();
    let effort = cfg.effort.clone();
    let (tx, rx) = mpsc::channel();
    let to_ui = tx.clone();
    let agent = agent::spawn(cfg, move |e| {
        let _ = to_ui.send(Msg::Agent(e));
    });

    let mut term = Term::new(bottom)?;
    crate::signals::watch(agent.tools.clone(), true);
    let _ = thread::Builder::new().name("keys".into()).spawn(move || {
        while let Ok(e) = ct::read() {
            if tx.send(Msg::Term(e)).is_err() {
                break;
            }
        }
    });

    let mut app = App::new(agent, model);
    app.effort = effort;
    app.header();
    // As if the last frame were long ago, so the first one draws at once.
    let mut last_draw = Instant::now().checked_sub(FRAME).unwrap_or_else(Instant::now);
    let mut pending = false;
    // When the terminal last changed size, until that has been drawn.
    let mut resized_at: Option<Instant> = None;
    loop {
        // With a frame held back, sleep until it's due and take everything
        // that arrived meanwhile in one go: streamed text then costs ~30
        // wakeups a second, however small the pieces. Otherwise block until
        // something happens, waking for the status dot while working.
        let msg = if let Some(at) = resized_at {
            match rx.recv_timeout(RESIZE_SETTLE.saturating_sub(at.elapsed())) {
                Ok(m) => Some(m),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        } else if pending {
            thread::sleep(FRAME.saturating_sub(last_draw.elapsed()));
            None
        } else {
            let wait = if app.work.is_some() { Some(TICK) } else { None };
            match wait.map(|t| rx.recv_timeout(t)) {
                Some(Ok(m)) => Some(m),
                Some(Err(RecvTimeoutError::Timeout)) => None,
                Some(Err(RecvTimeoutError::Disconnected)) => break,
                None => match rx.recv() {
                    Ok(m) => Some(m),
                    Err(_) => break,
                },
            }
        };
        // Handle everything that's waiting, then draw once.
        let mut key = false;
        for m in msg.into_iter().chain(rx.try_iter()) {
            match m {
                Msg::Term(TermEvent::Resize(_, rows)) => {
                    term.resize(rows as usize);
                    resized_at = Some(Instant::now());
                }
                Msg::Term(_) => key = true,
                Msg::Agent(_) => {}
            }
            app.handle(m);
        }
        if app.quit {
            break;
        }
        // Nothing is drawn while the size is still changing: every frame
        // would be rewrapped by the terminal a moment later.
        if resized_at.is_some_and(|at| at.elapsed() < RESIZE_SETTLE) {
            continue;
        }
        if !key && resized_at.is_none() && last_draw.elapsed() < FRAME {
            pending = true;
            continue;
        }
        if std::mem::take(&mut app.clear_screen) {
            term.clear(true);
            app.history.clear();
        } else if resized_at.take().is_some() {
            // The terminal keeps an old-width copy in its scrollback; wipe
            // that too and draw the kept history again at the new width.
            term.clear(true);
            app.redraw_history();
        }
        let (live, cursor) = app.live();
        term.draw(&mut app.commits, live, cursor)?;
        last_draw = Instant::now();
        pending = false;
    }
    if let Some(tools) = app.agent.tools.get() {
        tools.shutdown();
    }
    Ok(())
}

/// Text streaming in, not yet complete lines.
#[derive(Default)]
struct Stream {
    text: String,
    thinking: String,
    md: Markdown,
    /// Rows of a table still coming in; drawn when the table ends.
    table: Vec<String>,
    /// The next line starts a new block and gets the ● marker.
    block_start: bool,
}

/// The turn in progress.
struct Work {
    since: Instant,
    activity: String,
    /// Characters of reply streamed so far, for a rough token count.
    streamed: usize,
    /// The tool call being written, and how much of its input has arrived.
    tool: String,
    tool_bytes: usize,
}

impl Work {
    fn new() -> Work {
        Work { since: Instant::now(), activity: "listening".into(), streamed: 0, tool: String::new(), tool_bytes: 0 }
    }
}

/// Something waiting on the user. Several can queue up when subagents work
/// in parallel; they're shown one at a time.
enum Prompt {
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

/// Puts the terminal back to normal, for exits that skip `Term`'s cleanup.
pub fn restore_terminal() {
    term::restore();
}

struct App {
    agent: agent::Handle,
    model: String,
    input: Input,
    stream: Stream,
    /// Lines waiting to be written into scrollback.
    commits: Vec<String>,
    /// The last screens' worth of what was written, to redraw after a resize.
    history: History,
    /// Whether the last line written to scrollback was blank.
    last_blank: bool,
    work: Option<Work>,
    prompts: VecDeque<Prompt>,
    /// The row picked in the `/` command list.
    menu_at: usize,
    /// When the front prompt became the front one.
    prompt_since: Instant,
    /// Messages typed while the agent was busy (or before a key), sent in order.
    queued: VecDeque<String>,
    cost: f64,
    context: u64,
    /// The session's name, set with /rename.
    name: Option<String>,
    skills: Vec<String>,
    mcp: Vec<String>,
    /// Whether the agent has an API key (from the environment or /login).
    has_key: bool,
    /// Effort level chosen with /effort or --effort; None is the model's default.
    effort: Option<String>,
    /// Subagents running now: (id, task, latest step). Shown under the input.
    subagents: Vec<(usize, String, String)>,
    /// /clear asked for the terminal to be wiped before the next draw.
    clear_screen: bool,
    started: Instant,
    quit_armed: Option<Instant>,
    quit: bool,
    cols: usize,
    rows: usize,
}

impl App {
    fn new(agent: agent::Handle, model: String) -> App {
        let (cols, rows) = Term::size();
        App {
            agent,
            model,
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

    fn header(&mut self) {
        let cwd = std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default();
        let home = std::env::var("HOME").unwrap_or_default();
        let cwd = match cwd.strip_prefix(&home) {
            Some(rest) if !home.is_empty() => format!("~{rest}"),
            _ => cwd,
        };
        let version = env!("CARGO_PKG_VERSION");
        let sandbox = if crate::sandbox::active() {
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
    /// resize (only the last couple of screens' worth is kept).
    fn add(&mut self, block: Block) {
        let before = self.commits.len();
        block.render(self.cols, &mut self.commits);
        let lines = self.commits.len() - before;
        if let Some(last) = self.commits[before..].last() {
            self.last_blank = text::is_blank(last);
        }
        self.history.push(block, lines, HISTORY_LINES.max(self.rows * 2));
    }

    /// A blank line before a new block, unless there already is one.
    fn gap(&mut self) {
        if !self.last_blank {
            self.add(Block::Blank);
        }
    }

    /// Whether the next line starts a block (and so gets the ● marker, after
    /// a blank line).
    fn take_lead(&mut self) -> bool {
        let start = std::mem::take(&mut self.stream.block_start);
        if start {
            self.gap();
        }
        start
    }

    fn commit_text_line(&mut self, line: &str) {
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

    fn commit_thinking_line(&mut self, line: &str) {
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
    fn redraw_history(&mut self) {
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
    fn commit_finished_rows(&mut self) {
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
    fn commit_table(&mut self) {
        if self.stream.table.is_empty() {
            return;
        }
        let rows = std::mem::take(&mut self.stream.table);
        let lead = self.take_lead();
        self.add(Block::Table { rows, lead, md: self.stream.md });
    }

    /// Commits any partial line still being streamed.
    fn flush(&mut self) {
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

    fn note(&mut self, msg: &str, style: &'static str) {
        self.gap();
        self.add(Block::Note { msg: msg.to_string(), style });
    }

    fn set_activity(&mut self, activity: String) {
        if let Some(w) = &mut self.work {
            w.activity = activity;
        }
    }

    // ---- events ---------------------------------------------------------

    fn handle(&mut self, m: Msg) {
        let was_approve = matches!(self.prompts.front(), Some(Prompt::Approve { .. }));
        self.dispatch(m);
        if !was_approve && matches!(self.prompts.front(), Some(Prompt::Approve { .. })) {
            self.prompt_since = Instant::now();
        }
    }

    fn dispatch(&mut self, m: Msg) {
        match m {
            Msg::Agent(e) => self.on_agent(e),
            Msg::Term(TermEvent::Key(k)) => self.on_key(k),
            Msg::Term(TermEvent::Paste(s)) if !matches!(self.prompts.front(), Some(Prompt::Approve { .. })) => {
                self.input.insert(&s.replace("\r\n", "\n").replace('\r', "\n"));
            }
            Msg::Term(TermEvent::Resize(c, r)) => {
                (self.cols, self.rows) = (c as usize, r as usize);
            }
            Msg::Term(_) => {}
        }
    }

    fn on_agent(&mut self, e: Event) {
        match e {
            Event::Ready { resumed, name, skills, mcp, has_key } => {
                self.has_key = has_key;
                if !has_key {
                    self.prompts.push_back(Prompt::Key);
                }
                if resumed > 0 {
                    let which = name.as_deref().map_or("the last session".to_string(), |n| format!("\"{n}\""));
                    self.note(&format!("Resumed {which} ({resumed} messages)."), DIM);
                }
                let mut loaded = Vec::new();
                if !skills.is_empty() {
                    loaded.push(format!("{} skill{}", skills.len(), if skills.len() == 1 { "" } else { "s" }));
                }
                if !mcp.is_empty() {
                    loaded.push(format!("MCP: {}", mcp.join(", ")));
                }
                if !loaded.is_empty() {
                    self.note(&format!("Loaded {}.", loaded.join(" · ")), DIM);
                }
                (self.name, self.skills, self.mcp) = (name, skills, mcp);
            }
            Event::Notice(msg) => self.note(&msg, DIM),
            Event::SubStart { id, task } => self.subagents.push((id, task, "starting".into())),
            Event::SubStep { id, label } => {
                if let Some(s) = self.subagents.iter_mut().find(|s| s.0 == id) {
                    s.2 = label;
                }
            }
            Event::SubEnd { id } => self.subagents.retain(|s| s.0 != id),
            Event::RewindPoints(points) if points.is_empty() => self.note("Nothing to rewind to yet.", DIM),
            Event::RewindPoints(points) => self.prompts.push_front(Prompt::Rewind(points)),
            Event::Resumed { name, messages, recap } => {
                let which = name.as_deref().map_or("the conversation".to_string(), |n| format!("\"{n}\""));
                self.note(&format!("Resumed {which} ({messages} messages)."), DIM);
                if let Some((prompt, reply)) = recap {
                    self.gap();
                    self.add(Block::User(text::clean(&prompt)));
                    let short: Vec<&str> = reply.lines().filter(|l| !l.trim().is_empty()).take(6).collect();
                    self.stream.block_start = true;
                    for line in short {
                        self.commit_text_line(line);
                    }
                    self.stream.md.reset();
                }
                (self.name, self.context) = (name, 0);
            }
            Event::Rewound { prompt, files } => {
                let restored = match files {
                    0 => "No files needed restoring".to_string(),
                    1 => "Restored 1 file".to_string(),
                    n => format!("Restored {n} files"),
                };
                let first = prompt.lines().next().unwrap_or("");
                self.note(
                    &format!(
                        "Rewound to before \"{first}\". {restored}; changes made by shell commands aren't undone."
                    ),
                    DIM,
                );
                // Your message is back in the input box to edit and send again.
                self.input.clear();
                self.input.insert(&prompt);
            }
            Event::Text(t) => {
                if let Some(w) = &mut self.work {
                    w.streamed += t.len();
                    w.activity = "writing".into();
                }
                self.stream.text.push_str(&t);
                while let Some(i) = self.stream.text.find('\n') {
                    let line: String = self.stream.text.drain(..=i).collect();
                    self.commit_text_line(line.trim_end_matches('\n'));
                }
                self.commit_finished_rows();
            }
            Event::Thinking(t) => {
                self.set_activity("thinking".into());
                self.stream.thinking.push_str(&t);
                while let Some(i) = self.stream.thinking.find('\n') {
                    let line: String = self.stream.thinking.drain(..=i).collect();
                    self.commit_thinking_line(line.trim_end_matches('\n'));
                }
            }
            Event::BlockEnd => {
                self.flush();
                self.stream.block_start = true;
            }
            Event::ToolStart(name) => {
                if let Some(w) = &mut self.work {
                    w.activity = format!("preparing {name}");
                    w.tool = name;
                    w.tool_bytes = 0;
                }
            }
            Event::ToolInput(n) => {
                if let Some(w) = &mut self.work {
                    w.tool_bytes += n;
                    w.activity = format!("writing {} · {}", w.tool, size(w.tool_bytes));
                }
            }
            Event::Tool { label, diff } => {
                self.flush();
                self.gap();
                self.set_activity("running".into());
                // Only the changed lines are kept, not whole files.
                self.add(Block::Tool { label, diff: diff.as_ref().map(view::trim_diff) });
            }
            Event::Ask { label, reply } => self.prompts.push_back(Prompt::Approve { label, reply }),
            Event::Question { question, options, reply } => {
                self.prompts.push_back(Prompt::Question { question, options, reply });
            }
            Event::ToolResult { ok, summary } => {
                self.add(Block::Result { ok, summary });
                self.stream.block_start = true;
                self.set_activity("listening".into());
            }
            Event::Activity(a) => self.set_activity(a),
            Event::Usage { usage, cost, main } => {
                self.cost += cost.unwrap_or(0.0);
                if main {
                    self.context = usage.context();
                }
            }
            Event::Done => self.finish(None),
            Event::Cancelled => self.finish(Some(("Stopped.", DIM))),
            Event::Error(e) => self.finish(Some((&format!("Error: {e}"), RED))),
        }
    }

    fn finish(&mut self, note: Option<(&str, &'static str)>) {
        self.flush();
        self.stream.md.reset();
        self.work = None;
        self.subagents.clear();
        self.prompts.retain(|p| matches!(p, Prompt::Key));
        if let Some((msg, style)) = note {
            self.note(msg, style);
        }
        self.send_queued();
    }

    /// Sends the next queued message, if any.
    fn send_queued(&mut self) {
        if let Some(q) = self.queued.pop_front() {
            self.submit(q);
        }
    }

    // ---- keys -----------------------------------------------------------

    fn on_key(&mut self, k: KeyEvent) {
        if k.kind == KeyEventKind::Release {
            return;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match self.prompts.front() {
            Some(Prompt::Approve { .. }) => return self.on_approve_key(k),
            Some(Prompt::Question { .. }) => {
                if k.code == KeyCode::Esc || (ctrl && k.code == KeyCode::Char('c')) {
                    self.answer(String::new());
                    return self.cancel();
                }
            }
            Some(Prompt::Rewind(points)) => {
                match k.code {
                    KeyCode::Char(c @ '1'..='9') => {
                        if let Some(&(turn, _)) = points.get(c as usize - '1' as usize) {
                            self.prompts.pop_front();
                            let _ = self.agent.commands.send(Command::Rewind(Some(turn)));
                        }
                    }
                    KeyCode::Esc => {
                        self.prompts.pop_front();
                    }
                    _ => {}
                }
                return;
            }
            Some(Prompt::Resume(sessions)) => {
                match k.code {
                    KeyCode::Char(c @ '1'..='9') => {
                        if let Some((id, _)) = sessions.get(c as usize - '1' as usize) {
                            let pick = crate::session::Pick::Named(id.clone());
                            self.prompts.pop_front();
                            let _ = self.agent.commands.send(Command::Resume(pick));
                        }
                    }
                    KeyCode::Esc => {
                        self.prompts.pop_front();
                    }
                    _ => {}
                }
                return;
            }
            Some(Prompt::Model) => {
                match k.code {
                    KeyCode::Char(c @ '1'..='9') => {
                        if let Some((model, _)) = crate::api::CATALOGUE.get(c as usize - '1' as usize) {
                            self.prompts.pop_front();
                            self.set_model(model);
                        }
                    }
                    KeyCode::Esc => {
                        self.prompts.pop_front();
                    }
                    _ => {}
                }
                return;
            }
            Some(Prompt::Settings) => {
                match k.code {
                    KeyCode::Char(c @ '1'..='9') => self.change_setting(c as usize - '1' as usize),
                    KeyCode::Esc => {
                        self.prompts.pop_front();
                    }
                    _ => {}
                }
                return;
            }
            Some(Prompt::Key) => match k.code {
                KeyCode::Enter => return self.save_key(),
                KeyCode::Esc => {
                    self.prompts.pop_front();
                    self.input.clear();
                    return self.note("No key set. Type /login when you're ready.", DIM);
                }
                _ => {}
            },
            None => {}
        }

        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let text_cols = self.cols.saturating_sub(6);
        // With the `/` command list open: arrows pick, tab completes, enter runs.
        let menu = if self.prompts.is_empty() { menu_matches(self.input.text(), &self.skills) } else { Vec::new() };
        if !menu.is_empty() {
            let at = self.menu_at.min(menu.len() - 1);
            let picked = format!("/{}", menu[at].0);
            match k.code {
                KeyCode::Up => return self.menu_at = if at == 0 { menu.len() - 1 } else { at - 1 },
                KeyCode::Down => return self.menu_at = (at + 1) % menu.len(),
                KeyCode::Tab => {
                    self.input.clear();
                    self.input.insert(&format!("{picked} "));
                    return self.menu_at = 0;
                }
                KeyCode::Enter if !(shift || alt) => {
                    self.input.take();
                    self.menu_at = 0;
                    return self.submit(picked);
                }
                _ => self.menu_at = 0,
            }
        }
        match k.code {
            KeyCode::Char('c') if ctrl => self.ctrl_c(),
            KeyCode::Char('d') if ctrl && self.input.is_empty() => self.quit = true,
            KeyCode::Char('j') if ctrl => self.input.insert("\n"),
            KeyCode::Char('a') if ctrl => self.input.home(),
            KeyCode::Char('e') if ctrl => self.input.end(),
            KeyCode::Char('u') if ctrl => self.input.kill_to_start(),
            KeyCode::Char('k') if ctrl => self.input.kill_to_end(),
            KeyCode::Char('w') if ctrl => self.input.delete_word(),
            KeyCode::Char('v') if ctrl => match crate::images::from_clipboard() {
                Ok(path) => self.input.insert(&format!("{} ", path.display())),
                Err(e) => self.note(&format!("Can't paste an image: {e}."), DIM),
            },
            KeyCode::Backspace if ctrl || alt => self.input.delete_word(),
            KeyCode::Char(c) if !ctrl => {
                let mut buf = [0u8; 4];
                self.input.insert(c.encode_utf8(&mut buf));
            }
            KeyCode::Enter if shift || alt => self.input.insert("\n"),
            KeyCode::Enter => {
                if !self.input.continues_line() {
                    let text = self.input.take();
                    if matches!(self.prompts.front(), Some(Prompt::Question { .. })) {
                        self.answer(text);
                    } else if !text.trim().is_empty() {
                        self.submit(text);
                    }
                }
            }
            KeyCode::Esc => self.cancel(),
            KeyCode::BackTab => {
                let mode = &self.agent.mode;
                mode.set(mode.get().next());
            }
            KeyCode::Backspace => self.input.backspace(),
            KeyCode::Delete => self.input.delete(),
            KeyCode::Left => self.input.left(),
            KeyCode::Right => self.input.right(),
            KeyCode::Home => self.input.home(),
            KeyCode::End => self.input.end(),
            KeyCode::Up => self.input.up(text_cols),
            KeyCode::Down => self.input.down(text_cols),
            _ => {}
        }
    }

    fn on_approve_key(&mut self, k: KeyEvent) {
        let ctrl_c = k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL);
        let always = matches!(k.code, KeyCode::Esc | KeyCode::BackTab) || ctrl_c;
        if !always && !armed(self.prompt_since, Instant::now()) {
            return;
        }
        let answer = match k.code {
            KeyCode::Char('y' | 'Y' | '1') => Answer::Yes,
            KeyCode::Char('a' | 'A' | '2') => Answer::Always,
            KeyCode::Char('n' | 'N' | '3') | KeyCode::Esc => Answer::No,
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                self.cancel();
                Answer::No
            }
            KeyCode::BackTab => {
                let mode = &self.agent.mode;
                return mode.set(mode.get().next());
            }
            _ => return,
        };
        if let Some(Prompt::Approve { reply, .. }) = self.prompts.pop_front() {
            let _ = reply.send(answer);
        }
        self.prompt_since = Instant::now();
    }

    /// Answers the question at the front of the queue.
    /// Answers the question at the front of the queue. A number on its own
    /// picks that option; anything else is sent as typed.
    fn answer(&mut self, text: String) {
        if let Some(Prompt::Question { reply, options, .. }) = self.prompts.pop_front() {
            let picked = text.trim().parse::<usize>().ok().and_then(|n| options.get(n.wrapping_sub(1)));
            let _ = reply.send(picked.cloned().unwrap_or(text));
        }
    }

    /// Moves a setting to its next value and saves it.
    fn change_setting(&mut self, i: usize) {
        let Some(setting) = SETTINGS.get(i) else { return };
        let now = current_setting(setting);
        let at = setting.choices.iter().position(|c| *c == now).map_or(0, |p| p + 1);
        let next = setting.choices[at % setting.choices.len()];
        let value = match next {
            // The model's own default: no value in the file.
            "default" => serde_json::Value::Null,
            "on" => serde_json::Value::Bool(true),
            "off" => serde_json::Value::Bool(false),
            v => serde_json::Value::String(v.to_string()),
        };
        if let Err(e) = crate::config::set_user_setting(setting.key, value) {
            return self.note(&format!("Couldn't save the setting: {e}"), RED);
        }
        if setting.key == "effort" {
            self.set_effort(next);
        }
    }

    fn set_model(&mut self, model: &str) {
        self.model = model.to_string();
        let _ = self.agent.commands.send(Command::Model(self.model.clone()));
        self.note(&format!("Model set to {}. {}", self.model, crate::api::describe(model)), DIM);
    }

    /// /effort: how hard the model thinks. The API's default for the model is
    /// used until one is picked. Changing it mid-conversation means the next
    /// request misses the prompt cache once.
    fn set_effort(&mut self, arg: &str) {
        const LEVELS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
        match arg {
            "" => {
                let now = self.effort.as_deref().unwrap_or("the model's default");
                self.note(&format!("Effort: {now}. Choose with /effort {} or default.", LEVELS.join("|")), "");
            }
            "default" => {
                self.effort = None;
                let _ = self.agent.commands.send(Command::Effort(None));
                self.note("Effort set to the model's default.", DIM);
            }
            level if LEVELS.contains(&level) => {
                self.effort = Some(level.to_string());
                let _ = self.agent.commands.send(Command::Effort(self.effort.clone()));
                self.note(&format!("Effort set to {level}."), DIM);
            }
            other => self.note(&format!("Unknown effort \"{other}\". Use {} or default.", LEVELS.join(", ")), RED),
        }
    }

    /// Saves the key typed into the key prompt and hands it to the agent.
    fn save_key(&mut self) {
        let key = self.input.take_secret().trim().to_string();
        if key.is_empty() {
            return;
        }
        self.prompts.pop_front();
        match crate::config::save_api_key(&key) {
            Ok(path) => self.note(&format!("Saved your API key to {}.", path.display()), DIM),
            Err(e) => self.note(&format!("Couldn't save the key ({e}); it's used for this session only."), RED),
        }
        if !key.starts_with("sk-ant-") {
            self.note("That doesn't look like an Anthropic key (they start with sk-ant-). /login to change it.", RED);
        }
        let _ = self.agent.commands.send(Command::SetKey(key));
        self.has_key = true;
        self.send_queued();
    }

    fn ctrl_c(&mut self) {
        if !self.input.is_empty() {
            self.input.clear();
        } else if self.work.is_some() {
            self.cancel();
        } else if self.quit_armed.is_some_and(|t| t.elapsed() < QUIT_WINDOW) {
            self.quit = true;
        } else {
            self.quit_armed = Some(Instant::now());
        }
    }

    fn cancel(&mut self) {
        if self.work.is_some() {
            self.agent.cancel.store(true, Ordering::Relaxed);
            self.set_activity("stopping".into());
        }
    }

    // ---- input ----------------------------------------------------------

    fn submit(&mut self, text: String) {
        let trimmed = text.trim();
        if let Some(cmd) = trimmed.strip_prefix('/') {
            return self.command(cmd);
        }
        if let Some(note) = trimmed.strip_prefix('#').map(str::trim).filter(|n| !n.is_empty()) {
            return self.remember(note);
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
        let cmd = match shell {
            Some(shell) => Command::Shell(shell),
            None => Command::Send(text),
        };
        let _ = self.agent.commands.send(cmd);
    }

    fn remember(&mut self, note: &str) {
        match crate::prompt::remember(note) {
            Ok(path) => {
                let _ = self.agent.commands.send(Command::Remember(note.to_string()));
                self.note(&format!("Saved to {}.", path.display()), DIM);
            }
            Err(e) => self.note(&format!("Could not save the note: {e}"), RED),
        }
    }

    fn command(&mut self, cmd: &str) {
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
                let sessions: Vec<(String, String)> = crate::session::Session::list()
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
                let _ = crate::config::delete_api_key();
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

    // ---- live area ------------------------------------------------------

    /// The bottom area: partial streaming lines, status, prompt or input box,
    /// footer. Returns its lines and the cursor position.
    fn live(&self) -> (Vec<String>, Option<(usize, usize)>) {
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
                        let here = if *m == self.model { "  (current)" } else { "" };
                        format!("{m}{here} · {} · {what}", crate::api::describe(m))
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
                let hint = if self.work.is_some() { "type to queue a message" } else { "ask anything, or /help" };
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
    fn panel(&self, out: &mut Vec<String>) {
        const MAX: usize = 4;
        let jobs = self.agent.tools.get().map(|t| t.jobs()).unwrap_or_default();
        let mut lines: Vec<String> = self
            .subagents
            .iter()
            .map(|(_, task, step)| format!(" {ORANGE}◆{RESET} {task}{DIM} · {step}{RESET}"))
            .collect();
        lines.extend(jobs.iter().filter(|j| j.2).map(|(id, cmd, _)| {
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

    fn footer(&self) -> String {
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

        let window = crate::api::Caps::of(&self.model).window;
        let mut right = Vec::new();
        if crate::sandbox::active() {
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

fn size(bytes: usize) -> String {
    if bytes < 1024 { format!("{bytes} B") } else { format!("{:.1} KB", bytes as f64 / 1024.0) }
}

/// One line of /settings: the config key, and the values it cycles through
/// (the first is the default).
struct Setting {
    label: &'static str,
    key: &'static str,
    choices: &'static [&'static str],
}

const SETTINGS: [Setting; 6] = [
    Setting { label: "Input at the bottom of the screen", key: "layout", choices: &["bottom", "inline"] },
    Setting {
        label: "Model for new sessions",
        key: "model",
        choices: &["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5", "claude-fable-5-1"],
    },
    Setting { label: "Effort", key: "effort", choices: &["default", "low", "medium", "high", "xhigh", "max"] },
    Setting { label: "Start in mode", key: "mode", choices: &crate::agent::Mode::NAMES },
    Setting { label: "Web search and fetch", key: "web_tools", choices: &["on", "off"] },
    Setting { label: "Sandbox (writes only in the project)", key: "sandbox.enabled", choices: &["off", "on"] },
];

/// A setting's value in the user's config file, or its default.
fn current_setting(s: &Setting) -> &'static str {
    let root = crate::config::user_settings();
    let value = s.key.split('.').fold(&root, |node, part| &node[part]);
    let text = match value {
        serde_json::Value::Bool(true) => "on",
        serde_json::Value::Bool(false) => "off",
        serde_json::Value::String(v) => v.as_str(),
        _ => s.choices[0],
    };
    s.choices.iter().find(|c| **c == text).copied().unwrap_or(s.choices[0])
}

/// 90 -> "1 min ago", 7200 -> "2 h ago".
fn ago(secs: u64) -> String {
    match secs {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86_400 => format!("{} h ago", secs / 3600),
        _ => format!("{} days ago", secs / 86_400),
    }
}

/// 950 -> "950", 12_400 -> "12k", 1_000_000 -> "1M".
fn tokens(n: u64) -> String {
    match n {
        0..1000 => n.to_string(),
        1000..1_000_000 => format!("{}k", n / 1000),
        _ => format!("{}M", n / 1_000_000),
    }
}

#[cfg(test)]
mod guard_tests {
    use super::*;

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

    #[test]
    fn approval_keys_wait_for_the_guard() {
        let t = Instant::now();
        let at = |ms| t + Duration::from_millis(ms);
        assert!(!armed(t, at(0)));
        assert!(!armed(t, at(399)));
        assert!(armed(t, at(400)));
        assert!(armed(t, at(2000)));
    }
}
