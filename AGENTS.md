# claude-jr

A minimal Rust terminal coding agent. Targets: idle <= 8 MB RAM, peak <= 16 MB,
binary <= 4 MB. No async runtime; threads plus mpsc channels.

## Layout
- `src/cli`: the command line, one file per thing you can type
- `src/ui`: the interactive terminal
- `src/agent`: the agent loop
- `src/tools`: tool implementations
- `src/api`: talking to models (the client, each wire format, providers)
- `src/work`: background tasks, worktrees, budgets, the run log
- `src/store`: sessions, images and config on disk
- `src/sys`: what differs per OS: files and processes, the shell, sandbox, memory trim, signals
- `tests/cli`, `tests/ui`: the real binary against the fake API and in a pseudo-terminal
- `bench/`: resource benchmarks (memory, CPU, idle, pty UI tests) and a fake API
- `bench/evals/`: coding evals (run.py; --dry-run uses the fake API in bench/mock.py)

## Verify
- `cargo test --release`
- `cargo clippy --release --all-targets -- -D warnings`
- `cargo fmt --check`

## Windows
The code builds for Windows too; anything OS-specific belongs in `src/sys`.
CI builds and tests it on a Windows machine (`bench/windows.sh`). The
end-to-end suites in `tests/` are Unix-only.

## Release build
`CC_x86_64_unknown_linux_musl=gcc ./build.sh x86_64-unknown-linux-musl`
(nightly, std rebuilt for size; plain `cargo build --release` works too, about 0.7 MB bigger)

## Packaging
- `Dockerfile`: the static binary on busybox; `bench/docker.sh` builds and checks it.
- `Formula/claude-jr.rb` is written by `scripts/formula.sh` at each release (the release workflow commits it); don't edit it by hand.
- The recording in the README and on the site is made by `scripts/demo.py` from a real session (it uses the API: ask first).

## Rules
- Keep dependencies minimal; no new crates without a reason.
- Conversation history is append-only (it keeps the prompt cache valid).
- Commits have no co-author trailers.
- Stage files by path.
- Files stay under about 500 lines of code; each opens with a comment saying
  what it is. The full map is in docs/FILES.md.
