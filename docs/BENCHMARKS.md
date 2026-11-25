# Benchmarks

Every number claude-jr publishes, and how it was measured.

## Numbers

Measured with [`bench/compare.py`](../bench/compare.py): every agent runs against
the same fake API (no model time counted), with a fresh empty home folder.
Busy = one prompt that makes 20 bash calls with streamed text in between.

|                               | claude-jr | Claude Code | times less |
|-------------------------------|-----------|-------------|------------|
| install size                  | 2.0 MB    | 354.2 MB    | 177        |
| startup (screen settles)      | 4 ms      | 157 ms      | 40         |
| memory, idle                  | 0.7 MB    | 147.7 MB    | 200        |
| processes / threads, idle     | 1 / 4     | 1 / 16      |            |
| CPU while idle 5s             | 0 ms      | 50 ms       |            |
| busy: wall time               | 0.10 s    | 1.57 s      | 16         |
| busy: agent's own CPU         | 40 ms     | 1140 ms     | 28         |
| busy: agent's own peak memory | 2.2 MB    | 288.9 MB    | 130        |
| busy: CPU incl. commands      | 119 ms    | 1268 ms     | 11         |
| the same with 100 calls: own CPU | 250 ms | 3550 ms     | 14         |
| the same with 100 calls: peak memory | 2.2 MB | 335.0 MB | 150       |
| sent with every request: instructions | 1.5 KB | 6.1 KB  |            |
| sent with every request: tool definitions | 10 tools, 5.1 KB | 12 tools, 29.7 KB | |
| sent with every request: both | 6.6 KB (about 1.7k tokens) | 35.8 KB (about 9k tokens) | 5 |

"Sent with every request" is what each puts in front of the conversation in print mode (`-p`), read from
what the fake API received; the token figures are bytes divided by four, an estimate. It is paid for on
every request, at the cached price after the first. Interactive sessions offer more tools in both.

Claude Code 2.1.326 and claude-jr's static x86_64 build (2,030,368 bytes), 18 November 2026, on a Linux
laptop. Claude Code is a much larger product (plugins, hooks, IDE integration, an auto-updater and more);
the table shows what that costs in resources, not which is better.

