<p align="center">
  <img src="assets/logo.svg" width="140" alt="claude-jr mascot">
</p>

<h1 align="center">claude-jr</h1>

<p align="center">
  A coding agent for the terminal in one 3 MB binary.<br>
  It does what the big ones do, at a hundredth of the size.
</p>

<p align="center">
  <a href="https://github.com/IWhitebird/claude-jr/actions/workflows/ci.yml"><img src="https://github.com/IWhitebird/claude-jr/actions/workflows/ci.yml/badge.svg" alt="tests"></a>
  <img src="https://img.shields.io/badge/license-MIT-blue" alt="MIT license">
  <img src="https://img.shields.io/badge/platform-Linux-lightgrey" alt="Linux">
</p>

<table align="center">
  <tr>
    <td align="center"><b>3.2 MB</b><br>binary, no runtime</td>
    <td align="center"><b>2 MB</b><br>memory when idle</td>
    <td align="center"><b>5 ms</b><br>to start</td>
    <td align="center"><b>88–90%</b><br>SWE-bench Verified<br>(first sample)</td>
  </tr>
</table>

---

claude-jr talks to Claude, reads and edits your files, and runs commands,
asking before anything changes. It is written in Rust with no async runtime
and a handful of dependencies, so there is no Node or Python to install and
nothing running in the background. Claude Code needs 135 MB of memory to sit
idle; claude-jr needs 2, and a whole working session fits in a 16 MB
container on a twentieth of a CPU core.

```sh
git clone https://github.com/IWhitebird/claude-jr && cd claude-jr
cargo install --path .     # installs the command: claudejr
claudejr                   # asks for your Anthropic API key the first time
```

```
> add a --verbose flag to the cli

● Bash(grep -n "struct Args" -A12 src/cli.rs)
  ⎿ 9:pub struct Args {
    … +11 lines

● Edit(src/cli.rs)
    13 + /// Print debug output
    14 + #[arg(short, long)]
    15 + pub verbose: bool,
  ⎿ Edited src/cli.rs (1 replacement)

● Bash(cargo build)
  ⎿ Finished `dev` profile in 2.1s

● Added `--verbose` to `Args`. The build passes.
```

What's in it:

- **Tools:** read, write, edit, bash, background jobs, a todo list, questions to you, web search and fetch.
- **Subagents** that work in parallel, **skills** from `.claude/skills`, and **MCP** servers (local and remote).
- **Five permission modes**, from asking about everything to an auto mode where a small model reviews each command, plus a **sandbox** that only lets it write inside the project.
- **Sessions** that are saved as they go: continue, resume by name, rewind (which also undoes file changes), compact.
- **Images** by dragging a file in or pasting a screenshot, and **memory** that carries notes between sessions.
- **For scripts:** `-p` mode with JSON output and a spending limit.

## Numbers

Measured with [`bench/compare.py`](bench/compare.py): every agent runs against
the same fake API (no model time counted), with a fresh empty home folder.
Busy = one prompt that makes 20 bash calls with streamed text in between.

|                               | claude-jr | Claude Code | pi       |
|-------------------------------|-----------|-------------|----------|
| install size                  | 3.2 MB    | 324.5 MB    | 109.0 MB |
| startup (screen settles)      | 5 ms      | 167 ms      | ~260 ms  |
| memory, idle                  | 2.0 MB    | 135.0 MB    | 141.1 MB |
| processes / threads, idle     | 1 / 4     | 1 / 15      | 1 / 11   |
| CPU while idle 10s            | 0 ms      | 70 ms       | 0 ms     |
| busy: wall time               | 0.10 s    | 1.84 s      | 1.24 s   |
| busy: agent's own CPU         | 30 ms     | 1150 ms     | 580 ms   |
| busy: agent's own peak memory | 2.5 MB    | 270.2 MB    | 137.5 MB |
| busy: CPU incl. commands      | 114 ms    | 1285 ms     | 671 ms   |

