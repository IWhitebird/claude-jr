// -p mode: run one prompt, print the reply to stdout and progress to stderr.
//
// With CLAUDE_JR_REPORT=<file>, also writes what the run used there as JSON
// (requests, tokens, cost), for scripts such as bench/evals.

use crate::agent::{self, Command, Event};
use std::io::Write;
use std::sync::mpsc::{self, Receiver};

/// Returns true if the turn finished without an error.
pub fn run(cfg: agent::Config, prompt: String) -> bool {
    let (tx, rx) = mpsc::channel();
    let agent = agent::spawn(cfg, move |e| {
        let _ = tx.send(e);
    });
    crate::signals::watch(agent.tools.clone(), false);
    let ok = agent.commands.send(Command::Send(prompt)).is_ok() && print(rx);
    if let Some(tools) = agent.tools.get() {
        tools.shutdown();
    }
    ok
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
}

/// Prints events until the turn ends.
fn print(events: Receiver<Event>) -> bool {
    let mut report = Report::default();
    let ok = print_events(events, &mut report);
    if let Ok(path) = std::env::var("CLAUDE_JR_REPORT") {
        report.ok = ok;
        if let Ok(json) = serde_json::to_string_pretty(&report) {
            let _ = std::fs::write(path, json);
        }
    }
    ok
}

fn print_events(events: Receiver<Event>, report: &mut Report) -> bool {
    let mut out = std::io::stdout().lock();
    // Whether stdout / stderr were left in the middle of a line.
    let (mut mid_out, mut mid_err) = (false, false);
    for ev in events {
        match ev {
            Event::Text(t) => {
                let _ = out.write_all(t.as_bytes());
                let _ = out.flush();
                mid_out = !t.ends_with('\n');
            }
            Event::Thinking(t) => {
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
            Event::Tool { label, .. } => eprintln!("● {label}"),
            Event::ToolResult { ok, summary } => {
                let mark = if ok { "⎿" } else { "✗" };
                for (i, line) in summary.lines().enumerate() {
                    eprintln!("  {} {line}", if i == 0 { mark } else { " " });
                }
            }
            Event::Notice(msg) => eprintln!("{msg}"),
            Event::RewindPoints(_) | Event::Rewound { .. } | Event::Resumed { .. } => {}
            Event::SubStart { task, .. } => eprintln!("● agent: {task}"),
            Event::SubStep { .. } | Event::SubEnd { .. } => {}
            // Nobody is there to answer; the agent was told so (interactive: false).
            Event::Ask { reply, .. } => {
                let _ = reply.send(agent::Answer::No);
            }
            Event::Question { reply, .. } => {
                let _ = reply.send(String::new());
            }
            Event::Done | Event::Cancelled => return true,
            Event::Error(e) => {
                eprintln!("error: {e}");
                return false;
            }
            Event::Usage { usage, cost, .. } => {
                report.requests += 1;
                report.input_tokens += usage.input_tokens;
                report.output_tokens += usage.output_tokens;
                report.cache_read_tokens += usage.cache_read_input_tokens;
                report.cache_write_tokens += usage.cache_creation_input_tokens;
                report.cost_usd += cost.unwrap_or(0.0);
            }
            Event::Ready { .. } | Event::ToolStart(_) | Event::ToolInput(_) | Event::Activity(_) => {}
        }
    }
    false
}
