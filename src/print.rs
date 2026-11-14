// -p mode: run one prompt, print the reply to stdout and progress to stderr.
//
// With CLAUDE_JR_REPORT=<file>, also writes what the run used there as JSON
// (requests, tokens, cost), for scripts such as bench/evals.
// With --output-format json, stdout gets one JSON object at the end instead:
// the same totals plus `result` (the final answer), `error` and the files
// that changed. With --output-format stream-json, stdout gets the run log as
// it happens (see log.rs), ending with that same object. --log FILE writes
// the run log to a file, whatever stdout shows.
// With --max-cost, the run stops once it has spent that many dollars.

use crate::agent::{self, Command, Event};
use crate::log::Log;
use crate::worktree::Worktree;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};

/// What goes to stdout.
#[derive(Default, Clone, Copy, PartialEq)]
pub enum Format {
    /// The reply as it comes.
    #[default]
    Text,
    /// One JSON object at the end.
    Json,
    /// The run log, a JSON object per line.
    Stream,
}

#[derive(Default)]
pub struct Options {
    pub format: Format,
    /// Stop once this many dollars have been spent.
    pub max_cost: Option<f64>,
    /// Write the run log here.
    pub log: Option<std::path::PathBuf>,
    /// The worktree the run is in, to be wrapped up when it ends.
    pub worktree: Option<Worktree>,
    /// Set when a task's worker was told to stop (see `run`).
    pub stopped: std::sync::Arc<AtomicBool>,
}

/// Returns true if the turn finished without an error.
pub fn run(cfg: agent::Config, prompt: String, options: Options) -> Result<bool, String> {
    let mut sinks: Vec<Box<dyn Write + Send>> = Vec::new();
    if let Some(path) = &options.log {
        let file = std::fs::OpenOptions::new().create(true).append(true).open(path);
        sinks.push(Box::new(file.map_err(|e| format!("can't write the log {}: {e}", path.display()))?));
    }
    if options.format == Format::Stream {
        sinks.push(Box::new(std::io::stdout()));
    }
    let mut log = Log::new(sinks);
    log.started(&prompt, &cfg.model);
    let (tx, rx) = mpsc::channel();
    let agent = agent::spawn(cfg, move |e| {
        let _ = tx.send(e);
    });
    if std::env::var_os(crate::task::ENV).is_some() {
        // A task that is cancelled winds down like any other run, so what it
        // did is still committed and its result still written.
        for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT, signal_hook::consts::SIGHUP] {
            let _ = signal_hook::flag::register(signal, options.stopped.clone());
            let _ = signal_hook::flag::register(signal, agent.cancel.clone());
        }
    } else {
        crate::signals::watch(agent.tools.clone(), false);
    }
    let sent = agent.commands.send(Command::Send(prompt.clone())).is_ok();
    let mut report = Report::default();
    let start = std::time::Instant::now();
    report.ok = sent && print_events(rx, &mut report, &options, &agent.cancel, &mut log);
    report.duration_ms = start.elapsed().as_millis() as u64;
    // The commands it started are stopped before the worktree is wrapped up.
    if let Some(tools) = agent.tools.get() {
        tools.shutdown();
    }
    finish(&report, &options, &prompt, &mut log);
    Ok(report.ok)
}

/// Untrusted text with every control character removed except newline (and
/// tab, if `keep_tab`): escape codes in model or tool output must not reach the
/// terminal. C1 controls (U+0080..U+009F) count as control characters.
fn safe(s: &str, keep_tab: bool) -> String {
    s.chars().filter(|&c| c == '\n' || (keep_tab && c == '\t') || !c.is_control()).collect()
}

/// Same, for one-line labels: tabs become spaces too.
fn label(s: &str) -> String {
    safe(&s.replace('\t', " "), false)
}

/// Totals for CLAUDE_JR_REPORT.
#[derive(Default, serde::Serialize)]
struct Report {
    ok: bool,
    requests: u64,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    cost_usd: f64,
    duration_ms: u64,
    /// Why the run failed, if it did.
    error: Option<String>,
    /// The final answer: the text after the last tool call. Only in the
    /// JSON printed to stdout.
    #[serde(skip)]
    result: String,
}

/// Writes the run's outcome: the report file, the log's last line, the JSON
/// object. A run in a worktree has its changes committed first.
fn finish(report: &Report, options: &Options, prompt: &str, log: &mut Log) {
    if let Ok(path) = std::env::var("CLAUDE_JR_REPORT")
        && let Ok(json) = serde_json::to_string_pretty(report)
    {
        let _ = std::fs::write(path, json);
    }
    let mut fields = serde_json::to_value(report).unwrap_or_default();
    fields["result"] = report.result.trim().into();
    if options.stopped.load(Ordering::Relaxed) {
        fields["cancelled"] = true.into();
    }
    // Before the commit: afterwards nothing counts as changed any more.
    fields["files"] = log.files();
    if let Some(worktree) = &options.worktree {
        let left = worktree.finish(Some(prompt));
        eprintln!("{}", crate::worktree::summary(&left));
        fields["worktree"] = left;
    }
    log.finish(&fields);
    if let Some(dir) = std::env::var_os(crate::task::ENV) {
        let _ = crate::task::write_json(&std::path::Path::new(&dir).join("result.json"), &fields);
    }
    if options.format == Format::Json {
        println!("{fields}");
    }
}

