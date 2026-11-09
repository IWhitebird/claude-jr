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
`unicode-width`, `anyhow`.

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

## Later

- MCP client (stdio)
- Subagents
- Syntax highlighting in code blocks
- Persisting "always allow" answers to the project config

## References

Local clones in `.temp/` (gitignored): pi-mono, codex, mini-swe-agent, opencode.
Notes, including how Claude Code works, are in `.temp/NOTES.md`.
