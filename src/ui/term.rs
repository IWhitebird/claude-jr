// Inline terminal drawing. Finished lines are written once into the normal
// scrollback. Only the "live" area (streaming text, status, input box) is
// redrawn, inside synchronized output so it doesn't flicker.
//
// Two layouts. Inline: the live area follows the last line written. Bottom:
// the live area sits on the last rows of the screen and finished lines scroll
// up above it, like a full-screen app, while history stays in the terminal's
// own scrollback (so scrolling, selecting and copying work as usual).

use crossterm::event::{KeyboardEnhancementFlags, PushKeyboardEnhancementFlags};
use crossterm::terminal;
use std::fmt::Write as _;
use std::io::{self, Write};

/// The live area's lines and cursor position.
type Frame = (Vec<String>, Option<(usize, usize)>);

/// For the bottom layout: the screen height, the row where the next finished
/// line goes (content flows down from the top), and the row where the live
/// area starts (it ends on the last row).
#[derive(Clone, Copy)]
struct Anchor {
    rows: usize,
    next: usize,
    top: usize,
}

/// Room for an ordinary frame.
const FRAME_BYTES: usize = 16 * 1024;

pub struct Term {
    out: io::Stdout,
    buf: String,
    /// Cursor row inside the live area, counted from its top.
    row: usize,
    /// Set for the bottom layout.
    anchor: Option<Anchor>,
    enhanced_keys: bool,
    /// The live area as last drawn, to skip frames that wouldn't change anything.
    last: Option<Frame>,
    /// A wipe asked for by `clear`, done at the start of the next frame;
    /// true wipes the terminal's history too.
    wipe: Option<bool>,
}

impl Term {
    /// `bottom` keeps the input on the last rows of the screen. It needs the
    /// cursor's position, so it falls back to inline if the terminal doesn't
    /// answer.
    pub fn new(bottom: bool) -> io::Result<Term> {
        terminal::enable_raw_mode()?;
        let mut out = io::stdout();
        let anchor = if bottom {
            crossterm::cursor::position().ok().map(|(_, row)| {
                let row = row as usize;
                Anchor { rows: Term::size().1, next: row, top: row }
            })
        } else {
            None
        };
        // Lets Shift+Enter be told apart from Enter, where the terminal supports it.
        let enhanced_keys = terminal::supports_keyboard_enhancement().unwrap_or(false);
        if enhanced_keys {
            crossterm::execute!(
                out,
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            )?;
        }
        out.write_all(b"\x1b[?2004h")?; // bracketed paste
        out.flush()?;
        Ok(Term { out, buf: String::with_capacity(FRAME_BYTES), row: 0, anchor, enhanced_keys, last: None, wipe: None })
    }

    pub fn size() -> (usize, usize) {
        terminal::size().map_or((80, 24), |(w, h)| (w as usize, h as usize))
    }

    /// Forgets the last frame, so the next draw happens even if nothing changed.
    pub fn invalidate(&mut self) {
        self.last = None;
    }

    /// Wipes the screen, and with `scrollback` the terminal's history too
    /// (/clear, a resize). The next draw starts from the top-left corner. The
    /// wipe goes out in that draw's frame, so the screen is never seen empty.
    pub fn clear(&mut self, scrollback: bool) {
        self.wipe = Some(scrollback || self.wipe == Some(true));
        self.row = 0;
        if let Some(a) = &mut self.anchor {
            (a.next, a.top) = (0, 0);
        }
        self.invalidate();
    }

    /// After a resize: the bottom layout re-anchors to the new height.
    pub fn resize(&mut self, rows: usize) {
        let height = self.last.as_ref().map_or(0, |(l, _)| l.len());
        if let Some(a) = &mut self.anchor {
            a.rows = rows;
            a.top = rows.saturating_sub(height);
            a.next = a.next.min(a.top);
        }
        self.invalidate();
    }

