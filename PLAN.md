# claude-jr plan

A minimal coding agent for the terminal, written in Rust. It does what Claude
Code does at its core (read, edit, run commands) with a similar inline terminal
UI, and nothing more.

## Target

**Runs in 0.1 CPU and 64 MB of RAM.** A full session must work inside
`docker run --cpus=0.1 --memory=64m`.

| Budget | Limit |
|--------|-------|
| Idle RSS | 8 MB |
| Peak RSS, long session | 16 MB |
| Binary | 4 MB, static |
| Startup | 20 ms on a laptop |
| CPU while waiting on the model | 0% |

The 16 MB peak leaves about 45 MB for the commands the agent runs (bash, git,
rg, small scripts). Heavy builds (cargo, npm) are out of scope for the 64 MB box.

Also: about 2.5k lines, 6 dependencies, no async runtime, terminal only,
Anthropic API only (`ANTHROPIC_API_KEY`).

## Memory rules

- One copy of the conversation. Request bodies are serialized straight from it.
- Tool output is truncated while it streams in (head + tail), never collected
  whole and cut later.
- Files are read with a 50 KB / 2000 line cap per call.
- One reused HTTPS connection.
- Redraw only when something changed, at most ~15 times a second. Idle means
  blocked on input, no timers.
- Default glibc malloc; compare against musl before choosing the static build.

## Not doing

Other providers, MCP, plugins, hooks, skills, subagents, web tools, IDE
integration, Windows-specific work.

## Decisions

**No async runtime.** An agent does one thing at a time: wait for the model or
wait for a tool. Two plain threads and `std::sync::mpsc` cover it. That removes
tokio and reqwest, which are most of the memory and binary size in agents like
Codex.

**Blocking HTTP with `ureq`.** It reads the streamed response line by line from a
plain `Read`, which is all SSE needs.

**Hand-written terminal UI on `crossterm`.** Like pi, not like Codex (ratatui).
Finished messages are printed once and stay in scrollback. Only the bottom area
(live reply, input box, status line) is redrawn, and only lines that changed,
inside synchronized output so it doesn't flicker. This is a few hundred lines
and is easier to reason about than a framework's inline viewport.

**Four tools: read, write, edit, bash.** Same default set as pi. Search goes
through bash (`rg`, `find`, `ls`). grep/glob tools can come later if needed.

**No libraries for things we can write in 50 lines:** argument parsing, diffs
(an edit is one replacement, so we already know the removed and added lines),
markdown (headings, lists, bold, inline code, code fences), config paths.

## Dependencies

| Crate | Why |
|-------|-----|
| `ureq` | HTTPS with rustls, streaming response body |
| `serde` + `serde_json` | API messages, tool inputs, sessions, config |
| `crossterm` | raw mode, key events, cursor and color codes |
| `unicode-width` | correct wrapping for wide characters and emoji |
| `anyhow` | error handling |

Release profile: `lto = true`, `codegen-units = 1`, `panic = "abort"`, `strip = true`.

## Architecture

```
 main thread (UI)                         agent thread
 ────────────────                         ────────────
 loop:                                    loop:
   poll keys (50 ms)        Command         wait for Command::Send
   drain events      ─────────────────>     stream reply from API
   redraw if changed  <─────────────────    run tool calls
                            Event           repeat until no tool calls
```

- `Command`: `Send(text)`, `Permission(Answer)`, `Clear`, `Quit`
- `Event`: `Text(delta)`, `ToolStart`, `ToolEnd`, `AskPermission`, `Usage`,
  `Done`, `Error`
- **Permission**: the agent sends `AskPermission` and blocks until a
  `Command::Permission` comes back. The UI shows the prompt.
- **Cancel**: an `Arc<AtomicBool>`. The SSE reader checks it between lines and
  the bash runner checks it while waiting, killing the child process.
- **`-p` mode** uses the same agent thread with a plain printer instead of the
  UI, so the core can be tested without a terminal.

## Files

```
src/
  main.rs       args, start agent thread, pick UI or -p printer     ~100
  api.rs        request types, SSE stream parsing, retries          ~300
  agent.rs      conversation, tool loop, cancel, permission wait    ~250
  prompt.rs     system prompt, env info, AGENTS.md / CLAUDE.md      ~80
  tools.rs      read, write, edit, bash + schemas + truncation      ~350
  session.rs    save / load JSONL, compaction                       ~150
  config.rs     ~/.config/claude-jr/config.json, allow rules        ~80
  ui/
    mod.rs      main loop, key handling, slash commands             ~300
    term.rs     raw mode, differential redraw, scrollback writes    ~200
    input.rs    input box editing, history                          ~200
    view.rs     message / tool / diff / permission rendering        ~300
    md.rs       minimal markdown to styled lines                    ~150
```

## Tools

| Tool  | Asks? | Behavior |
|-------|-------|----------|
| read  | no    | `cat -n` style, `offset` / `limit`, 2000 lines max |
| write | yes   | must read an existing file before overwriting |
| edit  | yes   | `old_string` must match exactly once (or `replace_all`); file must be read first and unchanged since |
| bash  | yes   | 2 min timeout, cwd kept between calls, output capped at 2000 lines / 50 KB (head + tail) |

Results are plain strings, errors included, so the model can recover.
Allow rules in config skip the prompt, e.g. `"bash": ["cargo *", "git status"]`.

## UI

```
● Read(src/cli.rs)
  ⎿ 48 lines
● Edit(src/cli.rs)
  ⎿ +3 -0
     13 + /// Print debug output
     14 + #[arg(short, long)]
     15 + pub verbose: bool,

╭──────────────────────────────────────────╮
│ > _                                      │
╰──────────────────────────────────────────╯
  ⠋ Working… 4s · esc to cancel      12.4k tokens · $0.03
```

Keys: Enter send, Ctrl-J new line, Esc cancel, Ctrl-C twice quit, Up/Down history.
Commands: `/clear` `/compact` `/cost` `/model` `/exit`.

## Phases

### 1. Talk (no UI)
- `api.rs` streaming, `agent.rs` loop without tools, `-p` printer
- Done when `claude-jr -p "hello"` streams a reply

### 2. Act
- `tools.rs` with all four tools, tool loop, cancel flag
- Done when `claude-jr -p "add a test for X and run it"` works end to end

### 3. UI
- `term.rs` redraw, `input.rs`, `view.rs` for text and tool blocks, spinner, status
- Done when you can chat in the UI and see tool calls as they run

### 4. Safe
- Permission prompt (yes / always / no), diff view for edit and write, allow rules
- Done when nothing writes or runs without asking unless allowed

### 5. Context
- Prompt caching, AGENTS.md / CLAUDE.md, markdown rendering, slash commands
- Sessions saved as JSONL, `--continue`, compaction near the context limit
- Done when long sessions stay cheap and can be resumed

### 6. Measure
- `bench/` script: scripted long session, fails if peak RSS > 16 MB.
  Run it at the end of every phase from phase 2 on.
- Same task in `docker run --cpus=0.1 --memory=64m` for claude-jr, Claude
  Code, Codex and pi: finished or not, peak RSS, wall time
- Static builds for x86_64 and aarch64
- Benchmark run (Terminal-Bench or SWE-bench Lite), numbers in README

## References

Local clones in `.temp/` (gitignored): pi-mono, codex, mini-swe-agent, opencode.
Notes, including how Claude Code works, are in `.temp/NOTES.md`.
