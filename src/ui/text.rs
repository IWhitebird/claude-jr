// Styled text and word wrapping by display width.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub const RESET: &str = "\x1b[0m";
pub const BOLD: &str = "\x1b[1m";
pub const DIM: &str = "\x1b[2m";
pub const ITALIC_DIM: &str = "\x1b[2;3m";
pub const RED: &str = "\x1b[31m";
pub const GREEN: &str = "\x1b[32m";
pub const CYAN: &str = "\x1b[36m";
pub const GREY: &str = "\x1b[90m";
/// The claude-jr orange (256-color 173, close to #D97757), for the brand and anything "working".
pub const ORANGE: &str = "\x1b[38;5;173m";
pub const BOLD_ORANGE: &str = "\x1b[1;38;5;173m";

/// A run of text drawn in one style ("" is the terminal default).
#[derive(Clone, Copy)]
pub struct Span<'a> {
    pub text: &'a str,
    pub style: &'static str,
}

pub const fn span<'a>(text: &'a str, style: &'static str) -> Span<'a> {
    Span { text, style }
}

pub fn width(s: &str) -> usize {
    s.width()
}

/// Cuts plain text to `max` columns, ending with … when cut.
pub fn fit(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if width(s) <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw + 1 > max {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

/// Like `fit`, for text that already has escape codes in it.
pub fn fit_styled(s: &str, max: usize) -> String {
    let mut out = String::with_capacity(s.len());
    let (mut w, mut esc) = (0, false);
    for c in s.chars() {
        match c {
            '\x1b' => esc = true,
            'm' if esc => esc = false,
            _ if esc => {}
            c => {
                let cw = c.width().unwrap_or(0);
                if w + cw > max {
                    break;
                }
                w += cw;
            }
        }
        out.push(c);
    }
    out.push_str(RESET);
    out
}

/// Drops escape codes, to measure styled text.
pub fn strip(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut esc = false;
    for c in s.chars() {
        match c {
            '\x1b' => esc = true,
            'm' if esc => esc = false,
            _ if esc => {}
            c => out.push(c),
        }
    }
    out
}

/// Cuts a styled line to `max` columns if it is wider, and leaves it alone otherwise.
pub fn clip(line: String, max: usize) -> String {
    if width(&strip(&line)) <= max { line } else { fit_styled(&line, max) }
}

/// Removes control characters (C0, DEL, C1, so escape codes too) and text
/// direction overrides from untrusted text and expands tabs, so it can't move
/// the cursor, set the title or break the layout.
pub fn clean(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\t' => out.push_str("    "),
            '\n' => out.push('\n'),
            c if c.is_control() => {}
            // Direction overrides and isolates can reorder what is drawn.
            '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' => {}
            c => out.push(c),
        }
    }
    out
}

/// `clean` for text shown on one row: newlines become spaces.
pub fn clean_line(s: &str) -> String {
    clean(s).replace('\n', " ")
}

/// True if the line shows nothing but spaces.
pub fn is_blank(line: &str) -> bool {
    let mut in_escape = false;
    for c in line.chars() {
        match c {
            '\x1b' => in_escape = true,
            'm' if in_escape => in_escape = false,
            _ if in_escape => {}
            c if !c.is_whitespace() => return false,
            _ => {}
        }
    }
    true
}

/// Word-wraps spans to `width` columns. The first row starts with `first`,
/// later rows with `rest`. Words longer than a row are broken by character.
pub fn wrap<'a>(spans: &[Span<'a>], width: usize, first: Span<'a>, rest: Span<'a>, out: &mut Vec<String>) {
    let base = out.len();
    let mut w = Wrapper {
        out,
        line: String::new(),
        col: 0,
        start: 0,
        width: width.max(1),
        rest,
        open: "",
        space: None,
        first_row: true,
    };
    w.prefix(first);
    for s in spans {
        for word in words(s.text) {
            w.word(word, s.style);
        }
    }
    w.finish();
    // A prefix wider than the row, or a wide character in a one-column row,
    // can still overflow: cut those rows.
    for line in &mut out[base..] {
        *line = clip(std::mem::take(line), width.max(1));
    }
}

struct Wrapper<'a, 'o> {
    out: &'o mut Vec<String>,
    line: String,
    col: usize,
    /// Column where text starts, after the prefix.
    start: usize,
    width: usize,
    rest: Span<'a>,
    /// Style currently in effect on `line`.
    open: &'static str,
    /// Whitespace seen but not yet written: it's dropped if the line breaks there.
    space: Option<(&'a str, &'static str)>,
    first_row: bool,
}

