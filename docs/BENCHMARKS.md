# Benchmarks

How claude-jr is measured, and every number. The headline figures are in the [README](../README.md).

## Numbers

Measured with [`bench/compare.py`](../bench/compare.py): every agent runs against
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
with the official harness ([`bench/swe/run.py`](../bench/swe/run.py), results in `bench/swe/results/`):

| model | tasks | resolved | cost | median time per task |
|---|---|---|---|---|
| claude-sonnet-5-5 | 100 | 88 (88%) | $5.06 | 14 s |
| claude-opus-5-5 | 50 (the first 50 of the same list) | 45 (90%) | $5.33 | 27 s |

On those same 50 tasks Sonnet resolved 44. With samples this small the true rates could be anywhere
from about 80% to 95%, so the two models can't be told apart here, and this is not a leaderboard
entry. The tasks are public and old enough that the models may have seen them. Each task ran once,
in the task's own container, with no retries and no hints.

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

