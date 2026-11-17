<p align="center">
  <img src="assets/logo-animated.svg" width="140" alt="claude-jr (Claude Junior) mascot">
</p>

<h1 align="center">claude-jr</h1>

<p align="center">
  <b>Claude Junior</b>: a full coding agent for the terminal in one 2 MB binary.<br>
  About 180 times smaller than Claude Code and 200 times lighter in memory, and it resolves 93.4% of SWE-bench Verified.
</p>

<p align="center">
  <a href="https://github.com/IWhitebird/claude-jr/actions/workflows/ci.yml"><img src="https://github.com/IWhitebird/claude-jr/actions/workflows/ci.yml/badge.svg" alt="tests"></a>
  <img src="https://img.shields.io/badge/license-MIT-blue" alt="MIT license">
  <img src="https://img.shields.io/badge/platform-Linux%20%7C%20macOS-lightgrey" alt="Linux and macOS">
</p>

<table align="center">
  <tr>
    <td align="center"><b>2 MB</b><br>binary, no runtime</td>
    <td align="center"><b>0.7 MB</b><br>memory when idle</td>
    <td align="center"><b>5 ms</b><br>to start</td>
    <td align="center"><b>93.4%</b><br>SWE-bench Verified<br>all 500 tasks, Opus 5.5</td>
  </tr>
</table>

---

claude-jr (Claude Junior) is a small, fast alternative to Claude Code, written
in Rust. It reads and edits your files and runs commands, asking before
anything changes, with Claude or any other model. On SWE-bench Verified it resolved 467 of the
500 tasks (93.4%).

It is also built to be run many at once. Claude Code, Cursor or Codex can hand
it tasks over MCP; each worker gets its own git worktree and branch and holds
about half a megabyte, so a thousand of them fit in 500 MB.

## Install

```sh
curl -fsSL https://claudejr.iwhitebird.com/install | sh
```

Linux and macOS, x86_64 and ARM. On Windows, from the next release (0.3.0),
`irm https://claudejr.iwhitebird.com/install.ps1 | iex` in PowerShell; until
then, inside WSL. Then run
`claudejr`; it asks for your Anthropic API key the first time, or start it with
another provider's model (`claudejr -m ollama/qwen3`). `claudejr --update` gets
the newest release.

## Why

- **Tiny.** One static 2 MB file. No Node, no Python, nothing to install first.
- **Light.** 0.7 MB of memory when idle, and under 4 MB however long the session gets, because the conversation lives on disk instead of in memory. A whole session, commands included, fits in a 16 MB container on a twentieth of a CPU core.
- **Fast.** It starts in 5 ms and uses no CPU while it waits.
- **Cheap to run.** A short fixed prompt, about 3,000 tokens with its tools. A SWE-bench task costs about 5 cents with Sonnet and 13 with Opus.
- **Made for many.** Background tasks, each in its own worktree and branch, with a shared budget and a log of every step. Workers run on Haiku unless you say otherwise, at well under a cent for a small task. `claudejr fleet` runs a list of them; `claudejr install claude-code` lets Claude Code (or Cursor, or Codex) start and collect them.
- **Any model.** Claude by default; OpenAI, Gemini, Groq, OpenRouter, Ollama and other local servers with `-m provider/model`, at the same memory and CPU. Claude through Amazon Bedrock or Google Vertex AI with `-m bedrock/...` and `-m vertex/...` (in the next release; not yet tested against the real services).
- **Complete.** Subagents, skills, MCP, images, web search, rewind, a sandbox and five permission modes.
- **Tested.** 401 tests with 97% line coverage, plus random-input, resize and chaos runs against the real app in a terminal.

## Benchmarks

Next to Claude Code, on the same laptop and the same fake API, so no model time is counted:

