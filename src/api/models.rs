// What is known about each Claude model: what it supports beyond the basic
// Messages API, what it costs, and how much context it has.

use super::Usage;
use super::provider::{self, Providers};

pub(super) const BETA_FALLBACK: &str = "server-side-fallback-2026-07-01";
pub(super) const BETA_UPDATES: &str = "thinking-display-updates-2026-08-18";
pub(super) const BETA_COMPACT: &str = "compact-2026-01-12";
pub(super) const BETA_MCP: &str = "mcp-client-2025-11-20";

/// What a model supports beyond the basic Messages API.
#[derive(Clone, Copy)]
pub struct Caps {
    /// Server-side refusal fallback (`fallbacks: "default"`).
    pub fallback: bool,
    /// Progress notes between tool calls as visible thinking text.
    pub updates: bool,
    /// Server-side compaction near the context limit.
    pub compact: bool,
    /// Context window in tokens.
    pub window: u64,
    /// Anthropic's web search and fetch tools (the 2026-02-09 versions).
    pub web: bool,
}

impl Caps {
    pub fn of(model: &str) -> Caps {
        let (fallback, updates, compact) = match model {
            "claude-opus-5-5" | "claude-sonnet-5-5" | "claude-fable-5-2" | "claude-fable-5-1" => (true, true, true),
            "claude-opus-5" => (true, false, true),
            "claude-fable-5" => (false, true, true),
            "claude-haiku-5-5" | "claude-sonnet-5" | "claude-opus-4-8" | "claude-opus-4-7" | "claude-opus-4-6"
            | "claude-sonnet-4-6" => (false, false, true),
            // A newer version than this build knows: assume what its family
            // has, but leave out the refusal fallback, which is per model and
            // makes the API reject the request where it isn't supported.
            m if family(m) == Some("haiku") => (false, false, true),
            m if family(m).is_some() => (false, true, true),
            _ => (false, false, false),
        };
        let window = if model.starts_with("claude-haiku-4") { 200_000 } else { 1_000_000 };
        // Known models by name; newer Opus and Sonnet versions by family.
        let web = matches!(
            model,
            "claude-opus-5-5"
                | "claude-opus-5"
                | "claude-opus-4-8"
                | "claude-opus-4-7"
                | "claude-opus-4-6"
                | "claude-sonnet-5-5"
                | "claude-sonnet-5"
                | "claude-sonnet-4-6"
                | "claude-fable-5-2"
                | "claude-fable-5-1"
                | "claude-fable-5"
        ) || matches!(family(model), Some("opus" | "sonnet"));
        Caps { fallback, updates, compact, window, web }
    }

    pub(super) fn betas(self) -> String {
        let all = [(self.fallback, BETA_FALLBACK), (self.updates, BETA_UPDATES), (self.compact, BETA_COMPACT)];
        all.iter().filter(|(on, _)| *on).map(|(_, b)| *b).collect::<Vec<_>>().join(",")
    }
}

/// Models /model offers, with what each is good for. Any other model id
/// works too with /model <id>.
pub const CATALOGUE: [(&str, &str); 7] = [
    ("claude-opus-5-5", "default; strong at long coding tasks"),
    ("claude-sonnet-5-5", "fast, half the price; everyday edits"),
    ("claude-haiku-5-5", "fastest and cheapest; small changes"),
    ("claude-fable-5-2", "most capable; hardest problems"),
    ("claude-opus-5", "previous Opus"),
    ("claude-sonnet-5", "previous Sonnet"),
    ("claude-opus-4-8", "older Opus"),
];

/// "$4/$20 · 1M context": dollars per million tokens in and out.
pub fn describe(model: &str) -> String {
    let window = match Caps::of(model).window {
        w if w >= 1_000_000 => format!("{}M", w / 1_000_000),
        w => format!("{}k", w / 1000),
    };
    match price(model) {
        Some((i, o, _)) => format!("${i}/${o} · {window} context"),
        None => format!("{window} context"),
    }
}

/// "opus", "sonnet", "haiku" or "fable" for a Claude 4.6-or-later model id
/// (Mythos counts as Fable), including versions newer than this build.
pub(super) fn family(model: &str) -> Option<&'static str> {
    let rest = model.strip_prefix("claude-")?;
    let (name, version) = rest.split_once('-')?;
    let major: u32 = version.split('-').next()?.parse().ok()?;
    if major < 5 && !matches!(rest, "opus-4-6" | "opus-4-7" | "opus-4-8" | "sonnet-4-6") {
        return None;
    }
    Some(match name {
        "opus" => "opus",
        "sonnet" => "sonnet",
        "haiku" => "haiku",
        "fable" | "mythos" => "fable",
        _ => return None,
    })
}

/// Dollars per million tokens: input, output, cache read. Cache writes are 1.25x input.
/// Unknown newer versions are priced like the latest one this build knows
/// in their family: an estimate, until the table is updated.
pub(super) fn price(model: &str) -> Option<(f64, f64, f64)> {
    Some(match model {
        "claude-opus-5-5" => (4.0, 20.0, 0.20),
        "claude-sonnet-5-5" => (2.0, 10.0, 0.10),
        "claude-sonnet-5" => (2.0, 10.0, 0.20),
        "claude-fable-5-2" => (8.0, 40.0, 0.20),
        "claude-haiku-5-5" => (0.10, 0.50, 0.01),
        "claude-fable-5-1" => (10.0, 50.0, 0.25),
        "claude-fable-5" => (10.0, 50.0, 1.0),
        "claude-opus-5" | "claude-opus-4-8" | "claude-opus-4-7" | "claude-opus-4-6" => (5.0, 25.0, 0.50),
        "claude-sonnet-4-6" => (3.0, 15.0, 0.30),
        "claude-haiku-4-5" => (1.0, 5.0, 0.10),
        m => {
            return price(match family(m)? {
                "opus" => "claude-opus-5-5",
                "sonnet" => "claude-sonnet-5-5",
                "haiku" => "claude-haiku-5-5",
                _ => "claude-fable-5-2",
            });
        }
    })
}

pub fn cost(model: &str, u: &Usage) -> Option<f64> {
    let (i, o, r) = price(model)?;
    let tokens = u.input_tokens as f64 * i
        + u.cache_creation_input_tokens as f64 * i * 1.25
        + u.cache_read_input_tokens as f64 * r
        + u.output_tokens as f64 * o;
    Some(tokens / 1e6)
}

/// The context size assumed for a model of another provider that the config
/// says nothing about.
const OTHER_WINDOW: u64 = 128_000;

/// The context size of `model` in tokens.
pub fn window(model: &str, providers: &Providers) -> u64 {
    match provider::resolve(model, providers) {
        Some((p, _)) => p.context.unwrap_or(OTHER_WINDOW),
        None => Caps::of(model).window,
    }
}
