// The shell commands run in: bash everywhere it exists. On Windows that is
// the bash that comes with Git; without it, PowerShell. On a Unix system with
// no bash (a small container: Alpine, busybox) it is sh.
//
// CLAUDE_JR_SHELL names another one (a path to bash or sh, or to powershell or pwsh).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

pub struct Shell {
    program: PathBuf,
    powershell: bool,
    /// sh, with none of what bash adds to it.
    plain: bool,
}

/// The shell on this machine, looked for once.
pub fn shell() -> &'static Shell {
    static SHELL: OnceLock<Shell> = OnceLock::new();
    SHELL.get_or_init(|| {
        let program = std::env::var_os("CLAUDE_JR_SHELL").filter(|s| !s.is_empty()).map(PathBuf::from);
        let program = program.unwrap_or_else(find);
        Shell::of(program)
    })
}

/// Whether a program is installed.
pub fn on_path(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| {
        std::env::split_paths(&p).any(|d| d.join(format!("{name}{}", std::env::consts::EXE_SUFFIX)).is_file())
    })
}

fn stem(program: &Path) -> String {
    program.file_stem().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default()
}

#[cfg(unix)]
fn find() -> PathBuf {
    if on_path("bash") || !on_path("sh") { "bash" } else { "sh" }.into()
}

/// Git's bash, from where Git is usually installed or from beside the git on
/// PATH. Not a plain "bash": on Windows that name is often WSL's launcher,
/// which runs in another system with other paths.
#[cfg(windows)]
fn find() -> PathBuf {
    let roots = ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)", "LOCALAPPDATA"];
    let installed = roots.iter().filter_map(std::env::var_os).flat_map(|root| {
        let root = PathBuf::from(root);
        [root.join("Git"), root.join("Programs").join("Git")]
    });
    let path = std::env::var_os("PATH").unwrap_or_default();
    // ...\Git\cmd\git.exe: bash is at ...\Git\bin\bash.exe.
    let beside_git = std::env::split_paths(&path)
        .filter(|dir| dir.join("git.exe").is_file())
        .filter_map(|dir| dir.parent().map(Path::to_path_buf));
    installed
        .chain(beside_git)
        .map(|git| git.join("bin").join("bash.exe"))
        .find(|bash| bash.is_file())
        .unwrap_or_else(|| "powershell".into())
}

impl Shell {
    fn of(program: PathBuf) -> Shell {
        let name = stem(&program);
        Shell {
            powershell: matches!(name.as_str(), "powershell" | "pwsh"),
            plain: matches!(name.as_str(), "sh" | "ash" | "dash"),
            program,
        }
    }

    /// The program, for starting it some other way.
    #[cfg(test)]
    pub fn program(&self) -> String {
        self.program.display().to_string()
    }

    /// What to call it when talking to the model.
    pub fn name(&self) -> &'static str {
        if self.powershell {
            "PowerShell"
        } else if self.plain {
            "sh"
        } else {
            "bash"
        }
    }

    /// The shell, about to run `script`.
    pub fn command(&self, script: &str) -> Command {
        let mut cmd = Command::new(&self.program);
        if self.powershell {
            cmd.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", script]);
        } else {
            cmd.arg("-c").arg(script);
        }
        cmd
    }

    /// `cmd` with its errors on standard output, as the model reads one stream.
    pub fn merged(&self, cmd: &str) -> String {
        if self.powershell { format!("& {{\n{cmd}\n}} 2>&1") } else { format!("exec 2>&1\n{cmd}") }
    }

    /// The same, and afterwards the folder it ended in is written to `cwd_file`
    /// (as a path this system's own programs understand), so `cd` carries over.
    pub fn reporting(&self, cmd: &str, cwd_file: &Path) -> String {
        let file = cwd_file.display();
        if self.powershell {
            return format!(
                "& {{\n{cmd}\n}} 2>&1\n$rc = if ($LASTEXITCODE) {{ $LASTEXITCODE }} elseif ($?) {{ 0 }} else {{ 1 }}\n\
                 [IO.File]::WriteAllText('{file}', (Get-Location).Path)\nexit $rc"
            );
        }
        // Git's bash says /c/Users/...; -W gives the Windows form of it.
        let pwd = if cfg!(windows) { "pwd -W" } else { "pwd" };
        format!("exec 2>&1\n{{\n{cmd}\n}}\n__rc=$?\n{pwd} > '{file}'\nexit $__rc")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_shell_gets_its_own_wrapping() {
        let bash = Shell::of("bash".into());
        assert_eq!(bash.name(), "bash");
        assert_eq!(bash.merged("ls"), "exec 2>&1\nls");
        let script = bash.reporting("cd sub", Path::new("/tmp/x.cwd"));
        assert!(script.starts_with("exec 2>&1\n{\ncd sub\n}\n__rc=$?\n") && script.ends_with("exit $__rc"), "{script}");
        assert!(script.contains(" > '/tmp/x.cwd'"));
        let args: Vec<_> = bash.command("ls").get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(args, ["-c", "ls"]);

        for program in ["powershell", "C:\\Program Files\\PowerShell\\7\\pwsh.exe", "PWSH"] {
            assert!(Shell::of(program.into()).powershell || cfg!(unix) && program.contains('\\'), "{program}");
        }
        assert_eq!(Shell::of("/usr/bin/bash".into()).name(), "bash");
        // A plain sh is wrapped the way bash is, and called what it is.
        for program in ["sh", "/bin/sh", "/bin/ash", "dash"] {
            let sh = Shell::of(program.into());
            assert_eq!((sh.name(), sh.merged("ls")), ("sh", bash.merged("ls")), "{program}");
        }
        let ps = Shell::of("powershell".into());
        assert_eq!(ps.name(), "PowerShell");
        assert_eq!(ps.merged("dir"), "& {\ndir\n} 2>&1");
        let script = ps.reporting("cd sub", Path::new("C:\\t\\x.cwd"));
        assert!(script.contains("WriteAllText('C:\\t\\x.cwd', (Get-Location).Path)") && script.ends_with("exit $rc"));
        let args: Vec<_> = ps.command("dir").get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(args.last().unwrap(), "dir");
        assert!(args.contains(&"-NoProfile".to_string()));
    }

    #[test]
    fn the_shell_here_runs_a_command() {
        let out = shell().command("echo works").output().unwrap();
        assert!(String::from_utf8_lossy(&out.stdout).contains("works"));
    }
}
