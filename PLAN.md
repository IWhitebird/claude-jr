# claude-jr plan

A minimal coding agent for the terminal, written in Rust. It does what Claude
Code does at its core (read, edit, run commands) with a similar inline terminal
UI, and nothing more.

## Target

**Runs in 0.1 CPU and 64 MB of RAM.** A full session must work inside
`docker run --cpus=0.1 --memory=64m`.

| Budget | Limit | Measured (static build) |
|--------|-------|-------------------------|
| Idle RSS | 8 MB | 1.4 MB |
| Peak RSS, long session | 16 MB | 6.3 MB at 60 rounds, under 12 MB at 4 MB of history |
| Binary | 4 MB, static | 2.5 MB |
| Startup | 20 ms | 3 ms to first frame |
| CPU while idle | 0 | 0 |
| Whole container, 0.1 CPU / 64 MB | fits | 10 MB peak |

## Decisions

- **No async runtime.** A UI thread, an agent thread, a key reader thread and
  `std::sync::mpsc`. The UI blocks on one channel, so idle means zero CPU.
- **Blocking HTTP with `ureq`.** SSE is read line by line from the response.
- **Messages stored as JSON text** (`Box<RawValue>`). A request body is a list
  of slices over those strings, streamed without copying the history. Session
  files are the same strings, one per line.
- **Append-only history.** The system prompt is built once per session and
  saved with it; it has nothing that changes (no model name). This keeps the
  prompt cache warm and earlier thinking blocks valid.
- **Server-side compaction** (`compact_20260112`) instead of client-side
  summaries, so there is no `/compact` command.
- **Progress notes** (`thinking.display: "updates"`): on Opus 5.5 / Sonnet 5.5
  the text between tool calls comes back as thinking blocks; we ask for it as
  text and show it dimmed.
- **Refusal fallback** (`fallbacks: "default"`) on models that support it.
- **Hand-written terminal UI on `crossterm`.** Finished lines go to scrollback
  once; only the bottom area is redrawn, inside synchronized output.
- **Four tools: read, write, edit, bash.** Search goes through bash.
- **No libraries for small things:** args, diffs, markdown, globs, dates.
- **Static musl build is the release build.** It peaked lower than glibc
  (6.3 vs 8.6 MB on the same session) at the same CPU.

## Dependencies

`ureq` (rustls), `serde`, `serde_json` (raw_value), `crossterm`,
`unicode-width`, `anyhow`, `signal-hook` (already pulled in by crossterm).

## Phases

1. **Talk.** Streaming client, agent thread, `-p`. Done.
2. **Act.** read, write, edit, bash; tool loop; cancel. Done.
3. **UI.** Inline renderer, input editor, spinner, status, tool blocks. Done.
4. **Safe.** Approval prompt (yes / always / no), diffs, allow rules. Done.
5. **Context.** Caching, AGENTS.md / CLAUDE.md, git status, markdown, slash
   commands, JSONL sessions, `--continue`, server-side compaction. Done.
6. **Measure.**
   - `bench/mem.sh`: long scripted session, fails above 16 MB. Done.
   - `bench/box.sh`: run inside 0.1 CPU / 64 MB. Done for claude-jr.
   - Static x86_64 build. Done. aarch64 needs a cross C compiler for `ring`.
   - Same task in the box for Claude Code, Codex and pi. Needs an API key.
   - Benchmark run (Terminal-Bench or SWE-bench Lite). Needs an API key.

7. **Polish.** Orange theme and mascot; shift+tab modes (normal, accept
   edits, plan, skip permissions); `/rename`, `-r`, `--sessions`; `#note`
   memory and `/memory`; `--sandbox` with bubblewrap and `bench/sandbox.sh`;
   logo and README. Done.