    /// Writes `commits` above the live area, then redraws the live area.
    /// `cursor` is a (row, column) inside `live`; `None` hides it.
    ///
    /// When nothing is committed and the live area has the same number of
    /// lines as last time, only the lines that changed are rewritten; while
    /// text streams in that's usually one or two.
    pub fn draw(
        &mut self,
        commits: &mut Vec<String>,
        live: Vec<String>,
        cursor: Option<(usize, usize)>,
    ) -> io::Result<()> {
        let same_shape = commits.is_empty() && self.last.as_ref().is_some_and(|(l, _)| l.len() == live.len());
        if same_shape && self.last.as_ref().is_some_and(|(l, c)| *l == live && *c == cursor) {
            return Ok(());
        }
        self.buf.clear();
        self.buf.push_str("\x1b[?2026h\x1b[?25l");
        if let Some(scrollback) = self.wipe.take() {
            self.buf.push_str(if scrollback { "\x1b[2J\x1b[3J\x1b[H" } else { "\x1b[2J\x1b[H" });
        }
        if same_shape {
            self.update_lines(&live);
        } else {
            self.redraw(commits, &live);
        }
        if let Some((row, col)) = cursor {
            self.move_to(row);
            self.buf.push('\r');
            if col > 0 {
                let _ = write!(self.buf, "\x1b[{col}C");
            }
            self.buf.push_str("\x1b[?25h");
        }
        self.buf.push_str("\x1b[?2026l");
        self.out.write_all(self.buf.as_bytes())?;
        // Redrawing a long history after a resize is a big frame; the space
        // for it isn't kept for the small ones that follow.
        self.buf.shrink_to(FRAME_BYTES);
        commits.shrink_to(0);
        self.last = Some((live, cursor));
        self.out.flush()
    }

    /// Clears the live area, writes `commits` into scrollback, then the live
    /// area. Leaves the cursor on its last line.
    fn redraw(&mut self, commits: &mut Vec<String>, live: &[String]) {
        self.move_to(0);
        self.buf.push_str("\r\x1b[J");
        let mut next = 0;
        if let Some(a) = self.anchor {
            // Finished lines continue from where the last ones ended.
            let _ = write!(self.buf, "\x1b[{};1H", a.next + 1);
            next = a.next;
        }
        for line in commits.drain(..) {
            self.buf.push_str(&line);
            self.buf.push_str("\x1b[0m\r\n");
            next += 1;
        }
        if let Some(a) = &mut self.anchor {
            // A newline on the last row scrolls the screen and the cursor stays.
            let next = next.min(a.rows - 1);
            let top = a.rows.saturating_sub(live.len());
            if next > top {
                // The live area doesn't fit below the new lines: scroll them up.
                let _ = write!(self.buf, "\x1b[{};1H{}", a.rows, "\n".repeat(next - top));
            }
            let _ = write!(self.buf, "\x1b[{};1H", top + 1);
            (a.next, a.top) = (next.min(top), top);
        }
        for (i, line) in live.iter().enumerate() {
            if i > 0 {
                self.buf.push_str("\r\n");
            }
            self.buf.push_str(line);
        }
        self.buf.push_str("\x1b[0m");
        self.row = live.len().saturating_sub(1);
    }

    /// Rewrites only the lines that differ from the last frame.
    fn update_lines(&mut self, live: &[String]) {
        let Some((old, _)) = self.last.take() else { return };
        for (i, (new, old)) in live.iter().zip(&old).enumerate() {
            if new != old {
                self.move_to(i);
                self.buf.push_str("\r\x1b[2K");
                self.buf.push_str(new);
                self.buf.push_str("\x1b[0m");
            }
        }
        self.last = Some((old, None));
    }

    /// Moves the cursor to `row` of the live area.
    fn move_to(&mut self, row: usize) {
        match row.cmp(&self.row) {
            std::cmp::Ordering::Less => {
                let _ = write!(self.buf, "\x1b[{}A", self.row - row);
            }
            std::cmp::Ordering::Greater => {
                let _ = write!(self.buf, "\x1b[{}B", row - self.row);
            }
            std::cmp::Ordering::Equal => {}
        }
        self.row = row;
    }
}

impl Drop for Term {
    /// Clears the live area and gives the terminal back as it was.
    fn drop(&mut self) {
        let mut s = String::new();
        if self.row > 0 {
            let _ = write!(s, "\x1b[{}A", self.row);
        }
        s.push_str("\r\x1b[J\x1b[?25h\x1b[?2004l");
        if self.enhanced_keys {
            s.push_str("\x1b[<1u");
        }
        let _ = self.out.write_all(s.as_bytes());
        let _ = self.out.flush();
        let _ = terminal::disable_raw_mode();
    }
}

/// Leaves raw mode and shows the cursor, without the usual cleanup. For
/// panics and signals, where `Term` won't be dropped.
pub fn restore() {
    let _ = terminal::disable_raw_mode();
    let _ = io::stdout().write_all(b"\x1b[?25h\x1b[?2004l\x1b[<1u\r\n");
    let _ = io::stdout().flush();
}

pub fn restore_on_panic() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        default(info);
    }));
}
