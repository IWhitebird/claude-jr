# Plan 004: Don't panic when a job's output is cut mid-character

> **Executor instructions**: Follow step by step, verify each step, and STOP on any STOP condition.
> **Drift check (run first)**: `git diff --stat 890677c..HEAD -- src/tools/bash.rs`. On drift, compare the excerpts and STOP on a mismatch.

## Status
- **Priority**: P1 · **Effort**: S · **Risk**: LOW · **Depends on**: none · **Category**: bug
- **Planned at**: commit `890677c`, 2026-11-10

## Why this matters
claude-jr is a Rust terminal coding agent with background shell jobs. When the
model checks a job, its new output is cut to the last `MAX_BYTES` with a byte
slice. If that cut lands inside a multi-byte UTF-8 character (any non-ASCII
output: emoji, CJK, box drawing), the slice panics. Release builds use
`panic = "abort"`, so the whole app dies and the terminal is left in raw mode.

## Current state
- `src/tools/bash.rs:372-383`, `fn report(&self) -> String` on a job:
  ```rust
  let text = if text.len() > MAX_BYTES { &text[text.len() - MAX_BYTES..] } else { &text };
  s.push_str(if text.is_empty() { "(no new output)" } else { text });
  ```
  `text` is a `String` from `Tail::since`.
- Search the file for other `[..` byte slices on strings (`grep -n '\[.*\.\.\]' src/tools/bash.rs`) and check each one for the same problem, for example in foreground output capping (`Capped`).

## Commands
| Purpose | Command | Expected |
|---|---|---|
| Tests | `cargo test --release` | all pass (42 before) |
| Lint | `cargo clippy --release -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |

## Scope
**In scope**: `src/tools/bash.rs` only.

## Git workflow
Branch `advisor/004-jobs-char-boundary`. Commit message like `Fix: job output cut inside a character crashed the app`.
**No Co-Authored-By or any other trailer.** Stage by path. Don't push.

## Steps
1. Add a small helper `fn tail_str(s: &str, max: usize) -> &str` that returns the last ≤ `max` bytes, starting at a char boundary: move the start forward until `s.is_char_boundary(start)`. Use it in `report`.
2. Fix any other unsafe string slice found by the grep the same way. List them in NOTES.
3. Tests in bash.rs `mod tests`: `tail_str("ééé", 3)` is `"é"` (each é is 2 bytes, so the start moves from byte 3 to byte 4). `tail_str("abc", 10)` is `"abc"`. Also a string of 3 × MAX_BYTES of `"é"` doesn't panic and has length ≤ MAX_BYTES.

**Verify**: `cargo test --release` → all pass, including the new tests.

## Done criteria
- [ ] All three commands exit 0
- [ ] `grep -n 'text.len() - MAX_BYTES' src/tools/bash.rs` returns nothing
- [ ] Only `src/tools/bash.rs` changed

## STOP conditions
- `report` doesn't match the excerpt.

## Maintenance notes
`catch_unwind` doesn't help in release builds (`panic = "abort"` in Cargo.toml), so string slicing must be made safe at each site.
