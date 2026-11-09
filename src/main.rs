mod agent;
mod api;
mod config;
mod images;
mod permission;
mod print;
mod prompt;
mod sandbox;
mod session;
mod signals;
mod tools;
mod ui;

use agent::Mode;
use session::{Pick, Session};
use std::io::IsTerminal;
use std::process::ExitCode;

const USAGE: &str = "usage: claudejr [options] [-p PROMPT]

  -p, --print PROMPT   run one prompt, print the reply, exit
  -c, --continue       continue the last conversation in this directory
  -r, --resume NAME    continue a conversation by name (see /rename) or id
      --sessions       list saved conversations for this directory
      --sandbox        only let it write inside this directory (Linux, bubblewrap)
  -m, --model MODEL    model id (default: claude-opus-5-5)
  -e, --effort LEVEL   low, medium, high, xhigh or max
  -y, --yes            start in skip-permissions mode (also --dangerously-skip-permissions)
  -h, --help           show this help
  -V, --version        show the version

In -p mode, write, edit and bash are refused unless --yes is given or a
rule in the config allows them.

Config: ~/.config/claude-jr/config.json and ./.claude-jr.json
  { \"model\": \"...\", \"effort\": \"high\", \"allow\": [\"bash:cargo *\", \"edit\"] }
Project instructions: AGENTS.md or CLAUDE.md in this directory or its parents.
API key: type /login inside claude-jr (saved to ~/.config/claude-jr/credentials),
or set ANTHROPIC_API_KEY. Also read: ANTHROPIC_BASE_URL, CLAUDE_JR_MODEL.";

#[derive(Default)]
struct Args {
    prompt: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    yes: bool,
    resume: Option<Pick>,
    sandbox: bool,
}

enum Cli {
    Run(Args),
    /// --help or --version: print this and exit successfully.
    Show(String),
    Sessions,
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
            "-y" | "--yes" | "--dangerously-skip-permissions" => args.yes = true,
            "-c" | "--continue" => args.resume = Some(Pick::Latest),
            "-r" | "--resume" => args.resume = Some(Pick::Named(value()?)),
            "--sessions" => return Ok(Cli::Sessions),
            "--sandbox" => args.sandbox = true,
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
        Cli::Sessions => {
            print_sessions();
            return Ok(ExitCode::SUCCESS);
        }
    };
    let interactive = args.prompt.is_none();
    let project = config::project_files();
    if !project.is_empty() && !config::is_trusted() {
        let files = project.join(" and ");
        if interactive && std::io::stdin().is_terminal() {
            eprint!(
                "This folder has {files}, which can change permissions and start commands.\nTrust this folder? [y/N] "
            );
            let mut answer = String::new();
            let _ = std::io::stdin().read_line(&mut answer);
            if matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
                config::trust()?;
            }
        } else if !interactive {
            eprintln!("ignoring {files}: this folder isn't trusted (run claudejr here once to trust it)");
        }
    }
    let file = config::Config::load()?;
    if (args.sandbox || file.sandbox.enabled) && !sandbox::active() {
        return Err(sandbox::exec(&file.sandbox));
    }
    let key = config::api_key().unwrap_or_default();
    if key.is_empty() && !interactive {
        return Err("no API key: run claudejr and type /login, or set ANTHROPIC_API_KEY".into());
    }
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
        mode: if args.yes { Mode::Bypass } else { file.mode.as_deref().and_then(Mode::parse).unwrap_or(Mode::Default) },
        allow: file.allow,
        resume: args.resume,
        auto_model: file.auto_model.unwrap_or_else(|| "claude-haiku-5-5".into()),
        mcp: file.mcp_servers,
        web: file.web_tools.unwrap_or(true),
        interactive,
    };

    match args.prompt {
        Some(prompt) => Ok(if print::run(cfg, prompt) { ExitCode::SUCCESS } else { ExitCode::FAILURE }),
        None => {
            let bottom = file.layout.as_deref() != Some("inline");
            ui::run(cfg, bottom).map_err(|e| format!("terminal error: {e}"))?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn print_sessions() {
    let sessions = Session::list();
    if sessions.is_empty() {
        println!("No saved conversations in this directory.");
        return;
    }
    for s in sessions.iter().rev() {
        let name = s.name.as_deref().unwrap_or("-");
        let prompt: String = s.first_prompt.lines().next().unwrap_or("").chars().take(60).collect();
        println!("{:<14} {:<20} {:>4} msgs  {prompt}", s.id, name, s.messages);
    }
    println!("\nResume with: claudejr -r NAME_OR_ID   (or -c for the newest)");
}
