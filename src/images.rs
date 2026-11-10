// Images: attached by mentioning their path (dragging a file into the
// terminal pastes it), pasted from the clipboard, or read by the read tool.
//
// An attached image is copied once into claude-jr's image folder, named by a
// hash of its bytes, and the conversation holds only a marker with that path
// in place of the base64 data. The data is encoded from disk in small chunks
// while a request is sent, so images cost almost no memory however many
// there are. The copy never changes, so the conversation stays the same
// byte for byte (the prompt cache and the API's history checks need that)
// even if the original file is edited.

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

/// Marks where an image's base64 data goes: `"data":"` MARKER path END.
const MARKER: &str = "\"data\":\"@@claude-jr-image:";
const END: &str = "@@\"";

fn store_dir() -> PathBuf {
    crate::config::data_dir().join("images")
}

/// An image content block for the API, referring to a stored copy.
pub fn block(path: &Path) -> Result<Value, String> {
    let media_type = media_type(path).ok_or_else(|| format!("{} isn't a supported image", path.display()))?;
    let stored = store(path)?;
    let data = format!("@@claude-jr-image:{}@@", stored.display());
    Ok(json!({ "type": "image", "source": { "type": "base64", "media_type": media_type, "data": data } }))
}

/// Copies an image into the store (once per distinct content) and returns the copy's path.
fn store(path: &Path) -> Result<PathBuf, String> {
    use std::hash::{Hash, Hasher};
    let size = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?.len();
    if size > MAX_BYTES {
        return Err(format!("{} is {} MB; images can be at most 5 MB", path.display(), size / 1_048_576));
    }
    // Hashed in chunks and copied by the kernel, so a big image is never
    // all in memory at once.
    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = std::io::Read::read(&mut file, &mut buf).map_err(|e| format!("{}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        buf[..n].hash(&mut h);
    }
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("img").to_ascii_lowercase();
    let copy = store_dir().join(format!("{:016x}-{size}.{ext}", h.finish()));
    if !copy.exists() {
        std::fs::create_dir_all(store_dir()).map_err(|e| e.to_string())?;
        std::fs::copy(path, &copy).map_err(|e| format!("{}: {e}", copy.display()))?;
    }
    Ok(copy)
}

/// A piece of a message as sent: its own bytes, or a stored image's base64.
pub enum Piece<'a> {
    Text(&'a [u8]),
    Image(PathBuf),
}

/// Splits a message's JSON at its image markers. Only markers in an image's
/// data field that point into the image store count, so text that happens to
/// contain the marker can't pull in other files.
pub fn pieces(json: &str) -> Vec<Piece<'_>> {
    let mut out = Vec::new();
    let mut rest = json;
    let dir = store_dir();
    while let Some(at) = rest.find(MARKER) {
        let path_start = at + MARKER.len();
        let Some(len) = rest[path_start..].find(END) else { break };
        let path = PathBuf::from(&rest[path_start..path_start + len]);
        if path.parent() != Some(dir.as_path()) {
            out.push(Piece::Text(&rest.as_bytes()[..path_start]));
            rest = &rest[path_start..];
            continue;
        }
        out.push(Piece::Text(&rest.as_bytes()[..at + "\"data\":\"".len()]));
        out.push(Piece::Image(path));
        rest = &rest[path_start + len + END.len() - 1..]; // keep the closing quote
    }
    out.push(Piece::Text(rest.as_bytes()));
    out
}

/// Reads a stored image as base64, a few KB at a time.
pub struct Encoder {
    file: Option<std::fs::File>,
    /// Encoded bytes not handed out yet.
    out: Vec<u8>,
    pos: usize,
    /// The encoded length, known up front for Content-Length.
    pub len: usize,
}

/// A 1x1 transparent PNG, sent if a stored image has gone missing.
const MISSING: &[u8] = b"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";

impl Encoder {
    pub fn open(path: &Path) -> Encoder {
        match (std::fs::File::open(path), std::fs::metadata(path)) {
            (Ok(file), Ok(m)) => {
                let len = (m.len() as usize).div_ceil(3) * 4;
                Encoder { file: Some(file), out: Vec::new(), pos: 0, len }
            }
            _ => Encoder { file: None, out: MISSING.to_vec(), pos: 0, len: MISSING.len() },
        }
    }
}

