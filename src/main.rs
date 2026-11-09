mod agent;
mod api;
mod prompt;
mod tools;

use agent::{Command, Event, Permission};
use std::io::{BufRead, Write};
use std::process::ExitCode;

const USAGE: &str = "usage: claude-jr [-p PROMPT] [--model MODEL] [--effort LEVEL] [--yes]

  -p, --print PROMPT   run one prompt, print the reply, exit
  -m, --model MODEL    model id (default: claude-opus-5-5, or $CLAUDE_JR_MODEL)
  -e, --effort LEVEL   low, medium, high, xhigh or max (default: the model's own)
  -y, --yes            allow write, edit and bash without asking
  -h, --help           show this help

In -p mode, write, edit and bash are refused unless --yes is given.
Needs ANTHROPIC_API_KEY; ANTHROPIC_BASE_URL is optional.";

struct Args {
    prompt: Option<String>,
    model: String,
    effort: Option<String>,
    yes: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        prompt: None,
        model: std::env::var("CLAUDE_JR_MODEL").unwrap_or_else(|_| "claude-opus-5-5".into()),
        effort: None,
        yes: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{a} needs a value"));
        match a.as_str() {
            "-p" | "--print" => args.prompt = Some(value()?),
            "-m" | "--model" => args.model = value()?,
            "-e" | "--effort" => args.effort = Some(value()?),
            "-y" | "--yes" => args.yes = true,
            "-h" | "--help" => return Err(USAGE.into()),
            _ => return Err(format!("unknown argument: {a}\n\n{USAGE}")),
        }
    }
    Ok(args)
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::from(2);
        }
    };
    let Ok(key) = std::env::var("ANTHROPIC_API_KEY") else {
        eprintln!("ANTHROPIC_API_KEY is not set");
        return ExitCode::from(2);
    };

    let permission = match (args.yes, args.prompt.is_some()) {
        (true, _) => Permission::Allow,
        (false, true) => Permission::Deny,
        (false, false) => Permission::Ask,
    };
    let agent = agent::spawn(agent::Config {
        key,
        base_url: std::env::var("ANTHROPIC_BASE_URL").ok(),
        model: args.model,
        effort: args.effort,
        max_tokens: 64_000,
        permission,
    });

    if let Some(prompt) = args.prompt {
        return if run_turn(&agent, prompt) { ExitCode::SUCCESS } else { ExitCode::FAILURE };
    }

    // Plain line REPL until the TUI lands in phase 3.
    loop {
        eprint!("\n> ");
        let Some(line) = read_line() else { return ExitCode::SUCCESS };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line == "/exit" {
            return ExitCode::SUCCESS;
        }
        run_turn(&agent, line.to_string());
    }
}

fn read_line() -> Option<String> {
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line),
    }
}

/// Sends one message and prints events until the turn ends. Returns false on error.
fn run_turn(agent: &agent::Handle, text: String) -> bool {
    if agent.commands.send(Command::Send(text)).is_err() {
        return false;
    }
    let mut out = std::io::stdout();
    let mut mid_line = false;
    let newline = |mid: &mut bool| {
        if *mid {
            println!();
            *mid = false;
        }
    };

    for ev in &agent.events {
        match ev {
            Event::Text(t) => {
                print!("{t}");
                let _ = out.flush();
                mid_line = !t.ends_with('\n');
            }
            Event::ToolStart(_) => {}
            Event::Tool { label } => {
                newline(&mut mid_line);
                eprintln!("\n● {label}");
            }
            Event::Ask { label, reply } => {
                eprint!("  allow {label}? [y/N] ");
                let yes = read_line().is_some_and(|l| matches!(l.trim(), "y" | "Y" | "yes"));
                let _ = reply.send(yes);
            }
            Event::ToolResult { ok, output } => {
                let mark = if ok { "⎿" } else { "✗" };
                eprintln!("  {mark} {}", summary(&output));
            }
            Event::Usage(u) => {
                newline(&mut mid_line);
                eprintln!(
                    "  [in {} · cache read {} · cache write {} · out {}]",
                    u.input_tokens, u.cache_read_input_tokens, u.cache_creation_input_tokens, u.output_tokens
                );
            }
            Event::Done => return true,
            Event::Cancelled => {
                eprintln!("[cancelled]");
                return true;
            }
            Event::Error(e) => {
                newline(&mut mid_line);
                eprintln!("error: {e}");
                return false;
            }
        }
    }
    false
}

/// First line of a tool result, plus how many more lines there are.
fn summary(output: &str) -> String {
    let mut lines = output.lines();
    let first: String = lines.next().unwrap_or("").chars().take(100).collect();
    match lines.count() {
        0 => first,
        n => format!("{first} (+{n} lines)"),
    }
}
