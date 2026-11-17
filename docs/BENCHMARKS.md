# Benchmarks

How claude-jr is measured, and every number. The headline figures are in the [README](../README.md).

## Numbers

Measured with [`bench/compare.py`](../bench/compare.py): every agent runs against
the same fake API (no model time counted), with a fresh empty home folder.
Busy = one prompt that makes 20 bash calls with streamed text in between.

|                               | claude-jr | Claude Code | pi       |
|-------------------------------|-----------|-------------|----------|
| install size                  | 2.0 MB    | 324.5 MB    | 109.0 MB |
| startup (screen settles)      | 5 ms      | 167 ms      | ~260 ms  |
| memory, idle                  | 0.7 MB    | 135.0 MB    | 141.1 MB |
| processes / threads, idle     | 1 / 4     | 1 / 15      | 1 / 11   |
| CPU while idle 10s            | 0 ms      | 70 ms       | 0 ms     |
| busy: wall time               | 0.09 s    | 1.84 s      | 1.24 s   |
| busy: agent's own CPU         | 30 ms     | 1150 ms     | 580 ms   |
| busy: agent's own peak memory | 2.1 MB    | 270.2 MB    | 137.5 MB |
| busy: CPU incl. commands      | 116 ms    | 1285 ms     | 671 ms   |

Claude Code 2.1.319, pi 1.5.0, claude-jr static x86_64 build. Codex 0.167.0
was measured idle only (it speaks OpenAI's API): 150 MB, 37 threads, 322 ms
startup, with a 306 MB binary.

The release binary is built by [`build.sh`](../build.sh): optimised for size, with Rust's standard
library rebuilt the same way. On Linux, whenever claude-jr goes quiet it hands the pages of its own
program file back to the kernel ([`src/sys/mem.rs`](../src/sys/mem.rs)), which is why it idles in less
memory than its own size; they come back from the page cache as they're needed. The conversation
is kept in the session file rather than in memory: each request reads the saved messages straight
from it ([`src/store/session.rs`](../src/store/session.rs)), so a long session needs no more memory than a short one.

More of claude-jr's own numbers (`bench/`):

| | |
|---|---|
| 20, 60, 100 or 200 tool calls in one run (up to 8 MB of history) | 3.9 MB peak, the same for each |
| resuming a session with 1.3 MB of history and working on | 1.6 MB peak |
| 1,000 agents working at once, each in its own folder (`bench/density.py`) | 488 MB between them, 0.5 MB each |
| 40 background tasks started in one repository | 0.1 s to start them all; 20 MB between them |
| the same long session through another provider (Chat Completions) | the same 4.0 MB peak; 0.50 s of CPU against 0.42 s |
| the same long session through Vertex, or Bedrock with an API key | the same peak and the same CPU |
| the same through Bedrock with access keys (every request signed) | the same peak; 0.83 s of CPU, since signing hashes the body |
| the MCP server, waiting for calls | 1.2 MB |
| the UI while a reply streams at ~200 tokens/s | 0.6% of one core |
| a whole session (20 commands) in a container, agent plus the commands it runs | finishes in 6 MB of RAM (Docker's minimum) at 0.1 CPU, and at 0.01 CPU with 16 MB |
| the binary, compressed (xz) | 0.9 MB |

## SWE-bench Verified

All 500 tasks of [SWE-bench Verified](https://www.swebench.com/) with Opus, and a fixed random sample of
100 with Sonnet, scored with the official harness ([`bench/swe/run.py`](../bench/swe/run.py)):

| model | tasks | resolved | cost | median time per task |
|---|---|---|---|---|
| claude-opus-5-5 | all 500 | 467 (93.4%) | $67.32 (13 cents a task) | 34 s |
| claude-sonnet-5-5 | 100 | 88 (88%) | $5.06 (5 cents a task) | 14 s |

Of the 33 tasks Opus did not resolve, four (`psf__requests-1724`, `-1766`, `-1921`, `-2317`) fail on this
machine with the reference fix as well, so they could not have been scored as resolved here; they are
counted as failures all the same. Without them it is 467 of 496 (94.2%). One task ran into the 25-minute
limit; its patch passed and it counts as resolved, but its cost is missing from the total. The run was
made in two sittings with builds a few days apart, same prompt and tools. This is not a leaderboard entry.
The tasks are public and old enough that the models may have seen them. Each task ran once,
in the task's own container, with no retries and no hints.

## Handing work out: when it pays

The same main agent (Opus 5.5) was given ten independent pieces of work in one repository three ways:
do it all itself; give each piece to a subagent; give each to a claude-jr worker over MCP, as Claude Code
would. Subagents and workers ran on Haiku 5.5, each in its own worktree. Two sizes: ten one-line fixes,
and ten packages of six fixes each. One run of each ([`bench/workers/run.py`](../bench/workers/run.py)):

| | ten small fixes | | | ten packages (60 fixes) | | |
|---|---|---|---|---|---|---|
| | fixed | time | cost | fixed | time | cost |
| alone | 10/10 | 22 s | $0.12 | 60/60 | 39 s | $0.22 |
| with subagents | 10/10 | 42 s | $0.19 | 60/60 | 76 s | $0.32 |
| with workers | 10/10 | 39 s | $0.18 | 60/60 | 77 s | $0.34 |

On work of this size, doing it alone wins on both counts. Opus reads and edits many files in one reply, so
sixty small fixes take it a handful of requests; handing them out means writing ten briefs, waiting, and
merging ten branches, and that organising is what costs. The ten workers together cost 4 cents of the
$0.34 (0.4 cents each) and held 12.7 MB between them, the 22 processes of the run included.

So this does not show handing work out to be cheaper, and the numbers above should not be read that way.
What it does show is that the workers themselves are nearly free; whether delegation pays depends on the
pieces being long enough (many steps each, where one agent's context would grow with every step) to be
worth a brief and a merge. That case has not been measured here yet.

## Windows

The Windows build is 2.03 MB (`claudejr-windows-x86_64.exe`). CI builds it on a Windows machine, runs the
283 unit tests that apply there, and puts the binary through [`bench/windows.sh`](../bench/windows.sh)
against the fake API: a run with tools in Git Bash and in PowerShell, a session that continues, and
background tasks started, merged and cancelled. Its memory and CPU have not been measured.

## Measuring it yourself

| What | How |
|---|---|
| Against Claude Code, pi and Codex | `python3 bench/compare.py` |
| Inside the app | `/stats` |
| Idle and busy CPU, memory | `bench/cpu.sh` |
| The UI while a reply streams | `python3 bench/ui.py target/release/claudejr` |
| Resizing leaves nothing twice or missing, in 13 situations (needs tmux) | `python3 bench/resize.py target/release/claudejr` |
| Random keys, pastes and resizes never crash it or send a bad request | `python3 bench/chaos.py target/release/claudejr` |
| The MCP server, checked by the official MCP SDK's own client | `python bench/mcp_client.py target/release/claudejr` (needs `pip install mcp`) |
| A real model of another provider answers, calls tools and keeps its history | `bench/provider.sh ollama/qwen2.5:3b` (or any `PROVIDER/MODEL`, with its key set) |
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

