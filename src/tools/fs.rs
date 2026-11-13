// read, write, edit.

use super::{Diff, MAX_BYTES, Output, Tools, def, shorten};
use serde_json::value::RawValue;
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::SystemTime;

const MAX_LINES: usize = 2000;
const MAX_LINE_CHARS: usize = 2000;
/// Larger files are still edited, just without a diff preview.
const MAX_PREVIEW: u64 = 1024 * 1024;
/// An edit holds the whole file in memory, twice.
const MAX_EDIT: u64 = 64 * 1024 * 1024;

pub static READ: LazyLock<Box<RawValue>> = LazyLock::new(|| {
    def(json!({
        "name": "read",
        "description": "Read a file. Text comes back as lines prefixed with their line number and a tab; \
            images (png, jpg, gif, webp) come back as the image itself, so you can look at screenshots and designs. \
            Reads up to 2000 lines or 50 KB; use offset and limit for more.",
        "input_schema": {
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path, absolute or relative to the working directory" },
                "offset": { "type": "integer", "description": "First line to read, starting at 1" },
                "limit": { "type": "integer", "description": "Number of lines to read" }
            },
            "required": ["path"]
        }
    }))
});

pub static WRITE: LazyLock<Box<RawValue>> = LazyLock::new(|| {
    def(json!({
        "name": "write",
        "description": "Create a file or replace its whole content. Creates parent directories. \
            An existing file must be read first. Use edit to change part of a file.",
        "input_schema": {
            "type": "object",
            "properties": { "path": { "type": "string" }, "content": { "type": "string" } },
            "required": ["path", "content"]
        }
    }))
});

pub static EDIT: LazyLock<Box<RawValue>> = LazyLock::new(|| {
    def(json!({
        "name": "edit",
        "description": "Replace exact text in a file. old_string must match exactly once, including whitespace and \
            indentation, unless replace_all is true. Copy it from the file as it is now, without the line number \
            prefixes that read adds.",
        "input_schema": {
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "old_string": { "type": "string" },
                "new_string": { "type": "string" },
                "replace_all": { "type": "boolean" }
            },
            "required": ["path", "old_string", "new_string"]
        }
    }))
});

fn path(t: &Tools, input: &Value) -> Result<PathBuf, String> {
    input["path"].as_str().map(|p| t.resolve(p)).ok_or_else(|| "missing path".into())
}

/// Refuses what isn't a plain file. Opening a pipe waits for someone to
/// write to it, and a device like /dev/zero never ends.
fn plain_file(path: &Path) -> Result<(), String> {
    match fs::metadata(path) {
        Ok(m) if !m.is_file() && !m.is_dir() => {
            Err(format!("{} is a device, pipe or socket, not a file. Use bash if you need it.", path.display()))
        }
        _ => Ok(()),
    }
}

/// Reads one line, keeping at most `keep` bytes of it: a file can be one
/// line many gigabytes long. Returns how many bytes the line had.
fn read_line(r: &mut impl BufRead, keep: usize, line: &mut Vec<u8>) -> std::io::Result<usize> {
    let mut total = 0;
    loop {
        let chunk = r.fill_buf()?;
        if chunk.is_empty() {
            return Ok(total);
        }
        let (used, done) = match chunk.iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (chunk.len(), false),
        };
        let room = keep.saturating_sub(line.len());
        line.extend_from_slice(&chunk[..used.min(room)]);
        r.consume(used);
        total += used;
        if done {
            return Ok(total);
        }
    }
}

fn mtime(p: &Path) -> Option<SystemTime> {
    fs::metadata(p).and_then(|m| m.modified()).ok()
}

fn remember(t: &Tools, path: &Path) {
    t.seen.lock().unwrap().insert(path.to_path_buf(), mtime(path));
}

/// An existing file may only be changed if it was read and hasn't changed since.
fn check_fresh(t: &Tools, path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    match t.seen.lock().unwrap().get(path) {
        None => Err(format!("{} has not been read yet. Read it first.", path.display())),
        Some(seen) if *seen != mtime(path) => {
            Err(format!("{} changed since it was last read. Read it again.", path.display()))
        }
        Some(_) => Ok(()),
    }
}

