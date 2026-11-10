<p align="center">
  <img src="assets/logo.svg" width="140" alt="claude-jr (Claude Junior) mascot">
</p>

<h1 align="center">claude-jr</h1>

<p align="center">
  <b>Claude Junior</b>: a coding agent for the terminal in one 3 MB binary.<br>
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

claude-jr (Claude Junior) is a small, fast alternative to Claude Code, written
in Rust. It talks to Claude, reads and edits your files, and runs commands,
asking before anything changes.

```sh
git clone https://github.com/IWhitebird/claude-jr && cd claude-jr
cargo install --path .     # installs the command: claudejr
claudejr                   # asks for your Anthropic API key the first time
```

## Why

- **Tiny.** One static 3.2 MB file. No Node, no Python, nothing to install first.
- **Light.** 2 MB of memory when idle and 5 to 6 MB in a long session. A whole session, including the commands it runs, fits in a 16 MB container on a twentieth of a CPU core.
- **Fast.** It starts in 5 ms and uses no CPU while it waits.
- **Cheap to run.** A short fixed prompt, about 3,000 tokens with its tools. The first SWE-bench sample cost 5 to 10 cents a task.
- **Complete.** Subagents, skills, MCP, images, web search, rewind, a sandbox and five permission modes.
- **Tested.** 278 tests, 98% line coverage at the last measure, including the real app driven in a terminal.

## Benchmarks

Against other agents, on the same fake API so no model time is counted:

|                              | claude-jr | Claude Code | pi       |
|------------------------------|-----------|-------------|----------|
| install size                 | 3.2 MB    | 324.5 MB    | 109.0 MB |
| startup                      | 5 ms      | 167 ms      | ~260 ms  |
| memory, idle                 | 2.0 MB    | 135.0 MB    | 141.1 MB |
| memory, working              | 2.5 MB    | 270.2 MB    | 137.5 MB |
| its own CPU for 20 tool calls| 30 ms     | 1150 ms     | 580 ms   |

[SWE-bench Verified](https://www.swebench.com/), a fixed random sample, one attempt per task, official scoring:

| model             | tasks | resolved | cost  |
|-------------------|-------|----------|-------|
| claude-sonnet-5-5 | 100   | 88%      | $5.06 |
| claude-opus-5-5   | 50    | 90%      | $5.33 |

This is a first sample, not a leaderboard entry; the full 500-task run is in progress.
How everything is measured, and the rest of the numbers: [docs/BENCHMARKS.md](docs/BENCHMARKS.md).

## Use

```sh
claudejr                         # start in this folder
claudejr -c                      # continue the last conversation here
claudejr -p "explain main.rs"    # one prompt, print the answer, exit
claudejr --sandbox               # only allow writes inside this folder
claudejr -p "fix the tests" -y --output-format json --max-cost 2   # for scripts
```

Inside it, type `/` to see every command. Shift-tab changes the permission
mode, esc stops the current turn, and ctrl-v pastes text or a screenshot.

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

## Docs

- [Guide](docs/GUIDE.md): commands, keys, modes, tools, skills, MCP, the sandbox, memory, sessions, config.
- [Benchmarks](docs/BENCHMARKS.md): every number and how to reproduce it.
- [Architecture](ARCHITECTURE.md): how it is built.

Linux only for now. MIT licence.
