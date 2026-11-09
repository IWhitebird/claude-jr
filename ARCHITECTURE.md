# How claude-jr works

## Threads and processes

claude-jr is multi-threaded, with plain OS threads and blocking I/O. There is
no async runtime and no io_uring. Each thread spends almost all its time
blocked in the kernel, so an idle session uses no CPU.

```
                         claude-jr process
 ┌───────────────────────────────────────────────────────────────────────┐
 │                                                                       │
 │  keys thread ──── key presses ────┐                                   │
 │  (blocks on stdin)                │                                   │
 │                                   ▼                                   │
 │                        ┌──────────────────┐                           │
 │                        │   main thread    │──── draws ───▶ terminal   │
 │                        │   (UI loop)      │                           │
 │                        └──────────────────┘                           │
 │                           ▲           │                               │
 │                     events│           │commands                       │
 │                           │           ▼                               │
 │                        ┌──────────────────┐   HTTPS (one kept-alive   │
 │                        │  agent thread    │◀─────────────────────────▶│── api.anthropic.com
 │                        │  (the turn loop) │    connection, streamed)  │
 │                        └──────────────────┘                           │
 │                           │     │      │                              │
 │       parallel tool calls │     │      │ subagents: same loop,        │
 │       (scoped threads)    ▼     │      ▼ own context, own threads     │
 │                        ┌─────┐  │   ┌──────┐                          │
 │                        │tools│  │   │ sub  │                          │
 │                        └─────┘  │   └──────┘                          │
 │                                 │                                     │
 │  signals thread                 │  per command: one thread reading    │
 │  (SIGINT/TERM/HUP → clean exit) │  its output (bash-out / job-out)    │
 └─────────────────────────────────┼─────────────────────────────────────┘
                                   │ fork/exec, own process group
                                   ▼
              bash commands · background jobs · local MCP servers
```

| Thread | Blocks on | Wakes when |
|---|---|---|
| main (UI) | the event channel | a key, an agent event, or every 300 ms while working (status dot); at most ~30 draws/s while streaming |
| keys | `read()` on the terminal | you press a key |
| agent | the command channel, then the socket | you send something; each network packet while a reply streams |
| signals | a signal pipe | SIGINT, SIGTERM or SIGHUP |
| bash-out / job-out | the command's output pipe | the command prints |
| mcp-read | the server's stdout | the server replies |

Idle, that's 4 threads (main, keys, agent, signals), all blocked.

## One turn

```
 you type ──▶ UI ──Command::Send──▶ agent
                                      │
                                      │ request body = slices over stored messages
                                      ▼   (nothing copied), streamed to the API
                                   Claude API
                                      │ SSE: text, thinking, tool_use deltas
                                      ▼
                    agent parses each event, sends Event::Text … to the UI
                                      │
                         stop_reason = tool_use?
                          │yes                    │no
                          ▼                       ▼
            for each call: permit → run      Event::Done
            (reads, subagents, skills,
             MCP in parallel; the rest
             in order)
                          │
            tool results appended as one user message
                          │
                          └────────── next request ──▶ (loop)
```

Approval (`agent/permit.rs`): mode → allow rules → (auto mode) the reviewer
model → ask you. Plan mode refuses edits; skip-permissions allows everything.

## Data: what's in memory and how it's laid out

```
 Vec<Msg>                       one entry per message, append-only
 ┌──────┬──────────────────────────────────────────────────────────┐
 │ user │ json: Box<RawValue>  — the exact JSON text the API gets  │
 ├──────┼──────────────────────────────────────────────────────────┤
 │ asst │ {"role":"assistant","content":[...thinking, text, ...]}  │
 ├──────┼──────────────────────────────────────────────────────────┤
 │ user │ {"role":"user","content":[{"type":"tool_result",...}]}   │
 └──────┴──────────────────────────────────────────────────────────┘
          │                                   │
          │ request body                      │ session file
          ▼                                   ▼
 {"messages":[ m1 , m2 , m3 ], ...rest}   header line, then m1\n m2\n m3\n
 read straight from these strings         the same bytes, appended
```

Data-oriented choices, and why:

- **Stored in the form it's used.** Messages are kept as wire JSON, not as a
  tree of parsed values. Sending, saving and resuming all use the bytes as
  they are.
- **Append-only.** New messages are pushed; old ones are never edited. This
  keeps the API's prompt cache valid, keeps the model's earlier reasoning
  valid, and makes the session file a plain append.
- **No copies on the hot path.** A request is a list of slices over the stored
  strings, streamed to the socket. Tool output is cut to 50 KB (start + end)
  as it's read, so a noisy command never sits in memory whole.
- **Batched UI work.** Events are drained in batches and drawn once per frame;
  only changed lines are rewritten; finished rows go to scrollback once.

Memory in practice: about 1.7 MB idle, then about the size of the
conversation plus 1.5 MB. A 2.4 MB history (60 tool calls) peaks at 3.9 MB.

## Files

```
src/
  main.rs          args, config, picks the UI or -p
  agent/
    mod.rs         the main agent: commands, session, startup
    turn.rs        request → tools → repeat (shared with subagents)
    permit.rs      modes, approval, auto mode's reviewer
    sub.rs         subagents
  tools/
    mod.rs         definitions, dispatch, shared state behind locks
    fs.rs          read, write, edit
    bash.rs        bash, background jobs, the jobs tool
    todo.rs        task list
    skill.rs       SKILL.md discovery and loading
    mcp.rs         MCP over stdio, and the API connector for remote servers
  api.rs           request building, SSE parsing, retries, per-model features
  session.rs       messages as JSON text; JSONL sessions
  prompt.rs        system prompt, AGENTS.md, notes
  permission.rs    allow rules
  sandbox.rs       re-exec inside bubblewrap
  signals.rs       clean exit on signals
  config.rs        config files, the saved API key
  print.rs         -p output
  ui/
    mod.rs         event loop, keys, commands, the live area
    term.rs        raw mode, scrollback, line-diff redraw
    view.rs        tool blocks, diffs, boxes, status, footer, mascot
    table.rs       Markdown tables
    md.rs          Markdown, one line at a time
    text.rs        styles and word wrapping
    input.rs       the line editor
    stats.rs       /stats from /proc
```
