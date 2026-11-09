// Settings from ~/.config/claude-jr/config.json, then ./.claude-jr.json.
//
// {
//   "model": "claude-sonnet-5-5",
//   "effort": "high",
//   "allow": ["bash:cargo *", "bash:git status", "edit"],
//   "sandbox": { "enabled": true, "read": ["/opt/sdk"], "write": ["/home/me/.m2"] },
//   "mcp_servers": { "files": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "."] } }
// }

use crate::tools::mcp::ServerConfig;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub model: Option<String>,
    pub effort: Option<String>,
    /// The model auto mode uses to review commands (default: claude-haiku-5-5).
    pub auto_model: Option<String>,
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub sandbox: SandboxConfig,
    /// MCP servers, in the same shape as .mcp.json's "mcpServers".
    #[serde(default)]
    pub mcp_servers: BTreeMap<String, ServerConfig>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct SandboxConfig {
    /// Always run sandboxed, as if --sandbox were given.
    #[serde(default)]
    pub enabled: bool,
    /// Extra paths the sandbox can read.
    #[serde(default)]
    pub read: Vec<String>,
    /// Extra paths the sandbox can write.
    #[serde(default)]
    pub write: Vec<String>,
}

impl Config {
    pub fn load() -> Result<Config, String> {
        let mut cfg = Config::default();
        for path in [config_dir().join("config.json"), PathBuf::from(".claude-jr.json")] {
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let file: Config = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
            cfg.model = file.model.or(cfg.model);
            cfg.effort = file.effort.or(cfg.effort);
            cfg.auto_model = file.auto_model.or(cfg.auto_model);
            cfg.mcp_servers.extend(file.mcp_servers);
            cfg.allow.extend(file.allow);
            cfg.sandbox.enabled |= file.sandbox.enabled;
            cfg.sandbox.read.extend(file.sandbox.read);
            cfg.sandbox.write.extend(file.sandbox.write);
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
