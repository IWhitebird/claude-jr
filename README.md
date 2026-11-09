<p align="center">
  <img src="assets/logo.svg" width="140" alt="claude-jr mascot">
</p>

<h1 align="center">claude-jr</h1>

<p align="center">
  A small coding agent for the terminal, in one 2.5 MB binary.<br>
  It runs in 0.1 CPU and 64 MB of RAM.
</p>

---

claude-jr talks to Claude, reads and edits your files, and runs commands, asking
before anything changes. It's a minimal take on Claude Code: four tools, a
terminal UI, no runtime, nothing to install besides the binary.

```
 ╭─────╮
 │ ● ● │   claude-jr 0.1.0
 ╰──‿──╯   claude-opus-5-5 · ~/proj/app

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

Static x86_64 build, measured with the scripts in [`bench/`](bench):

| | |
|---|---|
| Binary | 2.5 MB, static |
| Startup to first frame | 3 ms |
| Memory when idle | 1.4 MB |
| CPU when idle | 0 |
| Memory after 60 tool calls (2.4 MB of history) | 6.3 MB |
| Whole container at 0.1 CPU / 64 MB | 10 MB peak |

## Install

```sh
git clone https://github.com/IWhitebird/claude-jr && cd claude-jr
cargo build --release
cp target/release/claude-jr ~/.local/bin/
```

Fully static binary (Linux):

```sh
rustup target add x86_64-unknown-linux-musl
CC_x86_64_unknown_linux_musl=gcc cargo build --release --target x86_64-unknown-linux-musl
```

## Use

```sh
export ANTHROPIC_API_KEY=sk-ant-...

claude-jr                         # start
claude-jr --sandbox               # only allow writes inside this directory
claude-jr -c                      # continue the last conversation here
claude-jr -r "auth refactor"      # continue a named conversation
claude-jr -p "explain main.rs"    # one prompt, print the answer, exit
claude-jr -m claude-sonnet-5-5    # another model (default: claude-opus-5-5)
```

| Key | |
|---|---|
| enter | send |
| ctrl-j, shift-enter, `\` + enter | new line |
| shift-tab | change mode |
| esc | stop the current turn |
| ctrl-c | clear input; twice to quit |
| up / down | history |

| Command | |
|---|---|
| `/rename NAME` | name this conversation |
| `/model NAME` | switch model |
| `/memory` | show loaded instruction files |
| `/cost` | tokens and spend so far |
| `/clear` | start over |
| `#some note` | save a note to `./AGENTS.md` |

## Modes

Shift-tab cycles through them; the current one shows under the input box.

| Mode | write / edit | bash |
|---|---|---|
| normal | asks | asks |
| accept edits | runs | asks |
| plan | refused; the model is told to plan instead | asks |
| skip permissions | runs | runs |

When it asks: `y` yes, `a` always for this session, `n` no. `-y` starts in
skip-permissions mode. With `-p`, anything that would ask is refused.

## Sandbox

```sh
claude-jr --sandbox
```

claude-jr restarts itself inside [bubblewrap](https://github.com/containers/bubblewrap),
so the kernel enforces the limits for every tool, including its own file edits:

- **writable**: the current directory, its session files, package caches, a private `/tmp`
- **read-only**: the rest of the system, and toolchains in `$HOME` (`.cargo`, `.rustup`, `.nvm`, ...)
- **hidden**: everything else in `$HOME`: ssh keys, tokens, other projects

`bench/sandbox.sh` checks this: a fake model tries to delete a file outside the
project and write to `$HOME`, and both must fail. To try the agent on a repo
without risking anything, use a throwaway worktree:

```sh
git worktree add /tmp/try && cd /tmp/try && claude-jr --sandbox
```

Combine it with skip-permissions mode and the agent can work without asking,
but only inside that directory. Linux only for now.

## Memory and sessions

- **Instructions**: `AGENTS.md` or `CLAUDE.md` in the current directory, its
  parents, and `~/.config/claude-jr/` go into the system prompt.
  `#some note` appends to `./AGENTS.md` and the model sees it right away.
- **Conversations** are saved as JSONL under `~/.local/share/claude-jr/sessions/<directory>/`,
  one message per line, exactly as sent to the API. `--sessions` lists them.
- **Long conversations** are compacted on the server when they get big.

## Config

`~/.config/claude-jr/config.json`, then `./.claude-jr.json`:

```json
{
  "model": "claude-sonnet-5-5",
  "effort": "high",
  "allow": ["bash:cargo *", "bash:git status", "edit"],
  "sandbox": { "enabled": true, "read": ["/opt/sdk"], "write": [] }
}
```

`allow` rules skip the prompt: `tool` for every call, or `tool:pattern` where
`*` matches anything. A command with `;`, `&&`, `|`, `$(...)` or redirects
only matches a rule that spells it out exactly, so `cargo *` can't approve
`cargo test; rm -rf ~`.

## How it works

```
 key reader ─┐
             ├─> UI thread ── commands ──> agent thread ──> Claude API
 agent ──────┘   (draws)    <── events ───  (runs tools)
```

- **No async runtime.** Three threads and channels. The UI blocks on one
  channel, so it uses no CPU while you think.
- **Messages are stored as the JSON text that goes over the wire.** A request
  is streamed straight from them, so the history is never copied, and the
  session file is the same text.
- **The history only grows.** That keeps the prompt cache warm and the model's
  earlier reasoning valid. The system prompt is fixed per session; mode
  changes and notes go into the next user message instead.
- **The UI writes finished lines once** into normal terminal scrollback and
  redraws only the few lines at the bottom, so scrolling and copying work as
  usual.

```
src/
  main.rs        args, config, start
  agent.rs       conversation, tool loop, modes, permissions
  api.rs         requests, streaming, retries, per-model features, prices
  tools.rs       read, write, edit, bash
  prompt.rs      system prompt, AGENTS.md, notes
  session.rs     saved conversations
  sandbox.rs     bubblewrap
  permission.rs  allow rules
  config.rs      config files
  print.rs       -p output
  ui/            terminal UI: loop, drawing, input, markdown, wrapping
bench/
  mock.py        fake API for tests and benchmarks
  mem.sh         long session; fails above 16 MB
  box.sh         run in a 0.1 CPU / 64 MB container
  sandbox.sh     check the sandbox
```

## Development

```sh
cargo test            # unit tests
bench/mem.sh          # memory budget
bench/sandbox.sh      # sandbox check
bench/box.sh          # container run (needs docker and the static build)
```

None of these need an API key; they run against `bench/mock.py`.

## Not included

Other providers, MCP, plugins, hooks, subagents, web tools, IDE integration,
Windows.
