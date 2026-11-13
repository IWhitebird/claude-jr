// What was written to scrollback, kept as source (not as finished lines) so
// it can be drawn again at a new width when the terminal is resized. It is
// what's on screen, not what was sent: a tool's output is here as the few
// lines that were shown. Everything is kept, so a resize loses nothing; only
// past a very large size do the oldest blocks go, and then a line says so.

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

/// The blocks drawn so far and how many lines each took.
#[derive(Default)]
pub struct History {
    blocks: VecDeque<(Block, usize)>,
    lines: usize,
    /// Lines of old blocks that were let go.
    dropped: usize,
}

impl History {
    /// Remembers a block that was drawn as `lines` lines. Past `keep` lines
    /// the oldest blocks are dropped, but never the header at the top.
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
        let oldest = usize::from(matches!(self.blocks.front(), Some((Block::Lines(_), _))));
        while let Some((_, first)) = self.blocks.get(oldest) {
            if self.lines - first < keep {
                break;
            }
            self.lines -= first;
            self.dropped += first;
            self.blocks.remove(oldest);
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
        (self.lines, self.dropped) = (0, 0);
    }

    /// Everything kept, drawn at `cols`.
    pub fn render(&self, cols: usize) -> Vec<String> {
        let mut out = Vec::new();
        for (i, (block, _)) in self.blocks.iter().enumerate() {
            if i == 1 && self.dropped > 0 {
                let gone =
                    format!("… {} earlier lines are no longer shown here. The saved session has them.", self.dropped);
                view::note(&gone, super::text::DIM, cols, &mut out);
                out.push(String::new());
            }
            block.render(cols, &mut out);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn everything_is_kept_up_to_the_limit() {
        let mut h = History::default();
        h.push(Block::Lines(vec!["the header".into()]), 1, 5000);
        for i in 0..3000 {
            h.push(Block::User(format!("message {i}")), 1, 5000);
        }
        let drawn = h.render(80);
        assert_eq!(drawn.len(), 3001);
        assert_eq!(drawn[0], "the header");
        assert!(drawn[1].contains("message 0") && drawn[3000].contains("message 2999"));
    }

    #[test]
    fn past_the_limit_the_oldest_go_but_the_header_stays_and_a_line_says_so() {
        let mut h = History::default();
        h.push(Block::Lines(vec!["the header".into()]), 1, 10);
        for i in 0..100 {
            h.push(Block::User(format!("message {i}")), 1, 10);
        }
        let drawn: Vec<String> = h.render(80).iter().map(|l| strip(l)).collect();
        assert_eq!(drawn[0], "the header");
        assert!(drawn[1].contains("91 earlier lines are no longer shown"), "{}", drawn[1]);
        assert!(drawn[3].contains("message 91"), "{}", drawn[3]);
        assert!(drawn.last().unwrap().contains("message 99"));
        assert_eq!(h.lines, 10);

        // Without a header the oldest block is simply the first one.
        let mut h = History::default();
        for i in 0..100 {
            h.push(Block::User(format!("message {i}")), 1, 10);
        }
        assert_eq!(h.blocks.len(), 10);
        h.clear();
        assert_eq!((h.lines, h.dropped, h.render(80).len()), (0, 0, 0));
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

    use super::super::text::strip;

    fn drawn(h: &History, cols: usize) -> Vec<String> {
        h.render(cols).iter().map(|l| strip(l)).collect()
    }

    fn text(line: &str, lead: bool) -> Block {
        Block::Text { line: line.into(), lead, md: Markdown::default(), more: false }
    }

    #[test]
    fn every_kind_of_block_is_drawn() {
        let mut h = History::default();
        h.push(Block::Lines(vec!["header 1".into(), "header 2".into()]), 2, 100);
        h.push(Block::Blank, 1, 100);
        h.push(Block::User("fix the bug".into()), 1, 100);
        h.push(Block::Thinking("looking at it".into()), 1, 100);
        h.push(text("Sure.", true), 1, 100);
        h.push(text("More text.", false), 1, 100);
        h.push(Block::Note { msg: "heads up".into(), style: "" }, 1, 100);
        let diff = Diff { line: 3, old: "a\nb".into(), new: "a\nc".into() };
        h.push(Block::Tool { label: "Edit(a.rs)".into(), diff: Some(diff) }, 3, 100);
        h.push(Block::Tool { label: "Bash(ls)".into(), diff: None }, 1, 100);
        h.push(Block::Result { ok: true, summary: "done".into() }, 1, 100);
        h.push(Block::Result { ok: false, summary: "boom".into() }, 1, 100);
        assert_eq!(
            drawn(&h, 40),
            [
                "header 1",
                "header 2",
                "",
                "> fix the bug",
                "  looking at it",
                "\u{25cf} Sure.",
                "  More text.",
                "  heads up",
                "\u{25cf} Edit(a.rs)",
                "    4 - b",
                "    4 + c",
                "\u{25cf} Bash(ls)",
                "  \u{23bf} done",
                "  \u{23bf} boom",
            ]
        );
    }

    #[test]
    fn a_table_is_drawn_boxed_or_as_plain_rows_when_narrow() {
        let rows: Vec<String> = ["| a | b |", "|---|---|", "| 1 | 2 |"].iter().map(|s| s.to_string()).collect();
        let mut h = History::default();
        h.push(Block::Table { rows: rows.clone(), lead: true, md: Markdown::default() }, 7, 100);
        let wide = drawn(&h, 40);
        assert!(wide[0].starts_with("\u{25cf} \u{250c}"), "{wide:?}");
        assert!(wide.iter().any(|l| l.contains("\u{2502} 1 \u{2502} 2")), "{wide:?}");

        let narrow = drawn(&h, 6);
        assert!(narrow.iter().all(|l| !l.contains('\u{250c}')), "{narrow:?}");
        assert!(narrow[0].starts_with("\u{25cf} "), "only the first row has the marker: {narrow:?}");
        assert!(narrow[1..].iter().all(|l| l.starts_with("  ")));
        let joined: String = narrow.iter().map(|l| l.trim()).collect::<Vec<_>>().join("");
        assert!(
            joined.contains("| a | b |") || joined.contains("|a|b|") || joined.replace(' ', "").contains("|a|b|"),
            "{narrow:?}"
        );
    }

    #[test]
    fn code_blocks_keep_their_state_between_lines() {
        let mut h = History::default();
        let open = Markdown::default();
        let inside = open.after("```rust");
        h.push(Block::Text { line: "let x = 1;".into(), lead: false, md: inside, more: false }, 1, 100);
        h.push(text("let x = 1;", false), 1, 100);
        let lines = h.render(40);
        assert!(lines[0].contains("\x1b[36m"), "inside a fence the line is code-coloured");
        assert!(!lines[1].contains("\x1b[36m"));
    }

    #[test]
    fn clearing_and_unmerged_pieces() {
        let mut h = History::default();
        h.push(text("one", true), 1, 100);
        // A piece that isn't marked as continuing stays separate, and so does one with its own marker.
        h.push(text("two", false), 1, 100);
        let streaming = Block::Text { line: "start".into(), lead: true, md: Markdown::default(), more: true };
        h.push(streaming, 1, 100);
        h.push(text("new paragraph", true), 1, 100);
        assert_eq!(h.blocks.len(), 4);
        assert!(h.take_partial().is_none(), "the last block is complete");
        h.push(Block::Blank, 1, 100);
        assert!(h.take_partial().is_none(), "a blank line is not a partial paragraph");
        h.clear();
        assert!(h.render(40).is_empty());
        assert_eq!(h.lines, 0);
        assert!(h.take_partial().is_none());
    }
}
