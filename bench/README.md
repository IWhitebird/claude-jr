# bench

Measurements and checks. None needs an API key unless it says so: they run
against the fake API in `mock.py`.

| Script | What it does | Run it |
|---|---|---|
| `mock.py` | The fake API: Anthropic's Messages format and Chat Completions, strict about what it accepts, able to fail on request | `python3 bench/mock.py 0` |
| `compare.py` | Size, start-up, memory and CPU against Claude Code, pi and Codex | `python3 bench/compare.py` |
| `density.py` | How much memory N agents working at once hold | `python3 bench/density.py target/release/claudejr 1000` |
| `mem.sh` | Peak memory over a long session | `bench/mem.sh` |
| `cpu.sh`, `idle.py` | CPU and memory, idle and busy | `bench/cpu.sh` |
| `ui.py` | The UI's CPU while a reply streams | `python3 bench/ui.py target/release/claudejr` |
| `box.sh` | A session in a small container | `CPUS=0.05 MEMORY=16m bench/box.sh` |
| `resize.py` | After any resize the screen matches a fresh run, in 13 situations (needs tmux) | `python3 bench/resize.py target/release/claudejr` |
| `chaos.py` | Random keys, pastes, resizes and API failures never crash it or send a bad request (needs tmux) | `python3 bench/chaos.py target/release/claudejr` |
| `sandbox.sh` | What the sandbox blocks | `bench/sandbox.sh` |
| `coverage.sh` | Line coverage of the tests | `bench/coverage.sh` |
| `provider.sh` | A real model of another provider answers, calls tools and keeps its history (uses that provider) | `bench/provider.sh ollama/qwen2.5:3b` |
| `mcp_client.py` | The MCP server, checked by the official MCP SDK's client (needs `pip install mcp`) | `python bench/mcp_client.py target/release/claudejr` |
| `evals/run.py` | Coding evals (uses the API unless `--dry-run`) | `python3 bench/evals/run.py frontend --dry-run` |
| `swe/run.py` | SWE-bench Verified in Docker (uses the API unless `--dry-run`) | `bench/swe/run.py --dry-run` |

Results are written under `bench/results/`.
