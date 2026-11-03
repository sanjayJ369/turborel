//! \[DESIGN\] The pager's counters (RULES.md D30): what it read and wrote,
//! for the heuristics above it. Split from pager.rs (RULES.md R-SIZE-2).

use std::sync::atomic::{AtomicU64, Ordering};

use super::Pager;

/// The counters of a pager, for the heuristics above it (RULES.md D30).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PagerCounts {
    /// Pages read with `read`.
    pub reads: u64,
    /// Of those, never written: new, with no content.
    pub new_reads: u64,
    /// Of those, whose content overflowed.
    pub overflow_reads: u64,
    /// Pages written with `write`.
    pub writes: u64,
    /// Overflow and directory pages written for them.
    pub overflow_pages_written: u64,
    /// Content bytes written with `write`.
    pub content_bytes: u64,
    /// Bytes they took once encoded: with compression, fewer.
    pub stored_bytes: u64,
    /// Syncs.
    pub syncs: u64,
    /// Pages read that failed their checksum or held another page's number.
    pub corrupt_reads: u64,
}

/// `PagerCounts`, counted lock-free.
#[derive(Debug, Default)]
pub(super) struct Counters {
    pub(super) reads: AtomicU64,
    pub(super) new_reads: AtomicU64,
    pub(super) overflow_reads: AtomicU64,
    pub(super) writes: AtomicU64,
    pub(super) overflow_pages_written: AtomicU64,
    pub(super) content_bytes: AtomicU64,
    pub(super) stored_bytes: AtomicU64,
    pub(super) syncs: AtomicU64,
    pub(super) corrupt_reads: AtomicU64,
}

/// Adds `n` to `counter`; it would wrap only after 2^64, which no file reaches.
pub(super) fn add(counter: &AtomicU64, n: u64) {
    counter.fetch_add(n, Ordering::Relaxed);
}

impl Pager {
    /// The counters now.
    pub fn counts(&self) -> PagerCounts {
        let get = |c: &AtomicU64| c.load(Ordering::Relaxed);
        let c = &self.counters;
        PagerCounts {
            reads: get(&c.reads),
            new_reads: get(&c.new_reads),
            overflow_reads: get(&c.overflow_reads),
            writes: get(&c.writes),
            overflow_pages_written: get(&c.overflow_pages_written),
            content_bytes: get(&c.content_bytes),
            stored_bytes: get(&c.stored_bytes),
            syncs: get(&c.syncs),
            corrupt_reads: get(&c.corrupt_reads),
        }
    }
}
