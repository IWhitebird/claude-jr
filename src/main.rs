// claude-jr: a small coding agent for the terminal.
//
//   cli/     the command line: one file for each thing you can type
//   ui/      the interactive terminal
//   agent/   the loop: ask the model, run its tools, repeat
//   tools/   what the model can do: files, the shell, MCP servers, notes
//   api/     talking to models, whichever provider serves them
//   work/    running many: background tasks, worktrees, budgets, the run log
//   store/   what is kept on disk: conversations, images, settings
//   sys/     what depends on the operating system: sandbox, memory, signals

// Printing that never crashes. Rust's print macros panic when the write
// fails, and this build aborts on a panic, so `claudejr -p ... 2>&1 | head`
// dumped core once head had gone.

/// A line to standard error. Progress nobody can see (the reader has gone, or
/// the disk is full) is dropped: there is nowhere left to report it.
macro_rules! note {
    ($($t:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), $($t)*);
    }};
}

/// The same without the newline.
macro_rules! note_part {
    ($($t:tt)*) => {{
        use std::io::Write as _;
        let _ = write!(std::io::stderr(), $($t)*);
    }};
}

mod agent;
mod api;
mod cli;
mod permission;
mod prompt;
mod store;
mod sys;
mod tools;
mod ui;
mod work;

use cli::args::{Cli, parse_args};
use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(msg) => {
            note!("{msg}");
            ExitCode::from(2)
        }
    }
}

/// A line to standard output. A reader that has gone (`| head`) is no error,
/// as for any command-line tool; any other failure, a full disk say, is.
pub fn say(text: impl std::fmt::Display) -> Result<(), String> {
    use std::io::Write as _;
    match writeln!(std::io::stdout(), "{text}").and_then(|()| std::io::stdout().flush()) {
        Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => Err(format!("couldn't write the output: {e}")),
        _ => Ok(()),
    }
}

fn run() -> Result<ExitCode, String> {
    let args = match parse_args()? {
        Cli::Run(args) => args,
        Cli::Show(text) => {
            say(text)?;
            return Ok(ExitCode::SUCCESS);
        }
        Cli::Update => {
            say(cli::update::run()?)?;
            return Ok(ExitCode::SUCCESS);
        }
        Cli::Sessions => {
            cli::sessions::print()?;
            return Ok(ExitCode::SUCCESS);
        }
        Cli::Task(args) | Cli::Fleet(args) if args.iter().any(|a| a == "--help" || a == "-h") => {
            say(if std::env::args().nth(1).as_deref() == Some("fleet") {
                cli::fleet::USAGE
            } else {
                cli::task::USAGE
            })?;
            return Ok(ExitCode::SUCCESS);
        }
        Cli::Mcp(args) if args.iter().any(|a| a == "--help" || a == "-h") => {
            say(cli::mcp::USAGE)?;
            return Ok(ExitCode::SUCCESS);
        }
        Cli::Mcp(args) => {
            cli::mcp::run(cli::mcp::parse(&args)?)?;
            return Ok(ExitCode::SUCCESS);
        }
        Cli::Install(args, _) if args.iter().any(|a| a == "--help" || a == "-h") => {
            say(cli::install::USAGE)?;
            return Ok(ExitCode::SUCCESS);
        }
        Cli::Install(args, install) => {
            say(cli::install::run(&args, install)?)?;
            return Ok(ExitCode::SUCCESS);
        }
        Cli::Fleet(args) => {
            let (text, ok) = cli::fleet::run(&args)?;
            say(text)?;
            return Ok(if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE });
        }
        Cli::Task(args) => {
            let (text, ok) = cli::task::run(&args)?;
            if !text.is_empty() {
                say(text)?;
            }
            return Ok(if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE });
        }
    };
    cli::start::run(args)
}