pub fn preview(t: &Tools, name: &str, input: &Value) -> Option<Diff> {
    let path = t.resolve(input["path"].as_str()?);
    if fs::metadata(&path).is_ok_and(|m| m.len() > MAX_PREVIEW) || plain_file(&path).is_err() {
        return None;
    }
    match name {
        "write" => {
            let old = fs::read_to_string(&path).unwrap_or_default();
            Some(Diff { line: 1, old, new: input["content"].as_str()?.to_string() })
        }
        "edit" => {
            let old = input["old_string"].as_str()?;
            let text = fs::read_to_string(&path).ok()?;
            let at = text.find(old)?;
            let line = text[..at].bytes().filter(|&b| b == b'\n').count() + 1;
            Some(Diff { line, old: old.to_string(), new: input["new_string"].as_str()?.to_string() })
        }
        _ => None,
    }
}

pub fn read(t: &Tools, input: &Value) -> Output {
    let path = path(t, input)?;
    if path.is_dir() {
        return Err(format!("{} is a directory. Use bash ls.", path.display()));
    }
    plain_file(&path)?;
    if crate::images::media_type(&path).is_some() {
        return super::image_output(&path);
    }
    let offset = input["offset"].as_u64().unwrap_or(1).max(1) as usize;
    let limit = input["limit"].as_u64().map_or(MAX_LINES, |n| n as usize).min(MAX_LINES);
    let file = fs::File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut r = BufReader::new(file);

    if r.fill_buf().map_err(|e| e.to_string())?.contains(&0) {
        return Err(format!("{} looks like a binary file", path.display()));
    }

    let mut out = String::new();
    let mut buf = Vec::new();
    let (mut n, mut shown, mut more) = (0, 0, false);
    loop {
        buf.clear();
        // Four bytes a character at most, so nothing shown is cut short by this.
        if read_line(&mut r, MAX_LINE_CHARS * 4 + 2, &mut buf).map_err(|e| e.to_string())? == 0 {
            break;
        }
        n += 1;
        if n < offset {
            continue;
        }
        if shown == limit || out.len() >= MAX_BYTES {
            more = true;
            break;
        }
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim_end_matches(['\n', '\r']);
        let _ = writeln!(out, "{n:>6}\t{}", shorten(line, MAX_LINE_CHARS));
        shown += 1;
    }
    remember(t, &path);

    if n == 0 {
        return Ok("(empty file)".into());
    }
    if shown == 0 {
        return Err(format!("offset {offset} is past the end of the file ({n} lines)"));
    }
    if more {
        let _ = write!(out, "\n(file continues after line {}; use offset to read more)", offset + shown - 1);
    }
    Ok(out)
}

pub fn write(t: &Tools, input: &Value) -> Output {
    let path = path(t, input)?;
    let content = input["content"].as_str().ok_or("missing content")?;
    plain_file(&path)?;
    check_fresh(t, &path)?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    t.checkpoints.before_change(&path);
    fs::write(&path, content).map_err(|e| format!("{}: {e}", path.display()))?;
    remember(t, &path);
    Ok(format!("Wrote {} lines to {}", content.lines().count(), path.display()))
}

pub fn edit(t: &Tools, input: &Value) -> Output {
    let path = path(t, input)?;
    let old = input["old_string"].as_str().ok_or("missing old_string")?;
    let new = input["new_string"].as_str().ok_or("missing new_string")?;
    let all = input["replace_all"].as_bool().unwrap_or(false);
    if !path.exists() {
        return Err(format!("{} does not exist. Use write to create it.", path.display()));
    }
    if old.is_empty() {
        return Err("old_string is empty".into());
    }
    if old == new {
        return Err("old_string and new_string are the same".into());
    }
    plain_file(&path)?;
    if fs::metadata(&path).is_ok_and(|m| m.len() > MAX_EDIT) {
        return Err(format!("{} is too big to edit this way (over 64 MB). Use bash, for example sed.", path.display()));
    }
    // No read is asked for first: old_string matching exactly is the proof
    // that the model knows what is there now.
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let count = text.matches(old).count();
    if count == 0 {
        return Err("old_string not found in the file. Check whitespace and indentation.".into());
    }
    if count > 1 && !all {
        return Err(format!(
            "old_string matches {count} places. Add surrounding lines to make it unique, or set replace_all."
        ));
    }
    let text = if all { text.replace(old, new) } else { text.replacen(old, new, 1) };
    t.checkpoints.before_change(&path);
    fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    remember(t, &path);
    let s = if count == 1 { "" } else { "s" };
    Ok(format!("Edited {} ({count} replacement{s})", path.display()))
}

