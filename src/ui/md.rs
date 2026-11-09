// Markdown rendered one line at a time, as it streams in. The only state that
// crosses lines is whether we're inside a code fence.

use super::text::{BOLD, CYAN, DIM, ITALIC_DIM, Span, span, width, wrap};

const CODE: &str = CYAN;

#[derive(Clone, Copy, Default)]
pub struct Markdown {
    in_code: bool,
}

impl Markdown {
    /// Whether `raw` would render as a plain paragraph: not code, a heading,
    /// a quote, a list item or a rule. Only those can be committed while
    /// they're still streaming, because their later rows look the same as a
    /// paragraph of their own.
    pub fn is_paragraph(&self, raw: &str) -> bool {
        let t = raw.trim_start();
        !self.in_code
            && raw.len() == t.len()
            && !t.starts_with("```")
            && !matches!(t, "---" | "***" | "___")
            && heading(t).is_none()
            && !t.starts_with('>')
            && !t.starts_with('|')
            && list_item(t).is_none()
    }

    /// The state after `raw`: the same, unless it opens or closes a code block.
    pub fn after(self, raw: &str) -> Markdown {
        let fence = raw.trim_start().starts_with("```");
        Markdown { in_code: self.in_code != fence }
    }

    pub fn in_code(&self) -> bool {
        self.in_code
    }

    pub fn reset(&mut self) {
        self.in_code = false;
    }

    /// Renders one source line. `lead` (two columns) starts the first row.
    pub fn line(&mut self, raw: &str, cols: usize, lead: Span, out: &mut Vec<String>) {
        let trimmed = raw.trim_start();
        let indent = raw.len() - trimmed.len();

        if trimmed.starts_with("```") {
            self.in_code = !self.in_code;
            return;
        }
        if self.in_code {
            let code = raw.replace('\t', "    ");
            return wrap(&[span(&code, CODE)], cols, lead, span("  ", ""), out);
        }
        if matches!(trimmed, "---" | "***" | "___") {
            let rule = "─".repeat(cols.saturating_sub(2).min(40));
            return wrap(&[span(&rule, DIM)], cols, lead, span("  ", ""), out);
        }

        let mut spans = Vec::new();
        if let Some(h) = heading(trimmed) {
            inline(h, BOLD, &mut spans);
            return wrap(&spans, cols, lead, span("  ", ""), out);
        }
        if let Some(q) = trimmed.strip_prefix("> ").or((trimmed == ">").then_some("")) {
            inline(q, ITALIC_DIM, &mut spans);
            let first = format!("{}│ ", lead.text);
            return wrap(&spans, cols, span(&first, DIM), span("  │ ", DIM), out);
        }
        if let Some((marker, body)) = list_item(trimmed) {
            inline(body, "", &mut spans);
            let first = format!("{}{}{marker}", lead.text, " ".repeat(indent));
            let rest = " ".repeat(width(&first));
            return wrap(&spans, cols, span(&first, lead.style), span(&rest, ""), out);
        }
        inline(raw, "", &mut spans);
        wrap(&spans, cols, lead, span("  ", ""), out);
    }
}

fn heading(s: &str) -> Option<&str> {
    let hashes = s.bytes().take_while(|&b| b == b'#').count();
    (1..=6).contains(&hashes).then(|| s[hashes..].strip_prefix(' ')).flatten()
}

/// `- x`, `* x`, `+ x` become bullets; `1. x` keeps its number.
fn list_item(s: &str) -> Option<(&str, &str)> {
    for m in ["- ", "* ", "+ "] {
        if let Some(body) = s.strip_prefix(m) {
            return Some(("• ", body));
        }
    }
    let digits = s.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 && s[digits..].starts_with(". ") {
        return Some((&s[..digits + 2], &s[digits + 2..]));
    }
    None
}

