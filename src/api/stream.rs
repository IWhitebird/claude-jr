// An event stream read a line at a time, with the patience a model's reply
// needs: a read can time out and be picked up again, and only a stream that
// stays silent is given up on.

use super::{Cancelled, Retryable};
use anyhow::{Result, anyhow};
use std::io::BufRead;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The `data:` lines of an event stream, read one at a time.
pub(crate) struct Lines<R> {
    r: R,
    /// A partly read line, kept across read timeouts.
    raw: Vec<u8>,
    last_byte: Instant,
}

impl<R: BufRead> Lines<R> {
    pub(crate) fn new(r: R) -> Self {
        Lines { r, raw: Vec::new(), last_byte: Instant::now() }
    }

    /// The next line's data, or None at the end of the stream. `emitted`:
    /// whether any of the reply has been shown yet (if not, a broken stream
    /// can be asked for again).
    pub(crate) fn next(&mut self, cancel: &AtomicBool, silence: Duration, emitted: bool) -> Result<Option<String>> {
        let broke = |msg: String| -> anyhow::Error { if emitted { anyhow!(msg) } else { Retryable(msg).into() } };
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(Cancelled.into());
            }
            let before = self.raw.len();
            match self.r.read_until(b'\n', &mut self.raw) {
                Ok(0) if self.raw.is_empty() => return Ok(None),
                Ok(_) => {}
                Err(e) if timed_out(&e) => {
                    if self.raw.len() != before {
                        self.last_byte = Instant::now();
                    }
                    if self.last_byte.elapsed() < silence {
                        continue;
                    }
                    return Err(broke(format!("the stream went silent for {}s", silence.as_secs())));
                }
                Err(e) => return Err(broke(format!("the connection broke: {e}"))),
            }
            self.last_byte = Instant::now();
            if self.raw.last() != Some(&b'\n') {
                // end of stream in the middle of a line
                self.raw.clear();
                return Ok(None);
            }
            let line = String::from_utf8_lossy(&self.raw).into_owned();
            self.raw.clear();
            if let Some(data) = line.trim_end_matches(['\n', '\r']).strip_prefix("data:") {
                return Ok(Some(data.trim_start().to_string()));
            }
        }
    }
}

/// A read that hit its time limit. ureq reports its own timeouts as
/// `ErrorKind::Other` wrapping `ureq::Error::Timeout`.
pub(super) fn timed_out(e: &std::io::Error) -> bool {
    use std::io::ErrorKind::{TimedOut, WouldBlock};
    matches!(e.kind(), TimedOut | WouldBlock)
        || e.get_ref()
            .and_then(|i| i.downcast_ref::<ureq::Error>())
            .is_some_and(|u| matches!(u, ureq::Error::Timeout(_)))
}
