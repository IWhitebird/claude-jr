// Turns conversation pieces into terminal lines.

use super::input::Input;
use super::text::*;
use crate::tools::Diff;

const MAX_DIFF_LINES: usize = 30;
const MAX_INPUT_ROWS: usize = 8;

pub fn user(text: &str, cols: usize, out: &mut Vec<String>) {
    let text = clean(text);
    for (i, line) in text.lines().enumerate() {
        let lead = if i == 0 { span("> ", GREY) } else { span("  ", "") };
        wrap(&[span(line, GREY)], cols, lead, span("  ", ""), out);
    }
}

pub fn thinking(text: &str, cols: usize, out: &mut Vec<String>) {
    wrap(&[span(text, ITALIC_DIM)], cols, span("  ", ""), span("  ", ""), out);
}

pub fn tool(label: &str, cols: usize, out: &mut Vec<String>) {
    out.push(format!("{GREEN}●{RESET} {BOLD}{}{RESET}", fit(&clean(label), cols.saturating_sub(2))));
}

pub fn result(ok: bool, summary: &str, cols: usize, out: &mut Vec<String>) {
    let style = if ok { GREY } else { RED };
    let text = clean(summary);
    for (i, line) in text.lines().enumerate() {
        let lead = if i == 0 { "  ⎿ " } else { "    " };
        out.push(format!("{GREY}{lead}{RESET}{style}{}{RESET}", fit(line, cols.saturating_sub(4))));
    }
}

pub fn note(text: &str, style: &'static str, cols: usize, out: &mut Vec<String>) {
    for line in text.lines() {
        wrap(&[span(line, style)], cols, span("  ", ""), span("  ", ""), out);
    }
}

/// The changed lines between `old` and `new`, with line numbers. Lines the two
/// share at the start and end are left out.
pub fn diff(d: &Diff, cols: usize, out: &mut Vec<String>) {
    let old: Vec<&str> = d.old.lines().collect();
    let new: Vec<&str> = d.new.lines().collect();
    let same_start = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let same_end =
        old[same_start..].iter().rev().zip(new[same_start..].iter().rev()).take_while(|(a, b)| a == b).count();
    let removed = &old[same_start..old.len() - same_end];
    let added = &new[same_start..new.len() - same_end];
    let first = d.line + same_start;
    let num_w = (first + removed.len().max(added.len())).to_string().len();
    let text_w = cols.saturating_sub(num_w + 9);

    let rows = removed.iter().map(|l| (l, '-', RED)).chain(added.iter().map(|l| (l, '+', GREEN)));
    let total = removed.len() + added.len();
    let mut n = [first, first];
    for (line, sign, color) in rows.take(MAX_DIFF_LINES) {
        let k = usize::from(sign == '+');
        out.push(format!("    {GREY}{:>num_w$}{RESET} {color}{sign} {}{RESET}", n[k], fit(&clean(line), text_w)));
        n[k] += 1;
    }
    if total > MAX_DIFF_LINES {
        out.push(format!("    {GREY}… +{} lines{RESET}", total - MAX_DIFF_LINES));
    }
}

/// A box around the input. Returns the cursor as (line index in `out`, column).
pub fn input_box(input: &Input, hint: &str, cols: usize, out: &mut Vec<String>) -> (usize, usize) {
    let inner = cols.saturating_sub(4).max(1);
    let text_w = inner.saturating_sub(2).max(1);
    let (rows, (cur_row, cur_col)) = input.layout(text_w);
    let first = cur_row.saturating_sub(MAX_INPUT_ROWS - 1);
    let top = out.len();

    out.push(format!("{GREY}╭{}╮{RESET}", "─".repeat(cols.saturating_sub(2))));
    if input.is_empty() {
        let hint = fit(hint, text_w);
        let pad = text_w.saturating_sub(width(&hint));
        out.push(format!("{GREY}│{RESET} > {DIM}{hint}{RESET}{} {GREY}│{RESET}", " ".repeat(pad)));
    } else {
        for (i, r) in rows.iter().enumerate().skip(first).take(MAX_INPUT_ROWS) {
            let text = &input.text()[r.clone()];
            let pad = text_w.saturating_sub(width(text));
            let lead = if i == 0 { "> " } else { "  " };
            out.push(format!("{GREY}│{RESET} {lead}{text}{} {GREY}│{RESET}", " ".repeat(pad)));
        }
    }
    out.push(format!("{GREY}╰{}╯{RESET}", "─".repeat(cols.saturating_sub(2))));
    (top + 1 + cur_row - first, 4 + cur_col)
}

pub fn ask(label: &str, cols: usize, out: &mut Vec<String>) {
    let inner = cols.saturating_sub(4);
    let question = fit(&format!("Allow {}?", clean(label)), inner);
    let keys = fit("y yes · a always (this session) · n no", inner);
    out.push(format!("{YELLOW}╭{}╮{RESET}", "─".repeat(cols.saturating_sub(2))));
    for (text, style) in [(question, BOLD), (keys, DIM)] {
        let pad = inner.saturating_sub(width(&text));
        out.push(format!("{YELLOW}│{RESET} {style}{text}{RESET}{} {YELLOW}│{RESET}", " ".repeat(pad)));
    }
    out.push(format!("{YELLOW}╰{}╯{RESET}", "─".repeat(cols.saturating_sub(2))));
}

pub fn status(activity: &str, secs: u64, millis: u128, cols: usize) -> String {
    const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    let frame = SPINNER[(millis / 100) as usize % SPINNER.len()];
    let tail = format!(" ({secs}s · esc to cancel)");
    let act = fit(&clean(activity), cols.saturating_sub(width(&tail) + 4));
    format!("{CYAN}{frame}{RESET} {act}…{DIM}{tail}{RESET}")
}

/// One line: `left` dimmed on the left, `right` dimmed on the right.
pub fn footer(left: &str, right: &str, cols: usize) -> String {
    let right = fit(right, cols.saturating_sub(2));
    let left = fit(left, cols.saturating_sub(width(&right) + 3));
    let pad = cols.saturating_sub(width(&left) + width(&right) + 2);
    format!(" {DIM}{left}{}{right}{RESET}", " ".repeat(pad))
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
    fn diff_skips_shared_lines() {
        let d = Diff { line: 10, old: "a\nb\nc\nd".into(), new: "a\nB\nC\nd".into() };
        let mut out = Vec::new();
        diff(&d, 40, &mut out);
        let lines: Vec<_> = out.iter().map(|l| plain(l)).collect();
        assert_eq!(lines, ["    11 - b", "    12 - c", "    11 + B", "    12 + C"]);
    }

    #[test]
    fn input_box_cursor() {
        let mut input = Input::default();
        input.insert("hi");
        let mut out = vec!["above".to_string()];
        let cursor = input_box(&input, "", 20, &mut out);
        assert_eq!(cursor, (2, 6));
        assert_eq!(plain(&out[2]), "│ > hi             │");
        assert!(out.iter().skip(1).all(|l| width(&plain(l)) == 20));
    }
}
