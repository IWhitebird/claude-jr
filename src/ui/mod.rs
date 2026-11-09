// The interactive terminal UI.
//
// Two threads feed one channel: the agent (events) and a key reader (terminal
// input). The loop blocks on that channel, so an idle session uses no CPU.
// While the agent works it also wakes every 300 ms for the status dot, and
// the terminal is only written to when the picture actually changed.

mod input;
mod md;
mod stats;
mod term;
mod text;
mod view;

use crate::agent::{self, Answer, Command, Event, Mode};
use crossterm::event::{self as ct, Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use input::Input;
use md::Markdown;
use std::collections::VecDeque;
use std::io;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};
use term::Term;
use text::{BOLD_ORANGE, CYAN, DIM, GREEN, ORANGE, RED, RESET, Span, span};

/// How often the status dot moves while the agent works.
const TICK: Duration = Duration::from_millis(300);
const QUIT_WINDOW: Duration = Duration::from_millis(1500);

const HELP: &str = "/clear          start a new conversation
/model [name]   show or change the model
/rename [name]  name this session (resume with claude-jr -r NAME)
/memory         instruction files in use
/skills         available skills (run one with /name)
/mcp            connected MCP servers
/stats          memory and CPU used by claude-jr and what it started
/cost           tokens and cost so far
/exit           quit
#note           save a note to ./AGENTS.md (used from now on)
!command        run a shell command yourself; the agent sees the output

enter sends · ctrl-j, shift-enter or \\ then enter for a new line
shift+tab: normal → accept edits → plan → auto → skip permissions
esc stops · ctrl-c clears the input, twice quits · up/down for history";

enum Msg {
    Agent(Event),
    Term(TermEvent),
}

