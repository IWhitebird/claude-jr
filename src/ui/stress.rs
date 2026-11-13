// Random input against the editor and everything that draws: whatever text
// arrives and however narrow the terminal is, nothing may panic, no drawn
// line may be wider than the terminal, and no control character may reach it.
// Seeds are fixed, so a failure can be run again.

use super::history::{Block, History};
use super::input::Input;
use super::md::Markdown;
use super::text::{self, span};
use super::{table, view};
use crate::tools::Diff;

/// xorshift: small, fast and the same on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<'a>(&mut self, from: &[&'a str]) -> &'a str {
        from[self.below(from.len())]
    }

    /// Text made of the pieces that tend to break layout code.
    fn text(&mut self, max_pieces: usize) -> String {
        const PIECES: &[&str] = &[
            "a",
            "word",
            "longerword",
            " ",
            "  ",
            "\n",
            "\t",
            "漢字",
            "日本語のテキスト",
            "é",
            "e\u{301}",
            "👍",
            "👨‍👩‍👧‍👦",
            "🇮🇳",
            "\u{200b}",
            "\u{200d}",
            "\u{feff}",
            "\x1b[31m",
            "\x1b]0;title\x07",
            "\r",
            "\x07",
            "\x00",
            "\x7f",
            "**bold**",
            "`code`",
            "```",
            "```rust",
            "# Heading",
            "- item",
            "1. item",
            "> quote",
            "| a | b |",
            "|---|---|",
            "[link](http://example.com/a/very/long/path)",
            "~~~",
            "---",
            "<tag>",
            "&amp;",
            "\\",
            "*",
            "_",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ｆｕｌｌｗｉｄｔｈ",
            "العربية",
            "\u{202e}",
            "/path/to/file.rs:12",
            "@@",
            "--flag=value",
        ];
        let mut s = String::new();
        for _ in 0..self.below(max_pieces + 1) {
            s.push_str(self.pick(PIECES));
        }
        s
    }

    fn cols(&mut self) -> usize {
        match self.below(4) {
            0 => 1 + self.below(12),
            1 => 20 + self.below(30),
            _ => 1 + self.below(200),
        }
    }
}

/// A drawn line without the color codes we write (ESC [ numbers m). Anything
/// else that starts with ESC stays, so `check` sees it.
fn plain(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(at) = rest.find("\x1b[") {
        let code = &rest[at + 2..];
        let len = code.find(|c: char| !(c.is_ascii_digit() || c == ';')).unwrap_or(code.len());
        if code[len..].starts_with('m') {
            out.push_str(&rest[..at]);
            rest = &code[len + 1..];
        } else {
            out.push_str(&rest[..at + 2]);
            rest = code;
        }
    }
    out + rest
}

/// Every line fits the terminal and holds no control characters other than
/// our own color codes.
#[track_caller]
fn check(what: &str, lines: &[String], cols: usize, source: &str) {
    for line in lines {
        let plain = plain(line);
        assert!(
            text::width(&plain) <= cols,
            "{what}: a line {} wide at {cols} columns\nline: {plain:?}\nfrom: {source:?}",
            text::width(&plain)
        );
        assert!(
            !plain.chars().any(|c| c.is_control()),
            "{what}: a control character reached the screen\nline: {plain:?}\nfrom: {source:?}"
        );
    }
}

#[test]
fn the_editor_survives_any_keys() {
    let mut rng = Rng(0x9E3779B97F4A7C15);
    for round in 0..300 {
        let mut input = Input::default();
        for step in 0..200 {
            let cols = rng.cols();
            match rng.below(16) {
                0..=4 => input.insert(&rng.text(4)),
                5 => input.left(),
                6 => input.right(),
                7 => input.backspace(),
                8 => input.delete(),
                9 => input.home(),
                10 => input.end(),
                11 => input.kill_to_start(),
                12 => input.kill_to_end(),
                13 => input.delete_word(),
                14 => input.up(cols),
                _ => input.down(cols),
            }
            let (rows, (row, col)) = input.layout(cols);
            let where_ = format!("round {round} step {step}, text {:?}", input.text());
            assert!(row < rows.len().max(1) + 1, "the cursor's row is past the text: {where_}");
            // (A double-width character can't fit a one-column row at all.)
            assert!(col <= cols.max(2), "the cursor's column {col} is past {cols} columns: {where_}");
            for r in &rows {
                assert!(input.text().get(r.clone()).is_some(), "a row cuts a character in half: {where_}");
            }
            let mut out = Vec::new();
            let width = cols + 6;
            let (line, column) = view::input_box(&input, "ask anything", width, &mut out);
            check("input box", &out, width, input.text());
            assert!(line < out.len(), "the cursor is below the box: {where_}");
            assert!(column <= width, "the cursor is right of the box: {where_}");
        }
        // Sending it and going back through history must work on anything.
        let sent = input.take();
        input.up(40);
        input.down(40);
        let _ = (sent, input.masked().text().len());
    }
}

