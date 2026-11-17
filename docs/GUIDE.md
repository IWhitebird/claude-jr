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
- **read-only**: the rest of the system, toolchains in `$HOME` (`.cargo`, `.rustup`, `.nvm`, ...),
  and the project's git hooks and git config, so nothing can be planted that runs on your next
  git command
- **hidden**: everything else in `$HOME` (ssh keys, tokens, other projects), your session's
  sockets in `$XDG_RUNTIME_DIR` (D-Bus, ssh and gpg agents, the desktop), and the Docker,
  Podman and containerd sockets, any of which would be a way out

What it doesn't cover:

- **The project's own files.** They are writable by design, so a command can still change a
  `Makefile`, a script, or `.envrc` that you later run outside the sandbox. Review changes
  before running them.
- **Package caches.** `~/.cargo/registry`, `~/.npm`, `~/.cache` stay writable so builds work;
  a command could change downloaded source there that a later build outside the sandbox uses.
- **The network.** It stays on, because the agent needs the API.
- **Pasting a screenshot** with ctrl+v doesn't work inside it, since the desktop's clipboard
  socket is hidden. Dragging an image file in still does.

`bench/sandbox.sh` checks all of this: a fake model tries to delete a file outside the
project, write to `$HOME`, plant a git hook, change the git config, and reach the Docker
socket and the session bus. All must fail, while a write and a commit in the project
must work. To try the agent on a repo without risking anything, use a throwaway worktree:

```sh
git worktree add /tmp/try && cd /tmp/try && claudejr --sandbox
```

Linux only for now.

## Windows

New, and so far checked only on GitHub's Windows machines and not by a person
at a Windows desk; please report what you find. Install from PowerShell:

```powershell
irm https://claudejr.iwhitebird.com/install.ps1 | iex
```

What is different there:

