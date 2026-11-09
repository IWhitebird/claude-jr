// Markdown tables drawn with box characters and aligned columns. Rows come
// in one line at a time, so the UI collects them until the table ends.

use super::text::{BOLD, GREY, RESET, Span, span, width, wrap};

/// The narrowest a column gets when the table is squeezed to fit.
const MIN_COL: usize = 3;

pub fn is_row(line: &str) -> bool {
    line.trim_start().starts_with('|')
}

/// The `|---|:--:|` line under the header.
fn is_separator(line: &str) -> bool {
    line.contains('-') && line.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ' | '\t'))
}

#[derive(Clone, Copy, PartialEq)]
enum Align {
    Left,
    Right,
    Center,
}

/// Column alignment from the separator row: `:--` left, `--:` right, `:-:` center.
fn aligns(rows: &[String], n: usize) -> Vec<Align> {
    let Some(sep) = rows.iter().find(|r| is_separator(r)) else { return vec![Align::Left; n] };
    let mut a: Vec<Align> = cells(sep)
        .iter()
        .map(|c| match (c.starts_with(':'), c.ends_with(':')) {
            (true, true) => Align::Center,
            (false, true) => Align::Right,
            _ => Align::Left,
        })
        .collect();
    a.resize(n, Align::Left);
    a
}

/// Cells of one row, with `code` and **bold** markers removed.
fn cells(line: &str) -> Vec<String> {
    let line = line.trim();
    let line = line.strip_prefix('|').unwrap_or(line);
    let line = line.strip_suffix('|').unwrap_or(line);
    line.split('|').map(|c| c.trim().replace("**", "").replace('`', "")).collect()
}

/// Draws the table. Returns false (drawing nothing) if it can't fit, so the
/// caller can show the rows as plain text instead.
pub fn render(rows: &[String], cols: usize, lead: Span, out: &mut Vec<String>) -> bool {
    let mut table: Vec<Vec<String>> = rows.iter().filter(|r| !is_separator(r)).map(|r| cells(r)).collect();
    let n = table.iter().map(Vec::len).max().unwrap_or(0);
    if n == 0 {
        return false;
    }
    for row in &mut table {
        row.resize(n, String::new());
    }
    let aligns = aligns(rows, n);

    // Each column as wide as its widest cell, then the widest ones trimmed
    // until the table fits: 2 for the lead, │ between and around, 1 space of
    // padding on each side of each cell.
    let mut widths: Vec<usize> = (0..n).map(|i| table.iter().map(|r| width(&r[i])).max().unwrap_or(0).max(1)).collect();
    let room = cols.saturating_sub(2 + (n + 1) + 2 * n);
    if room < n * MIN_COL {
        return false;
    }
    while widths.iter().sum::<usize>() > room {
        let widest = (0..n).max_by_key(|&i| widths[i]).unwrap_or(0);
        widths[widest] -= 1;
    }

    let border = |l: &str, m: &str, r: &str| {
        let parts: Vec<String> = widths.iter().map(|w| "─".repeat(w + 2)).collect();
        format!("{GREY}{l}{}{r}{RESET}", parts.join(m))
    };
    let mut lines = vec![border("┌", "┬", "┐")];
    for (i, row) in table.iter().enumerate() {
        let style = if i == 0 { BOLD } else { "" };
        // Each cell wrapped to its column; the row is as tall as its tallest cell.
        let wrapped: Vec<Vec<String>> = row
            .iter()
            .zip(&widths)
            .map(|(cell, &w)| {
                let mut out = Vec::new();
                wrap(&[span(cell, "")], w, span("", ""), span("", ""), &mut out);
                out
            })
            .collect();
        let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
        for k in 0..height {
            let mut line = format!("{GREY}│{RESET}");
            for ((cell, &w), &align) in wrapped.iter().zip(&widths).zip(&aligns) {
                let text = cell.get(k).map_or("", String::as_str);
                let pad = w.saturating_sub(width(text));
                // The header row follows its column's alignment too.
                let (left, right) = match align {
                    Align::Left => (0, pad),
                    Align::Right => (pad, 0),
                    Align::Center => (pad / 2, pad - pad / 2),
                };
                line.push_str(&format!(
                    " {}{style}{text}{RESET}{} {GREY}│{RESET}",
                    " ".repeat(left),
                    " ".repeat(right)
                ));
            }
            lines.push(line);
        }
        if i == 0 && table.len() > 1 {
            lines.push(border("├", "┼", "┤"));
        }
    }
    lines.push(border("└", "┴", "┘"));

    for (i, line) in lines.into_iter().enumerate() {
        let prefix = if i == 0 { format!("{}{}{RESET}", lead.style, lead.text) } else { "  ".to_string() };
        out.push(format!("{prefix}{line}"));
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(s: &str) -> String {
        let mut out = String::new();
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

    #[test]
    fn draws_aligned_box() {
        let rows =
            ["| name | size |", "|---|---:|", "| **claude-jr** | `2.8 MB` |", "| pi | 109 MB |"].map(String::from);
        // "size" is right-aligned by the `---:` in the separator row.
        let mut out = Vec::new();
        assert!(render(&rows, 80, span("  ", ""), &mut out));
        let lines: Vec<String> = out.iter().map(|l| plain(l)).collect();
        assert_eq!(
            lines,
            [
                "  ┌───────────┬────────┐",
                "  │ name      │   size │",
                "  ├───────────┼────────┤",
                "  │ claude-jr │ 2.8 MB │",
                "  │ pi        │ 109 MB │",
                "  └───────────┴────────┘",
            ]
        );
    }

    #[test]
    fn wraps_cells_to_fit() {
        let rows = ["| a | b |", "|---|---|", "| short | a much longer cell that will not fit |"].map(String::from);
        let mut out = Vec::new();
        assert!(render(&rows, 30, span("  ", ""), &mut out));
        assert!(out.iter().all(|l| width(&plain(l)) <= 30));
        assert!(out.len() > 6, "the long cell wraps onto more lines");
        assert!(!render(&rows, 10, span("  ", ""), &mut Vec::new()), "too narrow: give up");
    }
}
