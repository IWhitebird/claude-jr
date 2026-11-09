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

    pub fn backspace(&mut self) {
        let p = self.prev();
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
        let start = trimmed.rfind(char::is_whitespace).map_or(0, |i| i + 1);
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
}
