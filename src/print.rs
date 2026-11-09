// -p mode: run one prompt, print the reply to stdout and progress to stderr.

use crate::agent::{self, Command, Event};
use std::io::Write;
use std::sync::mpsc;

/// Returns true if the turn finished without an error.
pub fn run(cfg: agent::Config, prompt: String) -> bool {
    let (tx, rx) = mpsc::channel();
    let agent = agent::spawn(cfg, move |e| {
        let _ = tx.send(e);
    });
    if agent.commands.send(Command::Send(prompt)).is_err() {
        return false;
    }

    let mut out = std::io::stdout().lock();
    // Whether stdout / stderr were left in the middle of a line.
    let (mut mid_out, mut mid_err) = (false, false);
    for ev in rx {
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
            Event::Ask { reply, .. } => {
                let _ = reply.send(agent::Answer::No);
            }
            Event::Done => return true,
            Event::Cancelled => return true,
            Event::Error(e) => {
                eprintln!("error: {e}");
                return false;
            }
            Event::Ready { .. } | Event::ToolStart(_) | Event::Usage(_) => {}
        }
    }
    false
}
