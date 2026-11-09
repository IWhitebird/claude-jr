// Images: attached by mentioning their path (dragging a file into the
// terminal pastes it), pasted from the clipboard, or read by the read tool.
//
// They're sent as base64 content blocks and stay in the conversation, so a
// screenshot costs its size (x 4/3) in memory for the rest of the session.

use base64::Engine;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// The API's limit per image.
pub const MAX_BYTES: u64 = 5 * 1024 * 1024;

pub fn media_type(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => return None,
    })
}

/// Image files mentioned in `text` that exist. Handles what terminals paste
/// for a dragged file: plain paths, 'quoted' or "quoted" ones, and spaces
/// escaped with a backslash.
pub fn paths_in(text: &str) -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let mut found = Vec::new();
    for word in words(text) {
        let word = word.trim_end_matches([',', '.', ';', ':', ')']);
        let path = match word.strip_prefix("~/") {
            Some(rest) => home.join(rest),
            None => PathBuf::from(word),
        };
        if media_type(&path).is_some() && path.is_file() && !found.contains(&path) {
            found.push(path);
        }
    }
    found
}

/// Splits on whitespace, keeping quoted runs and backslash-escaped spaces.
fn words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote = None;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match (c, quote) {
            ('\\', None) => {
                if let Some(next) = chars.next() {
                    cur.push(next);
                }
            }
            ('\'' | '"', None) if cur.is_empty() => quote = Some(c),
            (c, Some(q)) if c == q => quote = None,
            (c, None) if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            (c, _) => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// An image content block for the API.
pub fn block(path: &Path) -> Result<Value, String> {
    let media_type = media_type(path).ok_or_else(|| format!("{} isn't a supported image", path.display()))?;
    let size = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?.len();
    if size > MAX_BYTES {
        return Err(format!("{} is {} MB; images can be at most 5 MB", path.display(), size / 1_048_576));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(json!({ "type": "image", "source": { "type": "base64", "media_type": media_type, "data": data } }))
}

/// Saves an image from the clipboard to a file and returns its path.
/// Uses wl-paste (Wayland) or xclip (X11).
pub fn from_clipboard() -> Result<PathBuf, String> {
    let tries: [(&str, &[&str]); 2] =
        [("wl-paste", &["--type", "image/png"]), ("xclip", &["-selection", "clipboard", "-t", "image/png", "-o"])];
    for (cmd, args) in tries {
        let Ok(out) = std::process::Command::new(cmd).args(args).stderr(std::process::Stdio::null()).output() else {
            continue;
        };
        // A PNG starts with these 8 bytes; anything else isn't an image.
        if out.status.success() && out.stdout.starts_with(b"\x89PNG\r\n\x1a\n") {
            let dir = crate::config::data_dir().join("images");
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
            let path = dir.join(format!("paste-{ms}.png"));
            std::fs::write(&path, out.stdout).map_err(|e| e.to_string())?;
            return Ok(path);
        }
    }
    Err("no image in the clipboard (needs wl-paste or xclip)".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_dragged_paths() {
        let dir = crate::tools::test_dir("images");
        let spaced = dir.join("my shot.png");
        let plain = dir.join("logo.PNG");
        std::fs::write(&spaced, b"x").unwrap();
        std::fs::write(&plain, b"x").unwrap();
        let text = format!(
            "match this '{}' and {}, also {} and missing.png",
            spaced.display(),
            plain.display(),
            spaced.display().to_string().replace(' ', "\\ ")
        );
        assert_eq!(paths_in(&text), [spaced.clone(), plain.clone()]);
        let b = block(&plain).unwrap();
        assert_eq!(
            (b["source"]["media_type"].as_str(), b["source"]["data"].as_str()),
            (Some("image/png"), Some("eA=="))
        );
    }
}