#[cfg(test)]
mod tests {
    use super::super::{result_content, summary, test_tools};
    use serde_json::json;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn edit_rules() {
        let (t, dir) = test_tools("edit");
        let no = AtomicBool::new(false);
        std::fs::write(dir.join("a.txt"), "one\ntwo\ntwo\n").unwrap();

        let edit =
            |old: &str, all: bool| json!({"path": "a.txt", "old_string": old, "new_string": "X", "replace_all": all});
        // No read is needed first: an exact match is proof enough.
        assert!(t.run("edit", &edit("two", false), &no).unwrap_err().contains("matches 2"));
        assert!(t.run("edit", &edit("nope", false), &no).unwrap_err().contains("not found"));
        t.run("edit", &edit("two", true), &no).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "one\nX\nX\n");

        // Changed on disk meanwhile: the edit works on what is there now.
        std::fs::write(dir.join("a.txt"), "changed\n").unwrap();
        assert!(t.run("edit", &edit("one", false), &no).unwrap_err().contains("not found"));
        t.run("edit", &edit("changed", false), &no).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "X\n");

        // Overwriting a whole file still needs a read first (an edit counts as one).
        std::fs::write(dir.join("b.txt"), "old\n").unwrap();
        let write = json!({"path": "b.txt", "content": "new"});
        assert!(t.run("write", &write, &no).unwrap_err().contains("not been read"));
        assert!(t.run("read", &json!({"path": "b.txt"}), &no).unwrap().contains("     1\told"));
        t.run("write", &write, &no).unwrap();
        t.run("write", &json!({"path": "a.txt", "content": "new"}), &no).unwrap();
    }

    #[test]
    fn reads_images_as_image_blocks() {
        let (t, dir) = test_tools("read-image");
        std::fs::write(dir.join("shot.png"), b"\x89PNG\r\n\x1a\n").unwrap();
        let out = t.run("read", &json!({"path": "shot.png"}), &AtomicBool::new(false));
        assert_eq!(summary("read", &out), "Read image (0 KB)");
        let content = result_content(out.unwrap());
        assert_eq!(content[0]["type"], "image");
        assert_eq!(content[0]["source"]["media_type"], "image/png");
    }

    use std::fs;

    fn run(t: &super::super::Tools, name: &str, input: serde_json::Value) -> super::super::Output {
        t.run(name, &input, &AtomicBool::new(false))
    }

    #[test]
    fn previews_show_the_change_without_making_it() {
        let (t, dir) = test_tools("fs-preview");
        fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();

        let d = t.preview("write", &json!({"path": "a.txt", "content": "new"})).unwrap();
        assert_eq!((d.line, d.old.as_str(), d.new.as_str()), (1, "one\ntwo\nthree\n", "new"));
        let d = t.preview("write", &json!({"path": "fresh.txt", "content": "new"})).unwrap();
        assert_eq!(d.old, "", "a new file has no old text");
        assert!(!dir.join("fresh.txt").exists());

        let d = t.preview("edit", &json!({"path": "a.txt", "old_string": "three", "new_string": "3"})).unwrap();
        assert_eq!((d.line, d.old.as_str(), d.new.as_str()), (3, "three", "3"));
        assert!(t.preview("edit", &json!({"path": "a.txt", "old_string": "nope", "new_string": "3"})).is_none());
        assert!(t.preview("edit", &json!({"path": "gone.txt", "old_string": "x", "new_string": "3"})).is_none());
        assert!(t.preview("edit", &json!({"path": "a.txt", "new_string": "3"})).is_none());
        assert!(t.preview("write", &json!({"path": "a.txt"})).is_none(), "no content, no preview");
        assert!(t.preview("write", &json!({"content": "x"})).is_none(), "no path");
        assert!(t.preview("read", &json!({"path": "a.txt"})).is_none());

        // Files past 1 MB are still changed, just not previewed.
        let big = dir.join("big.txt");
        fs::File::create(&big).unwrap().set_len(1024 * 1024 + 1).unwrap();
        assert!(t.preview("write", &json!({"path": "big.txt", "content": "x"})).is_none());
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "one\ntwo\nthree\n");
    }

    #[test]
    fn reading_pages_through_a_file() {
        let (t, dir) = test_tools("fs-read");
        let lines: String = (1..=10).map(|i| format!("line {i}\n")).collect();
        fs::write(dir.join("ten.txt"), &lines).unwrap();

        let out = run(&t, "read", json!({"path": "ten.txt", "offset": 3, "limit": 2})).unwrap();
        assert_eq!(out, "     3\tline 3\n     4\tline 4\n\n(file continues after line 4; use offset to read more)");
        let out = run(&t, "read", json!({"path": "ten.txt", "offset": 9})).unwrap();
        assert_eq!(out, "     9\tline 9\n    10\tline 10\n");
        let err = run(&t, "read", json!({"path": "ten.txt", "offset": 11})).unwrap_err();
        assert_eq!(err, "offset 11 is past the end of the file (10 lines)");
        // Offset 0 counts as the first line.
        assert!(
            run(&t, "read", json!({"path": "ten.txt", "offset": 0, "limit": 1}))
                .unwrap()
                .starts_with("     1\tline 1\n")
        );
    }

    #[test]
    fn reading_odd_files() {
        let (t, dir) = test_tools("fs-read-odd");
        fs::write(dir.join("empty.txt"), "").unwrap();
        assert_eq!(run(&t, "read", json!({"path": "empty.txt"})).unwrap(), "(empty file)");

        fs::create_dir(dir.join("folder")).unwrap();
        let err = run(&t, "read", json!({"path": "folder"})).unwrap_err();
        assert_eq!(err, format!("{} is a directory. Use bash ls.", dir.join("folder").display()));

        fs::write(dir.join("blob.bin"), b"abc\0def").unwrap();
        let err = run(&t, "read", json!({"path": "blob.bin"})).unwrap_err();
        assert!(err.ends_with("looks like a binary file"), "{err}");

        let err = run(&t, "read", json!({"path": "missing.txt"})).unwrap_err();
        assert!(err.contains("missing.txt: ") && err.contains("No such file"), "{err}");
        assert_eq!(run(&t, "read", json!({})).unwrap_err(), "missing path");

        // Windows line endings are trimmed; a long line is cut with an ellipsis.
        fs::write(dir.join("crlf.txt"), "a\r\nb\r\n").unwrap();
        assert_eq!(run(&t, "read", json!({"path": "crlf.txt"})).unwrap(), "     1\ta\n     2\tb\n");
        fs::write(dir.join("long.txt"), "x".repeat(2500)).unwrap();
        let out = run(&t, "read", json!({"path": "long.txt"})).unwrap();
        assert_eq!(out.trim_end(), format!("     1\t{}\u{2026}", "x".repeat(2000)));
        // Invalid UTF-8 is replaced, not an error.
        fs::write(dir.join("latin.txt"), b"caf\xe9\n").unwrap();
        assert_eq!(run(&t, "read", json!({"path": "latin.txt"})).unwrap(), "     1\tcaf\u{fffd}\n");
    }

    #[test]
    fn a_big_file_stops_at_the_line_or_byte_limit() {
        let (t, dir) = test_tools("fs-read-big");
        let many: String = (0..2500).map(|i| format!("{i}\n")).collect();
        fs::write(dir.join("many.txt"), many).unwrap();
        let out = run(&t, "read", json!({"path": "many.txt"})).unwrap();
        assert!(out.ends_with("\n(file continues after line 2000; use offset to read more)"));
        assert_eq!(out.lines().filter(|l| l.as_bytes().get(6) == Some(&b'\t')).count(), 2000);

        // 1000 lines of 100 bytes: the 50 KB budget runs out first.
        let wide: String = (0..1000).map(|_| format!("{}\n", "w".repeat(100))).collect();
        fs::write(dir.join("wide.txt"), wide).unwrap();
        let out = run(&t, "read", json!({"path": "wide.txt"})).unwrap();
        let shown = out.lines().filter(|l| l.as_bytes().get(6) == Some(&b'\t')).count();
        assert_eq!(shown, 475);
        assert!(out.contains(&format!("(file continues after line {shown};")));
    }

    #[test]
    fn writing_and_editing_report_their_problems() {
        let (t, dir) = test_tools("fs-write-edit");
        assert_eq!(run(&t, "write", json!({"path": "x.txt"})).unwrap_err(), "missing content");
        assert_eq!(run(&t, "write", json!({"content": "x"})).unwrap_err(), "missing path");
        // Creates parent folders and counts lines.
        let out = run(&t, "write", json!({"path": "deep/er/x.txt", "content": "a\nb\n"})).unwrap();
        assert_eq!(out, format!("Wrote 2 lines to {}", dir.join("deep/er/x.txt").display()));
        assert_eq!(fs::read_to_string(dir.join("deep/er/x.txt")).unwrap(), "a\nb\n");
        // Overwriting needs a read first.
        let again = json!({"path": "deep/er/x.txt", "content": "c"});
        assert!(run(&t, "write", again.clone()).is_ok(), "this agent wrote it, so it knows its content");
        // A file the agent never saw can't be replaced blindly.
        fs::write(dir.join("other.txt"), "mine").unwrap();
        assert!(run(&t, "write", json!({"path": "other.txt", "content": "c"})).unwrap_err().contains("not been read"));
        let err = run(&t, "write", json!({"path": "folder/../deep", "content": "c"})).unwrap_err();
        assert!(err.contains("deep"), "{err}");

        let edit = |old: &str, new: &str| json!({"path": "other.txt", "old_string": old, "new_string": new});
        assert!(
            run(&t, "edit", json!({"path": "nope.txt", "old_string": "a", "new_string": "b"}))
                .unwrap_err()
                .contains("does not exist. Use write")
        );
        run(&t, "read", json!({"path": "other.txt"})).unwrap();
        assert_eq!(run(&t, "edit", edit("", "x")).unwrap_err(), "old_string is empty");
        assert_eq!(run(&t, "edit", edit("mine", "mine")).unwrap_err(), "old_string and new_string are the same");
        assert_eq!(run(&t, "edit", json!({"path": "other.txt", "new_string": "x"})).unwrap_err(), "missing old_string");
        assert_eq!(run(&t, "edit", json!({"path": "other.txt", "old_string": "x"})).unwrap_err(), "missing new_string");
        assert_eq!(run(&t, "edit", json!({"old_string": "x", "new_string": "y"})).unwrap_err(), "missing path");
        let ok = run(&t, "edit", edit("mine", "yours")).unwrap();
        assert_eq!(ok, format!("Edited {} (1 replacement)", dir.join("other.txt").display()));
        assert_eq!(fs::read_to_string(dir.join("other.txt")).unwrap(), "yours");
        // Its own edit doesn't make the file stale.
        run(&t, "edit", edit("yours", "ours")).unwrap();
        fs::write(dir.join("rep.txt"), "a a a").unwrap();
        run(&t, "read", json!({"path": "rep.txt"})).unwrap();
        let all = json!({"path": "rep.txt", "old_string": "a", "new_string": "b", "replace_all": true});
        assert!(run(&t, "edit", all).unwrap().ends_with("(3 replacements)"));
        assert_eq!(fs::read_to_string(dir.join("rep.txt")).unwrap(), "b b b");
    }

    #[test]
    fn changes_are_checkpointed_for_rewind() {
        let (t, dir) = test_tools("fs-checkpoint");
        t.checkpoints.open(dir.join(".ckpt"));
        t.checkpoints.begin_turn(0);
        fs::write(dir.join("a.txt"), "before").unwrap();
        run(&t, "read", json!({"path": "a.txt"})).unwrap();
        run(&t, "edit", json!({"path": "a.txt", "old_string": "before", "new_string": "after"})).unwrap();
        run(&t, "write", json!({"path": "new.txt", "content": "created"})).unwrap();
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "after");
        assert_eq!(t.checkpoints.restore_from(0), Ok(2));
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "before");
        assert!(!dir.join("new.txt").exists());
    }
}
