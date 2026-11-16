# Plan 007: Resizing must not leave stale copies in the scroll history

> **Executor instructions**: Follow step by step, verify each step, and STOP on any STOP condition.
> **Drift check (run first)**: `git diff --stat 890677c..HEAD -- src/ui/mod.rs src/ui/term.rs src/ui/history.rs`. On drift, compare the excerpts and STOP on a mismatch.

## Status
- **Priority**: P1 · **Effort**: S · **Risk**: MED · **Depends on**: none · **Category**: bug
- **Planned at**: commit `890677c`, 2026-11-10

## Why this matters
claude-jr is a Rust terminal coding agent that draws inline: finished output is
written into the terminal's own scrollback, and a small "live" area (status,
input box) is redrawn at the bottom. When the terminal is resized (zooming
the font is a burst of resizes), the app clears the **visible** screen and
draws the recent history again at the new width. The old copy has already
been pushed into the terminal's scrollback, so every resize leaves one more
stale copy there: the user scrolls up and sees the header two or three times.
After this plan, a resize wipes the scrollback as well and redraws everything
that is kept, so there is exactly one copy.

## Current state
- `src/ui/mod.rs:130-139` (main loop):
  ```rust
  if std::mem::take(&mut app.clear_screen) {
      term.clear(true)?;
      app.history.clear();
  } else if std::mem::take(&mut app.resized) {
      // The terminal rewrapped what's on screen at its old width; start
      // the screen over and draw recent history at the new one.
      term.clear(false)?;
      app.redraw_history();
  }
  let (live, cursor) = app.live();
  term.draw(&mut app.commits, live, cursor)?;
  ```
- `src/ui/term.rs:79`: `pub fn clear(&mut self, scrollback: bool)` writes `\x1b[2J\x1b[3J\x1b[H` when `scrollback` is true, and `\x1b[2J\x1b[H` otherwise.
- `src/ui/mod.rs:366-372`:
  ```rust
  fn redraw_history(&mut self) {
      let mut lines = self.history.render(self.cols);
      let room = self.rows.saturating_sub(6);
      let skip = lines.len().saturating_sub(room);
      self.commits = lines.split_off(skip);
      self.last_blank = self.commits.last().is_none_or(|l| text::is_blank(l));
  }
  ```
  It keeps only what fits on one screen.
- `src/ui/mod.rs:322`: `self.history.push(block, lines, self.rows * 2);`. History keeps about two screens of blocks (`src/ui/history.rs`, `History::push(block, lines, keep)`).
- `term.draw(&mut commits, live, cursor)` writes the committed lines above the live area. In normal use it already receives more lines than fit on a screen (long tool output), and they scroll.

## Commands
| Purpose | Command | Expected |
|---|---|---|
| Tests | `cargo test --release` | all pass (42 before) |
| Lint | `cargo clippy --release -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |
| pty UI test | `ls bench/ui.py` then read its header for how to run it | see step 4 |

## Scope
**In scope**: `src/ui/mod.rs` (only the resize branch of the loop, `redraw_history`, and the `history.push` keep value), `src/ui/term.rs`, `src/ui/history.rs` (tests only, if needed).
**Out of scope**: everything else. In `src/ui/mod.rs`, do not touch `handle`, `on_key` or `on_approve_key`; another branch changes them.

## Git workflow
Branch `advisor/007-resize-duplicates`. Commit message like `Fix: resizing left old copies of the screen in the scroll history`.
**No Co-Authored-By or any other trailer.** Stage by path. Don't push.

## Steps
1. In the loop's resize branch, call `term.clear(true)` instead of `term.clear(false)`, and update the comment: the scrollback is wiped because the terminal keeps the old-width copy there.
2. `redraw_history`: commit **all** kept history lines, not just one screen's worth (drop `room` and `skip`). Keep the `last_blank` line.
3. Keep more history so scrolling up after a resize still shows the conversation: add `const HISTORY_LINES: usize = 1000;` and pass `HISTORY_LINES.max(self.rows * 2)` as `keep` in `history.push`. Update the comment at the top of `src/ui/history.rs` if it says "a couple of screens".
4. Verify in a pty. Read `bench/ui.py` to see how it drives the binary against the fake API (`bench/mock.py`). If it has a resize case, run it. If not, write a throwaway script in your scratch space (not committed) that: starts the release binary in a pty of 100x30, resizes it three times (to 90x25, 80x20, 100x30) with `TIOCSWINSZ` plus SIGWINCH, captures all bytes written after the first resize, and asserts that each resize is followed by `\x1b[3J`, and that after the last resize the text `/help for commands` appears exactly once after the final `\x1b[3J`. Report the result. If the pty test can't be made to work in 15 minutes, say so plainly and skip it.
5. Add a unit test if `Term` can be built over an in-memory writer (check how term.rs tests do it); otherwise skip it and say so.

**Verify**: all three commands exit 0.

## Done criteria
- [ ] All three commands exit 0
- [ ] `grep -n "term.clear(false)" src/ui/mod.rs` returns nothing
- [ ] `git diff --stat 890677c` lists only in-scope files

## STOP conditions
- `term.draw` turns out not to handle more committed lines than fit on screen in the bottom layout (the header ends up missing or the input box misplaced in the pty test).
- The fix needs changes to `handle`, `on_key` or `on_approve_key`.

## Maintenance notes
After a resize, anything older than the kept ~1000 lines is gone from the
scroll history. That is the trade for having no duplicates; Claude Code makes
the same trade.
