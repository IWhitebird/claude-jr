// The command line's flags: what each means, and the usage text.

use crate::cli::print;
use crate::store::session::Pick;

pub const USAGE: &str = "usage: claudejr [options] [-p PROMPT]

  -p, --print PROMPT   run one prompt, print the reply, exit (or pipe the prompt in: cmd | claudejr -p)
      task ...         run tasks in the background: claudejr task --help
      fleet ...        run a list of tasks side by side: claudejr fleet --help
      mcp ...          serve tasks to another agent over MCP: claudejr mcp --help
      install AGENT    set that up for claude-code, cursor or codex: claudejr install --help
  -c, --continue       continue the last conversation in this directory
  -r, --resume NAME    continue a conversation by name (see /rename) or id
      --sessions       list saved conversations for this directory
      --sandbox        only let it write inside this directory (Linux, bubblewrap)
      --worktree       work in a git worktree and branch of its own, leaving this folder alone
  -m, --model MODEL    model id (default: claude-opus-5-5). Other providers: PROVIDER/MODEL,
                       e.g. ollama/qwen3, openai/gpt-5, openrouter/..., groq/..., gemini/...
  -e, --effort LEVEL   low, medium, high, xhigh or max
  -y, --yes            start in skip-permissions mode (also --dangerously-skip-permissions)
      --output-format json   with -p: print one JSON object at the end (result, files, cost)
      --output-format stream-json   with -p: print each step as a JSON line while it runs
      --log FILE       with -p: write each step to FILE as a JSON line (what ran, what allowed it)
      --max-cost USD   with -p: stop once this many dollars have been spent
      --update         replace this binary with the newest release
  -h, --help           show this help
  -V, --version        show the version

In -p mode, write, edit and bash are refused unless --yes is given or a
rule in the config allows them.

Config: ~/.config/claude-jr/config.json and ./.claude-jr.json
  { \"model\": \"...\", \"effort\": \"high\", \"allow\": [\"bash:cargo *\", \"edit\"] }
Project instructions: AGENTS.md or CLAUDE.md in this directory or its parents.
API key: type /login inside claude-jr (saved to ~/.config/claude-jr/credentials),
or set ANTHROPIC_API_KEY. Also read: ANTHROPIC_BASE_URL, CLAUDE_JR_MODEL,
CLAUDE_JR_OFFLINE_SECS (how long to keep trying with no network; default 120).";

#[derive(Default)]
pub struct Args {
    pub prompt: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub yes: bool,
    pub resume: Option<Pick>,
    pub sandbox: bool,
    pub format: print::Format,
    pub log: Option<std::path::PathBuf>,
    pub worktree: bool,
    pub max_cost: Option<f64>,
}

pub enum Cli {
    Run(Args),
    /// --help or --version: print this and exit successfully.
    Show(String),
    Sessions,
    Update,
    /// `claudejr task ...`, with what came after it.
    Task(Vec<String>),
    /// `claudejr fleet ...`.
    Fleet(Vec<String>),
    /// `claudejr mcp ...`.
    Mcp(Vec<String>),
    /// `claudejr install ...` (true) or `claudejr uninstall ...`.
    Install(Vec<String>, bool),
}

pub fn parse_args() -> Result<Cli, String> {
    let mut args = Args::default();
    let mut it = std::env::args().skip(1).peekable();
    if it.peek().is_some_and(|a| a == "task") {
        return Ok(Cli::Task(it.skip(1).collect()));
    }
    if it.peek().is_some_and(|a| a == "fleet") {
        return Ok(Cli::Fleet(it.skip(1).collect()));
    }
    if it.peek().is_some_and(|a| a == "mcp") {
        return Ok(Cli::Mcp(it.skip(1).collect()));
    }
    if let Some(install) = it.peek().and_then(|a| ["uninstall", "install"].iter().position(|verb| a == verb)) {
        return Ok(Cli::Install(it.skip(1).collect(), install == 1));
    }
    while let Some(a) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{a} needs a value"));
        match a.as_str() {
            // With nothing after it, the prompt is whatever is piped in.
            "-p" | "--print" => args.prompt = Some(it.next().map_or_else(piped, Ok)?),
            "-m" | "--model" => args.model = Some(value()?),
            "-e" | "--effort" => args.effort = Some(value()?),
            "-y" | "--yes" | "--dangerously-skip-permissions" => args.yes = true,
            "-c" | "--continue" => args.resume = Some(Pick::Latest),
            "-r" | "--resume" => args.resume = Some(Pick::Named(value()?)),
            "--sessions" => return Ok(Cli::Sessions),
            "--update" => return Ok(Cli::Update),
            "--sandbox" => args.sandbox = true,
            "--worktree" => args.worktree = true,
            "--output-format" => match value()?.as_str() {
                "json" => args.format = print::Format::Json,
                "stream-json" => args.format = print::Format::Stream,
                "text" => args.format = print::Format::Text,
                other => return Err(format!("unknown output format: {other} (text, json or stream-json)")),
            },
            "--log" => args.log = Some(value()?.into()),
            "--max-cost" => {
                let v = value()?;
                let usd = v.trim_start_matches('$').parse::<f64>().ok().filter(|usd| *usd > 0.0);
                args.max_cost = Some(usd.ok_or_else(|| format!("--max-cost needs an amount in dollars, got {v}"))?);
            }
            "-h" | "--help" => return Ok(Cli::Show(USAGE.into())),
            "-V" | "--version" => return Ok(Cli::Show(format!("claude-jr {}", env!("CARGO_PKG_VERSION")))),
            _ => return Err(format!("unknown argument: {a}\n\n{USAGE}")),
        }
    }
    if args.prompt.as_ref().is_some_and(|p| p.trim().is_empty()) {
        return Err("-p needs a prompt; it was empty".into());
    }
    Ok(Cli::Run(args))
}

/// The prompt for a bare `-p`: standard input, when something is piped into it.
fn piped() -> Result<String, String> {
    use std::io::{IsTerminal, Read};
    let mut bytes = Vec::new();
    if std::io::stdin().is_terminal() || std::io::stdin().read_to_end(&mut bytes).is_err() || bytes.is_empty() {
        return Err("-p needs a value".into());
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
