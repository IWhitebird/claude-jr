// --sandbox: run claude-jr again inside bubblewrap, so the kernel enforces the
// limits for every tool, not just bash.
//
//   writable   the project directory, claude-jr's data dir, package caches,
//              a private /tmp
//   read-only  the rest of the system, plus toolchains under $HOME
//   hidden     everything else in $HOME (ssh keys, tokens, other projects),
//              the session's sockets ($XDG_RUNTIME_DIR: D-Bus, ssh and gpg
//              agents, the desktop) and container engines' sockets
//   read-only  the project's git hooks and git config, which would otherwise
//              let a command plant something that runs outside the sandbox
//              on your next git command
//
// The network stays on, because the agent needs the API.

use crate::config::{SandboxConfig, config_dir, data_dir};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Set inside the sandbox, so we don't wrap twice and the UI can say so.
pub const ENV: &str = "CLAUDE_JR_SANDBOX";

/// Toolchains and settings that commands commonly need, mounted read-only.
const HOME_READ: &[&str] = &[
    ".cargo",
    ".rustup",
    ".gitconfig",
    ".config/git",
    ".local/bin",
    ".nvm",
    ".bun",
    ".deno",
    ".pyenv",
    ".local/lib",
    ".npmrc",
    ".tool-versions",
    ".asdf",
    ".sdkman",
    ".go",
    "go/bin",
];

/// Sockets of container engines: talking to one is a way out of any sandbox
/// (it can start a container with the whole disk mounted).
const ENGINE_SOCKETS: &[&str] =
    &["/run/docker.sock", "/var/run/docker.sock", "/run/podman/podman.sock", "/run/containerd/containerd.sock"];

/// Caches that builds write to. Losing them costs a re-download, nothing more.
const HOME_WRITE: &[&str] = &[".cargo/registry", ".cargo/git", ".cache", ".npm", "go/pkg"];

pub fn active() -> bool {
    std::env::var_os(ENV).is_some()
}

/// Replaces this process with a sandboxed copy of itself. Only returns on error.
pub fn exec(cfg: &SandboxConfig) -> String {
    if !cfg!(target_os = "linux") {
        return "--sandbox needs Linux (it uses bubblewrap)".into();
    }
    let Ok(cwd) = std::env::current_dir() else { return "can't read the working directory".into() };
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    if cwd == home || cwd == Path::new("/") {
        return "run --sandbox from a project directory, not from / or your home directory".into();
    }
    let Ok(exe) = std::env::current_exe() else { return "can't find the claude-jr binary".into() };
    let data = data_dir();
    let _ = std::fs::create_dir_all(&data);

    let mut cmd = Command::new("bwrap");
    cmd.args(["--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc", "--tmpfs", "/tmp"]);
    cmd.args(["--unshare-pid", "--unshare-ipc", "--unshare-uts", "--die-with-parent"]);
    if !home.as_os_str().is_empty() {
        cmd.arg("--tmpfs").arg(&home);
        for p in HOME_READ.iter().map(|p| home.join(p)).chain(cfg.read.iter().map(PathBuf::from)) {
            cmd.arg("--ro-bind-try").arg(&p).arg(&p);
        }
        for p in HOME_WRITE.iter().map(|p| home.join(p)).chain(cfg.write.iter().map(PathBuf::from)) {
            cmd.arg("--bind-try").arg(&p).arg(&p);
        }
    }
    // Covered with an empty file, so connecting to them fails.
    let mut covered = Vec::new();
    for sock in ENGINE_SOCKETS.iter().map(Path::new).filter(|p| p.exists()) {
        let real = sock.canonicalize().unwrap_or_else(|_| sock.to_path_buf());
        if !covered.contains(&real) {
            cmd.arg("--ro-bind").arg("/dev/null").arg(&real);
            covered.push(real);
        }
    }
    if let Some(run) = std::env::var_os("XDG_RUNTIME_DIR").filter(|r| Path::new(r).is_dir()) {
        cmd.arg("--tmpfs").arg(run);
    }
    for var in ["SSH_AUTH_SOCK", "DBUS_SESSION_BUS_ADDRESS", "GPG_AGENT_INFO", "DOCKER_HOST"] {
        cmd.args(["--unsetenv", var]);
    }
    let config = config_dir();
    cmd.arg("--ro-bind-try").arg(&config).arg(&config);
    cmd.arg("--ro-bind").arg(&exe).arg(&exe);
    cmd.arg("--bind").arg(&data).arg(&data);
    cmd.arg("--bind").arg(&cwd).arg(&cwd);
    // A git hook runs on your next git command, outside the sandbox; the config
    // can point git at hooks elsewhere. With no hooks folder yet, an empty one
    // that vanishes at the end, so none can be created either.
    let git = cwd.join(".git");
    if git.is_dir() {
        let hooks = git.join("hooks");
        if hooks.is_dir() {
            cmd.arg("--ro-bind").arg(&hooks).arg(&hooks);
        } else {
            cmd.arg("--tmpfs").arg(&hooks);
        }
        if git.join("config").is_file() {
            cmd.arg("--ro-bind").arg(git.join("config")).arg(git.join("config"));
        }
    }
    cmd.arg("--chdir").arg(&cwd);
    cmd.args(["--setenv", ENV, "1"]);
    cmd.arg("--").arg(&exe).args(std::env::args_os().skip(1));

    let err = cmd.exec();
    if err.kind() == std::io::ErrorKind::NotFound {
        "--sandbox needs bubblewrap: install it with your package manager (bwrap)".into()
    } else {
        format!("could not start the sandbox: {err}")
    }
}
