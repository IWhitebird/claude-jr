// Inline terminal drawing. Finished lines are written once into the normal
// scrollback. Only the "live" area at the bottom (streaming text, status,
// input box) is redrawn, inside synchronized output so it doesn't flicker.

use crossterm::event::{KeyboardEnhancementFlags, PushKeyboardEnhancementFlags};
use crossterm::terminal;
use std::fmt::Write as _;
use std::io::{self, Write};

pub struct Term {
    out: io::Stdout,
    buf: String,
    /// Cursor row inside the live area, counted from its top.
    row: usize,
    enhanced_keys: bool,
}

impl Term {
    pub fn new() -> io::Result<Term> {
        terminal::enable_raw_mode()?;
        let mut out = io::stdout();
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
        Ok(Term { out, buf: String::with_capacity(16 * 1024), row: 0, enhanced_keys })
    }

    pub fn size() -> (usize, usize) {
        terminal::size().map_or((80, 24), |(w, h)| (w as usize, h as usize))
    }

    /// Writes `commits` above the live area, then redraws the live area.
    /// `cursor` is a (row, column) inside `live`; `None` hides it.
    pub fn draw(
        &mut self,
        commits: &mut Vec<String>,
        live: &[String],
        cursor: Option<(usize, usize)>,
    ) -> io::Result<()> {
        let b = &mut self.buf;
        b.clear();
        b.push_str("\x1b[?2026h\x1b[?25l");
        if self.row > 0 {
            let _ = write!(b, "\x1b[{}A", self.row);
        }
        b.push_str("\r\x1b[J");
        for line in commits.drain(..) {
            b.push_str(&line);
            b.push_str("\x1b[0m\r\n");
        }
        for (i, line) in live.iter().enumerate() {
            if i > 0 {
                b.push_str("\r\n");
            }
            b.push_str(line);
        }
        b.push_str("\x1b[0m");

        let last = live.len().saturating_sub(1);
        self.row = last;
        if let Some((row, col)) = cursor {
            if last > row {
                let _ = write!(b, "\x1b[{}A", last - row);
            }
            b.push('\r');
            if col > 0 {
                let _ = write!(b, "\x1b[{col}C");
            }
            b.push_str("\x1b[?25h");
            self.row = row;
        }
        b.push_str("\x1b[?2026l");
        self.out.write_all(b.as_bytes())?;
        self.out.flush()
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

/// Puts the terminal back to normal if we panic while in raw mode.
pub fn restore_on_panic() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = terminal::disable_raw_mode();
        let _ = io::stdout().write_all(b"\x1b[?25h\x1b[?2004l\r\n");
        default(info);
    }));
}
