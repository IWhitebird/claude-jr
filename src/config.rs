// Settings from ~/.config/claude-jr/config.json, then ./.claude-jr.json.
//
// {
//   "model": "claude-sonnet-5-5",
//   "effort": "high",
//   "allow": ["bash:cargo *", "bash:git status", "edit"]
// }

use serde::Deserialize;
use std::path::PathBuf;

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub model: Option<String>,
    pub effort: Option<String>,
    #[serde(default)]
    pub allow: Vec<String>,
}

impl Config {
    pub fn load() -> Result<Config, String> {
        let mut cfg = Config::default();
        for path in [config_dir().join("config.json"), PathBuf::from(".claude-jr.json")] {
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let file: Config = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
            cfg.model = file.model.or(cfg.model);
            cfg.effort = file.effort.or(cfg.effort);
            cfg.allow.extend(file.allow);
        }
        Ok(cfg)
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config")).join("claude-jr")
}

pub fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/share"))
        .join("claude-jr")
}