impl<'a> Wrapper<'a, '_> {
    fn prefix(&mut self, p: Span<'a>) {
        self.put(p.text, p.style);
        self.start = self.col;
    }

    fn style(&mut self, style: &'static str) {
        if style != self.open {
            if !self.open.is_empty() {
                self.line.push_str(RESET);
            }
            self.line.push_str(style);
            self.open = style;
        }
    }

    fn put(&mut self, text: &str, style: &'static str) {
        self.style(style);
        self.line.push_str(text);
        self.col += width(text);
    }

    fn newline(&mut self) {
        self.style("");
        self.out.push(std::mem::take(&mut self.line));
        self.col = 0;
        self.space = None;
        self.first_row = false;
        let rest = self.rest;
        self.prefix(rest);
    }

    fn word(&mut self, word: &'a str, style: &'static str) {
        if word.starts_with(char::is_whitespace) {
            // A newline or tab inside a row would break it: show one space.
            let word = if word.contains(char::is_control) { " " } else { word };
            // Indentation at the very start is kept; other spaces wait for the next word.
            if self.first_row && self.col == self.start {
                self.put(word, style);
            } else if self.col > self.start {
                self.space = Some((word, style));
            }
            return;
        }
        let w = width(word);
        let gap = self.space.map_or(0, |(s, _)| width(s));
        if self.col + gap + w > self.width && self.col > self.start {
            self.newline();
        }
        if let Some((s, st)) = self.space.take() {
            self.put(s, st);
        }
        if self.col + w <= self.width {
            self.put(word, style);
            return;
        }
        let mut buf = [0u8; 4];
        for c in word.chars() {
            let cw = c.width().unwrap_or(0);
            if self.col + cw > self.width && self.col > self.start {
                self.newline();
            }
            self.put(c.encode_utf8(&mut buf), style);
        }
    }

    fn finish(mut self) {
        self.style("");
        self.out.push(self.line);
    }
}

/// Splits text into alternating runs of whitespace and non-whitespace.
fn words(s: &str) -> impl Iterator<Item = &str> {
    let mut rest = s;
    std::iter::from_fn(move || {
        let first = rest.chars().next()?;
        let space = first.is_whitespace();
        let end = rest.find(|c: char| c.is_whitespace() != space).unwrap_or(rest.len());
        let (word, tail) = rest.split_at(end);
        rest = tail;
        Some(word)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: &[String]) -> Vec<String> {
        lines.iter().map(|l| l.replace('\x1b', "").replace("[0m", "").replace("[1m", "")).collect()
    }

    #[test]
    fn wraps_words_with_prefixes() {
        let mut out = Vec::new();
        wrap(&[span("the quick brown fox jumps", "")], 12, span("● ", ""), span("  ", ""), &mut out);
        assert_eq!(out, ["● the quick", "  brown fox", "  jumps"]);
    }

    #[test]
    fn breaks_long_words_and_keeps_styles_per_line() {
        let mut out = Vec::new();
        wrap(&[span("abcdefghij", BOLD)], 6, span("", ""), span("", ""), &mut out);
        assert_eq!(plain(&out), ["abcdef", "ghij"]);
        assert!(out.iter().all(|l| l.starts_with(BOLD) && l.ends_with(RESET)));
    }

    #[test]
    fn wide_chars_and_helpers() {
        let mut out = Vec::new();
        wrap(&[span("日本語のテキスト", "")], 6, span("", ""), span("", ""), &mut out);
        assert_eq!(out, ["日本語", "のテキ", "スト"]);
        assert_eq!(fit("hello world", 6), "hello…");
        assert_eq!(clean("a\x1b[31mb\tc"), "a[31mb    c");
        assert_eq!(clean("a\x1b]0;t\x07\u{9b}b\r\x7fc\u{202e}d"), "a]0;tbcd");
        assert!(is_blank("\x1b[2m  \x1b[0m"));
        assert!(!is_blank("\x1b[2m x\x1b[0m"));
    }

    #[test]
    fn wrap_never_overflows() {
        for w in [0usize, 1, 2, 3, 5, 10] {
            let mut out = Vec::new();
            wrap(&[span("日本語 😀😀 abcdefghij", BOLD)], w, span("● ", ""), span("  ", ""), &mut out);
            assert!(out.iter().all(|l| width(&strip(l)) <= w.max(1)), "{w}: {out:?}");
        }
        assert_eq!(fit("abc", 0), "");
        assert_eq!(width(&fit("日本語", 4)), 3);
    }
}