|                               | claude-jr | Claude Code | times less |
|-------------------------------|-----------|-------------|------------|
| install size                  | 2.0 MB    | 354 MB      | 177        |
| memory, idle                  | 0.7 MB    | 148 MB      | 200        |
| memory at its peak, 20 tool calls | 2.2 MB | 289 MB     | 130        |
| its own CPU for 20 tool calls | 40 ms     | 1,140 ms    | 28         |
| startup                       | 4 ms      | 157 ms      | 40         |

Claude Code 2.1.326, measured 18 November 2026 with `bench/compare.py`. Claude
Code does a good deal more than claude-jr; this is what the difference costs.
The memory of one idle Claude Code session holds 200 of claude-jr's, and a
thousand claude-jr agents working at once hold 492 MB between them
(`bench/density.py`).

On disk, next to other terminal agents (what each one's installer leaves on
Linux x86-64; for the npm ones, Node itself not counted):

| claude-jr | Gemini CLI 0.68 | pi 0.73 | Claude Code 2.1 | Codex CLI 0.169 | opencode 1.19 |
|-----------|-----------------|---------|-----------------|-----------------|---------------|
| **2 MB**  | 103 MB          | 150 MB  | 354 MB          | 423 MB          | 432 MB        |

[SWE-bench Verified](https://www.swebench.com/), one attempt per task, official scoring:

| model             | tasks        | resolved        | cost   |
|-------------------|--------------|-----------------|--------|
| claude-opus-5-5   | all 500      | 467 (93.4%)     | $67.32 |
| claude-sonnet-5-5 | 100 of 500   | 88 (88%)        | $5.06  |

Sonnet's 100 were picked at random from the 500. The score is mostly the
model's; what it shows is that a small agent doesn't hold the model back.

## How it stays small

- **The conversation lives on disk.** Messages are appended to the session file and requests are streamed from it, so a session with 8 MB of history uses the same 4 MB as a short one.
- **No runtime.** One static binary: no JavaScript engine to load, no garbage collector holding memory.
- **Threads, not an async framework.** It talks to one model at a time; a few threads that sleep in the kernel are enough, hence zero CPU when idle.
- **A short, fixed prompt** and a history that only grows, so the prompt cache stays valid for the whole session.
- **Unused pages go back** to the system when it goes quiet (Linux), which is how it settles at 0.7 MB.

How everything is measured, and the rest of the numbers, including the ones
that didn't go our way: [docs/BENCHMARKS.md](docs/BENCHMARKS.md).

## Use

| Command | What it does |
|---|---|
| `claudejr` | start in this folder |
| `claudejr -c` | continue the last conversation here |
| `claudejr -r NAME` | continue a conversation by name |
| `claudejr -p "explain main.rs"` | one prompt, print the answer, exit |
| `claudejr -p "..." --output-format json --max-cost 2` | for scripts: JSON result, stop at $2 |
| `claudejr --sandbox` | only allow writes inside this folder |
| `claudejr -m ollama/qwen3` | use another provider's model (`openai/...`, `gemini/...`, `groq/...`) |
| `claudejr task start -y "fix the flaky test"` | run a task in the background, on its own branch |
| `claudejr fleet -y --budget 5 "task one" "task two"` | run a list of tasks side by side |
| `claudejr install claude-code` | let Claude Code hand tasks to claude-jr workers |
| `claudejr --update` | get the newest release |

| Inside it | |
|---|---|
| `/` | list every command |
| `/model`, `/effort` | change the model or how hard it thinks |
| `/resume`, `/rewind`, `/compact` | switch conversation, go back, free up context |
| shift-tab | change the permission mode |
| esc | stop the current turn |
| ctrl-v | paste text or a screenshot |
| ctrl-s | stash what you've typed; it comes back after your next message |

## Docs

- [Guide](docs/GUIDE.md): commands, keys, modes, tools, skills, MCP, the sandbox, other models, tasks and fleets, working for another agent, config.
- [Benchmarks](docs/BENCHMARKS.md): every number and how to reproduce it.
- [Architecture](docs/ARCHITECTURE.md): how it is built, and where everything is.
- [Website](https://claudejr.iwhitebird.com)

MIT licence.