/// `code` and **bold**; everything else is plain text in `base` style.
fn inline<'a>(s: &'a str, base: &'static str, out: &mut Vec<Span<'a>>) {
    let mut rest = s;
    while let Some(i) = rest.find(['`', '*']) {
        if i > 0 {
            out.push(span(&rest[..i], base));
        }
        let r = &rest[i..];
        let (open, style) = if r.starts_with("**") {
            ("**", BOLD)
        } else if r.starts_with('`') {
            ("`", CODE)
        } else {
            ("*", "")
        };
        if !style.is_empty()
            && let Some(end) = r[open.len()..].find(open).filter(|&e| e > 0)
        {
            out.push(span(&r[open.len()..open.len() + end], style));
            rest = &r[2 * open.len() + end..];
            continue;
        }
        out.push(span(&r[..1], base));
        rest = &r[1..];
    }
    if !rest.is_empty() {
        out.push(span(rest, base));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(src: &str) -> Vec<String> {
        let mut md = Markdown::default();
        let mut out = Vec::new();
        for l in src.lines() {
            md.line(l, 40, span("  ", ""), &mut out);
        }
        out
    }

    #[test]
    fn blocks() {
        let out = render("# Title\n- one\n  - two\n3. three\n```rust\nlet x = 1;\n```\n> quote");
        assert_eq!(out[0], format!("  {BOLD}Title\x1b[0m"));
        assert_eq!(out[1], "  • one");
        assert_eq!(out[2], "    • two");
        assert_eq!(out[3], "  3. three");
        assert_eq!(out[4], format!("  {CODE}let x = 1;\x1b[0m"));
        assert!(out[5].contains("│ ") && out[5].contains("quote"));
        assert_eq!(out.len(), 6);
    }

    #[test]
    fn inline_styles() {
        let mut spans = Vec::new();
        inline("use `cargo` and **care** * x", "", &mut spans);
        let parts: Vec<_> = spans.iter().map(|s| (s.text, s.style)).collect();
        assert_eq!(
            parts,
            [("use ", ""), ("cargo", CODE), (" and ", ""), ("care", BOLD), (" ", ""), ("*", ""), (" x", "")]
        );
    }

    fn plain_lines(src: &str, cols: usize) -> Vec<String> {
        let mut md = Markdown::default();
        let mut out = Vec::new();
        for l in src.lines() {
            md.line(l, cols, span("  ", ""), &mut out);
        }
        out.iter().map(|l| super::super::text::strip(l)).collect()
    }

    #[test]
    fn only_plain_paragraphs_can_be_committed_while_streaming() {
        let md = Markdown::default();
        for yes in
            ["Just a sentence.", "Mentions `code` and **bold** inline", "1.5 is a number, not a list", "#nospace"]
        {
            assert!(md.is_paragraph(yes), "{yes}");
        }
        for no in [
            "# Heading",
            "###### small",
            "- item",
            "* item",
            "+ item",
            "12. item",
            "> quote",
            ">",
            "| a | b |",
            "```rust",
            "---",
            "***",
            "___",
            "  indented",
            "\tindented",
        ] {
            assert!(!md.is_paragraph(no), "{no}");
        }
        // Inside a code block nothing is a paragraph.
        assert!(!md.after("```").is_paragraph("plain text"));
        assert!(md.after("```").after("```").is_paragraph("plain text"));
    }

    #[test]
    fn code_fences_toggle_the_state() {
        let md = Markdown::default();
        assert!(!md.in_code());
        let open = md.after("  ```python");
        assert!(open.in_code());
        assert!(open.after("not a fence").in_code());
        assert!(!open.after("```").in_code());
        let mut m = open;
        m.reset();
        assert!(!m.in_code());
    }

    #[test]
    fn code_keeps_its_indent_and_expands_tabs() {
        let out = plain_lines("```\n\tif x {\n        y\n}\n```\nafter", 40);
        assert_eq!(out, ["      if x {", "          y", "  }", "  after"]);
    }

    #[test]
    fn rules_headings_and_quotes() {
        let out = plain_lines("---\n***\n___", 100);
        let rule = format!("  {}", "\u{2500}".repeat(40));
        assert_eq!(out, [rule.clone(), rule.clone(), rule]);
        assert_eq!(
            plain_lines("---", 12),
            [format!("  {}", "\u{2500}".repeat(10))],
            "narrow terminals get a shorter rule"
        );

        let out = plain_lines("###### six\n####### seven\n#none\n## ", 40);
        assert_eq!(out, ["  six", "  ####### seven", "  #none", "  "]);

        let out = plain_lines("> quoted\n>\n> **bold** text", 40);
        assert_eq!(out, ["  \u{2502} quoted", "  \u{2502} ", "  \u{2502} bold text"]);
    }

    #[test]
    fn lists_wrap_under_their_text() {
        let out = plain_lines(
            "- a long item that needs to wrap around the edge\n10. numbered with two digits that wraps too",
            20,
        );
        assert_eq!(
            out,
            [
                "  \u{2022} a long item that",
                "    needs to wrap",
                "    around the edge",
                "  10. numbered with",
                "      two digits",
                "      that wraps too",
            ]
        );
        assert_eq!(plain_lines("   * indented", 40), ["     \u{2022} indented"]);
        assert_eq!(plain_lines("+ plus", 40), ["  \u{2022} plus"]);
    }

    #[test]
    fn unfinished_inline_marks_stay_as_text() {
        let spans = |s: &str| {
            let mut out = Vec::new();
            inline(s, "", &mut out);
            out.iter().map(|x| (x.text.to_string(), x.style)).collect::<Vec<_>>()
        };
        assert_eq!(spans("a `b"), [("a ".to_string(), ""), ("`".to_string(), ""), ("b".to_string(), "")]);
        assert_eq!(spans("**open"), [("*".to_string(), ""), ("*".to_string(), ""), ("open".to_string(), "")]);
        assert_eq!(spans("``"), [("`".to_string(), ""), ("`".to_string(), "")], "empty code is not code");
        assert_eq!(spans("plain"), [("plain".to_string(), "")]);
        assert_eq!(spans("**a** and `b`").len(), 3);
        // The base style applies to the plain parts only.
        let mut out = Vec::new();
        inline("x `y`", DIM, &mut out);
        assert_eq!((out[0].style, out[1].style), (DIM, CODE));
    }
}
