// memory: notes the model keeps about this user and project across sessions,
// in ~/.local/share/claude-jr/memory/<project>/MEMORY.md, one "- note" per
// line. The file is read into the system prompt when a session starts.

use super::{Output, def};
use serde_json::value::RawValue;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::LazyLock;

/// Kept small: it's in every request's system prompt.
const MAX_BYTES: usize = 8 * 1024;

pub static MEMORY: LazyLock<Box<RawValue>> = LazyLock::new(|| {
    def(json!({
        "name": "memory",
        "description": "Your notes about this user and project, kept across sessions and shown to you at the \
            start of each one. Add a short note when you learn something durable: a preference the user states, \
            a project convention, a command that works, a mistake to avoid. Remove notes that turn out wrong or \
            outdated. Don't save what's already in the code or the instruction files, or details of the current task.",
        "input_schema": {
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["add", "remove"] },
                "note": { "type": "string", "description": "For remove: the note, or a unique part of it" }
            },
            "required": ["action", "note"]
        }
    }))
});

/// This project's memory file.
pub fn path() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_default();
    let name: String = cwd.to_string_lossy().chars().map(|c| if c.is_alphanumeric() { c } else { '-' }).collect();
    crate::config::data_dir().join("memory").join(name).join("MEMORY.md")
}

/// The notes, for the system prompt.
pub fn load() -> Option<String> {
    std::fs::read_to_string(path()).ok().filter(|t| !t.trim().is_empty())
}

pub fn update(input: &Value) -> Output {
    let note = input["note"].as_str().unwrap_or("").trim().replace('\n', " ");
    if note.is_empty() {
        return Err("the note is empty".into());
    }
    let file = path();
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    let mut notes: Vec<String> = text.lines().filter_map(|l| l.strip_prefix("- ")).map(String::from).collect();

    match input["action"].as_str() {
        Some("add") => {
            if notes.contains(&note) {
                return Ok("Already noted.".into());
            }
            notes.push(note);
        }
        Some("remove") => {
            let matching: Vec<usize> = (0..notes.len()).filter(|&i| notes[i].contains(&note)).collect();
            match matching[..] {
                [i] => {
                    notes.remove(i);
                }
                [] => return Err("no note matches that".into()),
                _ => return Err(format!("{} notes match that; give more of the text", matching.len())),
            }
        }
        _ => return Err("action must be add or remove".into()),
    }

    let out: String = notes.iter().map(|n| format!("- {n}\n")).collect();
    if out.len() > MAX_BYTES {
        return Err("memory is full (8 KB). Remove outdated notes first.".into());
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&file, out).map_err(|e| format!("{}: {e}", file.display()))?;
    Ok(format!("Saved. {} notes in memory.", notes.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn description_reads_correctly() {
        let d = MEMORY.get();
        assert!(!d.contains("\\\\"), "{d}");
        assert!(d.contains("shown to you at the start of each one"));
    }

    #[test]
    fn add_and_remove() {
        super::super::test_home();
        let add = |n: &str| update(&json!({"action": "add", "note": n}));
        assert_eq!(add("uses pnpm, not npm").unwrap(), "Saved. 1 notes in memory.");
        assert_eq!(add("uses pnpm, not npm").unwrap(), "Already noted.");
        add("tailwind for styles").unwrap();
        assert!(update(&json!({"action": "remove", "note": "s"})).unwrap_err().contains("2 notes match"));
        update(&json!({"action": "remove", "note": "pnpm"})).unwrap();
        assert_eq!(load().unwrap(), "- tailwind for styles\n");
    }
}
