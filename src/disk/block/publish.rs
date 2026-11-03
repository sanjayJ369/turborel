//! \[DESIGN\] Publishing appended blocks (RULES.md D30): an appender
//! reserves its range with one atomic step and, once its block is written,
//! publishes it when every block before it is, so readers never meet a
//! gap. An appender whose write finishes first waits for the earlier ones:
//! it checks a few times, then yields the processor a bounded number of
//! times, as an earlier write takes a few microseconds, and only then
//! parks until one of them wakes it, so a starved appender still sleeps.
//! A failed write wakes them all, to refuse their appends. Split from
//! block.rs (RULES.md R-SIZE-2).

use std::sync::PoisonError;
use std::sync::atomic::Ordering;

use super::BlockFile;
use crate::error::Result;
use crate::sync::lock_builtin;

/// \[DESIGN\] Times an appender checks whether the blocks before its own are
/// published before it yields: an earlier append in its last steps
/// publishes within a few checks, some 100 ns in all.
const SPINS_BEFORE_YIELDING: u32 = 64;

/// \[DESIGN\] Times an appender then yields the processor, checking after
/// each, before it parks. The kernel completes the writes to one file in
/// no fixed order, so an appender often waits for an earlier one still
/// writing, a few microseconds. A yield costs about a microsecond when no
/// other thread wants the processor, and lets the earlier appender run
/// when one does, so these rounds cover several such writes; a park and
/// its wake-up cost far more, tens of microseconds on a virtual machine,
/// with every later appender waiting behind them. An appender kept waiting
/// longer, behind one descheduled, parks.
const YIELDS_BEFORE_PARKING: u32 = 32;

impl BlockFile {
    /// Reserves `len` bytes after the last reservation, and returns where
    /// they start.
    pub(super) fn reserve(&self, len: usize) -> Result<u64> {
        let len = u64::try_from(len).map_err(|_| self.beyond())?;
        let mut start = self.reserved.load(Ordering::Acquire);
        loop {
            let end = start.checked_add(len).ok_or_else(|| self.beyond())?;
            match self.reserved.compare_exchange_weak(start, end, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return Ok(start),
                Err(now) => start = now,
            }
        }
    }

    /// Publishes the block written at `start..end` once every block before
    /// it is, or reports the failed write that left a gap before it. It
    /// checks, then yields, then parks until woken.
    pub(super) fn publish(&self, start: u64, end: u64, written: Result<()>) -> Result<()> {
        if let Err(e) = written {
            self.failed.store(true, Ordering::SeqCst);
            self.wake();
            return Err(e);
        }
        let (mut spins, mut yields) = (0, 0);
        loop {
            if self.failed.load(Ordering::SeqCst) {
                return Err(self.broken());
            }
            if self.end.compare_exchange(start, end, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
                self.wake();
                return Ok(());
            }
            if spins < SPINS_BEFORE_YIELDING {
                spins = spins.saturating_add(1);
                std::hint::spin_loop();
            } else if yields < YIELDS_BEFORE_PARKING {
                yields = yields.saturating_add(1);
                std::thread::yield_now();
            } else {
                self.park(start);
            }
        }
    }

    /// Waits until the published end reaches `start` or a write fails. The
    /// count of parked appenders is raised before the check, so an appender
    /// that publishes after the check sees it and wakes this one.
    fn park(&self, start: u64) {
        let mut gate = lock_builtin(&self.parking);
        self.parked.fetch_add(1, Ordering::SeqCst);
        while self.end.load(Ordering::SeqCst) != start && !self.failed.load(Ordering::SeqCst) {
            gate = self.advanced.wait(gate).unwrap_or_else(PoisonError::into_inner);
        }
        self.parked.fetch_sub(1, Ordering::SeqCst);
    }

    /// Wakes the parked appenders, if there are any, so they check again:
    /// all of them, as each waits on the one condition variable for its own
    /// start and only the one the new end reaches goes on. As appenders
    /// yield before they park, few are parked to wake.
    fn wake(&self) {
        if self.parked.load(Ordering::SeqCst) > 0 {
            drop(lock_builtin(&self.parking));
            self.advanced.notify_all();
        }
    }
}

#[cfg(test)]
mod tests;
