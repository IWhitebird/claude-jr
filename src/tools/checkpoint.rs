// Checkpoints for /rewind: before write or edit first changes a file in a
// turn, its original is copied to disk. Rewinding to a turn restores every
// file changed since, newest first.
//
// Only the index lives in memory. Changes made by shell commands aren't
// tracked.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Files bigger than this aren't copied (and so can't be rewound).
const MAX_FILE: u64 = 10 * 1024 * 1024;

#[derive(Serialize, Deserialize, Clone)]
struct Entry {
    /// The turn: where its user message starts in the conversation.
    turn: usize,
    path: PathBuf,
    /// The copy of the original, or None if the file didn't exist yet.
    saved: Option<PathBuf>,
}

#[derive(Default)]
pub struct Checkpoints(Mutex<State>);

#[derive(Default)]
struct State {
    dir: Option<PathBuf>,
    turn: usize,
    entries: Vec<Entry>,
}

impl Checkpoints {
    /// Where this session's copies go. Loads what earlier runs recorded.
    pub fn open(&self, dir: PathBuf) {
        let entries = std::fs::read_to_string(dir.join("index.jsonl"))
            .map(|t| t.lines().filter_map(|l| serde_json::from_str(l).ok()).collect())
            .unwrap_or_default();
        *self.0.lock().unwrap() = State { dir: Some(dir), turn: 0, entries };
    }

    /// Forgets every checkpoint (the files stay as they are). After /compact
    /// the old turns are gone, so there's nothing to rewind to.
    pub fn clear(&self) {
        let mut s = self.0.lock().unwrap();
        s.entries.clear();
        if let Some(dir) = &s.dir {
            let _ = std::fs::write(dir.join("index.jsonl"), "");
        }
    }

    pub fn begin_turn(&self, turn: usize) {
        self.0.lock().unwrap().turn = turn;
    }

    /// Call before changing `path`. Copies it the first time in a turn.
    pub fn before_change(&self, path: &Path) {
        let mut s = self.0.lock().unwrap();
        let turn = s.turn;
        let Some(dir) = s.dir.clone() else { return };
        if s.entries.iter().any(|e| e.turn == turn && e.path == path) {
            return;
        }
        let saved = if path.exists() {
            if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_FILE) {
                return;
            }
            let copy = dir.join(format!("{turn}-{}", s.entries.len()));
            if std::fs::create_dir_all(&dir).is_err() || std::fs::copy(path, &copy).is_err() {
                return;
            }
            Some(copy)
        } else {
            None
        };
        let entry = Entry { turn, path: path.to_path_buf(), saved };
        if let Ok(line) = serde_json::to_string(&entry) {
            let _ = std::fs::create_dir_all(&dir);
            let index = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("index.jsonl"));
            if let Ok(mut f) = index {
                let _ = writeln!(f, "{line}");
            }
        }
        s.entries.push(entry);
    }

    /// Puts back every file changed in `turn` or later. Returns how many.
    pub fn restore_from(&self, turn: usize) -> Result<usize, String> {
        let mut s = self.0.lock().unwrap();
        let (undo, keep): (Vec<Entry>, Vec<Entry>) = s.entries.drain(..).partition(|e| e.turn >= turn);
        s.entries = keep;
        let mut restored: Vec<&Path> = Vec::new();
        for e in undo.iter().rev() {
            match &e.saved {
                Some(copy) => std::fs::copy(copy, &e.path).map(|_| ()),
                None => std::fs::remove_file(&e.path).or_else(|err| match err.kind() {
                    std::io::ErrorKind::NotFound => Ok(()),
                    _ => Err(err),
                }),
            }
            .map_err(|err| format!("couldn't restore {}: {err}", e.path.display()))?;
            if !restored.contains(&e.path.as_path()) {
                restored.push(&e.path);
            }
        }
        // Rewrite the index without the undone entries.
        if let Some(dir) = &s.dir {
            let lines: String =
                s.entries.iter().filter_map(|e| serde_json::to_string(e).ok()).map(|l| l + "\n").collect();
            let _ = std::fs::write(dir.join("index.jsonl"), lines);
        }
        Ok(restored.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restores_newest_first() {
        let dir = super::super::test_dir("checkpoint");
        let cp = Checkpoints::default();
        cp.open(dir.join("cp"));
        let (a, b) = (dir.join("a.txt"), dir.join("b.txt"));
        std::fs::write(&a, "v1").unwrap();

        cp.begin_turn(1);
        cp.before_change(&a);
        std::fs::write(&a, "v2").unwrap();
        cp.before_change(&b); // b is new in turn 1
        std::fs::write(&b, "new").unwrap();

        cp.begin_turn(5);
        cp.before_change(&a);
        std::fs::write(&a, "v3").unwrap();

        // Back to before turn 5: a is v2 again, b stays.
        assert_eq!(cp.restore_from(5), Ok(1));
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "v2");
        assert!(b.exists());

        // Back to before turn 1, from a fresh index loaded off disk.
        let again = Checkpoints::default();
        again.open(dir.join("cp"));
        assert_eq!(again.restore_from(1), Ok(2));
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "v1");
        assert!(!b.exists());
    }
}
