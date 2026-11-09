# Plan 001: Ask before trusting a folder's .claude-jr.json and .mcp.json

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. If a
> STOP condition occurs, stop and report; don't improvise.
>
> **Drift check (run first)**: `git diff --stat 890677c..HEAD -- src/config.rs src/main.rs src/tools/mcp.rs`
> If anything changed, compare the excerpts below with the live code; on a mismatch, STOP.

## Status

- **Priority**: P1 · **Effort**: M · **Risk**: MED · **Depends on**: none · **Category**: security
- **Planned at**: commit `890677c`, 2026-11-10

## Why this matters

claude-jr is a Rust terminal coding agent. When it starts, it reads
`.claude-jr.json` and `.mcp.json` from the current folder without asking. A
cloned repo can therefore turn on skip-permissions mode (`"mode": "bypass"`),
add allow rules such as `"bash"`, and start arbitrary MCP server commands the
moment the user runs `claude-jr` inside it. Claude Code asks "do you trust this
folder?" first. After this plan, the project files are only used in folders
the user has trusted.

## Current state

- `src/config.rs:53-72`: `Config::load()` merges the user's config and then the project's:
  ```rust
  for path in [config_dir().join("config.json"), PathBuf::from(".claude-jr.json")] {
      let Ok(text) = std::fs::read_to_string(&path) else { continue };
      let file: Config = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
      cfg.model = file.model.or(cfg.model);
      ... cfg.mode = file.mode.or(cfg.mode); cfg.mcp_servers.extend(file.mcp_servers); cfg.allow.extend(file.allow); ...
  ```
  `config_dir()` lives in the same file (XDG_CONFIG_HOME or ~/.config, plus `claude-jr`).
  `save_api_key` in the same file writes a file there with mode 0600. Follow its style.
- `src/tools/mcp.rs:52-66`: `pub fn configured(extra)` reads `.mcp.json` from the current directory unconditionally.
- `src/main.rs:102`: `let file = config::Config::load()?;`, then the sandbox re-exec, then
  `let interactive = args.prompt.is_none();`. Interactive mode requires a TTY on stdin
  and stdout (checked at main.rs:111). Raw mode isn't on yet at this point, so a plain
  `stdin().read_line` works.
- Tests: `crate::tools::test_home()` (src/tools/mod.rs:371) points XDG_CONFIG_HOME and
  XDG_DATA_HOME at a temporary folder. Call it first in any test that touches config files.
  Unit tests live in `#[cfg(test)] mod tests` at the bottom of each file.

## Commands

| Purpose | Command | Expected |
|---|---|---|
| Tests | `cargo test --release` | all pass (42 before this plan) |
| Lint | `cargo clippy --release -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 (run `cargo fmt` to fix) |

## Scope

**In scope**: `src/config.rs`, `src/main.rs`, `src/tools/mcp.rs`.
**Out of scope**: everything else, especially `src/agent/*` and `src/ui/*`, which other plans are editing in parallel.

## Git workflow

Branch `advisor/001-trust-project-config`. Commit message in the repo's plain style,
for example `Ask before using a folder's .claude-jr.json and .mcp.json`.
**No Co-Authored-By or any other trailer.** Stage files by path; never `git add -A`. Don't push.

## Steps

### Step 1: Add the trust list to config.rs
- `fn trusted_file() -> PathBuf` = `config_dir().join("trusted")`. The file has one absolute folder path per line.
- `pub fn project_files() -> Vec<&'static str>`: whichever of `.claude-jr.json` and `.mcp.json` exist in the current directory.
- `pub fn is_trusted() -> bool`: the canonicalized current directory is a line in the file.
- `pub fn trust() -> Result<(), String>`: appends the canonicalized current directory. Create the folder if it's missing, and set the file's mode to 0600 the way `save_api_key` does.
- `Config::load()` reads `.claude-jr.json` only if `is_trusted()`.

**Verify**: `cargo test --release` → all pass.

### Step 2: mcp.rs reads .mcp.json only when trusted
In `configured()`, read `.mcp.json` only if `crate::config::is_trusted()`; otherwise start from an empty map.

**Verify**: `cargo build --release` → exit 0.

### Step 3: Ask in main.rs
Before `config::Config::load()`: if `project_files()` isn't empty and the folder isn't trusted, then:
- Interactive mode with stdin a TTY: print to stderr
  `This folder has <files>, which can change permissions and start commands.\nTrust this folder? [y/N] `,
  read one line from stdin, and call `config::trust()` if the trimmed answer is `y` or `yes` (any case).
  Otherwise carry on without the project files.
- `-p` mode: print to stderr one line, `ignoring <files>: this folder isn't trusted (run claude-jr here once to trust it)`, and continue.

Move the `interactive` computation above this check if needed. Don't change the order of anything else.

**Verify**: `cargo build --release` → exit 0.

### Step 4: Tests (in config.rs `mod tests`; create the module if absent)
1. After `test_home()`, `trust()` followed by `is_trusted()` returns true for a temporary folder. Use `std::env::set_current_dir` into a `crate::tools::test_dir("trust")` folder, and restore the old cwd at the end.
2. With a `.claude-jr.json` containing `{"mode":"bypass"}` in an **untrusted** folder, `Config::load().unwrap().mode` is `None`. After `trust()`, it is `Some("bypass")`.

Note: the cwd is process-global. If changing it makes other tests flaky, put both cases in **one** test function and say so in NOTES.

**Verify**: `cargo test --release` → all pass, including the new test(s). Run it 3 times; it passes each time.

## Done criteria
- [ ] `cargo test --release`, `cargo clippy --release -- -D warnings` and `cargo fmt --check` all exit 0
- [ ] `grep -n 'is_trusted' src/config.rs src/tools/mcp.rs` finds the gate in both files
- [ ] `git diff --stat 890677c` lists only the in-scope files

## STOP conditions
- `Config::load` or `mcp::configured` doesn't match the excerpts.
- Making this work seems to need edits to `src/agent/*` or `src/ui/*`.
- Changing the cwd in tests breaks other tests, and a single test function doesn't fix it.

## Maintenance notes
The trust list grows forever. That's fine; it's one line per folder. A later
change could hash the project files and ask again when they change; that's
deferred.