impl std::io::Read for Encoder {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pos == self.out.len() {
            let Some(file) = &mut self.file else { return Ok(0) };
            // A multiple of 3 bytes encodes without padding mid-stream.
            let mut raw = vec![0u8; 3 * 4096];
            let mut n = 0;
            while n < raw.len() {
                match file.read(&mut raw[n..])? {
                    0 => break,
                    k => n += k,
                }
            }
            if n == 0 {
                return Ok(0);
            }
            self.out = base64::engine::general_purpose::STANDARD.encode(&raw[..n]).into_bytes();
            self.pos = 0;
        }
        let n = buf.len().min(self.out.len() - self.pos);
        buf[..n].copy_from_slice(&self.out[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// What ctrl+v found in the clipboard.
pub enum Clip {
    /// An image, saved to this file.
    Image(PathBuf),
    Text(String),
}

/// The clipboard's image if it holds one, otherwise its text.
pub fn clipboard() -> Result<Clip, String> {
    if let Ok(path) = from_clipboard() {
        return Ok(Clip::Image(path));
    }
    // Wayland, X11 (two tools), macOS.
    let tries: [(&str, &[&str]); 4] = [
        ("wl-paste", &["--no-newline"]),
        ("xclip", &["-selection", "clipboard", "-o"]),
        ("xsel", &["-b", "-o"]),
        ("pbpaste", &[]),
    ];
    for (cmd, args) in tries {
        let Ok(out) = std::process::Command::new(cmd).args(args).stderr(std::process::Stdio::null()).output() else {
            continue;
        };
        if let (true, Ok(text)) = (out.status.success(), String::from_utf8(out.stdout))
            && !text.trim().is_empty()
        {
            return Ok(Clip::Text(text));
        }
    }
    Err("Nothing to paste: the clipboard is empty, or wl-paste / xclip isn't installed".into())
}

/// Saves an image from the clipboard to a file and returns its path.
/// Uses wl-paste (Wayland) or xclip (X11).
fn from_clipboard() -> Result<PathBuf, String> {
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
    }

    use std::io::Read;

    fn encode_all(path: &Path) -> (String, usize) {
        let mut e = Encoder::open(path);
        let declared = e.len;
        let mut out = String::new();
        e.read_to_string(&mut out).unwrap();
        (out, declared)
    }

    #[test]
    fn media_types_by_extension() {
        let t = |p: &str| media_type(Path::new(p));
        assert_eq!(t("a.png"), Some("image/png"));
        assert_eq!(t("a.JPG"), Some("image/jpeg"));
        assert_eq!(t("a.jpeg"), Some("image/jpeg"));
        assert_eq!(t("a.gif"), Some("image/gif"));
        assert_eq!(t("a.webp"), Some("image/webp"));
        assert_eq!(t("a.svg"), None);
        assert_eq!(t("noext"), None);
    }

    #[test]
    fn words_split_like_a_shell_would() {
        assert_eq!(words(r#"a  b\ c 'd e' "f g" h'i j"#), ["a", "b c", "d e", "f g", "h'i", "j"]);
        assert_eq!(words("trailing\\"), ["trailing"]);
        assert!(words("   ").is_empty());
        // An unclosed quote runs to the end.
        assert_eq!(words("x 'y z"), ["x", "y z"]);
    }

    #[test]
    fn paths_must_exist_and_are_listed_once() {
        let dir = crate::tools::test_dir("images-paths");
        let a = dir.join("a.png");
        std::fs::write(&a, b"x").unwrap();
        std::fs::create_dir(dir.join("folder.png")).unwrap();
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        let text = format!(
            "{} (see {}), {} {}; {}/folder.png {}/notes.txt",
            a.display(),
            a.display(),
            "\"".to_string() + &a.display().to_string() + "\"",
            dir.join("gone.png").display(),
            dir.display(),
            dir.display()
        );
        assert_eq!(paths_in(&text), [a], "punctuation is trimmed, directories and other types are ignored");
        assert!(paths_in("").is_empty());
    }

    #[test]
    fn a_block_refers_to_one_stored_copy_per_content() {
        crate::tools::test_home();
        let dir = crate::tools::test_dir("images-block");
        let a = dir.join("a.png");
        let b = dir.join("b.png");
        std::fs::write(&a, b"same bytes").unwrap();
        std::fs::write(&b, b"same bytes").unwrap();
        let (ba, bb) = (block(&a).unwrap(), block(&b).unwrap());
        assert_eq!(ba["source"]["data"], bb["source"]["data"], "equal content, equal copy");
        assert_eq!(ba["type"], "image");
        assert_eq!(ba["source"]["media_type"], "image/png");
        let data = ba["source"]["data"].as_str().unwrap();
        let stored = PathBuf::from(data.strip_prefix("@@claude-jr-image:").unwrap().strip_suffix("@@").unwrap());
        assert_eq!(stored.parent().unwrap(), store_dir());
        assert!(stored.to_string_lossy().ends_with("-10.png"), "named by hash and size: {}", stored.display());
        // The conversation keeps working if the original is edited or removed.
        std::fs::write(&a, b"changed").unwrap();
        assert_eq!(std::fs::read(&stored).unwrap(), b"same bytes");
        std::fs::write(dir.join("c.PNG"), b"other").unwrap();
        assert_ne!(block(&dir.join("c.PNG")).unwrap()["source"]["data"], ba["source"]["data"]);
    }

    #[test]
    fn bad_images_are_refused_with_a_reason() {
        crate::tools::test_home();
        let dir = crate::tools::test_dir("images-bad");
        let svg = dir.join("a.svg");
        std::fs::write(&svg, b"<svg/>").unwrap();
        assert_eq!(block(&svg).unwrap_err(), format!("{} isn't a supported image", svg.display()));
        let missing = dir.join("nope.png");
        assert!(block(&missing).unwrap_err().starts_with(&format!("{}: ", missing.display())));
        let big = dir.join("big.png");
        std::fs::File::create(&big).unwrap().set_len(MAX_BYTES + 1).unwrap();
        assert_eq!(block(&big).unwrap_err(), format!("{} is 5 MB; images can be at most 5 MB", big.display()));
        let exact = dir.join("exact.png");
        std::fs::File::create(&exact).unwrap().set_len(MAX_BYTES).unwrap();
        assert!(block(&exact).is_ok(), "5 MB exactly is allowed");
    }

    #[test]
    fn messages_are_split_at_image_markers_in_the_store_only() {
        crate::tools::test_home();
        let inside = store_dir().join("abc-1.png");
        let json =
            format!(r#"{{"x":[{{"source":{{"data":"@@claude-jr-image:{}@@","type":"base64"}}}}]}}"#, inside.display());
        let parts = pieces(&json);
        let [Piece::Text(head), Piece::Image(p), Piece::Text(tail)] = parts.as_slice() else {
            panic!("expected text, image, text");
        };
        assert_eq!(std::str::from_utf8(head).unwrap(), r#"{"x":[{"source":{"data":""#);
        assert_eq!(p, &inside);
        assert_eq!(std::str::from_utf8(tail).unwrap(), r#"","type":"base64"}}]}"#);

        // A marker pointing anywhere else is just text: it can't pull in other files.
        let outside = r#"{"data":"@@claude-jr-image:/etc/passwd@@"}"#;
        let parts = pieces(outside);
        assert!(parts.iter().all(|p| matches!(p, Piece::Text(_))));
        let joined: Vec<u8> =
            parts.iter().flat_map(|p| if let Piece::Text(t) = p { t.to_vec() } else { vec![] }).collect();
        assert_eq!(joined, outside.as_bytes());
        // An unterminated marker is left alone, and plain messages are one piece.
        assert_eq!(pieces(r#"{"data":"@@claude-jr-image:/x"#).len(), 1);
        assert_eq!(pieces("plain").len(), 1);
    }

    #[test]
    fn the_encoder_streams_base64_of_any_size() {
        use base64::Engine;
        let dir = crate::tools::test_dir("images-encoder");
        // Bigger than one chunk (12 KB) and not a multiple of 3.
        let bytes: Vec<u8> = (0..30_001u32).map(|i| (i * 7 % 251) as u8).collect();
        let file = dir.join("big.bin");
        std::fs::write(&file, &bytes).unwrap();
        let (text, declared) = encode_all(&file);
        assert_eq!(text, base64::engine::general_purpose::STANDARD.encode(&bytes));
        assert_eq!(declared, text.len(), "Content-Length is known up front");

        let empty = dir.join("empty.bin");
        std::fs::write(&empty, b"").unwrap();
        assert_eq!(encode_all(&empty), (String::new(), 0));
    }

    #[test]
    fn a_missing_stored_image_is_replaced_by_a_tiny_placeholder() {
        let (text, declared) = encode_all(Path::new("/no/such/stored/image.png"));
        assert_eq!(text, std::str::from_utf8(MISSING).unwrap());
        assert_eq!(declared, MISSING.len());
    }
}
