mod agent;
mod api;
mod prompt;

use agent::{Command, Event};
use std::io::{BufRead, Write};
use std::process::ExitCode;

const USAGE: &str = "usage: claude-jr [-p PROMPT] [--model MODEL] [--effort low|medium|high|xhigh|max]

  -p, --print PROMPT   run one prompt, print the reply, exit
  -m, --model MODEL    model id (default: claude-opus-5-5, or $CLAUDE_JR_MODEL)
  -e, --effort LEVEL   reasoning effort (default: the model's own default)
  -h, --help           show this help

needs ANTHROPIC_API_KEY; ANTHROPIC_BASE_URL is optional";

struct Args {
    prompt: Option<String>,
    model: String,
    effort: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        prompt: None,
        model: std::env::var("CLAUDE_JR_MODEL").unwrap_or_else(|_| "claude-opus-5-5".into()),
        effort: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{a} needs a value"));
        match a.as_str() {
            "-p" | "--print" => args.prompt = Some(value()?),
            "-m" | "--model" => args.model = value()?,
            "-e" | "--effort" => args.effort = Some(value()?),
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

    let agent = agent::spawn(agent::Config {
        key,
        base_url: std::env::var("ANTHROPIC_BASE_URL").ok(),
        model: args.model,
        effort: args.effort,
        max_tokens: 64_000,
    });

    if let Some(prompt) = args.prompt {
        return if run_turn(&agent, prompt) { ExitCode::SUCCESS } else { ExitCode::FAILURE };
    }

    // Plain line REPL until the TUI lands in phase 3.
    let stdin = std::io::stdin();
    loop {
        eprint!("> ");
        let mut line = String::new();
        if stdin.lock().read_line(&mut line).unwrap_or(0) == 0 {
            return ExitCode::SUCCESS;
        }
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

/// Sends one message and prints events until the turn ends. Returns false on error.
fn run_turn(agent: &agent::Handle, text: String) -> bool {
    if agent.commands.send(Command::Send(text)).is_err() {
        return false;
    }
    let mut out = std::io::stdout().lock();
    let mut ok = true;
    for ev in &agent.events {
        match ev {
            Event::Text(t) => {
                let _ = out.write_all(t.as_bytes());
                let _ = out.flush();
            }
            Event::ToolStart(name) => eprintln!("\n[tool: {name}]"),
            Event::Usage(u) => eprintln!(
                "\n[in {} · cache read {} · cache write {} · out {}]",
                u.input_tokens, u.cache_read_input_tokens, u.cache_creation_input_tokens, u.output_tokens
            ),
            Event::Done => break,
            Event::Cancelled => {
                eprintln!("[cancelled]");
                break;
            }
            Event::Error(e) => {
                eprintln!("\nerror: {e}");
                ok = false;
                break;
            }
        }
    }
    ok
}