8. **Agent features.** Background bash and the `jobs` tool (watch with
   `wait` + `until`), job-finished notices; parallel tool calls; subagents
   (`agent` tool, own context and shell, can't nest or ask questions);
   `ask_user` questions with options; `todo` list; skills in Claude Code's
   format; MCP over stdio (local) and through the API connector (remote);
   auto mode with a Haiku reviewer; `!cmd` shell. Done.
9. **Resources.** Redraw only when the picture changes; breathing status dot
   at ~3 frames/s; context `used/window` in the footer; `/stats` from /proc;
   `bench/cpu.sh` and `bench/idle.py`; clean exit on SIGINT/SIGTERM/SIGHUP
   that stops every child process. Done.

10. **Optimization pass.** `/login` for the API key. Measured against Claude
    Code, pi and Codex with `bench/compare.py`. Changes that paid off:
    committing finished rows of a streaming paragraph (terminal output for a
    35 KB reply 11 MB -> 0.7 MB), rewriting only changed lines, ~30 fps cap
    with the UI sleeping until a frame is due, waiting on the output pipe
    instead of polling (60 tool calls 0.81 s -> 0.32 s), no delayed first
    frame. Tried and dropped: opt-level "s" (20% smaller binary, ~10% more
    CPU), size-optimized dependencies (8% smaller, +0.2 MB idle and +0.3 MB
    busy memory), building the HTTP client lazily (no difference: ureq
    already sets up TLS on first use). Done.

    Where the CPU goes now, while streaming ~200 tokens/s: the agent thread
    waking for each network packet (~110 ms per 46 s) and ~30 frames a second
    in the UI (~90 ms). Parsing itself is ~10 ms for 35 KB.

11. **Frontend features.** Image input (dragged paths, ctrl+v, read on images);
    /rewind with on-disk checkpoints; /compact (client-side summary, safe with
    preserved thinking); /resume picker; auto-memory tool; /effort; Anthropic's
    web search and fetch; agent and job panel, /jobs, /kill; /model catalogue;
    input anchored to the bottom of the screen; /settings; /clear on a clean
    screen; mascot drawn from assets/mascot.txt. Done.
12. **Evals.** bench/evals/run.py with a budget cap; a 10-task frontend suite;
    an adapter for Aider's polyglot benchmark (not run yet: it executes code
    from that repo, so it waits for a go-ahead). Done.

Resource cost of 11 and 12: binary 2.8 -> 3.0 MB, idle 1.7 -> 1.9 MB, busy
unchanged. Tests: 51, line coverage 53% (the UI loop and terminal drawing
are covered by the pty scripts in bench/ rather than by cargo test).

## Next: benchmarking (needs an API key)

- Task suite in `bench/tasks/`: small repos, a prompt and a `check.sh` each
- Runner: each task in a fresh sandboxed copy, records pass/fail, tokens,
  cost, time, peak memory
- Same runner for Claude Code and pi, then all three inside the
  0.1 CPU / 64 MB box
- Terminal-Bench run once the suite is stable

## Open questions to verify against the real API

- Dropping messages before the newest compaction block, to shrink RAM and
  request size in very long sessions
- Remote MCP through the connector, end to end

## Browser / WASM

Not yet. What it would take:

- **HTTP**: `ureq` is blocking sockets; a browser build needs `fetch`
  (the API allows it with `anthropic-dangerous-direct-browser-access: true`,
  which exposes the key to the page, so it's for local use only).
- **Tools**: there is no shell or real filesystem in a browser. Options are an
  in-memory or OPFS file tree with read/write/edit only, or a WebContainer
  for bash, which is a large dependency.
- **UI**: xterm.js in place of the raw terminal, fed by the same event stream.
- **Threads**: the agent loop would become async, or run in a Web Worker.

The split that makes it possible: keep `agent`, `session`, `permission` and
the SSE parser free of I/O, and put HTTP, files, processes and the terminal
behind small traits. Running under WASI (wasmtime) works for the same reason
and doesn't help bash either, since WASI can't start processes.

## Later

- Split I/O behind traits (step one toward a browser build)
- macOS sandbox (sandbox-exec)
- Syntax highlighting in code blocks
- Persisting "always allow" answers to the project config

## References

Local clones in `.temp/` (gitignored): pi-mono, codex, mini-swe-agent, opencode.
Notes, including how Claude Code works, are in `.temp/NOTES.md`.
