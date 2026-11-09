// Turns conversation pieces into terminal lines.

use super::input::Input;
use super::text::*;
use crate::tools::Diff;
use std::time::Duration;

const MAX_DIFF_LINES: usize = 30;
const MAX_INPUT_ROWS: usize = 8;

pub fn user(text: &str, cols: usize, out: &mut Vec<String>) {
    let text = clean(text);
    for (i, line) in text.lines().enumerate() {
        let lead = if i == 0 { span("> ", ORANGE) } else { span("  ", "") };
        wrap(&[span(line, GREY)], cols, lead, span("  ", ""), out);
    }
}

pub fn thinking(text: &str, cols: usize, out: &mut Vec<String>) {
    wrap(&[span(text, ITALIC_DIM)], cols, span("  ", ""), span("  ", ""), out);
}

pub fn tool(label: &str, cols: usize, out: &mut Vec<String>) {
    out.push(format!("{ORANGE}●{RESET} {BOLD}{}{RESET}", fit(&clean(label), cols.saturating_sub(2))));
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

/// Just the changed part of an edit, for keeping in the redraw history:
/// shared lines at the start and end dropped, and at most KEEP_DIFF_LINES on
/// each side (more than `diff` shows, so its "+N lines" count stays right).
pub fn trim_diff(d: &Diff) -> Diff {
    let old: Vec<&str> = d.old.lines().collect();
    let new: Vec<&str> = d.new.lines().collect();
    let start = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let end = old[start..].iter().rev().zip(new[start..].iter().rev()).take_while(|(a, b)| a == b).count();
    const KEEP_DIFF_LINES: usize = 200;
    let keep = |lines: &[&str]| lines.iter().take(KEEP_DIFF_LINES).map(|l| format!("{l}\n")).collect::<String>();
    Diff { line: d.line + start, old: keep(&old[start..old.len() - end]), new: keep(&new[start..new.len() - end]) }
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
        out.push(format!("{GREY}│{RESET} {ORANGE}>{RESET} {DIM}{hint}{RESET}{} {GREY}│{RESET}", " ".repeat(pad)));
    } else {
        for (i, r) in rows.iter().enumerate().skip(first).take(MAX_INPUT_ROWS) {
            let text = &input.text()[r.clone()];
            let pad = text_w.saturating_sub(width(text));
            let lead = if i == 0 { format!("{ORANGE}>{RESET} ") } else { "  ".to_string() };
            out.push(format!("{GREY}│{RESET} {lead}{text}{} {GREY}│{RESET}", " ".repeat(pad)));
        }
    }
    out.push(format!("{GREY}╰{}╯{RESET}", "─".repeat(cols.saturating_sub(2))));
    (top + 1 + cur_row - first, 4 + cur_col)
}

pub fn approve(label: &str, cols: usize, out: &mut Vec<String>) {
    let question = format!("Allow {}?", clean(label));
    boxed(&[(&question, BOLD), ("y yes · a always (this session) · n no", DIM)], cols, out);
}

