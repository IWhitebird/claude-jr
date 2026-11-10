# Using claude-jr

Everything the command does, in one place. The short version is in the [README](../README.md).

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

## Not included

Other providers, plugins, hooks, IDE integration, Windows.
