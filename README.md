# claude-jr

A small coding agent for the terminal. It talks to Claude, reads and edits your
files, and runs commands, with a prompt before anything changes.

One static binary. Runs in 0.1 CPU and 64 MB of RAM.

```
> add a --verbose flag to the cli

● Read(src/cli.rs)
  ⎿ Read 48 lines

● Edit(src/cli.rs)
    13 + /// Print debug output
    14 + #[arg(short, long)]
    15 + pub verbose: bool,
  ⎿ Edited src/cli.rs (1 replacement)

● Bash(cargo build)
  ⎿ Finished `dev` profile in 2.1s

● Added `--verbose` to `Args`. The build passes.
```

## Numbers

Static build (x86_64, musl), measured with the scripts in `bench/`:

|                                          |                      |
|------------------------------------------|----------------------|
| Binary                                   | 2.5 MB               |
| First frame                              | 3 ms                 |
| Memory, idle                             | 1.4 MB               |
| CPU, idle                                | 0                    |
| Memory, 60 tool rounds (2.4 MB history)  | 6.3 MB               |
| Memory, 100 tool rounds (4 MB history)   | under 12 MB          |
| Whole container, 0.1 CPU / 64 MB limit   | 10 MB peak           |

## Build

```
cargo build --release
```

Static binary:

```
rustup target add x86_64-unknown-linux-musl
CC_x86_64_unknown_linux_musl=gcc cargo build --release --target x86_64-unknown-linux-musl
```

## Use

```
export ANTHROPIC_API_KEY=sk-ant-...
claude-jr                      # interactive
claude-jr -c                   # continue the last conversation here
claude-jr -p "explain main.rs" # one prompt, print the answer
claude-jr -m claude-sonnet-5-5 # another model (default: claude-opus-5-5)
```

Keys: enter sends, ctrl-j / shift-enter / `\` then enter for a new line, esc
cancels, ctrl-c clears the input, ctrl-c twice quits, up/down for history.

Commands: `/clear`, `/model [name]`, `/cost`, `/help`, `/exit`.

## Tools

| Tool  | Asks first | Notes |
|-------|------------|-------|
| read  | no         | line numbers, 2000 lines per call |
| write | yes        | an existing file must be read first |
| edit  | yes        | exact text, must match once; refuses if the file changed since it was read |
| bash  | yes        | 2 min timeout, `cd` carries over, long output keeps the start and end |

At the prompt: `y` yes, `a` always (this session), `n` no. In `-p` mode,
anything that asks is refused unless you pass `--yes` or a rule allows it.

## Config

`~/.config/claude-jr/config.json`, then `./.claude-jr.json`:

```json
{
  "model": "claude-sonnet-5-5",
  "effort": "high",
  "allow": ["bash:cargo *", "bash:git status", "edit"]
}
```

`tool` allows every call, `tool:pattern` allows matching commands or paths
(`*` matches anything). A command with `;`, `&&`, `|`, `$(...)` and the like
only matches a rule that spells it out exactly.

`AGENTS.md` and `CLAUDE.md` in the working directory and its parents (and in
`~/.config/claude-jr/`) are added to the system prompt.

Sessions are saved as JSONL under `~/.local/share/claude-jr/sessions/`.

## How it works

Two threads and two channels, no async runtime.

```
 key reader ─┐
             ├─> UI thread ── Command ──> agent thread ──> Claude API
 agent ──────┘   (draws)    <── Event ───  (runs tools)
```

- The agent thread owns the conversation. Messages are kept as the JSON text
  that goes over the wire; a request streams them straight from there, so the
  history is never copied. Session files are the same text, one message per line.
- The history is append-only, which keeps the prompt cache warm and the
  model's earlier reasoning valid. Compaction happens on the server.
- The UI writes finished lines once into normal terminal scrollback and only
  redraws the few lines at the bottom. It waits on a channel, so it uses no
  CPU when nothing is happening.

```
src/
  main.rs         args, config, pick UI or -p
  agent.rs        conversation, tool loop, permissions
  api.rs          request, SSE parsing, retries, model capabilities, pricing
  tools.rs        read, write, edit, bash
  prompt.rs       system prompt, AGENTS.md, git status
  session.rs      messages as JSON text, JSONL sessions
  permission.rs   allow rules
  config.rs       config files
  print.rs        -p output
  ui/
    mod.rs        event loop, keys, commands
    term.rs       raw mode, scrollback, redraw
    view.rs       tool blocks, diffs, input box
    input.rs      line editor, history
    md.rs         markdown, one line at a time
    text.rs       styles, wrapping
bench/
  mock.py         fake API for tests and benchmarks
  mem.sh          long session, fails above 16 MB
  box.sh          run inside a 0.1 CPU / 64 MB container
```

## Not included

Other providers, MCP, plugins, hooks, subagents, web tools, IDE integration,
Windows.
