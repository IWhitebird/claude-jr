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
    /// "bottom" (the default) keeps the input on the last rows of the screen;
    /// "inline" puts it right after the conversation.
    pub layout: Option<String>,
    /// The mode to start in: normal, accept-edits, plan, auto, skip-permissions.
    pub mode: Option<String>,
    /// Set to false to turn off Anthropic's web search and fetch tools.
    pub web_tools: Option<bool>,
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
            if path.as_os_str() == ".claude-jr.json" && !is_trusted() {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let file: Config = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
            cfg.model = file.model.or(cfg.model);
            cfg.effort = file.effort.or(cfg.effort);
            cfg.auto_model = file.auto_model.or(cfg.auto_model);
            cfg.web_tools = file.web_tools.or(cfg.web_tools);
            cfg.layout = file.layout.or(cfg.layout);
            cfg.mode = file.mode.or(cfg.mode);
            cfg.mcp_servers.extend(file.mcp_servers);
            cfg.allow.extend(file.allow);
            cfg.sandbox.enabled |= file.sandbox.enabled;
            cfg.sandbox.read.extend(file.sandbox.read);
            cfg.sandbox.write.extend(file.sandbox.write);
        }
        Ok(cfg)
    }
}

fn trusted_file() -> PathBuf {
    config_dir().join("trusted")
}

/// Whichever of the project files that can change settings exist in the current folder.
pub fn project_files() -> Vec<&'static str> {
    [".claude-jr.json", ".mcp.json"].into_iter().filter(|f| std::path::Path::new(f).exists()).collect()
}

/// Has the user said to trust the current folder's project files?
pub fn is_trusted() -> bool {
    let Ok(dir) = std::env::current_dir().and_then(|d| d.canonicalize()) else { return false };
    let Ok(text) = std::fs::read_to_string(trusted_file()) else { return false };
    text.lines().any(|l| std::path::Path::new(l) == dir)
}

/// Adds the current folder to the trusted list.
pub fn trust() -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let dir = std::env::current_dir().and_then(|d| d.canonicalize()).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(config_dir()).map_err(|e| e.to_string())?;
    let path = trusted_file();
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    f.set_permissions(std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    writeln!(f, "{}", dir.display()).map_err(|e| e.to_string())
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

/// The user's config file, as JSON (empty if there isn't one).
pub fn user_settings() -> serde_json::Value {
    std::fs::read_to_string(config_dir().join("config.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| serde_json::json!({}))
}

/// Sets one value in the user's config file, keeping everything else in it.
/// `key` can be nested with dots: "sandbox.enabled".
pub fn set_user_setting(key: &str, value: serde_json::Value) -> std::io::Result<()> {
    let mut root = user_settings();
    let mut node = &mut root;
    let mut parts = key.split('.').peekable();
    while let Some(part) = parts.next() {
        if !node.is_object() {
            *node = serde_json::json!({});
        }
        if parts.peek().is_none() {
            node[part] = value;
            break;
        }
        node = &mut node[part];
    }
    std::fs::create_dir_all(config_dir())?;
    let text = serde_json::to_string_pretty(&root).map_err(std::io::Error::other)?;
    std::fs::write(config_dir().join("config.json"), text + "\n")
}

/// Where /login keeps the API key.
fn credentials() -> PathBuf {
    config_dir().join("credentials")
}

/// The API key: ANTHROPIC_API_KEY if set, else the one saved with /login.
pub fn api_key() -> Option<String> {
    std::env::var("ANTHROPIC_API_KEY")
        .ok()
        .or_else(|| std::fs::read_to_string(credentials()).ok())
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
}

/// Saves the key so only this user can read it.
pub fn save_api_key(key: &str) -> std::io::Result<PathBuf> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let path = credentials();
    std::fs::create_dir_all(config_dir())?;
    let mut f = std::fs::OpenOptions::new().create(true).write(true).truncate(true).mode(0o600).open(&path)?;
    // The mode above only applies when the file is created.
    use std::os::unix::fs::PermissionsExt;
    f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    f.write_all(key.as_bytes())?;
    Ok(path)
}

pub fn delete_api_key() -> std::io::Result<()> {
    match std::fs::remove_file(credentials()) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The current folder is process-wide, so both cases share one test.
    #[test]
    fn project_config_needs_trust() {
        crate::tools::test_home();
        let old = std::env::current_dir().unwrap();
        let dir = crate::tools::test_dir("trust");
        std::fs::write(dir.join(".claude-jr.json"), r#"{"mode":"bypass"}"#).unwrap();
        std::env::set_current_dir(&dir).unwrap();
        assert!(!is_trusted());
        assert_eq!(Config::load().unwrap().mode, None);
        trust().unwrap();
        assert!(is_trusted());
        assert_eq!(Config::load().unwrap().mode.as_deref(), Some("bypass"));
        std::env::set_current_dir(old).unwrap();
    }
}
