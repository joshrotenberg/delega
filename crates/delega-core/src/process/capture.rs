//! Bounded capture of one output stream.

use std::borrow::Cow;

use super::limits::TAIL_TRIM_FACTOR;
use super::spec::{CaptureSpec, Keep};

/// What was kept of one output stream.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Captured {
    /// The kept bytes, at most the spec's limit.
    pub bytes: Vec<u8>,
    /// How many bytes the child wrote in all.
    pub total_bytes: u64,
    /// Whether bytes were discarded because of the limit.
    pub truncated: bool,
}

impl Captured {
    /// The kept bytes as text. Invalid UTF-8, including a character cut by the
    /// limit, is replaced with U+FFFD.
    pub fn text(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.bytes)
    }
}

/// A growing capture that never holds more than a bounded amount.
#[derive(Debug)]
pub(super) struct Bounded {
    spec: CaptureSpec,
    buffer: Vec<u8>,
    total: u64,
    dropped: bool,
}

impl Bounded {
    pub(super) fn new(spec: CaptureSpec) -> Self {
        Self {
            spec,
            buffer: Vec::new(),
            total: 0,
            dropped: false,
        }
    }

    pub(super) fn push(&mut self, chunk: &[u8]) {
        self.total += chunk.len() as u64;
        match self.spec.keep {
            Keep::Head => {
                let room = self.spec.limit.saturating_sub(self.buffer.len());
                let take = room.min(chunk.len());
                self.buffer.extend_from_slice(&chunk[..take]);
                if take < chunk.len() {
                    self.dropped = true;
                }
            }
            Keep::Tail => {
                self.buffer.extend_from_slice(chunk);
                // Trim in batches so each push stays cheap and memory stays
                // under TAIL_TRIM_FACTOR times the limit plus one chunk.
                if self.buffer.len() >= self.spec.limit.saturating_mul(TAIL_TRIM_FACTOR).max(1) {
                    self.trim_tail();
                }
            }
        }
    }

    fn trim_tail(&mut self) {
        if self.buffer.len() > self.spec.limit {
            let excess = self.buffer.len() - self.spec.limit;
            self.buffer.drain(..excess);
            self.dropped = true;
        }
    }

    #[cfg(test)]
    pub(super) fn held(&self) -> usize {
        self.buffer.len()
    }

    pub(super) fn finish(mut self) -> Captured {
        if self.spec.keep == Keep::Tail {
            self.trim_tail();
        }
        Captured {
            bytes: self.buffer,
            total_bytes: self.total,
            truncated: self.dropped,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(limit: usize, keep: Keep) -> CaptureSpec {
        CaptureSpec { limit, keep }
    }

    #[test]
    fn head_keeps_the_first_bytes_and_counts_all() {
        let mut b = Bounded::new(spec(5, Keep::Head));
        b.push(b"abc");
        b.push(b"defgh");
        let c = b.finish();
        assert_eq!(c.bytes, b"abcde");
        assert_eq!(c.total_bytes, 8);
        assert!(c.truncated);
    }

    #[test]
    fn tail_keeps_the_last_bytes_and_counts_all() {
        let mut b = Bounded::new(spec(5, Keep::Tail));
        for chunk in [&b"abc"[..], b"defgh", b"ij"] {
            b.push(chunk);
        }
        let c = b.finish();
        assert_eq!(c.bytes, b"fghij");
        assert_eq!(c.total_bytes, 10);
        assert!(c.truncated);
    }

    #[test]
    fn output_within_the_limit_is_kept_whole_and_not_truncated() {
        for keep in [Keep::Head, Keep::Tail] {
            let mut b = Bounded::new(spec(8, keep));
            b.push(b"abcd");
            let c = b.finish();
            assert_eq!(c.bytes, b"abcd");
            assert!(!c.truncated);
        }
    }

    #[test]
    fn a_tail_never_holds_more_than_the_trim_factor_times_the_limit_plus_a_chunk() {
        let limit = 1000;
        let chunk = [b'x'; 300];
        let mut b = Bounded::new(spec(limit, Keep::Tail));
        let mut peak = 0;
        for _ in 0..10_000 {
            b.push(&chunk);
            peak = peak.max(b.held());
        }
        assert!(peak < limit * TAIL_TRIM_FACTOR + chunk.len(), "peak {peak}");
        assert_eq!(b.finish().bytes.len(), limit);
    }

    #[test]
    fn a_zero_limit_keeps_nothing_in_either_mode() {
        for keep in [Keep::Head, Keep::Tail] {
            let mut b = Bounded::new(spec(0, keep));
            b.push(b"abc");
            let c = b.finish();
            assert!(c.bytes.is_empty());
            assert!(c.truncated);
            assert_eq!(c.total_bytes, 3);
        }
    }

    #[test]
    fn text_replaces_a_character_cut_by_the_limit() {
        let mut b = Bounded::new(spec(2, Keep::Head));
        b.push("aé".as_bytes());
        let c = b.finish();
        assert_eq!(c.bytes.len(), 2);
        assert_eq!(c.text(), "a\u{FFFD}");
    }
}
