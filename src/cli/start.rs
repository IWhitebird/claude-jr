// Starting a session, interactive or -p: the checks both share (is this
// folder trusted, is there a key for the model, the sandbox, the worktree),
// then the agent's settings, then the UI or the one prompt.

use super::args::Args;
use super::print;
use crate::agent::{self, Mode};
use crate::api::{cloud, provider};
use crate::store::config;
use crate::store::session::{Pick, Session};
use crate::sys::sandbox;
use crate::ui;
use crate::work::worktree::{self, Worktree};
use std::io::IsTerminal;
use std::process::ExitCode;

pub fn run(args: Args) -> Result<ExitCode, String> {
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
    // (Inside the sandbox, the worktree was made before it was entered.)
    let worktree = match Worktree::current() {
        None if args.worktree => {
            // A task's worktree and branch carry the task's id.
            let id = std::env::var("CLAUDE_JR_ID").unwrap_or_else(|_| worktree::new_id());
            Some(Worktree::create(&id)?)
        }
        current => current,
    };
    if (args.sandbox || file.sandbox.enabled) && !sandbox::active() {
        return Err(sandbox::exec(&file.sandbox));
    }
    let model = args
        .model
        .or_else(|| std::env::var("CLAUDE_JR_MODEL").ok())
        .or(file.model)
        .unwrap_or_else(|| "claude-opus-5-5".into());
    let key = config::api_key().unwrap_or_default();
    // A model of another provider needs that provider's key, not Anthropic's.
    let other = provider::resolve(&model, &file.providers).map(|(p, _)| p);
    let cloud = other.is_none() && cloud::of(&model).is_some();
    match &other {
        Some(p) => {
            if let Some(why) = p.missing_key() {
                return Err(why);
            }
        }
        None if cloud => {
            if let Some(why) = cloud::missing(&model, &cloud::environment) {
                return Err(why);
            }
        }
        None if model.contains('/') && !interactive => {
            let known = provider::names(&file.providers).join(", ");
            return Err(format!("{model}: no provider by that name (there are: {known})"));
        }
        None if key.is_empty() && !interactive => {
            return Err("no API key: run claudejr and type /login, or set ANTHROPIC_API_KEY".into());
        }
        None => {}
    }
    // In a script, a mistyped name must not quietly start a new conversation.
    if let (false, Some(Pick::Named(name))) = (interactive, &args.resume)
        && Session::load(&Pick::Named(name.clone())).is_none()
    {
        return Err(format!("no saved session named \"{name}\" here (see claudejr --sessions)"));
    }
    if interactive && !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        return Err("not a terminal; use -p PROMPT for non-interactive use".into());
    }

    let cfg = agent::Config {
        key,
        base_url: std::env::var("ANTHROPIC_BASE_URL").ok(),
        // The reviewer is asked through the same provider as the model.
        auto_model: file.auto_model.unwrap_or_else(|| match other.is_some() || cloud {
            true => model.clone(),
            false => "claude-haiku-5-5".into(),
        }),
        model,
        effort: args.effort.or(file.effort),
        mode: if args.yes { Mode::Bypass } else { file.mode.as_deref().and_then(Mode::parse).unwrap_or(Mode::Default) },
        allow: file.allow,
        resume: args.resume,
        mcp: file.mcp_servers,
        web: file.web_tools.unwrap_or(true),
        interactive,
        providers: file.providers,
    };

    match args.prompt {
        Some(prompt) => {
            let options = print::Options {
                format: args.format,
                max_cost: args.max_cost,
                log: args.log,
                worktree,
                stopped: Default::default(),
            };
            Ok(if print::run(cfg, prompt, options)? { ExitCode::SUCCESS } else { ExitCode::FAILURE })
        }
        None => {
            let bottom = file.layout.as_deref() != Some("inline");
            ui::run(cfg, bottom).map_err(|e| format!("terminal error: {e}"))?;
            if let Some(worktree) = worktree {
                println!("{}", worktree::summary(&worktree.finish(None)));
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}
