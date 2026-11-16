# Plan 002: Ignore typed-ahead keys on approval prompts

> **Executor instructions**: Follow step by step, verify each step, and STOP on any STOP condition.
> **Drift check (run first)**: `git diff --stat 890677c..HEAD -- src/ui/mod.rs`. On drift, compare the excerpts with the live code and STOP on a mismatch.

## Status
- **Priority**: P1 · **Effort**: S · **Risk**: LOW · **Depends on**: none · **Category**: security
- **Planned at**: commit `890677c`, 2026-11-10

## Why this matters
claude-jr is a Rust terminal coding agent. While it works, the user can keep
typing their next message. If a permission prompt ("Allow Bash(rm ...)?")
appears mid-typing, the next keystroke answers it: `a` means "always allow",
and `y`, `1` and Enter mean yes. So typing "ok and then..." silently approves
a command. After this plan, keys are ignored for a short moment after an
approval prompt becomes visible.

## Current state
- `src/ui/mod.rs:183`: `Prompt::Approve { label, reply }` is a variant of the `Prompt` enum. `App.prompts` is a `VecDeque<Prompt>`, and only the front one is shown and answered.
- `src/ui/mod.rs:578`: `Event::Ask { label, reply } => self.prompts.push_back(Prompt::Approve { label, reply }),`
- `src/ui/mod.rs:627`: `on_key` routes to `on_approve_key` when the front prompt is `Approve`.
- `src/ui/mod.rs:753-768`:
  ```rust
  fn on_approve_key(&mut self, k: KeyEvent) {
      let answer = match k.code {
          KeyCode::Char('y' | 'Y' | '1') | KeyCode::Enter => Answer::Yes,
          KeyCode::Char('a' | 'A' | '2') => Answer::Always,
          KeyCode::Char('n' | 'N' | '3') | KeyCode::Esc => Answer::No,
          KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => { self.cancel(); Answer::No }
          KeyCode::BackTab => { ... return mode.set(mode.get().next()); }
          _ => return,
      };
      if let Some(Prompt::Approve { reply, .. }) = self.prompts.pop_front() { let _ = reply.send(answer); }
  }
  ```
- Prompts are popped with `self.prompts.pop_front()` in several places (approve, answer, pickers).

## Commands
| Purpose | Command | Expected |
|---|---|---|
| Tests | `cargo test --release` | all pass (42 before) |
| Lint | `cargo clippy --release -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |

## Scope
**In scope**: `src/ui/mod.rs` only.
**Out of scope**: `src/ui/view.rs`, `src/agent/*` and `src/tools/*` (other plans are editing them in parallel).

## Git workflow
Branch `advisor/002-approval-typeahead`. Commit message like `Approval prompts ignore keys typed just before they appear`.
**No Co-Authored-By or any other trailer.** Stage by path. Don't push.

## Steps

### Step 1: Track when the front prompt became visible
Add a field `prompt_since: std::time::Instant` to `App`, initialized with `Instant::now()`.
Set it to `Instant::now()` whenever the front of `self.prompts` changes:
- when `push_back` happens on an empty queue, and
- after **every** `pop_front` that leaves a prompt at the front.

The simplest way is to make every such site update it. A small helper is fine,
for example `fn prompts_changed(&mut self)`.

### Step 2: Guard approval keys
Add `const APPROVE_GUARD: Duration = Duration::from_millis(400);` and a pure function:
```rust
/// Keys pressed this soon after an approval appears were typed for something else.
fn armed(since: Instant, now: Instant) -> bool { now.duration_since(since) >= APPROVE_GUARD }
```
In `on_approve_key`, if `!armed(self.prompt_since, Instant::now())`, return early
for every key **except** Esc, Ctrl-C and BackTab, which always work.
Also stop Enter from meaning yes; Enter does nothing on this prompt. y/1, a/2
and n/3/Esc still work once armed.

### Step 3: Tests
Add `#[cfg(test)] mod tests` (or extend an existing one) with a test of `armed`:
- false at +0 ms and at +399 ms;
- true at +400 ms and at +2 s.

**Verify**: `cargo test --release` → all pass, including the new test.

## Done criteria
- [ ] `cargo test --release`, `cargo clippy --release -- -D warnings` and `cargo fmt --check` exit 0
- [ ] `grep -n "KeyCode::Enter => Answer::Yes" src/ui/mod.rs` returns nothing
- [ ] `git diff --stat 890677c` shows only `src/ui/mod.rs`

## STOP conditions
- `on_approve_key` or `Prompt::Approve` doesn't match the excerpts.
- The fix needs changes outside `src/ui/mod.rs`.

## Maintenance notes
The hint text "y yes · a always · n no" lives in `src/ui/view.rs::approve` and
doesn't mention Enter, so it needs no change. If a selectable-list approval UI
is added later, keep the guard.
