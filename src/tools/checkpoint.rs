// Checkpoints for /rewind: before write or edit first changes a file in a
// turn, its original is copied to disk. Rewinding to a turn restores every
// file changed since, newest first.
//
// Only the index lives in memory. Changes made by shell commands aren't
// tracked.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
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

/// The index is read back from disk, so it is checked before use: the target
/// must be an absolute path without `..` or `.`, and the copy a plain file
/// directly inside `dir`.
fn check(e: &Entry, dir: &Path) -> Result<(), String> {
    let plain =
        |p: &Path| p.is_absolute() && !p.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir));
    if !plain(&e.path) {
        return Err(format!("{}: not a plain absolute path, skipped", e.path.display()));
    }
    if let Some(copy) = &e.saved {
        let name_ok = copy.file_name().is_some_and(|n| Path::new(n).components().count() == 1);
        if copy.parent() != Some(dir) || !name_ok || copy.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(format!("{}: backup is not inside the checkpoint folder, skipped", e.path.display()));
        }
    }
    Ok(())
}

/// Writes the index through a temp file in the same folder, then renames it.
fn write_index(dir: &Path, entries: &[Entry]) -> std::io::Result<()> {
    let lines: String = entries.iter().filter_map(|e| serde_json::to_string(e).ok()).map(|l| l + "\n").collect();
    // Nothing may have been saved yet (a rewind before any file changed).
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join("index.jsonl.tmp");
    std::fs::write(&tmp, lines)?;
    std::fs::rename(&tmp, dir.join("index.jsonl"))
}

impl Checkpoints {
    /// Where this session's copies go. Loads what earlier runs recorded.
    pub fn open(&self, dir: PathBuf) {
        let entries = std::fs::read_to_string(dir.join("index.jsonl"))
            .map(|t| {
                t.lines()
                    .filter_map(|l| serde_json::from_str::<Entry>(l).ok())
                    .filter(|e| check(e, &dir).is_ok())
                    .collect()
            })
            .unwrap_or_default();
        *self.0.lock().unwrap() = State { dir: Some(dir), turn: 0, entries };
    }

    /// Forgets every checkpoint (the files stay as they are). After /compact
    /// the old turns are gone, so there's nothing to rewind to.
    pub fn clear(&self) {
        let mut s = self.0.lock().unwrap();
        s.entries.clear();
        if let Some(dir) = &s.dir {
            let _ = write_index(dir, &[]);
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
    /// Entries that can't be restored stay in the index for another try; the
    /// error names them. Entries that fail validation are dropped.
    pub fn restore_from(&self, turn: usize) -> Result<usize, String> {
        let mut s = self.0.lock().unwrap();
        let dir = s.dir.clone().unwrap_or_default();
        let all = std::mem::take(&mut s.entries);
        let (undo, mut keep): (Vec<Entry>, Vec<Entry>) = all.into_iter().partition(|e| e.turn >= turn);
        let mut restored: Vec<PathBuf> = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        let mut failed: Vec<Entry> = Vec::new();
        for e in undo.into_iter().rev() {
            if let Err(msg) = check(&e, &dir) {
                errors.push(msg);
                continue;
            }
            let done = match &e.saved {
                Some(copy) => std::fs::copy(copy, &e.path).map(|_| ()),
                None => std::fs::remove_file(&e.path).or_else(|err| match err.kind() {
                    std::io::ErrorKind::NotFound => Ok(()),
                    _ => Err(err),
                }),
            };
            match done {
                Ok(()) => {
                    if !restored.contains(&e.path) {
                        restored.push(e.path);
                    }
                }
                Err(err) => {
                    errors.push(format!("couldn't restore {}: {err}", e.path.display()));
                    failed.push(e);
                }
            }
        }
        // The index is kept oldest first (failed is newest first).
        failed.reverse();
        keep.extend(failed);
        keep.sort_by_key(|e| e.turn);
        s.entries = keep;
        if let Some(Err(err)) = s.dir.as_ref().map(|d| write_index(d, &s.entries)) {
            errors.push(format!("couldn't update the checkpoint index: {err}"));
        }
        if errors.is_empty() { Ok(restored.len()) } else { Err(errors.join("; ")) }
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

    fn index(dir: &Path) -> String {
        std::fs::read_to_string(dir.join("cp/index.jsonl")).unwrap_or_default()
    }

    #[test]
    fn failed_entry_stays_for_another_try() {
        let dir = super::super::test_dir("checkpoint-fail");
        let cp = Checkpoints::default();
        cp.open(dir.join("cp"));
        let (a, b) = (dir.join("a.txt"), dir.join("b.txt"));
        std::fs::write(&a, "a1").unwrap();
        std::fs::write(&b, "b1").unwrap();
        cp.begin_turn(1);
        cp.before_change(&a);
        std::fs::write(&a, "a2").unwrap();
        cp.before_change(&b);
        std::fs::write(&b, "b2").unwrap();

        // Lose the backup of a (entry 0), keep b's.
        std::fs::remove_file(dir.join("cp/1-0")).unwrap();
        let err = cp.restore_from(1).unwrap_err();
        assert!(err.contains("a.txt"), "{err}");
        assert!(!err.contains("b.txt"), "{err}");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "b1");
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "a2");
        assert_eq!(index(&dir).lines().count(), 1);

        // Put the backup back and try again.
        std::fs::write(dir.join("cp/1-0"), "a1").unwrap();
        assert_eq!(cp.restore_from(1), Ok(1));
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "a1");
        assert_eq!(index(&dir), "");
    }

