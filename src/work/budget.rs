// A group's shared budget (--group NAME --budget USD): every worker in the
// group adds what it spends to one file, and all of them stop when it is
// used up.

use super::task::{read_json, write_json};
use crate::config::data_dir;
use serde_json::{Value, json};
use std::fs;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

/// Set for a worker in a group: the file the group's spending is kept in.
pub const ENV: &str = "CLAUDE_JR_BUDGET";

/// The file for group `name`'s spending. A budget given now replaces the one
/// it had; without one the group must already exist.
pub fn open(name: &str, budget: Option<f64>) -> Result<PathBuf, String> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) {
        return Err(format!("a group's name is letters, digits, - _ and . only (got {name:?})"));
    }
    let dir = data_dir().join("groups");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{name}.json"));
    match (budget, path.exists()) {
        (Some(usd), _) => {
            let spent = read_json(&path).and_then(|v| v["spent_usd"].as_f64()).unwrap_or(0.0);
            write_json(&path, &json!({ "budget_usd": usd, "spent_usd": spent })).map_err(|e| e.to_string())?;
        }
        (None, false) => return Err(format!("group {name:?} has no budget yet: give --budget USD")),
        (None, true) => {}
    }
    Ok(path)
}

/// Adds `usd` to what the group has spent and returns (spent, budget). The
/// file is locked meanwhile, so workers adding at once don't lose a sum.
pub fn spend(file: &Path, usd: f64) -> Option<(f64, f64)> {
    let mut f = fs::OpenOptions::new().read(true).write(true).open(file).ok()?;
    f.lock().ok()?;
    let mut text = String::new();
    f.read_to_string(&mut text).ok()?;
    let mut v: Value = serde_json::from_str(&text).ok()?;
    let spent = v["spent_usd"].as_f64().unwrap_or(0.0) + usd;
    v["spent_usd"] = json!(spent);
    f.rewind().ok()?;
    f.set_len(0).ok()?;
    f.write_all(format!("{v:#}\n").as_bytes()).ok()?;
    Some((spent, v["budget_usd"].as_f64().unwrap_or(f64::MAX)))
}