- **Commands run in Git Bash**, the bash that comes with
  [Git for Windows](https://git-scm.com/download/win), so the model's usual
  `grep`, `ls` and pipes work. Without Git, commands run in PowerShell and the
  model is told to write PowerShell. `CLAUDE_JR_SHELL` names another shell.
- **Files** are in `%APPDATA%\claude-jr` (config, the saved key) and
  `%LOCALAPPDATA%\claude-jr` (sessions, tasks, worktrees). `XDG_CONFIG_HOME`
  and `XDG_DATA_HOME` move them, as elsewhere.
- **No sandbox** (`--sandbox` is Linux's bubblewrap) and no idle memory trim,
  so memory at rest is higher than the Linux figures.
- Esc while a reply has not yet begun stops as soon as the server reacts to
  the closed connection, which the real ones do at once; a server that
  ignores it can keep the wait going for up to a minute.
- `/stats` has no numbers to show there.
- Windows Terminal, or the terminal in VS Code, is the one to use; the old
  console window has not been tried.
- 64-bit Intel/AMD builds; on ARM Windows they run emulated. WSL still works
  too, with the Linux build.

## Other models

A model id with a provider's name in front goes to that provider; a plain id
is Anthropic's, as before. (Claude through a cloud account is the next section.)

```sh
claudejr -m ollama/qwen3:8b            # a local model, no key
claudejr -m openai/gpt-5               # needs OPENAI_API_KEY
claudejr -m openrouter/meta/llama-4    # needs OPENROUTER_API_KEY
```

| Provider | Key |
|---|---|
| `openai`, `openrouter`, `gemini`, `groq`, `deepseek`, `mistral`, `xai` | `OPENAI_API_KEY`, `OPENROUTER_API_KEY`, `GEMINI_API_KEY`, `GROQ_API_KEY`, `DEEPSEEK_API_KEY`, `MISTRAL_API_KEY`, `XAI_API_KEY` |
| `ollama`, `lmstudio`, `llamacpp` (local servers on their usual ports; `OLLAMA_HOST` is read) | none |

Any other server that speaks Chat Completions or the Messages format can be
added in the config and used as `-m work/some-model`:

```json
{ "providers": {
    "work": { "base_url": "https://llm.example.com/v1", "key_env": "WORK_LLM_KEY", "context": 128000 },
    "other": { "base_url": "https://api.example.ai", "api": "anthropic", "key_env": "OTHER_KEY" }
} }
```

Things to know:

- The model has to be good at calling tools; small local models often aren't.
- Ollama gives a model a 4,096-token context unless told otherwise, which is
  less than claude-jr's own instructions and tools need. Raise it (for example
  `OLLAMA_CONTEXT_LENGTH=32768 ollama serve`).
- Web search, cost figures, automatic compaction and effort levels are
  Anthropic's; with other models `/compact` still works when you run it.
- A conversation can be continued with a different provider's model: the
  history is kept in one format and translated for whoever is asked.

## Claude through Amazon Bedrock or Google Vertex AI

The same Claude models, billed by your cloud account. **Not yet tested
against the real services**: there was no AWS or Google Cloud account to test
with, so this is built from the documentation and checked against a stand-in
that refuses what each cloud documents it refuses (and checks every AWS
signature). `bench/provider.sh bedrock/claude-haiku-5-5` is the real test; if
you run it, please say how it went.

```sh
claudejr -m bedrock/claude-opus-5-5
claudejr -m vertex/claude-opus-5-5
```

| | Bedrock | Vertex |
|---|---|---|
| Login | `AWS_BEARER_TOKEN_BEDROCK` (a Bedrock API key), or `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY` (and `AWS_SESSION_TOKEN` if you have one) | `VERTEX_ACCESS_TOKEN`, or else whatever `gcloud auth print-access-token` prints |
| Where | `AWS_REGION` (then `AWS_DEFAULT_REGION`, then `us-east-1`) | `ANTHROPIC_VERTEX_PROJECT_ID` (or `GOOGLE_CLOUD_PROJECT`), and `CLOUD_ML_REGION` (`global` if unset) |
| Model ids | Claude's own names; `anthropic.` is put in front for you | Claude's own names; older models with their date, as in `claude-haiku-4-5@20251001` |
| A gateway in front | `ANTHROPIC_BEDROCK_BASE_URL` | `ANTHROPIC_VERTEX_BASE_URL` |

Things to know:

- Thinking, effort levels, prompt caching, automatic compaction, costs and
  context sizes work as they do with Anthropic directly.
- Neither cloud has web search and fetch as claude-jr uses them, the refusal
  fallback, or remote MCP servers, so those are left out. Local MCP servers work.
- Bedrock is spoken to at its Messages address (`bedrock-mantle`). Models it
  serves only at the older address (Fable 5.2 today) are not available, and
  neither are `~/.aws` profiles, SSO or instance roles: export the keys.
- With access keys every request is signed, which means reading the
  conversation through once more to hash it: about 2 ms for a 4 MB history. A
  Bedrock API key avoids that.
- Costs shown are Anthropic's list prices; a single-region endpoint costs a
  tenth more than that.
- On Vertex, a service-account key file is not read: use `gcloud`, or pass a token.

## Running many at once

Three pieces make a run something another program, or another agent, can
start, watch and collect.

**A worktree of its own.** `--worktree` does the work in a git worktree on a
new branch, so several runs in one repository can't touch each other's files
or yours. It starts from the current commit and lives outside the repository.
A headless run commits what it changed; a worktree in which nothing changed is
removed again.

```sh
claudejr --worktree -y -p "fix the flaky test"
# The work is on branch claudejr/6a7d01 (1 commit) ...
```

**A record of what it did.** `--log FILE` writes one JSON line per step:
each tool call with its input, what allowed it to run (the mode, an allow
rule, the reviewer, you) or that it was refused, how it went and how long it
took. `--output-format stream-json` prints the same lines to stdout as they
happen. Both end with the result, which `--output-format json` prints alone:
the final answer, the files changed, the branch, cost and tokens.

**Tasks.** A task is a run that carries on in the background:

```sh
id=$(claudejr task start -y "add tests for the parser")   # returns at once
claudejr task list                # what is running, what finished
claudejr task wait $id            # blocks until it has finished
claudejr task result $id          # the outcome as JSON, or how far it is
claudejr task log $id -f          # follow every step
claudejr task merge $id           # take its branch, clean up
claudejr task drop $id            # or throw it away
```

In a git repository each task gets its own worktree and branch, named after
its id. A group of tasks can share one budget, and all stop when it is used
up:

```sh
for t in "fix lint in api" "fix lint in ui" "fix lint in tools"; do
  claudejr task start -y --group lint --budget 2.00 "$t"
done
claudejr task wait
```

Each worker holds about half a megabyte, so the number you can run at once is
set by your API rate limits, not by memory.

**A fleet.** The same in one command: a list of tasks, run a few at a time,
with a table at the end of what each did and the branch it left.

```sh
claudejr fleet -y --max 4 --budget 5 "fix lint in api" "fix lint in ui" "fix lint in tools"
claudejr fleet -y --merge --file tasks.txt     # one task per line; merge each branch when done
```

**Subagents in worktrees.** Inside a session the agent can do the same with
its own subagents: one started with `worktree: true` works in its own copy of
the repository, its work is committed when it finishes, and its report ends
with the branch for the main agent to merge. A subagent can start subagents of
its own, one level down and no further, and at most 16 run at once.

**The worker model.** Work that is handed off (a task, a fleet's tasks, a
task another agent starts over MCP, a subagent) is done by the worker model,
not the one you talk to. Beside a Claude model that is `claude-haiku-5-5`,
which does a well-described job at a fortieth of Opus's price; beside any
other provider's model, or Claude through Bedrock or Vertex, it is that model
itself.

| To change it | |
|---|---|
| for one task | `claudejr task start -m claude-sonnet-5-5 "..."`, `claudejr fleet -m ...`, or `model` in an MCP `start_task` call |
| for every worker | `"worker_model": "claude-sonnet-5-5"` in the config, or `CLAUDE_JR_WORKER_MODEL` |
| for an agent's workers | `claudejr install claude-code -m claude-sonnet-5-5` |

`task list`, a fleet's table and every task's result name the model that did
the work.

## Working for another agent

Another coding agent can hand work to claude-jr workers and stay the one you
talk to. `claudejr mcp` is an MCP server whose tools start tasks, wait for
them, read their results, logs and diffs, and merge or drop their branches.

```sh
claudejr install claude-code     # this project: .mcp.json and a skill
claudejr install cursor          # this project: .cursor/mcp.json and a rule
claudejr install codex           # ~/.codex/config.toml
claudejr install claude-code --user     # every project
claudejr uninstall claude-code   # takes it out again
```

What the workers may do is yours to decide, not the calling agent's: it is set
when you install, with the server's options.

| Installed with | Workers can |
|---|---|
| nothing, on Linux with bubblewrap | edit and run commands, confined to their worktree |
| nothing, elsewhere | read only; the install says so |
| `-y` | edit and run commands |
| `-y --sandbox` | the same, confined to their worktree (Linux) |
| `-m MODEL` | use that model, not the worker model (a call can still name another) |
| `--budget USD` | spend that much between them, then stop |

Any other MCP client can use it too: the command is `claudejr mcp` plus those
options, over stdio.

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
  "worker_model": "claude-haiku-5-5",
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

Plugins, hooks, IDE integration.