fn print_events(
    events: Receiver<Event>,
    report: &mut Report,
    options: &Options,
    cancel: &AtomicBool,
    log: &mut Log,
) -> bool {
    // (Not locked: in stream mode the log writes to stdout too.)
    let mut out = std::io::stdout();
    // Whether stdout / stderr were left in the middle of a line.
    let (mut mid_out, mut mid_err) = (false, false);
    for ev in events {
        log.see(&ev);
        match ev {
            // With JSON output the reply goes to stderr as it comes, like the
            // rest of the progress, and to stdout once at the end.
            Event::Text(t) if options.format != Format::Text => {
                let t = safe(&t, true);
                eprint!("{t}");
                mid_err = !t.ends_with('\n');
                report.result.push_str(&t);
            }
            Event::Text(t) => {
                let t = safe(&t, true);
                let _ = out.write_all(t.as_bytes());
                let _ = out.flush();
                mid_out = !t.ends_with('\n');
                report.result.push_str(&t);
            }
            Event::Thinking(t) => {
                let t = safe(&t, true);
                eprint!("{t}");
                mid_err = !t.ends_with('\n');
            }
            Event::BlockEnd => {
                if std::mem::take(&mut mid_out) {
                    let _ = writeln!(out);
                }
                if std::mem::take(&mut mid_err) {
                    eprintln!();
                }
            }
            Event::Tool { label: label_text, .. } => {
                // What was said before a tool call is not the final answer.
                report.result.clear();
                eprintln!("● {}", label(&label_text));
            }
            Event::ToolResult { ok, summary } => {
                let mark = if ok { "⎿" } else { "✗" };
                for (i, line) in label(&summary).lines().enumerate() {
                    eprintln!("  {} {line}", if i == 0 { mark } else { " " });
                }
            }
            Event::Notice(msg) => eprintln!("{}", label(&msg)),
            Event::RewindPoints(_) | Event::Rewound { .. } | Event::Resumed { .. } | Event::Replay(_) => {}
            Event::SubStart { task, .. } => eprintln!("● agent: {}", label(&task)),
            Event::SubStep { .. } | Event::SubEnd { .. } => {}
            // Nobody is there to answer; the agent was told so (interactive: false).
            Event::Ask { reply, .. } => {
                let _ = reply.send(agent::Answer::No);
            }
            Event::Question { reply, .. } => {
                let _ = reply.send(String::new());
            }
            Event::Done => return true,
            // Only a cost limit cancels a -p run, or `task cancel`.
            Event::Cancelled => {
                if options.stopped.load(Ordering::Relaxed) {
                    report.error.get_or_insert("cancelled".into());
                }
                return report.error.is_none();
            }
            Event::Error(e) => {
                eprintln!("error: {}", label(&e));
                report.error.get_or_insert(label(&e));
                return false;
            }
            Event::Usage { usage, cost, .. } => {
                report.requests += 1;
                report.input_tokens += usage.input_tokens;
                report.output_tokens += usage.output_tokens;
                report.cache_read_tokens += usage.cache_read_input_tokens;
                report.cache_write_tokens += usage.cache_creation_input_tokens;
                report.cost_usd += cost.unwrap_or(0.0);
                // A group's budget is spent by all its workers together.
                let group = std::env::var_os(crate::task::BUDGET_ENV)
                    .and_then(|file| crate::task::spend(std::path::Path::new(&file), cost.unwrap_or(0.0)))
                    .filter(|(spent, budget)| spent >= budget && report.error.is_none());
                if let Some((spent, budget)) = group {
                    let msg = format!("stopped: the group's budget of ${budget:.2} is used up (${spent:.2} spent)");
                    eprintln!("{msg}");
                    report.error = Some(msg);
                    cancel.store(true, Ordering::Relaxed);
                }
                if let Some(max) = options.max_cost.filter(|max| report.cost_usd >= *max && report.error.is_none()) {
                    let msg =
                        format!("stopped: the cost limit of ${max:.2} was reached (${:.2} spent)", report.cost_usd);
                    eprintln!("{msg}");
                    report.error = Some(msg);
                    cancel.store(true, Ordering::Relaxed);
                }
            }
            Event::Ready { .. } | Event::ToolStart(_) | Event::ToolInput(_) | Event::Activity(_) | Event::Step(_) => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_are_removed() {
        let n = "a\x1b]0;t\x07b\x1b[2Jc\u{9b}31md\re\tf\ng";
        assert_eq!(safe(n, true), "a]0;tb[2Jc31mde\tf\ng");
        assert_eq!(safe(n, false), "a]0;tb[2Jc31mdef\ng");
        assert_eq!(label(n), "a]0;tb[2Jc31mde f\ng");
    }
}