Claude Code 2.1.319, pi 1.5.0, claude-jr static x86_64 build. Codex 0.167.0
was measured idle only (it speaks OpenAI's API): 150 MB, 37 threads, 322 ms
startup, with a 306 MB binary.

More of claude-jr's own numbers (`bench/`):

| | |
|---|---|
| after 60 tool calls (2.5 MB of history) | 4.9 MB peak; `/clear` brings it back to 2 MB |
| 100 tool calls (4.2 MB of history, beyond a full context) | 6.4 MB peak |
| the UI while a reply streams at ~200 tokens/s | 0.6% of one core |
| a whole session (20 commands) in a container, agent plus the commands it runs | finishes in 6 MB of RAM (Docker's minimum) at 0.1 CPU, and at 0.01 CPU with 16 MB |
| the binary, compressed (xz) | 1.3 MB |

## SWE-bench Verified

A first run, on a fixed random sample of [SWE-bench Verified](https://www.swebench.com/) tasks, scored
with the official harness ([`bench/swe/run.py`](bench/swe/run.py), results in `bench/swe/results/`):

| model | tasks | resolved | cost | median time per task |
|---|---|---|---|---|
| claude-sonnet-5-5 | 100 | 88 (88%) | $5.06 | 14 s |
| claude-opus-5-5 | 50 (the first 50 of the same list) | 45 (90%) | $5.33 | 27 s |

On those same 50 tasks Sonnet resolved 44. With samples this small the true rates could be anywhere
from about 80% to 95%, so the two models can't be told apart here, and this is not a leaderboard
entry. The tasks are public and old enough that the models may have seen them. Each task ran once,
in the task's own container, with no retries and no hints.

## Install

```sh
git clone https://github.com/IWhitebird/claude-jr && cd claude-jr
cargo install --path .        # installs the command: claudejr
```

Fully static binary (Linux):

```sh
rustup target add x86_64-unknown-linux-musl
CC_x86_64_unknown_linux_musl=gcc cargo build --release --target x86_64-unknown-linux-musl
```

## Use

```sh
claudejr                         # start; the first time, it asks for your API key
claudejr --sandbox               # only allow writes inside this directory
claudejr -c                      # continue the last conversation here
claudejr -r "auth refactor"      # continue a named conversation
claudejr -p "explain main.rs"    # one prompt, print the answer, exit
claudejr -m claude-sonnet-5-5    # another model (default: claude-opus-5-5)
claudejr -p "fix the tests" -y --output-format json --max-cost 2   # for scripts: JSON result, stop at $2
```

| Key | |
|---|---|
| enter | send |
| ctrl-j, shift-enter, `\` + enter | new line |
| shift-tab | change mode |
| esc | stop the current turn |
| ctrl-c | clear input; twice to quit |
| up / down | history |
| ctrl-v | paste: an image if the clipboard has one, otherwise its text |

A long paste shows in the input as a short label, such as `[Pasted #1: 40 lines, 3104 chars]`;
the whole text is sent. Backspace removes the label in one go.

Type `/` to see every command; keep typing to narrow the list, up/down to pick,
tab to complete, enter to run.

| Command | |
|---|---|
| `/model` | pick a model from the list (price, what it's for), or `/model <id>` |
| `/effort LEVEL` | low, medium, high, xhigh, max, or default |
| `/rewind` | go back to before an earlier message; undoes the agent's file changes |
| `/compact [focus]` | replace the conversation with a summary to free up context |
| `/resume` | pick an earlier conversation in this folder |
| `/rename NAME` | name this conversation |
| `/clear`, `/new` | start over on a clean screen |
| `/jobs`, `/kill N` | background jobs |
| `/settings` | layout, model, effort, starting mode, web tools, sandbox |
| `/memory`, `/skills`, `/mcp` | what's loaded |
| `/stats` | memory and CPU of claude-jr and everything it started |
| `/cost` | tokens and spend so far |
| `/login`, `/logout` | add, change or remove your API key |
| `/<skill> args` | run a skill |
| `#some note` | save a note to `./AGENTS.md` |
| `!command` | run a command yourself; the agent sees the output |

Images: drag a file into the terminal (its path is pasted and the image is
attached) or press ctrl+v with a screenshot in the clipboard. The agent can
also `read` image files, like a design or a screenshot of your app. Images are
copied once into `~/.local/share/claude-jr/images/` and encoded from disk while
a request is sent, so they don't sit in memory.

The input stays on the bottom rows of the screen and the conversation scrolls
above it; running subagents and background jobs are listed under the input.
Resizing the terminal redraws what's on screen at the new width. Tables are
drawn with boxes and a line between rows.
`"layout": "inline"` in the config (or `/settings`) puts the input right after
the conversation instead.

Your key is saved to `~/.config/claude-jr/credentials`, readable only by you.
`ANTHROPIC_API_KEY` works too and takes priority.

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
| `memory` | notes the agent keeps about you and the project, loaded every session |
| `skill` | loads a skill's instructions |
| `web_search`, `web_fetch` | Anthropic's server-side tools: nothing runs locally, and they are not asked about first (see MCP below); set `"web_tools": false` to turn them off |
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

Remote MCP tools and web search/fetch run on Anthropic's servers inside one
request, so claude-jr only sees them after they have run: they are not
approved one by one, and plan mode does not stop them. Each call is still
listed in the transcript with its arguments, and failures are marked. To avoid
this, use local servers (every call goes through the normal permission check),
leave out `url` servers, and set `"web_tools": false` in the config. Remote
servers come only from your own config or from a trusted folder's `.mcp.json`;
an untrusted folder's `.mcp.json` is ignored.

## Sandbox

```sh
claudejr --sandbox
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
git worktree add /tmp/try && cd /tmp/try && claudejr --sandbox
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
- **The agent's own notes**: `~/.local/share/claude-jr/memory/<project>/MEMORY.md`,
  kept with the `memory` tool (8 KB cap) and loaded at the start of each session.
- **Rewind**: before the agent first changes a file in a turn, the original is
  copied next to the session. `/rewind` puts files back; shell commands' changes
  aren't tracked.
- **Compaction**: `/compact` replaces the conversation with a detailed summary.
  Near the limit, the API also compacts on its side.

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
  "layout": "bottom",
  "mode": "normal",
  "web_tools": true,
  "mcp_servers": {}
}
```

`allow` rules skip the prompt: `tool` for every call, or `tool:pattern` where
`*` matches anything; `mcp__server` covers all of a server's tools. A command
with `;`, `&&`, `|`, `$(...)` or redirects only matches a rule that spells it
out exactly, so `cargo *` can't approve `cargo test; rm -rf ~`.

## How it works

The full picture, with diagrams, is in [ARCHITECTURE.md](ARCHITECTURE.md).

```
 key reader ─┐
             ├─> UI thread ── commands ──> agent thread ──> Claude API
 agent ──────┘   (draws)    <── events ───  (runs tools; subagents and
                                             parallel calls on more threads)
```

- **No async runtime.** Plain threads and channels. The UI blocks on one
  channel, so it uses no CPU while you think. While text streams in it draws
  at most ~30 frames a second and rewrites only the lines that changed;
  finished rows of a long paragraph go to scrollback as soon as they can't
  change.
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
  ui.py           the UI while a reply streams in
  compare.py      side by side with Claude Code, pi and Codex
  box.sh          run in a small container (CPUS=0.05 MEMORY=16m bench/box.sh)
  sandbox.sh      check the sandbox
```

## Measuring it yourself

| What | How |
|---|---|
| Against Claude Code, pi and Codex | `python3 bench/compare.py` |
| Inside the app | `/stats` |
| Idle and busy CPU, memory | `bench/cpu.sh` |
| The UI while a reply streams | `python3 bench/ui.py target/release/claudejr` |
| Resizing leaves nothing twice (needs tmux) | `python3 bench/resize.py target/release/claudejr` |
| Line coverage of the tests (98%) | `bench/coverage.sh` |
| SWE-bench Verified, one task at a time in Docker | `bench/swe/run.py --dry-run` (setup at the top of the file) |
| Memory budget over a long session | `bench/mem.sh` |
| In a small container | `CPUS=0.05 MEMORY=16m bench/box.sh` |
| A running session | `ps -o rss,pcpu,nlwp -p $(pgrep -x claude-jr)` |
| One prompt | `/usr/bin/time -v claudejr -p "..."` |

None of the scripts need an API key; they run against `bench/mock.py`.

## Evals

```sh
python3 bench/evals/run.py frontend --budget 2          # 10 frontend tasks
python3 bench/evals/run.py frontend --model claude-sonnet-5-5
python3 bench/evals/run.py polyglot-python --limit 10   # Aider's polyglot benchmark
python3 bench/evals/run.py frontend --agent claude-code # the same tasks with Claude Code
python3 bench/evals/run.py frontend --dry-run           # check the runner, free
```

`frontend/` has 10 tasks: semantic HTML, form accessibility, a responsive grid,
dark mode, a cart bug, relative dates, form validation, debounce, safe HTML
rendering, and a pricing page built from a design image. JS tasks are graded by
hidden tests; every check was verified to fail on the starting files and pass on
a correct solution. `--budget` stops before a task that would likely go over.

## Development

```sh
cargo test                                      # 42 tests
bench/mem.sh && bench/cpu.sh && bench/sandbox.sh
```

## Not included

Other providers, plugins, hooks, web tools, IDE integration, Windows.