/// A question from the model, with numbered options.
pub fn question(question: &str, options: &[String], cols: usize, out: &mut Vec<String>) {
    let question = clean(question);
    let numbered: Vec<String> = options.iter().enumerate().map(|(i, o)| format!("{}. {}", i + 1, clean(o))).collect();
    let mut rows: Vec<(&str, &'static str)> = vec![(&question, BOLD)];
    rows.extend(numbered.iter().map(|o| (o.as_str(), "")));
    boxed(&rows, cols, out);
}

/// A numbered list in a box, for pickers (/rewind, /resume).
pub fn picker(title: &str, items: &[String], hint: &str, cols: usize, out: &mut Vec<String>) {
    let numbered: Vec<String> = items.iter().enumerate().map(|(i, t)| format!("{}. {}", i + 1, clean(t))).collect();
    let mut rows: Vec<(&str, &'static str)> = vec![(title, BOLD)];
    rows.extend(numbered.iter().map(|t| (t.as_str(), "")));
    rows.push((hint, DIM));
    boxed(&rows, cols, out);
}

pub fn key_prompt(cols: usize, out: &mut Vec<String>) {
    let path = crate::config::config_dir().join("credentials");
    let where_ = format!("It's saved to {}, readable only by you.", path.display());
    boxed(
        &[
            ("Paste your Anthropic API key", BOLD),
            (&where_, DIM),
            ("Get one at console.anthropic.com · esc to skip", DIM),
        ],
        cols,
        out,
    );
}

/// Text in an orange box, each entry word-wrapped to fit.
fn boxed(rows: &[(&str, &'static str)], cols: usize, out: &mut Vec<String>) {
    let inner = cols.saturating_sub(4).max(4);
    out.push(format!("{ORANGE}╭{}╮{RESET}", "─".repeat(cols.saturating_sub(2))));
    for (text, style) in rows {
        let mut lines = Vec::new();
        wrap(&[span(text, style)], inner, span("", ""), span("", ""), &mut lines);
        for line in lines {
            let pad = inner.saturating_sub(width(&strip(&line)));
            out.push(format!("{ORANGE}│{RESET} {line}{} {ORANGE}│{RESET}", " ".repeat(pad)));
        }
    }
    out.push(format!("{ORANGE}╰{}╯{RESET}", "─".repeat(cols.saturating_sub(2))));
}

/// Drops escape codes, to measure styled text.
fn strip(s: &str) -> String {
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

/// The working line: a breathing dot, what's happening, how long, and a
/// rough count of tokens written so far.
pub fn status(activity: &str, elapsed: Duration, tokens: usize, cols: usize) -> String {
    const BREATH: [char; 4] = ['·', '•', '●', '•'];
    let dot = BREATH[(elapsed.as_millis() / 300) as usize % BREATH.len()];
    let secs = elapsed.as_secs();
    let mut tail = if secs < 60 { format!(" · {secs}s") } else { format!(" · {}m {:02}s", secs / 60, secs % 60) };
    if tokens > 0 {
        tail.push_str(&format!(" · ~{tokens} tokens"));
    }
    let act = fit(&clean(activity), cols.saturating_sub(width(&tail) + 3));
    format!("{ORANGE}{dot} {act}{RESET}{DIM}{tail}{RESET}")
}

/// One line: `left` dimmed on the left, `right` dimmed on the right.
/// One line: styled pieces on the left, `right` dimmed on the right.
pub fn footer(left: &[Span], right: &str, cols: usize) -> String {
    let right = fit(right, cols.saturating_sub(2));
    let mut room = cols.saturating_sub(width(&right) + 3);
    let (mut line, mut used) = (String::from(" "), 1);
    for s in left {
        let text = fit(s.text, room);
        room = room.saturating_sub(width(&text));
        used += width(&text);
        line.push_str(&format!("{}{text}{RESET}", s.style));
    }
    let pad = cols.saturating_sub(used + width(&right) + 1);
    format!("{line}{}{DIM}{right}{RESET}", " ".repeat(pad))
}

/// The mascot as pixel art, from assets/mascot.txt (the same pixels as the
/// README logo). Each text row shows two pixel rows using half blocks.
pub fn mascot() -> Vec<String> {
    const ART: &str = include_str!("../../assets/mascot.txt");
    let rows: Vec<&[u8]> = ART.lines().filter(|l| !l.starts_with('#') && !l.is_empty()).map(str::as_bytes).collect();

    // Crop to the pixels actually drawn.
    let used = |r: &&[u8]| r.iter().any(|&c| c != b'.');
    let (top, bottom) = (rows.iter().position(used).unwrap_or(0), rows.iter().rposition(used).unwrap_or(0));
    let cols = |r: &[u8]| r.iter().position(|&c| c != b'.');
    let left = rows.iter().filter_map(|r| cols(r)).min().unwrap_or(0);
    let right = rows.iter().filter_map(|r| r.iter().rposition(|&c| c != b'.')).max().unwrap_or(0);
    let mut art: Vec<&[u8]> = rows[top..=bottom].iter().map(|r| &r[left..=right]).collect();
    let blank = vec![b'.'; right - left + 1];
    if art.len() % 2 == 1 {
        art.push(&blank);
    }

    let truecolor = std::env::var("COLORTERM").is_ok_and(|c| c == "truecolor" || c == "24bit");
    art.chunks(2)
        .map(|pair| {
            let mut line = String::new();
            for (&t, &b) in pair[0].iter().zip(pair[1]) {
                let (fg, bg) = (pixel(t), pixel(b));
                let code = |rgb: (u8, u8, u8), layer: u8| {
                    if truecolor {
                        format!("{layer}8;2;{};{};{}", rgb.0, rgb.1, rgb.2)
                    } else {
                        format!("{layer}8;5;{}", nearest_256(rgb))
                    }
                };
                match (fg, bg) {
                    (None, None) => line.push(' '),
                    (Some(t), None) => line.push_str(&format!("\x1b[{}m▀{RESET}", code(t, 3))),
                    (None, Some(b)) => line.push_str(&format!("\x1b[{}m▄{RESET}", code(b, 3))),
                    (Some(t), Some(b)) => line.push_str(&format!("\x1b[{};{}m▀{RESET}", code(t, 3), code(b, 4))),
                }
            }
            line
        })
        .collect()
}

/// Colors from assets/logo.svg; None is see-through.
fn pixel(c: u8) -> Option<(u8, u8, u8)> {
    Some(match c {
        b'O' => (0xD9, 0x77, 0x57), // orange
        b'D' => (0xB9, 0x58, 0x3A), // darker orange
        b'K' => (0x2B, 0x21, 0x1C), // eyes, mouth
        b'W' => (0xFF, 0xFF, 0xFF), // eye shine
        b'P' => (0xF2, 0xA4, 0x8B), // cheeks
        b'G' => (0x8D, 0xBF, 0x5A), // leaves
        b's' => (0x5E, 0x8A, 0x3A), // stem
        _ => return None,
    })
}

/// The closest color in the 256-color palette's 6x6x6 cube, for terminals
/// without 24-bit color.
fn nearest_256((r, g, b): (u8, u8, u8)) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let idx = |v: u8| (0..6).min_by_key(|&i| (LEVELS[i] as i32 - v as i32).abs()).unwrap_or(0) as u8;
    16 + 36 * idx(r) + 6 * idx(g) + idx(b)
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
