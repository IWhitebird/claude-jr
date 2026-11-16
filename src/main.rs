// claude-jr: a small coding agent for the terminal.
//
//   cli/     the command line: one file for each thing you can type
//   ui/      the interactive terminal
//   agent/   the loop: ask the model, run its tools, repeat
//   tools/   what the model can do: files, the shell, MCP servers, notes
//   api/     talking to models, whichever provider serves them
//   work/    running many: background tasks, worktrees, budgets, the run log
//   (the rest: sessions, images and config on disk; the sandbox; the prompt)

mod agent;
mod api;
mod cli;
mod config;
mod images;
mod mem;
mod permission;
mod prompt;
mod sandbox;
mod session;
mod signals;
mod tools;
mod ui;
mod work;

use cli::args::{Cli, parse_args};
use std::process::ExitCode;

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
        Cli::Update => {
            println!("{}", cli::update::run()?);
            return Ok(ExitCode::SUCCESS);
        }
        Cli::Sessions => {
            cli::sessions::print();
            return Ok(ExitCode::SUCCESS);
        }
        Cli::Task(args) | Cli::Fleet(args) if args.iter().any(|a| a == "--help" || a == "-h") => {
            println!(
                "{}",
                if std::env::args().nth(1).as_deref() == Some("fleet") { cli::fleet::USAGE } else { cli::task::USAGE }
            );
            return Ok(ExitCode::SUCCESS);
        }
        Cli::Mcp(args) if args.iter().any(|a| a == "--help" || a == "-h") => {
            println!("{}", cli::mcp::USAGE);
            return Ok(ExitCode::SUCCESS);
        }
        Cli::Mcp(args) => {
            cli::mcp::run(cli::mcp::parse(&args)?)?;
            return Ok(ExitCode::SUCCESS);
        }
        Cli::Install(args, _) if args.iter().any(|a| a == "--help" || a == "-h") => {
            println!("{}", cli::install::USAGE);
            return Ok(ExitCode::SUCCESS);
        }
        Cli::Install(args, install) => {
            println!("{}", cli::install::run(&args, install)?);
            return Ok(ExitCode::SUCCESS);
        }
        Cli::Fleet(args) => {
            let (text, ok) = cli::fleet::run(&args)?;
            println!("{text}");
            return Ok(if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE });
        }
        Cli::Task(args) => {
            let (text, ok) = cli::task::run(&args)?;
            if !text.is_empty() {
                println!("{text}");
            }
            return Ok(if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE });
        }
    };
    cli::start::run(args)
}
