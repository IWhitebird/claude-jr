# claude-jr

A minimal Rust terminal coding agent. Targets: idle <= 8 MB RAM, peak <= 16 MB,
binary <= 4 MB. No async runtime; threads plus mpsc channels.

## Layout
- `src/agent`: the agent loop
- `src/tools`: tool implementations
- `src/ui`: terminal UI and input handling
- `src/api.rs`: API client
- `bench/`: memory and pty benchmark scripts
- `evals/`: evaluation tasks (not present in every checkout)

## Verify
- `cargo test --release`
- `cargo clippy --release -- -D warnings`
- `cargo fmt --check`

## Release build
`CC_x86_64_unknown_linux_musl=gcc cargo build --release --target x86_64-unknown-linux-musl`

## Rules
- Keep dependencies minimal; no new crates without a reason.
- Conversation history is append-only (it keeps the prompt cache valid).
- Commits have no co-author trailers.
- Stage files by path.
