<p align="center">
  <img src="assets/logo.svg" width="140" alt="claude-jr mascot">
</p>

<h1 align="center">claude-jr</h1>

<p align="center">
  A small coding agent for the terminal, in one 2.8 MB binary.<br>
  It runs in 0.1 CPU and 64 MB of RAM.
</p>

---

claude-jr talks to Claude, reads and edits your files, and runs commands,
asking before anything changes. It has what you'd expect from a coding agent
(subagents, background jobs, skills, MCP, an auto-approve mode, a sandbox)
without a runtime: no Node, no Python, no async framework.

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

Static x86_64 build, measured with the scripts in [`bench/`](bench):

| | |
|---|---|
| Binary | 2.8 MB, static |
| Startup to first frame | 3 ms |
| Memory when idle | 1.7 MB |
| CPU when idle | 0 |
| Memory after 60 tool calls (2.4 MB of history) | 4.1 MB |
| CPU per tool call (claude-jr's own work) | ~5 ms |
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
| `/skills`, `/mcp` | what's loaded |
| `/memory` | instruction files in use |
| `/stats` | memory and CPU of claude-jr and everything it started |
| `/cost` | tokens and spend so far |
| `/clear` | start over |
| `/<skill> args` | run a skill |
| `#some note` | save a note to `./AGENTS.md` |
| `!command` | run a command yourself; the agent sees the output |

## Modes

Shift-tab cycles through them; the current one shows under the input box.

| Mode | edits | commands |
|---|---|---|
| normal | ask | ask |
| accept edits | run | ask |
| plan | refused; the model is told to plan instead | ask |
| auto | run inside the project | a small model (Haiku) reviews each one: routine ones run, risky ones ask you, with the reason |
| skip permissions | run | run |

When it asks: `y` yes, `a` always for this session, `n` no. `-y` starts in
skip-permissions mode. With `-p`, anything that would ask is refused.

Auto mode costs a fraction of a cent per reviewed command. It blocks things
like `git push`, deleting outside the project, `sudo`, and sending data over
the network unless your request plausibly needs them.

## Tools

| Tool | What it does |
|---|---|
| `read`, `write`, `edit` | files; edits must match exactly once, and a file must be read before it's changed |
| `bash` | commands; `cd` carries over; `background: true` for servers, watchers and long builds |
| `jobs` | list, read, wait on or kill background jobs; `wait` can stop as soon as output contains some text ("Listening on") |
| `agent` | a subagent with its own context for a self-contained task; several run in parallel |
| `ask_user` | the model asks you something, with options you pick by number |
| `todo` | a task list for longer work, shown as it changes |
| `skill` | loads a skill's instructions |
| MCP tools | whatever your MCP servers provide |

Reads, subagents, skills and MCP calls in the same reply run at the same time.
When a background job ends, you see a notice and the model is told with your
next message.

## Skills

Same format as Claude Code, so existing skills work. Put them in
`.claude/skills/<name>/SKILL.md` (project) or `~/.claude/skills/` (yours):

```markdown
---
name: release
description: Cut a release: bump the version, tag, write notes.
---
1. ...
```

Only names and descriptions are sent up front. The model loads a skill when a
task matches it, or you run it with `/release`.

## MCP

`.mcp.json` (Claude Code's format) or `"mcp_servers"` in the config:

```json
{
  "mcpServers": {
    "files":  { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "."] },
    "linear": { "url": "https://mcp.linear.app/mcp", "headers": { "Authorization": "Bearer ${LINEAR_TOKEN}" } }
  }
}
```

- **Local servers** (`command`) run as their own processes and talk JSON-RPC
  over stdin/stdout. claude-jr adds one small thread per server; the server's
  own memory is separate, and `/stats` shows it.
- **Remote servers** (`url`) are handed to the API's MCP connector, so
  Anthropic calls them and they cost nothing locally. The URL must be
  reachable from the internet.

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

Linux only for now.

## Memory and sessions

- **In RAM**: only the current conversation, kept as the exact JSON text sent
  to the API. Requests are streamed straight from it, so it's never copied.
  Tool output is cut to 50 KB (start and end) before it's stored.
- **Context**: the footer shows `used/window` tokens. Near the limit the API
  compacts older turns on its side.
- **On disk**: `~/.local/share/claude-jr/sessions/<directory>/`, one message
  per line, only ever appended to. `--sessions` lists them.
- **Instructions**: `AGENTS.md` or `CLAUDE.md` in the current directory, its
  parents, and `~/.config/claude-jr/`. `#note` appends to `./AGENTS.md`.

When claude-jr exits, by quitting, a signal or closing the terminal, it stops
every command, background job and MCP server it started.

## Config

`~/.config/claude-jr/config.json`, then `./.claude-jr.json`:

```json
{
  "model": "claude-sonnet-5-5",
  "effort": "high",
  "auto_model": "claude-haiku-5-5",
  "allow": ["bash:cargo *", "bash:git status", "edit"],
  "sandbox": { "enabled": true, "read": ["/opt/sdk"], "write": [] },
  "mcp_servers": {}
}
```

`allow` rules skip the prompt: `tool` for every call, or `tool:pattern` where
`*` matches anything; `mcp__server` covers all of a server's tools. A command
with `;`, `&&`, `|`, `$(...)` or redirects only matches a rule that spells it
out exactly, so `cargo *` can't approve `cargo test; rm -rf ~`.

## How it works

```
 key reader ─┐
             ├─> UI thread ── commands ──> agent thread ──> Claude API
 agent ──────┘   (draws)    <── events ───  (runs tools; subagents and
                                             parallel calls on more threads)
```

- **No async runtime.** Plain threads and channels. The UI blocks on one
  channel, so it uses no CPU while you think, and it only writes to the
  terminal when the picture changed.
- **Messages are stored as the JSON text that goes over the wire.** A request
  is streamed straight from them and the session file is the same text.
- **The history only grows.** That keeps the prompt cache warm and the model's
  earlier reasoning valid. The system prompt and tool list are fixed per
  session; mode changes, notes and job notices go into the next user message.
- **The UI writes finished lines once** into normal terminal scrollback and
  redraws only the few lines at the bottom.

```
src/
  main.rs         args, config, start
  agent/          conversation, tool loop, subagents, modes and approval
  tools/          read/write/edit, bash and jobs, todo, skills, MCP client
  api.rs          requests, streaming, retries, per-model features, prices
  prompt.rs       system prompt, AGENTS.md, notes
  session.rs      saved conversations
  sandbox.rs      bubblewrap
  signals.rs      clean exit on signals
  permission.rs   allow rules
  config.rs       config files
  print.rs        -p output
  ui/             terminal UI: loop, drawing, input, markdown, /stats
bench/
  mock.py         fake API for tests and benchmarks
  mem.sh          long session; fails above 16 MB
  cpu.sh          idle and busy CPU
  box.sh          run in a 0.1 CPU / 64 MB container
  sandbox.sh      check the sandbox
```

## Measuring it yourself

| What | How |
|---|---|
| Inside the app | `/stats` |
| Idle and busy CPU, memory | `bench/cpu.sh` |
| Memory budget over a long session | `bench/mem.sh` |
| In a 0.1 CPU / 64 MB box | `bench/box.sh` |
| A running session | `ps -o rss,pcpu,nlwp -p $(pgrep -x claude-jr)` |
| One prompt | `/usr/bin/time -v claude-jr -p "..."` |

None of the scripts need an API key; they run against `bench/mock.py`.

## Development

```sh
cargo test
bench/mem.sh && bench/cpu.sh && bench/sandbox.sh
```

## Not included

Other providers, plugins, hooks, web tools, IDE integration, Windows.