pub fn run(cfg: agent::Config) -> io::Result<()> {
    term::restore_on_panic();
    let model = cfg.model.clone();
    let (tx, rx) = mpsc::channel();
    let to_ui = tx.clone();
    let agent = agent::spawn(cfg, move |e| {
        let _ = to_ui.send(Msg::Agent(e));
    });

    let mut term = Term::new()?;
    crate::signals::watch(agent.tools.clone(), true);
    thread::spawn(move || {
        while let Ok(e) = ct::read() {
            if tx.send(Msg::Term(e)).is_err() {
                break;
            }
        }
    });

    let mut app = App::new(agent, model);
    app.header();
    loop {
        let msg = if app.work.is_some() {
            match rx.recv_timeout(TICK) {
                Ok(m) => Some(m),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match rx.recv() {
                Ok(m) => Some(m),
                Err(_) => break,
            }
        };
        // Handle everything that's waiting, then draw once.
        for m in msg.into_iter().chain(rx.try_iter()) {
            if let Msg::Term(TermEvent::Resize(..)) = m {
                term.invalidate();
            }
            app.handle(m);
        }
        if app.quit {
            break;
        }
        let (live, cursor) = app.live();
        term.draw(&mut app.commits, live, cursor)?;
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
    Approve { label: String, reply: Sender<Answer> },
    Question { question: String, options: Vec<String>, reply: Sender<String> },
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
    /// Whether the last line written to scrollback was blank.
    last_blank: bool,
    work: Option<Work>,
    prompts: VecDeque<Prompt>,
    queued: Option<String>,
    cost: f64,
    context: u64,
    /// The session's name, set with /rename.
    name: Option<String>,
    skills: Vec<String>,
    mcp: Vec<String>,
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
            last_blank: true,
            work: None,
            prompts: VecDeque::new(),
            queued: None,
            cost: 0.0,
            context: 0,
            name: None,
            skills: Vec::new(),
            mcp: Vec::new(),
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
        let [m0, m1, m2, m3] = view::mascot();
        let sandbox = if crate::sandbox::active() {
            format!("{ORANGE}sandboxed:{RESET} {DIM}writes only inside {cwd}{RESET}")
        } else {
            String::new()
        };
        self.commits.extend([
            format!(" {m0}  {BOLD_ORANGE}claude-jr{RESET} {DIM}{version}{RESET}"),
            format!(" {m1}  {DIM}{} · {cwd}{RESET}", self.model),
            format!(" {m2}  {DIM}/help for commands · shift+tab to change mode{RESET}"),
            format!(" {m3}  {sandbox}"),
            String::new(),
        ]);
        self.last_blank = true;
    }

    // ---- scrollback -----------------------------------------------------

    /// Runs a renderer that appends to scrollback, keeping `last_blank` right.
    fn emit(&mut self, f: impl FnOnce(usize, &mut Vec<String>)) {
        let before = self.commits.len();
        f(self.cols, &mut self.commits);
        if let Some(last) = self.commits[before..].last() {
            self.last_blank = text::is_blank(last);
        }
    }

    /// A blank line before a new block, unless there already is one.
    fn gap(&mut self) {
        if !self.last_blank {
            self.commits.push(String::new());
            self.last_blank = true;
        }
    }

    fn lead(&mut self) -> Span<'static> {
        if std::mem::take(&mut self.stream.block_start) {
            self.gap();
            span("● ", ORANGE)
        } else {
            span("  ", "")
        }
    }

    fn commit_text_line(&mut self, line: &str) {
        let lead = self.lead();
        let mut md = self.stream.md;
        self.emit(|cols, out| md.line(&text::clean(line), cols, lead, out));
        self.stream.md = md;
    }

    fn commit_thinking_line(&mut self, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        if std::mem::take(&mut self.stream.block_start) {
            self.gap();
        }
        self.emit(|cols, out| view::thinking(&text::clean(line), cols, out));
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
    }

    fn note(&mut self, msg: &str, style: &'static str) {
        self.gap();
        self.emit(|cols, out| view::note(msg, style, cols, out));
    }

    fn set_activity(&mut self, activity: String) {
        if let Some(w) = &mut self.work {
            w.activity = activity;
        }
    }

    // ---- events ---------------------------------------------------------

    fn handle(&mut self, m: Msg) {
        match m {
            Msg::Agent(e) => self.on_agent(e),
            Msg::Term(TermEvent::Key(k)) => self.on_key(k),
            Msg::Term(TermEvent::Paste(s)) if !matches!(self.prompts.front(), Some(Prompt::Approve { .. })) => {
                self.input.insert(&s.replace("\r\n", "\n").replace('\r', "\n"));
            }
            Msg::Term(TermEvent::Resize(c, r)) => (self.cols, self.rows) = (c as usize, r as usize),
            Msg::Term(_) => {}
        }
    }

    fn on_agent(&mut self, e: Event) {
        match e {
            Event::Ready { resumed, name, skills, mcp } => {
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
                self.emit(|cols, out| {
                    view::tool(&label, cols, out);
                    if let Some(d) = &diff {
                        view::diff(d, cols, out);
                    }
                });
            }
            Event::Ask { label, reply } => self.prompts.push_back(Prompt::Approve { label, reply }),
            Event::Question { question, options, reply } => {
                self.prompts.push_back(Prompt::Question { question, options, reply });
            }
            Event::ToolResult { ok, summary } => {
                self.emit(|cols, out| view::result(ok, &summary, cols, out));
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
        self.prompts.clear();
        if let Some((msg, style)) = note {
            self.note(msg, style);
        }
        if let Some(q) = self.queued.take() {
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
            Some(Prompt::Question { options, .. }) => {
                if let KeyCode::Char(c @ '1'..='9') = k.code {
                    let i = c as usize - '1' as usize;
                    if self.input.is_empty() && i < options.len() {
                        let answer = options[i].clone();
                        return self.answer(answer);
                    }
                }
                if k.code == KeyCode::Esc || (ctrl && k.code == KeyCode::Char('c')) {
                    self.answer(String::new());
                    return self.cancel();
                }
            }
            None => {}
        }

        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let text_cols = self.cols.saturating_sub(6);
        match k.code {
            KeyCode::Char('c') if ctrl => self.ctrl_c(),
            KeyCode::Char('d') if ctrl && self.input.is_empty() => self.quit = true,
            KeyCode::Char('j') if ctrl => self.input.insert("\n"),
            KeyCode::Char('a') if ctrl => self.input.home(),
            KeyCode::Char('e') if ctrl => self.input.end(),
            KeyCode::Char('u') if ctrl => self.input.kill_to_start(),
            KeyCode::Char('k') if ctrl => self.input.kill_to_end(),
            KeyCode::Char('w') if ctrl => self.input.delete_word(),
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
        let answer = match k.code {
            KeyCode::Char('y' | 'Y' | '1') | KeyCode::Enter => Answer::Yes,
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
    }

    /// Answers the question at the front of the queue.
    fn answer(&mut self, text: String) {
        if let Some(Prompt::Question { reply, .. }) = self.prompts.pop_front() {
            let _ = reply.send(text);
        }
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
        if self.work.is_some() {
            self.queued = Some(text);
            return;
        }
        self.gap();
        self.emit(|cols, out| view::user(&text, cols, out));
        self.stream.block_start = true;
        self.work = Some(Work::new());
        let cmd = match trimmed.strip_prefix('!') {
            Some(shell) => Command::Shell(shell.trim().to_string()),
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
            "model" if arg.is_empty() => self.note(&format!("Model: {}", self.model), ""),
            "model" => {
                self.model = arg.to_string();
                let _ = self.agent.commands.send(Command::Model(self.model.clone()));
                self.note(&format!("Model set to {}.", self.model), DIM);
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
                self.note(&format!("Renamed to \"{arg}\". Resume it later with: claude-jr -r \"{arg}\""), DIM);
            }
            "clear" if self.work.is_some() => self.note("Wait for the current turn to finish.", RED),
            "clear" => {
                let _ = self.agent.commands.send(Command::Clear);
                self.context = 0;
                self.name = None;
                self.note("Started a new conversation.", DIM);
            }
            _ if self.skills.iter().any(|s| s == name) => {
                if self.work.is_some() {
                    return self.note("Wait for the current turn to finish.", RED);
                }
                self.gap();
                self.emit(|cols, out| view::user(&format!("/{cmd}"), cols, out));
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
        if !self.stream.text.is_empty() {
            let lead = if self.stream.block_start { span("● ", ORANGE) } else { span("  ", "") };
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
                Some(view::input_box(&self.input, "type an answer, or press a number", cols, &mut out))
            }
            None => {
                let hint = if self.work.is_some() { "type to queue a message" } else { "ask anything, or /help" };
                Some(view::input_box(&self.input, hint, cols, &mut out))
            }
        };
        out.push(self.footer());

        // Never taller than the screen, or the cursor math breaks.
        let max = self.rows.max(2) - 1;
        let extra = out.len().saturating_sub(max);
        out.drain(..extra);
        (out, cursor.map(|(r, c)| (r.saturating_sub(extra), c)))
    }

    fn footer(&self) -> String {
        let queued;
        let mut left = Vec::new();
        if self.quit_armed.is_some_and(|t| t.elapsed() < QUIT_WINDOW) {
            left.push(span("press ctrl-c again to quit", DIM));
        } else if let Some(q) = &self.queued {
            queued = format!("queued: {}", q.lines().next().unwrap_or(""));
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
        right.push(self.model.clone());
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

/// 950 -> "950", 12_400 -> "12k", 1_000_000 -> "1M".
fn tokens(n: u64) -> String {
    match n {
        0..1000 => n.to_string(),
        1000..1_000_000 => format!("{}k", n / 1000),
        _ => format!("{}M", n / 1_000_000),
    }
}
