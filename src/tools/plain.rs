// Command output as a terminal would show it, for the model: colours and
// other escape sequences taken out, and a line rewritten with \r (a progress
// bar) or backspaces reduced to what ends up on screen. The raw bytes cost
// tokens and hide what the output says.

/// `s` without escape sequences or overwritten text.
pub fn plain(s: &str) -> String {
    if !s.contains(['\x1b', '\r', '\x08']) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    // The line being written, and where on it the next character lands.
    let mut line: Vec<char> = Vec::new();
    let mut at = 0usize;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => skip_escape(&mut chars),
            '\n' => {
                out.extend(line.drain(..));
                out.push('\n');
                at = 0;
            }
            // \r\n is a line end; a \r on its own goes back to the start.
            '\r' if chars.peek() == Some(&'\n') => {}
            '\r' => at = 0,
            '\x08' => at = at.saturating_sub(1),
            '\t' => put(&mut line, &mut at, c),
            c if c.is_control() => {}
            c => put(&mut line, &mut at, c),
        }
    }
    out.extend(line);
    out
}

fn put(line: &mut Vec<char>, at: &mut usize, c: char) {
    match line.get_mut(*at) {
        Some(old) => *old = c,
        None => line.push(c),
    }
    *at += 1;
}

/// Skips what follows an ESC: a control sequence (ESC [ ... final), a string
/// (ESC ] or ESC P ... ended by BEL or ESC \), or a short escape (ESC ( B).
fn skip_escape(chars: &mut std::iter::Peekable<std::str::Chars>) {
    match chars.next() {
        Some('[') => {
            for c in chars.by_ref() {
                if ('\x40'..='\x7e').contains(&c) {
                    break;
                }
            }
        }
        Some(']' | 'P' | 'X' | '^' | '_') => {
            while let Some(c) = chars.next() {
                if c == '\x07' || (c == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                    break;
                }
            }
        }
        Some(c) if ('\x20'..='\x2f').contains(&c) => {
            // Intermediate characters, then one final one.
            while chars.next_if(|c| ('\x20'..='\x2f').contains(c)).is_some() {}
            chars.next();
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::plain;

    #[test]
    fn output_is_what_a_terminal_would_show() {
        // Plain text, tabs and Unicode pass through untouched.
        assert_eq!(plain("a\tb\nünï\n"), "a\tb\nünï\n");
        // Colours, a title, a cleared screen, the alternate screen, a charset switch.
        let raw = "\x1b[1;31merror\x1b[0m: x\n\x1b]0;title\x07\x1b[2J\x1b[?1049hok \x1b(Bdone\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\";
        assert_eq!(plain(raw), "error: x\nok donelink");
        // A progress bar, rewritten in place; a shorter rewrite leaves the end of the longer.
        assert_eq!(plain("10%\r50%\r100%\ndone\n"), "100%\ndone\n");
        assert_eq!(plain("100%\r50%"), "50%%");
        // Windows line ends, backspaces, other control characters.
        assert_eq!(plain("a\r\nb\r\n"), "a\nb\n");
        assert_eq!(plain("abc\x08\x08XY\x07\x00z"), "aXYz");
        // A sequence cut off at the end, as the cap can leave one.
        assert_eq!(plain("ok\x1b[3"), "ok");
        assert_eq!(plain("ok\x1b"), "ok");
    }
}
