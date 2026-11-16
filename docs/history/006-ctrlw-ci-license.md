# Plan 006: Fix Ctrl-W on multi-byte whitespace; add CI, LICENSE, AGENTS.md

> **Executor instructions**: Follow step by step, verify each step, and STOP on any STOP condition.
> **Drift check (run first)**: `git diff --stat 890677c..HEAD -- src/ui/input.rs PLAN.md`. On drift, compare the excerpts and STOP on a mismatch.

## Status
- **Priority**: P2 · **Effort**: S · **Risk**: LOW · **Depends on**: none · **Category**: bug + dx
- **Planned at**: commit `890677c`, 2026-11-10

## Why this matters
claude-jr is a Rust terminal coding agent. Ctrl-W (delete word) panics when the
whitespace before the word is multi-byte (for example an ideographic space,
U+3000): `rfind` returns the start byte of that char, and `i + 1` lands inside
it. Release builds abort on panic, so the app dies. Separately, the repo is
public but has no CI, no LICENSE, and no agent guide. PLAN.md also states stale
facts (for example it says 37 tests; there are 42+).

## Current state
- `src/ui/input.rs:119-125`:
  ```rust
  pub fn delete_word(&mut self) {
      let before = &self.buf[..self.cur];
      let trimmed = before.trim_end();
      let start = trimmed.rfind(char::is_whitespace).map_or(0, |i| i + 1);
      self.buf.drain(start..self.cur);
      self.cur = start;
  }
  ```
  `buf: String`, and `cur` is a byte index. Check whether input.rs already has a `mod tests`.
- The build is a single Cargo crate (edition 2024; `rustfmt.toml` at the root). The tests need no network.
- Commit style: plain sentences, for example `Redraw properly when the terminal is resized`.

## Commands
| Purpose | Command | Expected |
|---|---|---|
| Tests | `cargo test --release` | all pass (42 before) |
| Lint | `cargo clippy --release -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |

## Scope
**In scope**: `src/ui/input.rs`, `.github/workflows/ci.yml` (create), `LICENSE` (create), `AGENTS.md` (create), `PLAN.md`.
**Out of scope**: all other source files (parallel plans), README.md.

## Git workflow
Branch `advisor/006-ctrlw-ci-license`. Two commits: `Fix: ctrl-w after a wide space crashed the app` and `CI, MIT license, AGENTS.md`.
**No Co-Authored-By or any other trailer.** Stage by path. Don't push.

## Steps
1. Fix `delete_word`: use `trimmed.char_indices().rev().find(|(_, c)| c.is_whitespace()).map_or(0, |(i, c)| i + c.len_utf8())`. Add tests: `"a\u{3000}bc"` with the cursor at the end leaves `"a\u{3000}"`, and `"foo bar  "` leaves `"foo "`.
   **Verify**: `cargo test --release` → all pass, including the new tests.
2. `.github/workflows/ci.yml`: on push and pull_request to main, a job on ubuntu-latest that runs `actions/checkout@v4`, installs stable Rust with clippy and rustfmt (`dtolnay/rust-toolchain@stable`), uses `Swatinem/rust-cache@v2`, then runs `cargo fmt --check`, `cargo clippy --release -- -D warnings` and `cargo test --release`.
   **Verify**: `python3 -c "import yaml,sys;yaml.safe_load(open('.github/workflows/ci.yml'))"` → exit 0 (skip if PyYAML is missing and say so).
3. `LICENSE`: the standard MIT text, `Copyright (c) 2026 IWhitebird`.
4. `AGENTS.md` (≤ 40 lines): what the project is (a minimal Rust terminal coding agent; targets: idle ≤ 8 MB RAM, peak ≤ 16 MB, binary ≤ 4 MB, no async runtime, threads plus mpsc); layout (`src/agent`, `src/tools`, `src/ui`, `src/api.rs`, `bench/`, `evals/`); the three verify commands above; the release build: `CC_x86_64_unknown_linux_musl=gcc cargo build --release --target x86_64-unknown-linux-musl`; rules: keep dependencies minimal, no new crates without reason, conversation history is append-only (prompt cache), commits have no co-author trailers, stage files by path.
5. `PLAN.md`: replace stale counts (grep for `37 tests`, or any test count) with the current number from `cargo test --release`. Change nothing else.

## Done criteria
- [ ] All three commands exit 0
- [ ] `ls LICENSE AGENTS.md .github/workflows/ci.yml` lists all three files
- [ ] `git diff --stat 890677c` lists only in-scope files

## STOP conditions
- `delete_word` doesn't match the excerpt.
- `cargo clippy --release -- -D warnings` fails on code **outside** input.rs. Don't fix that; report it, and drop `-D warnings` from CI with a note.