#[test]
fn everything_drawn_fits_the_terminal() {
    let mut rng = Rng(0xD1B54A32D192ED03);
    for _ in 0..4000 {
        let cols = rng.cols();
        let s = rng.text(12);
        let mut out = Vec::new();
        let mut draw = |what: &str, f: &dyn Fn(&mut Vec<String>)| {
            out.clear();
            f(&mut out);
            check(what, &out, cols, &s);
        };
        draw("user", &|o| view::user(&s, cols, o));
        draw("thinking", &|o| view::thinking(&s, cols, o));
        draw("tool", &|o| view::tool(&s, cols, o));
        draw("result", &|o| view::result(true, &s, cols, o));
        draw("note", &|o| view::note(&s, text::DIM, cols, o));
        draw("approve", &|o| view::approve(&s, cols, o));
        draw("question", &|o| view::question(&s, &[s.clone(), "two".into()], cols, o));
        draw("picker", &|o| view::picker(&s, &[s.clone(), "two".into()], &s, cols, o));
        draw("menu", &|o| view::menu(&[("/one", s.as_str()), (s.as_str(), "two")], 1, 8, cols, o));
        draw("key prompt", &|o| view::key_prompt(cols, o));
        draw("status", &|o| o.push(view::status(&s, std::time::Duration::from_secs(75), 1234, cols)));
        draw("footer", &|o| o.push(view::footer(&[span(&s, ""), span("mode", text::DIM)], &s, cols)));
        let d = Diff { line: 1 + rng.below(99999), old: rng.text(8), new: rng.text(8) };
        draw("diff", &|o| view::diff(&view::trim_diff(&d), cols, o));
        draw("fit", &|o| o.push(text::fit(&text::clean_line(&s), cols)));
    }
}

#[test]
fn any_markdown_streams_and_redraws_at_any_width() {
    let mut rng = Rng(0x2545F4914F6CDD1D);
    for _ in 0..1500 {
        // Model text is cleaned as it arrives, before it is laid out.
        let source = text::clean(&rng.text(30));
        let cols = rng.cols();
        // As it streams: line by line, tables collected into one block.
        let mut history = History::default();
        let mut md = Markdown::default();
        let mut rows: Vec<String> = Vec::new();
        let mut out = Vec::new();
        let mut first = true;
        for line in source.lines() {
            if table::is_row(line) && !md.in_code() {
                rows.push(line.to_string());
                continue;
            }
            if !rows.is_empty() {
                let block = Block::Table { rows: std::mem::take(&mut rows), lead: first, md };
                block.render(cols, &mut out);
                history.push(block, 1, 100_000);
                first = false;
            }
            let block = Block::Text { line: line.to_string(), lead: first, md, more: false };
            block.render(cols, &mut out);
            history.push(block, 1, 100_000);
            md = md.after(line);
            first = false;
        }
        check("markdown", &out, cols, &source);
        // Then at other widths, as after a resize.
        for _ in 0..3 {
            let cols = rng.cols();
            check("markdown, redrawn", &history.render(cols), cols, &source);
        }
        let mut table_out = Vec::new();
        let lines: Vec<String> = source.lines().map(String::from).collect();
        table::render(&lines, cols, span("● ", text::ORANGE), &mut table_out);
        check("table", &table_out, cols, &source);
    }
}

/// Table rows are kept as they arrived (the columns are worked out from the
/// source), so drawing them has to clean them.
#[test]
fn a_table_of_anything_is_drawn_clean() {
    let mut rng = Rng(0x369DEA0F31A53F85);
    for _ in 0..3000 {
        let rows: Vec<String> = (0..1 + rng.below(5))
            .map(|i| {
                if i == 1 && rng.below(2) == 0 {
                    "|---|---|".to_string()
                } else {
                    format!("| {} | {} |", rng.text(4), rng.text(4))
                }
            })
            .map(|row| row.replace('\n', " "))
            .collect();
        let source = rows.join("\n");
        let cols = rng.cols();
        let mut out = Vec::new();
        Block::Table { rows, lead: true, md: Markdown::default() }.render(cols, &mut out);
        check("table block", &out, cols, &source);
    }
}

#[test]
fn every_block_redraws_at_any_width() {
    let mut rng = Rng(0x94D049BB133111EB);
    for _ in 0..800 {
        let mut history = History::default();
        let mut source = String::new();
        for _ in 0..rng.below(12) {
            let s = rng.text(10);
            source.push_str(&s);
            let block = match rng.below(7) {
                0 => Block::User(s),
                1 => Block::Thinking(s),
                2 => Block::Note { msg: s, style: text::DIM },
                3 => Block::Tool { label: s, diff: Some(Diff { line: 3, old: rng.text(6), new: rng.text(6) }) },
                4 => Block::Result { ok: rng.below(2) == 0, summary: s },
                5 => Block::Text {
                    line: text::clean(&s),
                    lead: rng.below(2) == 0,
                    md: Markdown::default(),
                    more: rng.below(3) == 0,
                },
                _ => Block::Blank,
            };
            history.push(block, 1 + rng.below(3), 50);
        }
        let cols = rng.cols();
        check("history", &history.render(cols), cols, &source);
        let _ = history.take_partial();
        check("history, after taking the partial line", &history.render(cols), cols, &source);
    }
}

#[test]
fn cleaned_text_has_no_control_characters_and_fits_when_cut() {
    let mut rng = Rng(0xBF58476D1CE4E5B9);
    for _ in 0..5000 {
        let s = rng.text(10);
        let cleaned = text::clean(&s);
        assert!(!cleaned.chars().any(|c| c.is_control() && c != '\n'), "{cleaned:?} from {s:?}");
        let line = text::clean_line(&s);
        assert!(!line.chars().any(|c| c.is_control()), "{line:?} from {s:?}");
        let max = rng.below(60);
        assert!(text::width(&text::fit(&line, max)) <= max, "fit({line:?}, {max})");
        let styled = format!("{}{line}{}", text::DIM, text::RESET);
        assert!(text::width(&text::strip(&text::fit_styled(&styled, max))) <= max, "fit_styled({line:?}, {max})");
        assert_eq!(text::strip(&text::strip(&s)), text::strip(&s), "stripping twice changes nothing");
    }
}
