// Big pastes. A long pasted text would fill the input box, so it is kept
// aside and the box shows a short label instead, such as
// `[Pasted #2: 40 lines, 3104 chars]`. The label is swapped for the text when
// the message is sent.

use std::collections::VecDeque;

/// A paste longer than either of these is shown as a label.
const MAX_LINES: usize = 4;
const MAX_CHARS: usize = 400;
/// How many pastes are remembered; older labels are sent as they read.
const KEEP: usize = 16;

#[derive(Default)]
pub struct Pastes {
    count: usize,
    /// The label and what it stands for, newest last.
    kept: VecDeque<(String, String)>,
}

pub fn is_big(text: &str) -> bool {
    text.lines().count() > MAX_LINES || text.chars().count() > MAX_CHARS
}

impl Pastes {
    /// Keeps `text` and returns the label to show in its place.
    pub fn add(&mut self, text: String) -> String {
        self.count += 1;
        let (lines, chars) = (text.lines().count(), text.chars().count());
        let label = if lines > 1 {
            format!("[Pasted #{}: {lines} lines, {chars} chars]", self.count)
        } else {
            format!("[Pasted #{}: {chars} chars]", self.count)
        };
        self.kept.push_back((label.clone(), text));
        if self.kept.len() > KEEP {
            self.kept.pop_front();
        }
        label
    }

    /// `text` with every label replaced by what was pasted.
    pub fn expand(&self, text: &str) -> String {
        let mut out = text.to_string();
        for (label, pasted) in &self.kept {
            if out.contains(label.as_str()) {
                out = out.replace(label.as_str(), pasted);
            }
        }
        out
    }
}

/// If `before` (the text left of the cursor) ends with a paste label, where
/// the label starts: backspace then removes the whole label at once.
pub fn label_start(before: &str) -> Option<usize> {
    let inside = before.strip_suffix(']')?;
    let start = inside.rfind("[Pasted #")?;
    let body = &inside[start + "[Pasted #".len()..];
    let (number, rest) = body.split_once(": ")?;
    let ok = !number.is_empty()
        && number.bytes().all(|b| b.is_ascii_digit())
        && rest.ends_with(" chars")
        && !rest.contains(['[', ']', '\n']);
    ok.then_some(start)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_pastes_stay_as_they_are() {
        assert!(!is_big("one line"));
        assert!(!is_big("a\nb\nc\nd"));
        assert!(is_big("a\nb\nc\nd\ne"));
        assert!(is_big(&"x".repeat(401)));
    }

    #[test]
    fn labels_stand_for_the_pasted_text() {
        let mut p = Pastes::default();
        let many = "l1\nl2\nl3\nl4\nl5";
        let a = p.add(many.to_string());
        let b = p.add("y".repeat(500));
        assert_eq!(a, "[Pasted #1: 5 lines, 14 chars]");
        assert_eq!(b, "[Pasted #2: 500 chars]");
        let sent = p.expand(&format!("look at {a} and {b} please"));
        assert_eq!(sent, format!("look at {many} and {} please", "y".repeat(500)));
        // A label that was edited no longer stands for anything.
        assert_eq!(p.expand("[Pasted #1: 5 lines]"), "[Pasted #1: 5 lines]");
        assert_eq!(p.expand("no labels"), "no labels");
    }

    #[test]
    fn only_the_newest_are_remembered() {
        let mut p = Pastes::default();
        let first = p.add("a\n".repeat(10));
        for _ in 0..KEEP {
            p.add("b\n".repeat(10));
        }
        assert_eq!(p.expand(&first), first, "too old: sent as it reads");
        assert_eq!(p.kept.len(), KEEP);
    }

    #[test]
    fn finds_a_label_before_the_cursor() {
        assert_eq!(label_start("see [Pasted #12: 5 lines, 14 chars]"), Some(4));
        assert_eq!(label_start("[Pasted #1: 500 chars]"), Some(0));
        assert_eq!(label_start("[Pasted #1: 500 chars] "), None);
        assert_eq!(label_start("an [ordinary] bracket]"), None);
        assert_eq!(label_start("[Pasted #x: 5 chars]"), None);
        assert_eq!(label_start("[Pasted #1: 5 lines,\n3 chars]"), None);
    }
}
