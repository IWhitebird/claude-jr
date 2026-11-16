# Plan 003: Show the whole command in approval prompts

> **Executor instructions**: Follow step by step, verify each step, and STOP on any STOP condition.
> **Drift check (run first)**: `git diff --stat 890677c..HEAD -- src/tools/mod.rs src/agent/turn.rs src/ui/view.rs`. On drift, compare the excerpts and STOP on a mismatch.

## Status
- **Priority**: P1 · **Effort**: S · **Risk**: LOW · **Depends on**: none · **Category**: security
- **Planned at**: commit `890677c`, 2026-11-10

## Why this matters
claude-jr is a Rust terminal coding agent. The permission prompt shows
`Allow Bash(<command>)?`, but the command is cut to its **first line and 80
characters**. A command like `ls\nrm -rf ~/x` is shown as `Bash(ls)`, so the
user approves something they never saw. After this plan, the prompt shows
every line of the command (wrapped, capped at a sane length). The short
one-line label stays for the transcript row.

## Current state
- `src/tools/mod.rs:161-172`:
  ```rust
  /// Short label for the UI, like `Edit(src/main.rs)`.
  pub fn label(&self, name: &str, input: &Value) -> String {
      let subject = self.subject(name, input);
      let arg = shorten(subject.lines().next().unwrap_or(""), 80);
      let head = match name { "ask_user" => return "Question".into(), "todo" => return "Todos".into(),
          n if mcp::is_mcp(n) => mcp::display_name(n), n => capitalize(n) };
      let bg = if name == "bash" && input["background"] == true { " &" } else { "" };
      format!("{head}({arg}){bg}")
  }
  ```
- `src/agent/turn.rs`: `call_one` computes `let label = ctx.tools.label(&c.name, &c.input);`, uses it for `Event::Tool { label, .. }`, and passes `&label` to `self.check(ctx, c, &label)`. `call_group` does the same with a `labels` Vec. `check` passes the label on to `permit`, which puts it in the approval prompt (`Event::Ask { label, .. }`). permit.rs is **out of scope**; changing what turn.rs passes to `check` is enough.
- `src/ui/view.rs:107-110`:
  ```rust
  pub fn approve(label: &str, cols: usize, out: &mut Vec<String>) {
      let question = format!("Allow {}?", clean(label));
      boxed(&[(&question, BOLD), ("y yes · a always (this session) · n no", DIM)], cols, out);
  }
  ```
  `boxed` (view.rs:145) wraps each row with `wrap()`, but it may not handle `\n` inside a row. Check this. Also check what `clean()` (src/ui/text.rs) does with newlines.

## Commands
| Purpose | Command | Expected |
|---|---|---|
| Tests | `cargo test --release` | all pass (42 before) |
| Lint | `cargo clippy --release -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |

## Scope
**In scope**: `src/tools/mod.rs`, `src/agent/turn.rs`, `src/ui/view.rs`.
**Out of scope**: `src/agent/permit.rs`, `src/ui/mod.rs` and `src/tools/bash.rs`, which other plans are editing in parallel.

## Git workflow
Branch `advisor/003-full-approval-label`. Commit message like `Approval prompts show the whole command`.
**No Co-Authored-By or any other trailer.** Stage by path. Don't push.

## Steps

### Step 1: `Tools::full_label`
In `src/tools/mod.rs`, add `pub fn full_label(&self, name, input) -> String`: the same as `label`, but with the whole subject (all lines), capped at 2000 characters with `…` appended when cut. Share code with `label` rather than duplicating the `head` match.

### Step 2: Pass it to `check`
In `turn.rs` `call_one` and `call_group`, keep the short label for `Event::Tool`, and pass `ctx.tools.full_label(..)` to `check`.

### Step 3: Multi-line approval box
In `view::approve`, put each line of the label on its own row of the box, after the first row `Allow <head>(`… Keep it simple: the first line is `Allow {first line}`, and later lines are their own rows. Show at most 12 lines; if there are more, add a row `… N more lines`. Then end with `?` and the hint row. Each row must still be `clean()`ed. The box must never be wider than `cols`.

### Step 4: Tests
- `tools/mod.rs` tests: for bash with command `"ls\nrm -rf /tmp/x"`, `label` has no `rm` and `full_label` contains `rm -rf /tmp/x`. Model it after existing tests that use `test_tools()`.
- `view.rs` tests (add a `mod tests` if absent): `approve("Bash(ls\nrm -rf x)", 40, &mut out)` gives output containing `rm -rf x`, and every line's visible width is ≤ 40 (use the `strip` helper in view.rs plus `text::width`). A 30-line label produces a `more lines` row.

**Verify**: `cargo test --release` → all pass, including the new tests.

## Done criteria
- [ ] `cargo test --release`, `cargo clippy --release -- -D warnings` and `cargo fmt --check` exit 0
- [ ] `grep -n full_label src/agent/turn.rs` shows the 2 call sites
- [ ] `git diff --stat 890677c` lists only the in-scope files

## STOP conditions
- The label reaches the approval prompt by some path other than turn.rs → check → permit.
- The fix needs edits to permit.rs or ui/mod.rs.

## Maintenance notes
Subagent approvals prefix the label with `task · ` (src/agent/sub.rs), and that
still works. Auto-mode "flagged" reasons are appended to the label in permit.rs.
