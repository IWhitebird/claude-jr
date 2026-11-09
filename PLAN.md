# claude-jr plan

A small coding agent for the terminal, written in Rust. It does the core of what
Claude Code does (read, edit, run, search) in a single binary with low memory and
CPU use.

## Goals

- One static binary, no runtime dependencies
- Under 20 MB RSS while idle, under 50 ms to start
- Codebase small enough to read in an afternoon (target under 5k lines)
- Works with the Anthropic Messages API directly (`ANTHROPIC_API_KEY`)

## Non-goals (for now)

- IDE plugins, a web UI, or multiple model providers
- Plugin system, hooks, skills

## How it works

```
loop {
    stream a response for the current messages and tool definitions
    print text as it arrives
    if the response has no tool calls: wait for user input
    for each tool call: check permission, run it, append the result
}
```

## Layout

```
src/
  main.rs        args, startup, REPL
  api.rs         Messages API client, SSE streaming, retries
  types.rs       message / content block / tool types (serde)
  agent.rs       the loop: send, stream, dispatch tools, repeat
  tools/
    mod.rs       tool trait + registry + JSON schemas
    read.rs      read file (with line ranges)
    write.rs     write file
    edit.rs      exact string replace, must match once
    bash.rs      run a command, timeout, truncate output
    glob.rs      find files (respects .gitignore)
    grep.rs      search contents (ripgrep crates)
  permission.rs  ask / allow / deny, allowlist from config
  session.rs     save and resume conversations as JSONL
  compact.rs     summarize old turns near the context limit
  config.rs      ~/.config/claude-jr/config.toml + project overrides
  prompt.rs      system prompt, loads AGENTS.md / CLAUDE.md
```

## Crates

- `tokio`, `reqwest` (rustls, stream) for async HTTP and SSE
- `serde`, `serde_json` for API types
- `rustyline` for the input line
- `ignore`, `grep-searcher`, `grep-regex` for glob/grep
- `clap` for args, `toml` for config, `anyhow` for errors
- later: `ratatui` + `crossterm` for a TUI

## Milestones

### 1. It talks
- [ ] API client with streaming text output
- [ ] REPL: type a message, see the reply stream in
- [ ] Model and max tokens from args/env

### 2. It acts
- [ ] Tool trait and registry
- [ ] read, write, edit, bash
- [ ] Tool loop: run tools, send results, continue until done
- [ ] Ctrl-C cancels the current turn, not the program

### 3. It's safe and cheap
- [ ] Permission prompts for write/edit/bash, reads auto-allowed
- [ ] Allowlist in config (for example `bash: cargo test*`)
- [ ] Prompt caching on system prompt, tools and conversation prefix
- [ ] Token and cost count per turn and per session

### 4. It finds things
- [ ] glob and grep tools
- [ ] Load AGENTS.md / CLAUDE.md into the system prompt
- [ ] Show git status and cwd in the system prompt

### 5. It remembers
- [ ] Save sessions as JSONL
- [ ] `--resume` and `--continue`
- [ ] Compaction when near the context limit
- [ ] Non-interactive mode: `claude-jr -p "fix the failing test"`

### 6. It's measured
- [ ] Startup time, idle RSS and peak RSS vs Claude Code and Codex CLI
- [ ] Run a benchmark (Terminal-Bench or SWE-bench Lite) and publish results
- [ ] README with numbers

### Later
- [ ] ratatui TUI with diff view
- [ ] MCP client (stdio)
- [ ] Subagents
- [ ] Extended thinking display

## Notes

- Edit tool follows Claude Code's rule: `old_string` must match exactly once,
  and a file must be read before it can be edited.
- Bash output is truncated (head + tail) so a noisy command can't fill the
  context.
- Every tool result is a plain string, errors included, so the model can recover.
