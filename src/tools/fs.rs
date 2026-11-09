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

pub static READ: LazyLock<Box<RawValue>> = LazyLock::new(|| {
    def(json!({
        "name": "read",
        "description": "Read a file. Text comes back as lines prefixed with their line number and a tab; \
            images (png, jpg, gif, webp) come back as the image itself, so you can look at screenshots and designs. \
            Reads up to 2000 lines; use offset and limit for more. You must read a file before editing or overwriting it.",
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
            An existing file must be read first. Prefer edit for changes to existing files.",
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
        "description": "Replace exact text in a file. old_string must match exactly once, including whitespace, \
            unless replace_all is true. The file must be read first. Do not include the line number prefixes from read.",
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
    if fs::metadata(&path).is_ok_and(|m| m.len() > MAX_PREVIEW) {
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
        if r.read_until(b'\n', &mut buf).map_err(|e| e.to_string())? == 0 {
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
    check_fresh(t, &path)?;
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
    use std::time::Duration;

    #[test]
    fn edit_rules() {
        let (t, dir) = test_tools("edit");
        let no = AtomicBool::new(false);
        std::fs::write(dir.join("a.txt"), "one\ntwo\ntwo\n").unwrap();

        let edit =
            |old: &str, all: bool| json!({"path": "a.txt", "old_string": old, "new_string": "X", "replace_all": all});
        assert!(t.run("edit", &edit("one", false), &no).unwrap_err().contains("not been read"));
        assert!(t.run("read", &json!({"path": "a.txt"}), &no).unwrap().contains("     1\tone"));
        assert!(t.run("edit", &edit("two", false), &no).unwrap_err().contains("matches 2"));
        assert!(t.run("edit", &edit("nope", false), &no).unwrap_err().contains("not found"));
        t.run("edit", &edit("two", true), &no).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "one\nX\nX\n");

        // Changed on disk after the read: refuse.
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(dir.join("a.txt"), "changed\n").unwrap();
        assert!(t.run("edit", &edit("changed", false), &no).unwrap_err().contains("changed since"));
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
}
