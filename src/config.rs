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
use std::path::{Path, PathBuf};

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
        Config::load_in(&std::env::current_dir().unwrap_or_default())
    }

    /// The user's config, then the project's in `dir` if that folder is trusted.
    fn load_in(dir: &Path) -> Result<Config, String> {
        let mut cfg = Config::default();
        let project = is_trusted_dir(dir).then(|| dir.join(".claude-jr.json"));
        for path in std::iter::once(config_dir().join("config.json")).chain(project) {
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
    [".claude-jr.json", ".mcp.json"].into_iter().filter(|f| Path::new(f).exists()).collect()
}

/// Has the user said to trust the current folder's project files?
pub fn is_trusted() -> bool {
    std::env::current_dir().is_ok_and(|d| is_trusted_dir(&origin(&d)))
}

/// A worktree made by --worktree is as trusted as the folder it is a copy of.
fn origin(dir: &Path) -> PathBuf {
    match crate::worktree::Worktree::current() {
        Some(w) => dir.strip_prefix(&w.path).map_or_else(|_| dir.to_path_buf(), |rest| w.repo.join(rest)),
        None => dir.to_path_buf(),
    }
}

fn is_trusted_dir(dir: &Path) -> bool {
    let Ok(dir) = dir.canonicalize() else { return false };
    let Ok(text) = std::fs::read_to_string(trusted_file()) else { return false };
    text.lines().any(|l| Path::new(l) == dir)
}

/// Adds the current folder to the trusted list.
pub fn trust() -> Result<(), String> {
    trust_dir(&std::env::current_dir().map_err(|e| e.to_string())?)
}

fn trust_dir(dir: &Path) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let dir = dir.canonicalize().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(config_dir()).map_err(|e| e.to_string())?;
    let path = trusted_file();
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
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

    /// Tests that write the user's config or credentials, which every test in
    /// this process shares, take this so they run one at a time.
    static FILES: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn files() -> std::sync::MutexGuard<'static, ()> {
        crate::tools::test_home();
        FILES.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn project_config_needs_trust() {
        let _g = files();
        let dir = crate::tools::test_dir("trust");
        std::fs::write(dir.join(".claude-jr.json"), r#"{"mode":"bypass"}"#).unwrap();
        assert!(!is_trusted_dir(&dir));
        assert_eq!(Config::load_in(&dir).unwrap().mode, None);
        trust_dir(&dir).unwrap();
        assert!(is_trusted_dir(&dir));
        assert_eq!(Config::load_in(&dir).unwrap().mode.as_deref(), Some("bypass"));
    }

    fn reset_user_files() {
        let _ = std::fs::remove_file(config_dir().join("config.json"));
    }

    #[test]
    fn user_and_trusted_project_settings_are_merged() {
        let _g = files();
        let dir = crate::tools::test_dir("config-merge");
        std::fs::create_dir_all(config_dir()).unwrap();
        std::fs::write(
            config_dir().join("config.json"),
            r#"{"model":"m1","effort":"low","allow":["bash:ls"],"layout":"inline","web_tools":false,
                "sandbox":{"enabled":false,"read":["/r1"]},
                "mcp_servers":{"files":{"command":"npx","args":["-y","srv"]}}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join(".claude-jr.json"),
            r#"{"model":"m2","auto_model":"h","allow":["edit"],"sandbox":{"enabled":true,"write":["/w"]},
                "mcp_servers":{"web":{"url":"https://x.test/mcp"}}}"#,
        )
        .unwrap();

        // Untrusted: the project file is ignored.
        let cfg = Config::load_in(&dir).unwrap();
        assert_eq!(cfg.model.as_deref(), Some("m1"));
        assert_eq!(cfg.allow, ["bash:ls"]);
        assert!(!cfg.sandbox.enabled);

        trust_dir(&dir).unwrap();
        let cfg = Config::load_in(&dir).unwrap();
        assert_eq!(cfg.model.as_deref(), Some("m2"), "the project's value wins");
        assert_eq!(cfg.effort.as_deref(), Some("low"), "what it leaves out comes from the user");
        assert_eq!(cfg.auto_model.as_deref(), Some("h"));
        assert_eq!(cfg.layout.as_deref(), Some("inline"));
        assert_eq!(cfg.web_tools, Some(false));
        assert_eq!(cfg.allow, ["bash:ls", "edit"], "lists add up");
        assert!(cfg.sandbox.enabled, "sandbox is on if either file turns it on");
        assert_eq!(cfg.sandbox.read, ["/r1"]);
        assert_eq!(cfg.sandbox.write, ["/w"]);
        assert_eq!(cfg.mcp_servers.keys().collect::<Vec<_>>(), ["files", "web"]);
        reset_user_files();
    }

    #[test]
    fn a_broken_config_names_the_file() {
        let _g = files();
        let dir = crate::tools::test_dir("config-broken");
        std::fs::create_dir_all(config_dir()).unwrap();
        std::fs::write(config_dir().join("config.json"), "{ not json").unwrap();
        let Err(e) = Config::load_in(&dir) else { panic!("should not load") };
        assert!(e.starts_with(&config_dir().join("config.json").display().to_string()), "{e}");

        // Unknown keys are mistakes too, so typos don't go unnoticed.
        std::fs::write(config_dir().join("config.json"), r#"{"modle":"x"}"#).unwrap();
        let Err(e) = Config::load_in(&dir) else { panic!("should not load") };
        assert!(e.contains("unknown field `modle`"), "{e}");
        reset_user_files();
        assert!(Config::load_in(&dir).is_ok(), "no file at all is fine");
    }

    #[test]
    fn settings_are_set_by_dotted_key_and_keep_the_rest() {
        let _g = files();
        reset_user_files();
        assert_eq!(user_settings(), serde_json::json!({}));
        set_user_setting("effort", serde_json::json!("high")).unwrap();
        set_user_setting("sandbox.enabled", serde_json::json!(true)).unwrap();
        set_user_setting("sandbox.read", serde_json::json!(["/a"])).unwrap();
        assert_eq!(
            user_settings(),
            serde_json::json!({"effort": "high", "sandbox": {"enabled": true, "read": ["/a"]}})
        );
        // A value in the way of a nested key is replaced by an object.
        set_user_setting("effort.deep", serde_json::json!(1)).unwrap();
        assert_eq!(user_settings()["effort"], serde_json::json!({"deep": 1}));
        let text = std::fs::read_to_string(config_dir().join("config.json")).unwrap();
        assert!(text.ends_with("}\n") && text.contains("\n  \"sandbox\""), "pretty printed: {text}");
        std::fs::write(config_dir().join("config.json"), "garbage").unwrap();
        assert_eq!(user_settings(), serde_json::json!({}), "an unreadable file counts as empty");
        reset_user_files();
    }

    #[test]
    fn the_api_key_is_saved_private_and_removed() {
        use std::os::unix::fs::PermissionsExt;
        let _g = files();
        std::fs::create_dir_all(config_dir()).unwrap();
        let path = credentials();
        // A looser file that already exists is tightened.
        std::fs::write(&path, "old-and-much-longer-key").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(save_api_key("sk-abc\n").unwrap(), path);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "sk-abc\n", "the old content is replaced");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        // ANTHROPIC_API_KEY, if set outside, takes precedence over the file.
        let from_env = std::env::var_os("ANTHROPIC_API_KEY").is_some();
        if !from_env {
            assert_eq!(api_key().as_deref(), Some("sk-abc"), "whitespace is trimmed");
            save_api_key("  \n").unwrap();
            assert_eq!(api_key(), None, "a blank key is no key");
        }
        delete_api_key().unwrap();
        assert!(!path.exists());
        delete_api_key().unwrap();
        if !from_env {
            assert_eq!(api_key(), None);
        }
    }

    #[test]
    fn the_current_folder_is_trusted_only_after_trust() {
        let _g = files();
        reset_user_files();
        assert!(!is_trusted());
        assert!(Config::load().is_ok());
        trust().unwrap();
        assert!(is_trusted());
        let last = std::fs::read_to_string(trusted_file()).unwrap();
        let here = std::env::current_dir().unwrap().canonicalize().unwrap();
        assert_eq!(last.lines().last(), Some(here.to_str().unwrap()));
        for f in project_files() {
            assert!(Path::new(f).exists());
        }
    }

    #[test]
    fn folders_follow_the_xdg_variables() {
        crate::tools::test_home();
        let home = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap();
        assert_eq!(config_dir(), home.join("claude-jr"));
        let data = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap();
        assert_eq!(data_dir(), data.join("claude-jr"));
        assert_eq!(trusted_file(), config_dir().join("trusted"));
    }
}
