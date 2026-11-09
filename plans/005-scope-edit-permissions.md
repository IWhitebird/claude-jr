# Plan 005: Keep accept-edits and "always" inside the project

> **Executor instructions**: Follow step by step, verify each step, and STOP on any STOP condition.
> **Drift check (run first)**: `git diff --stat 890677c..HEAD -- src/agent/permit.rs src/permission.rs`. On drift, compare the excerpts and STOP on a mismatch.

## Status
- **Priority**: P1 · **Effort**: S · **Risk**: MED · **Depends on**: none · **Category**: security
- **Planned at**: commit `890677c`, 2026-11-10

## Why this matters
claude-jr is a Rust terminal coding agent with permission modes. In
"accept edits" mode, every write and edit is allowed, **including files
outside the project** such as `~/.bashrc` or `~/.ssh/authorized_keys`. Auto mode
already limits itself to the project. Likewise, answering "always" to one edit
adds a rule allowing that tool for every path. After this plan, both only
cover files inside the project. Edits elsewhere still ask, and "always" for an
outside file allows only that exact file.

## Current state
- `src/agent/permit.rs:86-98` (`Core::permit`):
  ```rust
  let subject = ctx.tools.subject(name, input);
  let inside_project = !subject.starts_with('/') && !subject.starts_with("..");
  match (mode, access) {
      (_, Access::Free) | (Mode::Bypass, _) | (Mode::AcceptEdits, Access::Edit) => return Ok(()),
      (Mode::Auto, Access::Edit) if inside_project => return Ok(()),
      (Mode::Plan, Access::Edit) => return Err(...),
      _ => {}
  }
  if self.rules.lock().unwrap().allows(name, &subject) { return Ok(()); }
  ```
  For file tools, `subject` is the path relative to the project root when it's inside the project, and an absolute path otherwise.
  `Access::Edit` covers the write and edit tools; `Access` lives in `src/tools/mod.rs`, which is out of scope.
- `src/agent/permit.rs:127`: on "always", `self.rules.lock().unwrap().add(Rules::always(name, &subject));`
- `src/permission.rs`: `Rules::allows(tool, subject)`. A bare rule `"edit"` (empty pattern) matches every subject. `Rules::always(tool, subject)` returns `bash:<subject>` for bash, and the bare tool name otherwise. Tests are at the bottom (`fn rules()`). Extend them.

## Commands
| Purpose | Command | Expected |
|---|---|---|
| Tests | `cargo test --release` | all pass (42 before) |
| Lint | `cargo clippy --release -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |

## Scope
**In scope**: `src/agent/permit.rs`, `src/permission.rs`.
**Out of scope**: `src/agent/turn.rs`, `src/tools/*` and `src/ui/*` (other plans are editing them in parallel).

## Git workflow
Branch `advisor/005-scope-edit-permissions`. Commit message like `Accept-edits and "always" cover only files inside the project`.
**No Co-Authored-By or any other trailer.** Stage by path. Don't push.

## Steps
1. In `permission.rs`, add `pub fn outside_project(subject: &str) -> bool` = `subject.starts_with('/') || subject.starts_with("..")`, and use it in permit.rs instead of the inline check.
2. `Rules::allows`: for tools `write` and `edit`, a bare rule (empty pattern) matches only when `!outside_project(subject)`. Pattern rules keep working as before. Update the header comment of permission.rs to say so.
3. `Rules::always`: for `write`/`edit` with an outside subject, return `"{tool}:{subject}"`, which the existing glob treats as an exact match unless the path has `*`. Otherwise keep today's behavior.
4. `permit`: change the first arm so that `(Mode::AcceptEdits, Access::Edit)` returns Ok only `if inside_project`, the same as Auto. Outside edits fall through to rules and then to asking.
5. Tests in `permission.rs`:
   - `Rules::new(vec!["edit".into()])` allows `("edit","src/a.rs")` but not `("edit","/home/u/.bashrc")`.
   - `always("edit","/etc/hosts")` is `"edit:/etc/hosts"`, and that rule allows `/etc/hosts` but not `/etc/passwd`.
   - `always("edit","src/a.rs")` is still `"edit"`.

**Verify**: `cargo test --release` → all pass, including the new assertions.

## Done criteria
- [ ] All three commands exit 0
- [ ] `grep -n "(Mode::AcceptEdits, Access::Edit) =>" src/agent/permit.rs` returns nothing (the arm now has an `if` guard)
- [ ] Only the two in-scope files changed

## STOP conditions
- `subject()` for file tools turns out to return something other than relative-inside / absolute-outside paths.
- Any existing test needs its expectation changed beyond the new rules above. Report which one.

## Maintenance notes
This is the base for a follow-up (not in this plan): Auto mode should also ask
before edits to `.git/hooks`, `.mcp.json` and `.claude-jr.json`, and should
resolve symlinks before deciding a path is inside the project.