    #[test]
    fn tampered_entries_are_ignored() {
        let dir = super::super::test_dir("checkpoint-tamper");
        let cpdir = dir.join("cp");
        std::fs::create_dir_all(&cpdir).unwrap();
        let secret = dir.join("secret.txt");
        std::fs::write(&secret, "secret").unwrap();
        let victim = dir.join("v.txt");
        std::fs::write(&victim, "keep").unwrap();
        let entry = |path: &Path, saved: Option<PathBuf>| {
            serde_json::to_string(&Entry { turn: 1, path: path.to_path_buf(), saved }).unwrap()
        };
        let lines = [
            entry(Path::new("v.txt"), None),                   // relative target
            entry(&dir.join("sub/../v.txt"), None),            // `..` in the target
            entry(&victim, Some(cpdir.join("../secret.txt"))), // `..` in the backup
            entry(&victim, Some(cpdir.join("sub/x"))),         // separator in the backup
            entry(&victim, Some(secret.clone())),              // backup outside the folder
        ];
        std::fs::write(cpdir.join("index.jsonl"), lines.join("\n")).unwrap();

        let cp = Checkpoints::default();
        cp.open(cpdir.clone());
        assert_eq!(cp.restore_from(0), Ok(0));
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");

        // Entries that get past loading are still refused on restore.
        let bad = Entry { turn: 1, path: PathBuf::from("v.txt"), saved: None };
        cp.0.lock().unwrap().entries.push(bad);
        let err = cp.restore_from(0).unwrap_err();
        assert!(err.contains("v.txt"), "{err}");
        assert_eq!(index(&dir), "");
    }

    #[test]
    fn files_outside_the_project_are_restored() {
        let proj = super::super::test_dir("checkpoint-proj");
        let other = super::super::test_dir("checkpoint-other");
        let cp = Checkpoints::default();
        cp.open(proj.join("cp"));
        let f = other.join("f.txt");
        std::fs::write(&f, "old").unwrap();
        cp.begin_turn(1);
        cp.before_change(&f);
        std::fs::write(&f, "new").unwrap();
        assert_eq!(cp.restore_from(1), Ok(1));
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "old");
    }

    #[test]
    fn files_created_in_the_turn_are_removed() {
        let dir = super::super::test_dir("checkpoint-new");
        let cp = Checkpoints::default();
        cp.open(dir.join("cp"));
        let n = dir.join("new.txt");
        cp.begin_turn(2);
        cp.before_change(&n);
        std::fs::write(&n, "x").unwrap();
        assert_eq!(cp.restore_from(2), Ok(1));
        assert!(!n.exists());
    }
}