An earlier run (claude-jr 0.2.0, Claude Code 2.1.319) also had pi 1.5.0: 109 MB installed, 141 MB idle,
~260 ms to start, 580 ms of CPU and a 138 MB peak for the 20 calls; and Codex 0.167.0, idle only (it
speaks OpenAI's API): 150 MB, 37 threads, 322 ms to start.

### Installed size next to other terminal agents

What each one's installer leaves on disk on Linux x86-64, 18 November 2026. For the npm packages it is the
`node_modules` folder that `npm install` fills, without Node itself. Only size was measured for these.

| agent | version | installed |
|---|---|---|
| claude-jr | this build | 2.0 MB |
| Gemini CLI (`@google/gemini-cli`) | 0.68.0 | 103 MB |
| pi (`@mariozechner/pi-coding-agent`) | 0.73.1 | 150 MB |
| Claude Code | 2.1.326 | 354 MB |
| Codex CLI (`@openai/codex`) | 0.169.0 | 423 MB |
| opencode (`opencode-ai`) | 1.19.6 | 432 MB |

<!-- repo only -->
The release binary is built by [`build.sh`](../build.sh): optimised for size, with Rust's standard
library rebuilt the same way. On Linux, whenever claude-jr goes quiet it hands the pages of its own
program file back to the kernel, which is why it idles in less
memory than its own size; they come back from the page cache as they're needed. The conversation
is kept in the session file rather than in memory: each request reads the saved messages straight
from it, so a long session needs no more memory than a short one.
<!-- /repo only -->

More of claude-jr's own numbers:

| | |
|---|---|
| 20, 60, 100 or 200 tool calls in one run (up to 8 MB of history) | 3.9 MB peak, the same for each |
| resuming a session with 1.3 MB of history and working on | 1.6 MB peak |
| 1,000 agents working at once, each in its own folder | 492 MB between them, 0.5 MB each |
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

| model | resolved | cost | a task |
|---|---|---|---|
| Opus 5.5 | 467 of 500 (93.4%) | $67.32 | 13 cents, 34 s |
| Sonnet 5.5 | 88 of 100 (88%) | $5.06 | 5 cents, 14 s |

(Time is the median.)

Of the 33 tasks Opus did not resolve, four (`psf__requests-1724`, `-1766`, `-1921`, `-2317`) fail on this
machine with the reference fix as well, so they could not have been scored as resolved here; they are
counted as failures all the same. Without them it is 467 of 496 (94.2%). One task ran into the 25-minute
limit; its patch passed and it counts as resolved, but its cost is missing from the total. The run was
made in two sittings with builds a few days apart, same prompt and tools. This is not a leaderboard entry.
The tasks are public and old enough that the models may have seen them. Each task ran once,
in the task's own container, with no retries and no hints.

No agent resolves all 500. For scale, the best score on [Vals.ai's independent leaderboard](https://www.vals.ai/benchmarks/swebench)
was 97.0% when it was last updated (1 September 2026), with a different agent and other models. All 33
of our misses produced a patch and finished normally; apart from the four above, they are fixes that
did not satisfy the hidden tests.

## Handing work out: when it pays

The same main agent (Opus 5.5) was given ten independent pieces of work three ways: do it all itself,
give each piece to a subagent, or give each to a claude-jr worker over MCP, as Claude Code would.
Subagents and workers ran on Haiku 5.5, each in its own worktree. Every arrangement fixed everything.
One run of each ([`bench/workers/run.py`](../bench/workers/run.py)):

| Ten small fixes | time | cost |
|---|---|---|
| alone | 22 s | $0.12 |
| with subagents | 42 s | $0.19 |
| with workers | 39 s | $0.18 |

| Sixty fixes, in ten packages | time | cost |
|---|---|---|
| alone | 39 s | $0.22 |
| with subagents | 76 s | $0.32 |
| with workers | 77 s | $0.34 |

On jobs this small, one agent doing everything is faster and cheaper. Opus edits many files in one
reply, while handing the work out means writing ten briefs, waiting, and merging ten branches. The
workers themselves cost 0.4 cents each and held 12.7 MB between them. Whether it pays on long tasks,
where one agent's context would keep growing, has not been measured yet.

## Windows

New in 0.3.0. The Windows build is 2.05 MB (`claudejr-windows-x86_64.exe`). Every change is built and
tested on a Windows machine ([`bench/windows.sh`](../bench/windows.sh)): a run with tools in Git Bash and
in PowerShell, a session that continues, background tasks started, merged and cancelled, the installer
(which must refuse a download that fails its checksum) and `--update` replacing the program while it runs.

On that machine a run of 20 commands peaked at 8.2 MB, and the MCP server waiting for calls held 5.8 MB
(1.2 MB of it its own; the rest is Windows' shared libraries, which every program is charged for). Those
are working-set figures, Windows' counterpart of the Linux numbers above; there is no idle trim on
Windows, and the interactive UI has not been measured there.

## Measuring it yourself

| What | How |
|---|---|
| Against Claude Code, pi and Codex | `python3 bench/compare.py` |
| Inside the app | `/stats` |
| Idle and busy CPU, memory | `bench/cpu.sh` |
| The UI while a reply streams | `python3 bench/ui.py target/release/claudejr` |
| SWE-bench Verified, one task at a time in Docker | `bench/swe/run.py --dry-run` (setup at the top of the file) |
| Memory budget over a long session | `bench/mem.sh` |
| In a small container | `CPUS=0.05 MEMORY=16m bench/box.sh` |
| A running session | `ps -o rss,pcpu,nlwp -p $(pgrep -x claude-jr)` |
| One prompt | `/usr/bin/time -v claudejr -p "..."` |

None of the scripts need an API key; they run against a stand-in for the API.

<!-- repo only -->
Checks, for people working on the code:

| What | How |
|---|---|
| Resizing leaves nothing twice or missing, in 13 situations (needs tmux) | `python3 bench/resize.py target/release/claudejr` |
| Random keys, pastes and resizes never crash it or send a bad request | `python3 bench/chaos.py target/release/claudejr` |
| The MCP server, checked by the official MCP SDK's own client | `python bench/mcp_client.py target/release/claudejr` (needs `pip install mcp`) |
| A real model of another provider answers, calls tools and keeps its history | `bench/provider.sh ollama/qwen2.5:3b` (or any `PROVIDER/MODEL`, with its key set) |
| The website's pages at six widths | `python3 bench/site.py` |
| Windows, on a Windows machine | `bash bench/windows.sh claudejr.exe` |
| Line coverage of the tests (97%) | `bench/coverage.sh` |
<!-- /repo only -->

<!-- repo only -->
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
<!-- /repo only -->
