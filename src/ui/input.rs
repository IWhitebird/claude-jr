// The input line editor: a text buffer, a cursor (byte offset) and history.

use std::ops::Range;
use unicode_width::UnicodeWidthChar;

#[derive(Default)]
pub struct Input {
    buf: String,
    cur: usize,
    history: Vec<String>,
    /// Position in history while browsing; `history.len()` when not.
    hist: usize,
    /// What was typed before browsing history started.
    draft: String,
}

impl Input {
    pub fn text(&self) -> &str {
        &self.buf
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn clear(&mut self) {
        self.buf.clear();
        self.cur = 0;
    }

    /// Takes the text out and remembers it in history.
    pub fn take(&mut self) -> String {
        let text = std::mem::take(&mut self.buf);
        self.cur = 0;
        if !text.trim().is_empty() && self.history.last() != Some(&text) {
            self.history.push(text.clone());
        }
        self.hist = self.history.len();
        text
    }

    /// Takes the text out without remembering it (for secrets).
    pub fn take_secret(&mut self) -> String {
        self.cur = 0;
        std::mem::take(&mut self.buf)
    }

    /// A copy showing • for every character, with the cursor in the same place.
    pub fn masked(&self) -> Input {
        let chars_before = self.buf[..self.cur].chars().count();
        let buf = "•".repeat(self.buf.chars().count());
        let cur = "•".len() * chars_before;
        Input { buf, cur, ..Input::default() }
    }

    pub fn insert(&mut self, s: &str) {
        self.buf.insert_str(self.cur, s);
        self.cur += s.len();
    }

    /// Enter after a trailing backslash becomes a newline.
    pub fn continues_line(&mut self) -> bool {
        if self.cur == self.buf.len() && self.buf.ends_with('\\') {
            self.buf.pop();
            self.cur -= 1;
            self.insert("\n");
            return true;
        }
        false
    }

    fn prev(&self) -> usize {
        self.buf[..self.cur].chars().next_back().map_or(0, |c| self.cur - c.len_utf8())
    }

    fn next(&self) -> usize {
        self.buf[self.cur..].chars().next().map_or(self.cur, |c| self.cur + c.len_utf8())
    }

    pub fn left(&mut self) {
        self.cur = self.prev();
    }

    pub fn right(&mut self) {
        self.cur = self.next();
    }

    /// Deletes the character before the cursor, or a whole paste label.
    pub fn backspace(&mut self) {
        let p = super::paste::label_start(&self.buf[..self.cur]).unwrap_or_else(|| self.prev());
        self.buf.drain(p..self.cur);
        self.cur = p;
    }

    pub fn delete(&mut self) {
        let n = self.next();
        self.buf.drain(self.cur..n);
    }

    pub fn home(&mut self) {
        self.cur = self.buf[..self.cur].rfind('\n').map_or(0, |i| i + 1);
    }

    pub fn end(&mut self) {
        self.cur = self.buf[self.cur..].find('\n').map_or(self.buf.len(), |i| self.cur + i);
    }

    pub fn kill_to_start(&mut self) {
        let start = self.buf[..self.cur].rfind('\n').map_or(0, |i| i + 1);
        self.buf.drain(start..self.cur);
        self.cur = start;
    }

    pub fn kill_to_end(&mut self) {
        let end = self.buf[self.cur..].find('\n').map_or(self.buf.len(), |i| self.cur + i);
        self.buf.drain(self.cur..end);
    }

    pub fn delete_word(&mut self) {
        let before = &self.buf[..self.cur];
        let trimmed = before.trim_end();
        let start = trimmed.char_indices().rev().find(|(_, c)| c.is_whitespace()).map_or(0, |(i, c)| i + c.len_utf8());
        self.buf.drain(start..self.cur);
        self.cur = start;
    }

    /// Moves up a row, or to the previous history entry from the top row.
    pub fn up(&mut self, cols: usize) {
        let (rows, (r, c)) = self.layout(cols);
        if r == 0 {
            if self.hist == self.history.len() {
                self.draft = self.buf.clone();
            }
            if self.hist > 0 {
                self.hist -= 1;
                self.load(self.history[self.hist].clone());
            }
        } else {
            self.cur = self.offset_at(rows[r - 1].clone(), c);
        }
    }

    /// Moves down a row, or to the next history entry from the bottom row.
    pub fn down(&mut self, cols: usize) {
        let (rows, (r, c)) = self.layout(cols);
        if r + 1 < rows.len() {
            self.cur = self.offset_at(rows[r + 1].clone(), c);
        } else if self.hist < self.history.len() {
            self.hist += 1;
            let text = match self.history.get(self.hist) {
                Some(h) => h.clone(),
                None => std::mem::take(&mut self.draft),
            };
            self.load(text);
        }
    }

    fn load(&mut self, text: String) {
        self.buf = text;
        self.cur = self.buf.len();
    }

    fn offset_at(&self, row: Range<usize>, col: usize) -> usize {
        let mut w = 0;
        for (i, c) in self.buf[row.clone()].char_indices() {
            let cw = c.width().unwrap_or(0);
            if w + cw > col {
                return row.start + i;
            }
            w += cw;
        }
        row.end
    }

    /// Splits the text into rows of at most `cols` columns. Returns each row's
    /// byte range and the cursor's (row, column).
    pub fn layout(&self, cols: usize) -> (Vec<Range<usize>>, (usize, usize)) {
        let cols = cols.max(1);
        let mut rows = Vec::new();
        let (mut start, mut col) = (0, 0);
        let mut cursor = None;
        for (i, c) in self.buf.char_indices() {
            let cw = c.width().unwrap_or(0);
            if c != '\n' && col + cw > cols {
                rows.push(start..i);
                (start, col) = (i, 0);
            }
            if i == self.cur {
                cursor = Some((rows.len(), col));
            }
            if c == '\n' {
                rows.push(start..i);
                (start, col) = (i + 1, 0);
            } else {
                col += cw;
            }
        }
        if cursor.is_none() && col >= cols {
            rows.push(start..self.buf.len());
            (start, col) = (self.buf.len(), 0);
        }
        rows.push(start..self.buf.len());
        let cursor = cursor.unwrap_or((rows.len() - 1, col));
        (rows, cursor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing() {
        let mut i = Input::default();
        i.insert("héllo wörld");
        i.left();
        i.backspace();
        assert_eq!(i.text(), "héllo wörd");
        i.delete_word();
        assert_eq!(i.text(), "héllo d");
        i.home();
        i.kill_to_end();
        assert_eq!(i.text(), "");
    }

    #[test]
    fn delete_word_after_wide_space() {
        let mut i = Input::default();
        i.insert("a\u{3000}bc");
        i.delete_word();
        assert_eq!(i.text(), "a\u{3000}");
        let mut i = Input::default();
        i.insert("foo bar  ");
        i.delete_word();
        assert_eq!(i.text(), "foo ");
    }

    #[test]
    fn layout_and_vertical_moves() {
        let mut i = Input::default();
        i.insert("abcdef\nxy");
        let (rows, cur) = i.layout(4);
        let parts: Vec<_> = rows.iter().map(|r| &i.text()[r.clone()]).collect();
        assert_eq!(parts, ["abcd", "ef", "xy"]);
        assert_eq!(cur, (2, 2));
        i.up(4);
        assert_eq!(i.layout(4).1, (1, 2));
        i.up(4);
        assert_eq!(i.layout(4).1, (0, 2));
    }

    #[test]
    fn history() {
        let mut i = Input::default();
        i.insert("first");
        i.take();
        i.insert("second");
        i.take();
        i.insert("draft");
        i.up(80);
        assert_eq!(i.text(), "second");
        i.up(80);
        assert_eq!(i.text(), "first");
        i.down(80);
        i.down(80);
        assert_eq!(i.text(), "draft");
    }

    fn typed(s: &str) -> Input {
        let mut i = Input::default();
        i.insert(s);
        i
    }

    fn rows(i: &Input, cols: usize) -> Vec<String> {
        i.layout(cols).0.iter().map(|r| i.text()[r.clone()].to_string()).collect()
    }

    #[test]
    fn clearing_and_taking_text() {
        let mut i = typed("hello");
        assert!(!i.is_empty());
        i.clear();
        assert!(i.is_empty());
        i.insert("x");
        assert_eq!(i.text(), "x", "the cursor went back to the start of an empty line");

        let mut i = typed("one");
        assert_eq!(i.take(), "one");
        assert!(i.is_empty());
        // Blank lines and repeats are not remembered.
        i.insert("   ");
        i.take();
        i.insert("one");
        i.take();
        i.up(80);
        assert_eq!(i.text(), "one");
        i.up(80);
        assert_eq!(i.text(), "one", "already at the oldest entry");
    }

    #[test]
    fn secrets_stay_out_of_history_and_are_masked() {
        let mut i = typed("sk-h\u{e9}llo");
        let m = i.masked();
        assert_eq!(m.text(), "\u{2022}".repeat(8));
        let (rows, (r, c)) = m.layout(80);
        assert_eq!((rows.len(), r, c), (1, 0, 8), "the cursor is after the last dot");
        i.left();
        i.left();
        let m = i.masked();
        assert_eq!(m.layout(80).1, (0, 6));
        assert_eq!(i.take_secret(), "sk-h\u{e9}llo");
        assert!(i.is_empty());
        i.up(80);
        assert!(i.is_empty(), "nothing was remembered");
    }

    #[test]
    fn a_trailing_backslash_continues_the_line() {
        let mut i = typed("first \\");
        assert!(i.continues_line());
        assert_eq!(i.text(), "first \n");
        i.insert("second");
        assert!(!i.continues_line());
        assert_eq!(i.text(), "first \nsecond");
        // Only at the very end of the text.
        let mut i = typed("a\\b");
        i.left();
        i.left();
        assert!(!i.continues_line());
        assert_eq!(i.text(), "a\\b");
        let mut i = Input::default();
        assert!(!i.continues_line());
    }

    #[test]
    fn moving_and_deleting_stop_at_the_ends() {
        let mut i = typed("a\u{e9}\u{4e16}");
        i.right();
        assert_eq!(i.text(), "a\u{e9}\u{4e16}");
        i.delete();
        assert_eq!(i.text(), "a\u{e9}\u{4e16}", "nothing after the cursor");
        i.left();
        i.delete();
        assert_eq!(i.text(), "a\u{e9}");
        i.left();
        i.right();
        i.right();
        i.backspace();
        assert_eq!(i.text(), "a");
        i.left();
        i.left();
        i.backspace();
        assert_eq!(i.text(), "a", "nothing before the cursor");
        i.delete();
        assert!(i.is_empty());
    }

    #[test]
    fn home_end_and_kills_work_on_the_current_line() {
        let mut i = typed("one\ntwo three\nfour");
        i.left();
        i.left(); // fo|ur
        i.home();
        i.insert(">");
        assert_eq!(i.text(), "one\ntwo three\n>four");
        i.end();
        i.insert("<");
        assert_eq!(i.text(), "one\ntwo three\n>four<");

        i.up(80); // the middle line, at column 6 ("two th|ree")
        i.kill_to_end();
        assert_eq!(i.text(), "one\ntwo th\n>four<");
        i.kill_to_start();
        assert_eq!(i.text(), "one\n\n>four<");
        i.insert("x");
        i.up(80);
        i.home();
        i.kill_to_start();
        assert_eq!(i.text(), "one\nx\n>four<", "at the start of a line there is nothing to kill");
        i.end();
        i.kill_to_end();
        assert_eq!(i.text(), "one\nx\n>four<");
    }

    #[test]
    fn deleting_a_word_at_the_start_does_nothing() {
        let mut i = typed("word");
        i.delete_word();
        assert!(i.is_empty());
        i.delete_word();
        assert!(i.is_empty());
        let mut i = typed("   ");
        i.delete_word();
        assert_eq!(i.text(), "");
    }

    #[test]
    fn layout_wraps_by_display_width() {
        assert_eq!(
            rows(&typed("a\u{4e16}b"), 2),
            ["a", "\u{4e16}", "b"],
            "a wide character moves to the next row whole"
        );
        assert_eq!(typed("a\u{4e16}b").layout(2).1, (2, 1));
        // Text that exactly fills the last row puts the cursor on a new, empty one.
        let i = typed("abcd");
        assert_eq!(rows(&i, 4), ["abcd", ""]);
        assert_eq!(i.layout(4).1, (1, 0));
        // A zero-width column count is treated as one.
        assert_eq!(rows(&typed("ab"), 0), ["a", "b", ""]);
        // The cursor in the middle of wrapped text.
        let mut i = typed("abcdefgh");
        i.left();
        i.left();
        i.left();
        assert_eq!(i.layout(3).1, (1, 2));
        assert_eq!(rows(&Input::default(), 10), [""]);
    }

    #[test]
    fn up_and_down_keep_the_column_across_wide_characters() {
        let mut i = typed("\u{4e16}\u{4e16}\u{4e16}\nabcdef");
        // Cursor at the end of "abcdef": column 6; the row above is 3 wide characters = 6 columns.
        i.up(80);
        assert_eq!(i.layout(80).1, (0, 6));
        i.left();
        assert_eq!(i.layout(80).1, (0, 4));
        i.down(80);
        assert_eq!(i.layout(80).1, (1, 4));
        // Landing in the middle of a wide character puts the cursor before it.
        let mut i = typed("abc\n\u{4e16}\u{4e16}");
        i.up(80);
        i.home();
        i.right();
        i.right();
        i.right(); // column 3 on the first line
        i.down(80);
        assert_eq!(i.layout(80).1, (1, 2));
    }

    #[test]
    fn history_browsing_restores_the_draft_and_stops_at_the_ends() {
        let mut i = Input::default();
        i.down(80); // nothing to go down to
        assert!(i.is_empty());
        i.up(80);
        assert!(i.is_empty(), "no history yet");
        for t in ["a", "b", "c"] {
            i.insert(t);
            i.take();
        }
        i.insert("draft");
        i.up(80);
        i.up(80);
        i.up(80);
        i.up(80);
        assert_eq!(i.text(), "a");
        i.down(80);
        assert_eq!(i.text(), "b");
        i.down(80);
        i.down(80);
        assert_eq!(i.text(), "draft");
        i.down(80);
        assert_eq!(i.text(), "draft");
        // Browsing a multi-line entry moves within it before moving on.
        let mut i = Input::default();
        i.insert("l1\nl2");
        i.take();
        i.up(80);
        assert_eq!(i.text(), "l1\nl2");
        i.up(80);
        assert_eq!(i.layout(80).1, (0, 2));
        i.down(80);
        assert_eq!(i.layout(80).1, (1, 2));
    }
}
