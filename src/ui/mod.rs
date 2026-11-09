// The interactive terminal UI.
//
// Two threads feed one channel: the agent (events) and a key reader (terminal
// input). The loop blocks on that channel, so an idle session uses no CPU;
// while the agent works it also wakes every 100 ms to move the spinner.

mod input;
mod md;
mod term;
mod text;
mod view;

use crate::agent::{self, Answer, Command, Event, Mode};
use crate::api;
use crossterm::event::{self as ct, Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use input::Input;
use md::Markdown;
use std::io;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};
use term::Term;
use text::{BOLD_ORANGE, CYAN, DIM, ORANGE, RED, RESET, span};

const TICK: Duration = Duration::from_millis(100);
const QUIT_WINDOW: Duration = Duration::from_millis(1500);

const HELP: &str = "/clear          start a new conversation
/model [name]   show or change the model
/rename [name]  name this session (resume with claude-jr -r NAME)
/cost           tokens and cost so far
/exit           quit

enter sends · ctrl-j, shift-enter or \\ then enter for a new line
shift+tab cycles modes: normal → accept edits → plan → skip permissions
esc cancels · ctrl-c clears the input, twice quits · up/down for history";

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
        let msg = if app.busy.is_some() {
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
            app.handle(m);
        }
        if app.quit {
            break;
        }
        let (live, cursor) = app.live();
        term.draw(&mut app.commits, &live, cursor)?;
    }
    Ok(())
}

