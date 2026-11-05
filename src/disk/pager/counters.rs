//! \[DESIGN\] The pager's counters (RULES.md D30): what it read and wrote,
//! for the heuristics above it. Split from pager.rs (RULES.md R-SIZE-2).
//!
//! Those every read or write adds to are kept on stripes by thread, each
//! on cache lines of its own, as the buffer pool's hits are
//! (`disk/buffer/hits.rs`): threads reading at once never write one line.

use std::sync::atomic::{AtomicU64, Ordering};

use super::Pager;
use crate::sync::thread_stripe;

/// \[DESIGN\] Stripes of the counters reads and writes add to: more than
/// the threads that read at once, so they seldom share one.
const STRIPES: usize = 64;

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

/// The counters a read or a write adds to, of the threads given one
/// stripe, alone on their cache lines.
#[repr(align(128))]
#[derive(Debug, Default)]
pub(super) struct Stripe {
    pub(super) reads: AtomicU64,
    pub(super) new_reads: AtomicU64,
    pub(super) overflow_reads: AtomicU64,
    pub(super) writes: AtomicU64,
    pub(super) content_bytes: AtomicU64,
    pub(super) stored_bytes: AtomicU64,
}

/// `PagerCounts`, counted lock-free: those of every read and write by
/// stripe, the others once each.
#[derive(Debug)]
pub(super) struct Counters {
    stripes: Box<[Stripe]>,
    pub(super) overflow_pages_written: AtomicU64,
    pub(super) syncs: AtomicU64,
    pub(super) corrupt_reads: AtomicU64,
}

impl Default for Counters {
    fn default() -> Counters {
        Counters {
            stripes: (0..STRIPES).map(|_| Stripe::default()).collect(),
            overflow_pages_written: AtomicU64::new(0),
            syncs: AtomicU64::new(0),
            corrupt_reads: AtomicU64::new(0),
        }
    }
}

impl Counters {
    /// Adds `n` to the counter `which` of the calling thread's stripe.
    pub(super) fn count(&self, which: fn(&Stripe) -> &AtomicU64, n: u64) {
        if let Some(stripe) = self.stripes.get(thread_stripe(STRIPES)) {
            add(which(stripe), n);
        }
    }

    /// The sum of the counter `which` over the stripes.
    fn sum(&self, which: fn(&Stripe) -> &AtomicU64) -> u64 {
        self.stripes.iter().fold(0_u64, |n, s| n.saturating_add(which(s).load(Ordering::Relaxed)))
    }
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
            reads: c.sum(|s| &s.reads),
            new_reads: c.sum(|s| &s.new_reads),
            overflow_reads: c.sum(|s| &s.overflow_reads),
            writes: c.sum(|s| &s.writes),
            overflow_pages_written: get(&c.overflow_pages_written),
            content_bytes: c.sum(|s| &s.content_bytes),
            stored_bytes: c.sum(|s| &s.stored_bytes),
            syncs: get(&c.syncs),
            corrupt_reads: get(&c.corrupt_reads),
        }
    }
}
