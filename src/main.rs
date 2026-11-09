mod agent;
mod api;
mod config;
mod permission;
mod print;
mod prompt;
mod session;
mod tools;
mod ui;

use agent::Mode;
use std::io::IsTerminal;
use std::process::ExitCode;

const USAGE: &str = "usage: claude-jr [options] [-p PROMPT]

  -p, --print PROMPT   run one prompt, print the reply, exit
  -c, --continue       continue the last conversation in this directory
  -m, --model MODEL    model id (default: claude-opus-5-5)
  -e, --effort LEVEL   low, medium, high, xhigh or max
  -y, --yes            allow write, edit and bash without asking
  -h, --help           show this help
  -V, --version        show the version

In -p mode, write, edit and bash are refused unless --yes is given or a
rule in the config allows them.

Config: ~/.config/claude-jr/config.json and ./.claude-jr.json
  { \"model\": \"...\", \"effort\": \"high\", \"allow\": [\"bash:cargo *\", \"edit\"] }
Project instructions: AGENTS.md or CLAUDE.md in this directory or its parents.
Environment: ANTHROPIC_API_KEY (required), ANTHROPIC_BASE_URL, CLAUDE_JR_MODEL.";

#[derive(Default)]
struct Args {
    prompt: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    yes: bool,
    resume: bool,
}

enum Cli {
    Run(Args),
    /// --help or --version: print this and exit successfully.
    Show(String),
}

fn parse_args() -> Result<Cli, String> {
    let mut args = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{a} needs a value"));
        match a.as_str() {
            "-p" | "--print" => args.prompt = Some(value()?),
            "-m" | "--model" => args.model = Some(value()?),
            "-e" | "--effort" => args.effort = Some(value()?),
            "-y" | "--yes" => args.yes = true,
            "-c" | "--continue" => args.resume = true,
            "-h" | "--help" => return Ok(Cli::Show(USAGE.into())),
            "-V" | "--version" => return Ok(Cli::Show(format!("claude-jr {}", env!("CARGO_PKG_VERSION")))),
            _ => return Err(format!("unknown argument: {a}\n\n{USAGE}")),
        }
    }
    Ok(Cli::Run(args))
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(msg) => {
            eprintln!("{msg}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode, String> {
    let args = match parse_args()? {
        Cli::Run(args) => args,
        Cli::Show(text) => {
            println!("{text}");
            return Ok(ExitCode::SUCCESS);
        }
    };
    let file = config::Config::load()?;
    let key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| "ANTHROPIC_API_KEY is not set")?;
    let interactive = args.prompt.is_none();
    if interactive && !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        return Err("not a terminal; use -p PROMPT for non-interactive use".into());
    }

    let cfg = agent::Config {
        key,
        base_url: std::env::var("ANTHROPIC_BASE_URL").ok(),
        model: args
            .model
            .or_else(|| std::env::var("CLAUDE_JR_MODEL").ok())
            .or(file.model)
            .unwrap_or_else(|| "claude-opus-5-5".into()),
        effort: args.effort.or(file.effort),
        mode: match (args.yes, interactive) {
            (true, _) => Mode::Allow,
            (false, true) => Mode::Ask,
            (false, false) => Mode::Deny,
        },
        allow: file.allow,
        resume: args.resume,
    };

    match args.prompt {
        Some(prompt) => Ok(if print::run(cfg, prompt) { ExitCode::SUCCESS } else { ExitCode::FAILURE }),
        None => {
            ui::run(cfg).map_err(|e| format!("terminal error: {e}"))?;
            Ok(ExitCode::SUCCESS)
        }
    }
}
