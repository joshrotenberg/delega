//! Worker and run ids: opaque, unique for the life of one supervisor, never
//! reused.
//!
//! An id is a fixed word, a short per-supervisor tag and a counter. The tag
//! keeps an id from one supervisor from naming something in another, such as
//! after a restart. Callers must treat the whole string as opaque.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static SUPERVISORS: AtomicU64 = AtomicU64::new(0);

/// Hands out ids.
#[derive(Debug)]
pub(super) struct Ids {
    tag: String,
    workers: u64,
    runs: u64,
}

impl Ids {
    pub fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        let n = SUPERVISORS.fetch_add(1, Ordering::Relaxed);
        let mixed = nanos ^ n.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        Self {
            tag: format!("{:06x}", mixed & 0xff_ffff),
            workers: 0,
            runs: 0,
        }
    }

    pub fn worker(&mut self) -> String {
        self.workers += 1;
        format!("worker-{}-{}", self.tag, self.workers)
    }

    pub fn run(&mut self) -> String {
        self.runs += 1;
        format!("run-{}-{}", self.tag, self.runs)
    }
}
