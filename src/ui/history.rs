// What was written to scrollback, kept as source (not as finished lines) so
// it can be drawn again at a new width when the terminal is resized. Only the
// most recent ~1000 lines are kept: enough to refill the scrollback after a
// resize, never the whole conversation.

use super::md::Markdown;
use super::text::{ORANGE, Span, clean, span};
use super::{table, view};
use crate::tools::Diff;
use std::collections::VecDeque;

pub enum Block {
    /// Drawn the same at any width (the header).
    Lines(Vec<String>),
    Blank,
    /// One line of the model's markdown; `lead` puts the ● marker on it. `md`
    /// is the markdown state before the line (inside a code block or not).
    /// `more`: the start of a paragraph that is still streaming; the next
    /// piece is joined on, so the paragraph wraps as one at any width.
    Text {
        line: String,
        lead: bool,
        md: Markdown,
        more: bool,
    },
    Thinking(String),
    Table {
        rows: Vec<String>,
        lead: bool,
        md: Markdown,
    },
    Note {
        msg: String,
        style: &'static str,
    },
    Tool {
        label: String,
        diff: Option<Diff>,
    },
    Result {
        ok: bool,
        summary: String,
    },
    User(String),
}

fn lead(marker: bool) -> Span<'static> {
    if marker { span("● ", ORANGE) } else { span("  ", "") }
}

impl Block {
    pub fn render(&self, cols: usize, out: &mut Vec<String>) {
        match self {
            Block::Lines(lines) => out.extend(lines.iter().cloned()),
            Block::Blank => out.push(String::new()),
            Block::Text { line, lead: l, md, .. } => md.clone().line(line, cols, lead(*l), out),
            Block::Thinking(text) => view::thinking(text, cols, out),
            Block::Table { rows, lead: l, md } => {
                if !table::render(rows, cols, lead(*l), out) {
                    // Too narrow for a table: the rows as plain text.
                    let mut md = *md;
                    for (i, row) in rows.iter().enumerate() {
                        md.line(&clean(row), cols, lead(*l && i == 0), out);
                    }
                }
            }
            Block::Note { msg, style } => view::note(msg, style, cols, out),
            Block::Tool { label, diff } => {
                view::tool(label, cols, out);
                if let Some(d) = diff {
                    view::diff(d, cols, out);
                }
            }
            Block::Result { ok, summary } => view::result(*ok, summary, cols, out),
            Block::User(text) => view::user(text, cols, out),
        }
    }
}

/// The most recent blocks and how many lines each took when drawn.
#[derive(Default)]
pub struct History {
    blocks: VecDeque<(Block, usize)>,
    lines: usize,
}

impl History {
    /// Remembers a block that was drawn as `lines` lines, dropping the oldest
    /// ones while what's left still covers `keep` lines.
    pub fn push(&mut self, block: Block, lines: usize, keep: usize) {
        self.lines += lines;
        match (self.blocks.back_mut(), block) {
            (Some((Block::Text { line, more, .. }, n)), Block::Text { line: next, lead: false, more: m, .. })
                if *more =>
            {
                line.push(' ');
                line.push_str(&next);
                (*more, *n) = (m, *n + lines);
            }
            (_, block) => self.blocks.push_back((block, lines)),
        }
        while let Some((_, first)) = self.blocks.front() {
            if self.lines - first < keep {
                break;
            }
            self.lines -= first;
            self.blocks.pop_front();
        }
    }

    /// Takes back the start of a paragraph that is still streaming, so it
    /// can be laid out again together with the rest: its text, whether it
    /// has the ● marker, and the markdown state before it.
    pub fn take_partial(&mut self) -> Option<(String, bool, Markdown)> {
        if !matches!(self.blocks.back(), Some((Block::Text { more: true, .. }, _))) {
            return None;
        }
        let (block, lines) = self.blocks.pop_back()?;
        self.lines -= lines;
        match block {
            Block::Text { line, lead, md, .. } => Some((line, lead, md)),
            _ => None,
        }
    }

    pub fn clear(&mut self) {
        self.blocks.clear();
        self.lines = 0;
    }

    /// Everything kept, drawn at `cols`.
    pub fn render(&self, cols: usize) -> Vec<String> {
        let mut out = Vec::new();
        for (block, _) in &self.blocks {
            block.render(cols, &mut out);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_enough_to_refill_the_screen() {
        let mut h = History::default();
        for i in 0..100 {
            h.push(Block::User(format!("message {i}")), 1, 10);
        }
        assert_eq!(h.blocks.len(), 10);
        assert_eq!(h.render(80).last().map(|l| l.contains("message 99")), Some(true));
    }

    #[test]
    fn rewraps_at_a_new_width() {
        let mut h = History::default();
        let text = "a paragraph long enough that it has to wrap when the terminal gets narrow".to_string();
        h.push(Block::Text { line: text, lead: true, md: Markdown::default(), more: false }, 1, 100);
        assert_eq!(h.render(100).len(), 1);
        assert!(h.render(30).len() >= 3);
    }

    #[test]
    fn pieces_of_a_streaming_paragraph_wrap_as_one() {
        let piece = |line: &str, lead, more| Block::Text { line: line.into(), lead, md: Markdown::default(), more };
        let mut h = History::default();
        h.push(piece("one two three four", true, true), 1, 100);
        h.push(piece("five six seven", false, true), 1, 100);
        h.push(piece("eight nine", false, false), 1, 100);
        h.push(piece("a new paragraph", false, false), 1, 100);
        assert_eq!(h.blocks.len(), 2);
        assert_eq!(h.lines, 4);
        // Wide enough for the whole paragraph: one row, not one per piece.
        assert_eq!(h.render(80).len(), 2);
        assert!(h.take_partial().is_none(), "the paragraph has ended");

        let mut h = History::default();
        h.push(piece("one two", true, true), 1, 100);
        h.push(piece("three", false, true), 1, 100);
        let (line, lead, _) = h.take_partial().unwrap();
        assert_eq!((line.as_str(), lead), ("one two three", true));
        assert_eq!((h.blocks.len(), h.lines), (0, 0));
    }
}
