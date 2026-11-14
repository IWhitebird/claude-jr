<p align="center">
  <img src="assets/logo.svg" width="140" alt="claude-jr (Claude Junior) mascot">
</p>

<h1 align="center">claude-jr</h1>

<p align="center">
  <b>Claude Junior</b>: a full coding agent for the terminal in one 1.8 MB binary.<br>
  A small, fast alternative to Claude Code: 180 times less disk and 190 times less memory.
</p>

<p align="center">
  <a href="https://github.com/IWhitebird/claude-jr/actions/workflows/ci.yml"><img src="https://github.com/IWhitebird/claude-jr/actions/workflows/ci.yml/badge.svg" alt="tests"></a>
  <img src="https://img.shields.io/badge/license-MIT-blue" alt="MIT license">
  <img src="https://img.shields.io/badge/platform-Linux%20%7C%20macOS-lightgrey" alt="Linux and macOS">
</p>

<table align="center">
  <tr>
    <td align="center"><b>1.8 MB</b><br>binary, no runtime</td>
    <td align="center"><b>0.7 MB</b><br>memory when idle</td>
    <td align="center"><b>5 ms</b><br>to start</td>
    <td align="center"><b>88%</b><br>SWE-bench Verified<br>100-task sample, Sonnet 5.5</td>
  </tr>
</table>

---

claude-jr (Claude Junior) is a small, fast alternative to Claude Code, written
in Rust. It talks to Claude, reads and edits your files, and runs commands,
asking before anything changes. On a 100-task sample of SWE-bench Verified it
resolved 88%.

## Install

```sh
curl -fsSL https://claudejr.iwhitebird.com/install | sh
```

Linux and macOS, x86_64 and ARM. On Windows, run it inside WSL. Then run
`claudejr`; it asks for your Anthropic API key the first time. `claudejr --update`
gets the newest release.

## Why

- **Tiny.** One static 1.8 MB file. No Node, no Python, nothing to install first.
- **Light.** 0.7 MB of memory when idle, and under 4 MB however long the session gets, because the conversation lives on disk instead of in memory. A whole session, commands included, fits in a 16 MB container on a twentieth of a CPU core.
- **Fast.** It starts in 5 ms and uses no CPU while it waits.
- **Cheap to run.** A short fixed prompt, about 3,000 tokens with its tools. A SWE-bench task costs 5 to 10 cents.
- **Complete.** Subagents, skills, MCP, images, web search, rewind, a sandbox and five permission modes.
- **Tested.** 315 tests with 98% line coverage, plus random-input, resize and chaos runs against the real app in a terminal.

## Benchmarks

Against other agents, on the same fake API so no model time is counted:

|                              | claude-jr | Claude Code | pi       |
|------------------------------|-----------|-------------|----------|
| install size                 | 1.8 MB    | 324.5 MB    | 109.0 MB |
| startup                      | 5 ms      | 167 ms      | ~260 ms  |
| memory, idle                 | 0.7 MB    | 135.0 MB    | 141.1 MB |
| memory, working              | 2.0 MB    | 270.2 MB    | 137.5 MB |
| its own CPU for 20 tool calls| 30 ms     | 1150 ms     | 580 ms   |

[SWE-bench Verified](https://www.swebench.com/), one attempt per task, official scoring:

| model             | tasks        | resolved | cost  |
|-------------------|--------------|----------|-------|
| claude-sonnet-5-5 | 100 of 500   | 88%      | $5.06 |
| claude-opus-5-5   | 50 of 500    | 90%      | $5.33 |

The tasks were picked at random from the full set of 500. How everything is
measured, and the rest of the numbers: [docs/BENCHMARKS.md](docs/BENCHMARKS.md).

## Use

| Command | What it does |
|---|---|
| `claudejr` | start in this folder |
| `claudejr -c` | continue the last conversation here |
| `claudejr -r NAME` | continue a conversation by name |
| `claudejr -p "explain main.rs"` | one prompt, print the answer, exit |
| `claudejr -p "..." --output-format json --max-cost 2` | for scripts: JSON result, stop at $2 |
| `claudejr --sandbox` | only allow writes inside this folder |
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

- [Guide](docs/GUIDE.md): commands, keys, modes, tools, skills, MCP, the sandbox, memory, sessions, config.
- [Benchmarks](docs/BENCHMARKS.md): every number and how to reproduce it.
- [Architecture](ARCHITECTURE.md): how it is built.
- [Website](https://claudejr.iwhitebird.com)

MIT licence.
