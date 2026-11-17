// The request body, streamed: messages come from memory or straight from the
// session file, and stored images are encoded as they are read, so a long
// history is never copied into one buffer.

/// A message's JSON text: at hand, or a stretch of the session file.
#[derive(Clone, Copy)]
pub enum Text<'a> {
    Memory(&'a str),
    Saved { file: &'a std::fs::File, at: u64, len: usize },
}

/// Anything that can go in a request's `messages`.
pub trait Message {
    fn text(&self) -> Text<'_>;
}

impl Message for &str {
    fn text(&self) -> Text<'_> {
        Text::Memory(self)
    }
}

impl Message for Text<'_> {
    fn text(&self) -> Text<'_> {
        *self
    }
}

/// The request body, read in order without copying the history into one
/// buffer: `{"messages":[` m1 `,` m2 ... `],` + the rest of the request.
/// Saved messages are read from the session file and stored images encoded
/// from disk, both as the body is read.
pub(crate) struct Body<'a> {
    parts: Vec<Part<'a>>,
    pub(crate) len: usize,
    part: usize,
    offset: usize,
}

pub(crate) enum Part<'a> {
    Bytes(&'a [u8]),
    /// Bytes made for this request.
    Owned(Vec<u8>),
    File {
        file: &'a std::fs::File,
        at: u64,
        left: usize,
    },
    /// A stretch of a file that is kept alive for as long as the body is.
    Kept {
        file: std::sync::Arc<std::fs::File>,
        at: u64,
        left: usize,
    },
    Image(crate::store::images::Encoder),
}

impl<'a> Body<'a> {
    pub(super) fn new<M: Message>(head: &'a [u8], messages: &'a [M]) -> Self {
        use crate::store::images::{Encoder, Piece};
        let mut parts = Vec::with_capacity(messages.len() * 2 + 2);
        parts.push(Part::Bytes(b"{\"messages\":["));
        for (i, m) in messages.iter().enumerate() {
            if i > 0 {
                parts.push(Part::Bytes(b","));
            }
            let json = match m.text() {
                Text::Memory(json) => json,
                Text::Saved { file, at, len } => {
                    parts.push(Part::File { file, at, left: len });
                    continue;
                }
            };
            for piece in crate::store::images::pieces(json) {
                parts.push(match piece {
                    Piece::Text(t) => Part::Bytes(t),
                    Piece::Image(path) => Part::Image(Encoder::open(&path)),
                });
            }
        }
        parts.push(Part::Bytes(b"],"));
        parts.push(Part::Bytes(&head[1..])); // the request object without its opening brace
        Body::from_parts(parts)
    }

    pub(crate) fn from_parts(parts: Vec<Part<'a>>) -> Self {
        let len = parts
            .iter()
            .map(|p| match p {
                Part::Bytes(b) => b.len(),
                Part::Owned(b) => b.len(),
                Part::File { left, .. } | Part::Kept { left, .. } => *left,
                Part::Image(e) => e.len,
            })
            .sum();
        Body { parts, len, part: 0, offset: 0 }
    }
}

/// Reads up to `left` bytes of `file` at `at` and moves both along. The
/// length was promised up front: a file that got shorter can't be papered over.
pub(super) fn read_file(
    file: &std::fs::File,
    at: &mut u64,
    left: &mut usize,
    buf: &mut [u8],
) -> std::io::Result<usize> {
    let want = buf.len().min(*left);
    let n = crate::sys::os::read_at(file, &mut buf[..want], *at)?;
    if n == 0 && want > 0 {
        return Err(std::io::ErrorKind::UnexpectedEof.into());
    }
    *at += n as u64;
    *left -= n;
    Ok(n)
}

impl std::io::Read for Body<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        while let Some(p) = self.parts.get_mut(self.part) {
            let n = match p {
                Part::Bytes(b) => {
                    let rest = &b[self.offset..];
                    let n = rest.len().min(buf.len());
                    buf[..n].copy_from_slice(&rest[..n]);
                    self.offset += n;
                    n
                }
                Part::Owned(b) => {
                    let rest = &b[self.offset..];
                    let n = rest.len().min(buf.len());
                    buf[..n].copy_from_slice(&rest[..n]);
                    self.offset += n;
                    n
                }
                Part::File { file, at, left } => read_file(file, at, left, buf)?,
                Part::Kept { file, at, left } => read_file(file, at, left, buf)?,
                Part::Image(e) => e.read(buf)?,
            };
            if n > 0 {
                return Ok(n);
            }
            self.part += 1;
            self.offset = 0;
        }
        Ok(0)
    }
}