struct App {
    agent: agent::Handle,
    model: String,
    input: Input,
    md: Markdown,
    /// Lines waiting to be written into scrollback.
    commits: Vec<String>,
    /// Whether the last line written to scrollback was blank.
    last_blank: bool,
    /// The unfinished last line of streaming text / progress notes.
    text: String,
    thinking: String,
    /// The next line starts a new block and gets the ● marker.
    block_start: bool,
    busy: Option<Instant>,
    activity: String,
    ask: Option<(String, Sender<Answer>)>,
    queued: Option<String>,
    cost: f64,
    context: u64,
    /// The session's name, set with /rename.
    name: Option<String>,
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
            md: Markdown::default(),
            commits: Vec::new(),
            last_blank: true,
            text: String::new(),
            thinking: String::new(),
            block_start: true,
            busy: None,
            activity: String::new(),
            ask: None,
            queued: None,
            cost: 0.0,
            context: 0,
            name: None,
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
        let lines = [
            format!(" {ORANGE}╭─────╮{RESET}"),
            format!(" {ORANGE}│ ● ● │{RESET}   {BOLD_ORANGE}claude-jr{RESET} {DIM}{version}{RESET}"),
            format!(" {ORANGE}╰──‿──╯{RESET}   {DIM}{} · {cwd}{RESET}", self.model),
            format!("           {DIM}/help for commands · shift+tab to change mode{RESET}"),
            String::new(),
        ];
        self.commits.extend(lines);
        self.last_blank = true;
    }

    // ---- scrollback -----------------------------------------------------

    fn push(&mut self, line: String) {
        self.last_blank = text::is_blank(&line);
        self.commits.push(line);
    }

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
            self.push(String::new());
        }
    }

    fn lead(&mut self) -> text::Span<'static> {
        if std::mem::take(&mut self.block_start) {
            self.gap();
            span("● ", ORANGE)
        } else {
            span("  ", "")
        }
    }

    fn commit_text_line(&mut self, line: &str) {
        let lead = self.lead();
        let mut md = self.md;
        self.emit(|cols, out| md.line(&text::clean(line), cols, lead, out));
        self.md = md;
    }

    fn commit_thinking_line(&mut self, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        let lead_gap = std::mem::take(&mut self.block_start);
        if lead_gap {
            self.gap();
        }
        self.emit(|cols, out| view::thinking(&text::clean(line), cols, out));
    }

    /// Commits any partial line still being streamed.
    fn flush(&mut self) {
        let thinking = std::mem::take(&mut self.thinking);
        if !thinking.is_empty() {
            self.commit_thinking_line(&thinking);
        }
        let text = std::mem::take(&mut self.text);
        if !text.is_empty() {
            self.commit_text_line(&text);
        }
    }

    fn note(&mut self, msg: &str, style: &'static str) {
        self.gap();
        self.emit(|cols, out| view::note(msg, style, cols, out));
    }

    // ---- events ---------------------------------------------------------

    fn handle(&mut self, m: Msg) {
        match m {
            Msg::Agent(e) => self.on_agent(e),
            Msg::Term(TermEvent::Key(k)) => self.on_key(k),
            Msg::Term(TermEvent::Paste(s)) if self.ask.is_none() => {
                self.input.insert(&s.replace("\r\n", "\n").replace('\r', "\n"));
            }
            Msg::Term(TermEvent::Resize(c, r)) => (self.cols, self.rows) = (c as usize, r as usize),
            Msg::Term(_) => {}
        }
    }

    fn on_agent(&mut self, e: Event) {
        match e {
            Event::Ready { resumed, name } => {
                if resumed > 0 {
                    let which = name.as_deref().map_or("the last session".to_string(), |n| format!("\"{n}\""));
                    self.note(&format!("Resumed {which} ({resumed} messages)."), DIM);
                }
                self.name = name;
            }
            Event::Notice(msg) => self.note(&msg, DIM),
            Event::Text(t) => {
                self.activity = "Writing".into();
                self.text.push_str(&t);
                while let Some(i) = self.text.find('\n') {
                    let line: String = self.text.drain(..=i).collect();
                    self.commit_text_line(line.trim_end_matches('\n'));
                }
            }
            Event::Thinking(t) => {
                self.activity = "Thinking".into();
                self.thinking.push_str(&t);
                while let Some(i) = self.thinking.find('\n') {
                    let line: String = self.thinking.drain(..=i).collect();
                    self.commit_thinking_line(line.trim_end_matches('\n'));
                }
            }
            Event::BlockEnd => {
                self.flush();
                self.block_start = true;
            }
            Event::ToolStart(name) => self.activity = format!("Preparing {name}"),
            Event::Tool { label, diff } => {
                self.flush();
                self.gap();
                self.activity = format!("Running {label}");
                self.emit(|cols, out| {
                    view::tool(&label, cols, out);
                    if let Some(d) = &diff {
                        view::diff(d, cols, out);
                    }
                });
            }
            Event::Ask { label, reply } => self.ask = Some((label, reply)),
            Event::ToolResult { ok, summary } => {
                self.emit(|cols, out| view::result(ok, &summary, cols, out));
                self.block_start = true;
                self.activity = "Thinking".into();
            }
            Event::Usage(u) => {
                self.cost += api::cost(&self.model, &u).unwrap_or(0.0);
                self.context = u.context();
            }
            Event::Done => self.finish(None),
            Event::Cancelled => self.finish(Some(("Cancelled.", DIM))),
            Event::Error(e) => self.finish(Some((&format!("Error: {e}"), RED))),
        }
    }

    fn finish(&mut self, note: Option<(&str, &'static str)>) {
        self.flush();
        self.md.reset();
        self.busy = None;
        self.ask = None;
        if let Some((msg, style)) = note {
            self.note(msg, style);
        }
        if let Some(q) = self.queued.take() {
            self.submit(q);
        }
    }

    fn on_key(&mut self, k: KeyEvent) {
        if k.kind == KeyEventKind::Release {
            return;
        }
        if self.ask.is_some() {
            return self.on_ask_key(k);
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
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
                    if !text.trim().is_empty() {
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

    fn on_ask_key(&mut self, k: KeyEvent) {
        let answer = match k.code {
            KeyCode::Char('y' | 'Y' | '1') | KeyCode::Enter => Answer::Yes,
            KeyCode::Char('a' | 'A' | '2') => Answer::Always,
            KeyCode::Char('n' | 'N' | '3') | KeyCode::Esc => Answer::No,
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                self.cancel();
                Answer::No
            }
            _ => return,
        };
        if let Some((_, reply)) = self.ask.take() {
            let _ = reply.send(answer);
        }
    }

    fn ctrl_c(&mut self) {
        if !self.input.is_empty() {
            self.input.clear();
        } else if self.busy.is_some() {
            self.cancel();
        } else if self.quit_armed.is_some_and(|t| t.elapsed() < QUIT_WINDOW) {
            self.quit = true;
        } else {
            self.quit_armed = Some(Instant::now());
        }
    }

    fn cancel(&mut self) {
        if self.busy.is_some() {
            self.agent.cancel.store(true, Ordering::Relaxed);
            self.activity = "Cancelling".into();
        }
    }

    fn submit(&mut self, text: String) {
        if let Some(cmd) = text.trim().strip_prefix('/') {
            return self.command(cmd);
        }
        if self.busy.is_some() {
            self.queued = Some(text);
            return;
        }
        self.gap();
        self.emit(|cols, out| view::user(&text, cols, out));
        self.block_start = true;
        self.busy = Some(Instant::now());
        self.activity = "Thinking".into();
        let _ = self.agent.commands.send(Command::Send(text));
    }

    fn command(&mut self, cmd: &str) {
        let (name, arg) = cmd.split_once(' ').map_or((cmd, ""), |(n, a)| (n, a.trim()));
        match name {
            "help" => self.note(HELP, ""),
            "exit" | "quit" => self.quit = true,
            "cost" => {
                let msg = format!("${:.4} so far · {} tokens in context", self.cost, self.context);
                self.note(&msg, "");
            }
            "model" if arg.is_empty() => self.note(&format!("Model: {}", self.model), ""),
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
            "model" => {
                self.model = arg.to_string();
                let _ = self.agent.commands.send(Command::Model(self.model.clone()));
                self.note(&format!("Model set to {}.", self.model), DIM);
            }
            "clear" if self.busy.is_some() => self.note("Wait for the current turn to finish.", RED),
            "clear" => {
                let _ = self.agent.commands.send(Command::Clear);
                self.context = 0;
                self.name = None;
                self.note("Started a new conversation.", DIM);
            }
            _ => self.note(&format!("Unknown command /{name}. Try /help."), RED),
        }
    }

    // ---- live area ------------------------------------------------------

    /// The bottom area: partial streaming lines, status, input or approval
    /// prompt, footer. Returns its lines and the cursor position.
    fn live(&self) -> (Vec<String>, Option<(usize, usize)>) {
        let cols = self.cols;
        let mut out = Vec::new();
        if !self.thinking.is_empty() {
            view::thinking(&text::clean(&self.thinking), cols, &mut out);
        }
        if !self.text.is_empty() {
            let lead = if self.block_start { span("● ", ORANGE) } else { span("  ", "") };
            let mut md = self.md;
            md.line(&text::clean(&self.text), cols, lead, &mut out);
        }
        if let Some(since) = self.busy.filter(|_| self.ask.is_none()) {
            let elapsed = since.elapsed();
            out.push(view::status(&self.activity, elapsed.as_secs(), elapsed.as_millis(), cols));
        }
        out.push(String::new());

        let cursor = match &self.ask {
            Some((label, _)) => {
                view::ask(label, cols, &mut out);
                None
            }
            None => {
                let hint = if self.busy.is_some() { "type to queue a message" } else { "ask anything, or /help" };
                Some(view::input_box(&self.input, hint, cols, &mut out))
            }
        };

        let queued;
        let left = if self.quit_armed.is_some_and(|t| t.elapsed() < QUIT_WINDOW) {
            span("press ctrl-c again to quit", DIM)
        } else if let Some(q) = &self.queued {
            queued = format!("queued: {}", q.lines().next().unwrap_or(""));
            span(&queued, DIM)
        } else {
            match self.agent.mode.get() {
                Mode::Default => span("shift+tab to change mode", DIM),
                Mode::AcceptEdits => span("⏵⏵ accept edits on (shift+tab to cycle)", ORANGE),
                Mode::Plan => span("⏸ plan mode on (shift+tab to cycle)", CYAN),
                Mode::Bypass => span("⏵⏵ skip permissions on (shift+tab to cycle)", RED),
            }
        };
        let name = self.name.as_deref().map(|n| format!("{n} · ")).unwrap_or_default();
        let right = format!("{name}{} · {}k tokens · ${:.2}", self.model, self.context / 1000, self.cost);
        out.push(view::footer(left, &right, cols));

        // Never taller than the screen, or the cursor math breaks.
        let max = self.rows.max(2) - 1;
        let extra = out.len().saturating_sub(max);
        out.drain(..extra);
        (out, cursor.map(|(r, c)| (r.saturating_sub(extra), c)))
    }
}
