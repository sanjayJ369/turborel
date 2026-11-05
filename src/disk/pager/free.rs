//! \[DESIGN\] The pager's free list (RULES.md D30), in memory: the free pages
//! as runs of consecutive pages, merged when they touch, and the number of
//! pages in the file. It does no I/O; the pager persists it on trunk pages
//! at `sync` and loads it on open.
//!
//! Allocation uses free pages before it grows the file, so the file stays
//! as small as its content allows, and prefers consecutive pages, so
//! overflowing content is read with few, large reads: the smallest free run
//! that holds the request; failing that, if the free pages hold it, free
//! runs largest first; failing that, every free run, the one at the end of
//! the file (if there is one) last and grown, so the new pages continue it.
//!
//! The pager keeps it behind a lock that reads of pages take shared, to
//! check the page read is not free, and its page count beside it, set by
//! every change as the lock is let go, so a read checks its page is in the
//! file with no lock (`Shared`).

use std::collections::BTreeMap;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use super::{Extent, PageId};

/// \[DESIGN\] The pager's free list, shared between its threads: reads of
/// pages look at it together, and only allocating and freeing change it.
/// Its page count is kept in an atomic too, read with no lock. No plug-in
/// is called while it is locked, so a poisoned lock is used as it is.
pub(super) struct Shared {
    list: RwLock<FreeList>,
    pages: AtomicU64,
}

/// The free list, locked to change; its page count is set as it is let go.
pub(super) struct Changing<'a> {
    list: RwLockWriteGuard<'a, FreeList>,
    pages: &'a AtomicU64,
}

impl Shared {
    /// Shares `list`.
    pub(super) fn new(list: FreeList) -> Shared {
        let pages = AtomicU64::new(list.pages());
        Shared { list: RwLock::new(list), pages }
    }

    /// Pages in the file, with no lock: as the last change let go left it.
    pub(super) fn pages(&self) -> u64 {
        self.pages.load(Ordering::Acquire)
    }

    /// The list, locked shared, to look at.
    pub(super) fn read(&self) -> RwLockReadGuard<'_, FreeList> {
        self.list.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// The list, locked exclusively, to change.
    pub(super) fn write(&self) -> Changing<'_> {
        Changing { list: self.list.write().unwrap_or_else(PoisonError::into_inner), pages: &self.pages }
    }
}

impl Deref for Changing<'_> {
    type Target = FreeList;
    fn deref(&self) -> &FreeList {
        &self.list
    }
}

impl DerefMut for Changing<'_> {
    fn deref_mut(&mut self) -> &mut FreeList {
        &mut self.list
    }
}

impl Drop for Changing<'_> {
    /// Sets the page count while the lock is still held, so it changes in
    /// the order the list does.
    fn drop(&mut self) {
        self.pages.store(self.list.pages(), Ordering::Release);
    }
}

/// The free runs, by first page, the pages in the file, and whether either
/// changed since the list was last recorded.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct FreeList {
    runs: BTreeMap<u64, u64>,
    pages: u64,
    changed: bool,
}

impl FreeList {
    /// A file of `pages` pages, all in use.
    pub(super) fn new(pages: u64) -> FreeList {
        FreeList { runs: BTreeMap::new(), pages, changed: false }
    }

    /// Pages in the file.
    pub(super) fn pages(&self) -> u64 {
        self.pages
    }

    /// Free pages.
    pub(super) fn free_pages(&self) -> u64 {
        self.runs.values().fold(0, |sum, &count| sum.saturating_add(count))
    }

    /// The free runs, in page order.
    pub(super) fn runs(&self) -> Vec<Extent> {
        self.runs.iter().map(|(&start, &count)| Extent { start: PageId(start), count }).collect()
    }

    /// Whether page `id` is free.
    pub(super) fn is_free(&self, id: u64) -> bool {
        self.runs.range(..=id).next_back().is_some_and(|(&start, &count)| id < start.saturating_add(count))
    }

    /// Whether the list changed since `recorded` was last called.
    pub(super) fn changed(&self) -> bool {
        self.changed
    }

    /// Notes that the list as it is now is recorded.
    pub(super) fn recorded(&mut self) {
        self.changed = false;
    }

    /// Notes that the list differs from its record, which failed.
    pub(super) fn mark_changed(&mut self) {
        self.changed = true;
    }

    /// Frees `extents`, which the current record already lists as free, so
    /// the list does not differ from its record because of them.
    pub(super) fn free_recorded(&mut self, extents: &[Extent]) {
        let changed = self.changed;
        // The record took them from no list, so they cannot overlap.
        let _ = self.free(extents);
        self.changed = changed;
    }

    /// Takes `n` pages: consecutive where it can.
    pub(super) fn allocate(&mut self, n: u64) -> Vec<Extent> {
        if n == 0 {
            return Vec::new();
        }
        self.changed = true;
        if let Some((&start, _)) = self.runs.iter().filter(|&(_, &count)| count >= n).min_by_key(|&(_, &count)| count) {
            return vec![self.take(start, n)];
        }
        let enough = self.free_pages() >= n;
        let trailing =
            self.runs.iter().next_back().filter(|&(&start, &count)| !enough && start.saturating_add(count) == self.pages).map(|(&s, _)| s);
        let mut runs: Vec<(u64, u64)> = self.runs.iter().filter(|&(&start, _)| Some(start) != trailing).map(|(&s, &c)| (s, c)).collect();
        runs.sort_by_key(|&(start, count)| (std::cmp::Reverse(count), start));
        let mut extents = Vec::new();
        let mut needed = n;
        for (start, count) in runs {
            if needed == 0 {
                break;
            }
            let taken = count.min(needed);
            extents.push(self.take(start, taken));
            needed = needed.saturating_sub(taken);
        }
        if needed > 0 {
            // The free run at the end of the file, if any, grown by the rest.
            let start = trailing.map_or(self.pages, |t| {
                self.runs.remove(&t);
                t
            });
            self.pages = start.saturating_add(needed);
            extents.push(Extent { start: PageId(start), count: needed });
        }
        extents
    }

    /// Takes the first `n` pages of the run at `start`.
    fn take(&mut self, start: u64, n: u64) -> Extent {
        if let Some(count) = self.runs.remove(&start)
            && count > n
        {
            self.runs.insert(start.saturating_add(n), count.saturating_sub(n));
        }
        Extent { start: PageId(start), count: n }
    }

    /// Returns `extents` to the free list.
    ///
    /// # Errors
    ///
    /// Why, if an extent is empty, reaches past the file, or overlaps a
    /// page already free; nothing is freed then.
    pub(super) fn free(&mut self, extents: &[Extent]) -> Result<(), String> {
        self.check_free(extents)?;
        for e in extents {
            // Checked, so none fails: freeing them in turn finds what
            // checking did.
            self.free_one(*e)?;
        }
        self.changed = self.changed || !extents.is_empty();
        Ok(())
    }

    /// Checks that `extents` can be freed, with nothing changed or copied:
    /// each within the file, overlapping no free page and none of the
    /// extents before it. Each is checked against the free runs and against
    /// those before it apart, as freeing them in turn would find them in one
    /// list.
    ///
    /// # Errors
    ///
    /// As `free`'s, of the first extent freeing them in turn would refuse.
    pub(super) fn check_free(&self, extents: &[Extent]) -> Result<(), String> {
        // Only those before the last are looked up again.
        let mut before = BTreeMap::new();
        for (n, e) in extents.iter().enumerate() {
            let end = self.end_within(*e)?;
            let start = e.start.0;
            if overlaps(&self.runs, start, end) || overlaps(&before, start, end) {
                return Err(includes_free(start, end));
            }
            if n.saturating_add(1) < extents.len() {
                before.insert(start, e.count);
            }
        }
        Ok(())
    }

    /// The page after `e`, if it is not empty and lies within the file.
    fn end_within(&self, e: Extent) -> Result<u64, String> {
        let (start, count) = (e.start.0, e.count);
        start
            .checked_add(count)
            .filter(|&end| count > 0 && end <= self.pages)
            .ok_or_else(|| format!("{count} pages from page {start} are not within the {} pages of the file", self.pages))
    }

    fn free_one(&mut self, e: Extent) -> Result<(), String> {
        let (start, count) = (e.start.0, e.count);
        let end = self.end_within(e)?;
        if overlaps(&self.runs, start, end) {
            return Err(includes_free(start, end));
        }
        let before = self.runs.range(..start).next_back().map(|(&s, &c)| (s, s.saturating_add(c)));
        let mut run = (start, count);
        if let Some((before_start, before_end)) = before.filter(|&(_, before_end)| before_end == start) {
            self.runs.remove(&before_start);
            run = (before_start, before_end.saturating_sub(before_start).saturating_add(count));
        }
        if let Some(after_count) = self.runs.remove(&end) {
            run.1 = run.1.saturating_add(after_count);
        }
        self.runs.insert(run.0, run.1);
        Ok(())
    }

    /// Takes the free run that ends the file, if there is one, so that no
    /// allocation uses its pages while the file is cut to its start. The
    /// page count stays until `release_trailing`.
    pub(super) fn take_trailing(&mut self) -> Option<Extent> {
        let (&start, &count) = self.runs.iter().next_back().filter(|&(&start, &count)| start.saturating_add(count) == self.pages)?;
        self.runs.remove(&start);
        Some(Extent { start: PageId(start), count })
    }

    /// Ends what `take_trailing` began: if `run` still ends the file, the
    /// file now ends where it starts; otherwise (pages were allocated past
    /// it meanwhile) it is free again.
    pub(super) fn release_trailing(&mut self, run: Extent) {
        self.changed = true;
        if run.start.0.saturating_add(run.count) == self.pages {
            self.pages = run.start.0;
        } else {
            // It was taken from this list, so returning it cannot overlap.
            let _ = self.free_one(run);
        }
    }

    /// Grows the file to `pages` pages, the new ones free.
    pub(super) fn grow_free(&mut self, pages: u64) -> Result<(), String> {
        if pages > self.pages {
            self.changed = true;
            let old = self.pages;
            self.pages = pages;
            self.free_one(Extent { start: PageId(old), count: pages.saturating_sub(old) })?;
        }
        Ok(())
    }
}

/// Whether pages `start` to `end`, less one, include one of `runs`, runs
/// by first page that do not overlap one another.
fn overlaps(runs: &BTreeMap<u64, u64>, start: u64, end: u64) -> bool {
    let overlaps_before = runs.range(..start).next_back().is_some_and(|(&s, &c)| s.saturating_add(c) > start);
    let overlaps_after = runs.range(start..end).next().is_some();
    overlaps_before || overlaps_after
}

/// Why pages `start` to `end`, less one, cannot be freed: they include a
/// free page.
fn includes_free(start: u64, end: u64) -> String {
    format!("pages from {start} to {} include a free page", end.saturating_sub(1))
}

// Unit tests: allocation and freeing, which do no I/O (RULES.md R-TEST-1).
#[cfg(test)]
mod tests {
    use super::*;

    fn extent(start: u64, count: u64) -> Extent {
        Extent { start: PageId(start), count }
    }

    /// Verifies: [DESIGN] RULES.md D30, free list: with no free pages, pages
    /// come from the end of the file; freed pages merge with touching runs,
    /// before and after; the smallest run that fits is used, and what is
    /// left of it stays free.
    #[test]
    fn allocation_prefers_the_smallest_run_that_fits() {
        let mut f = FreeList::new(0);
        assert_eq!(f.allocate(10), vec![extent(0, 10)]);
        assert_eq!(f.allocate(0), vec![]);
        f.free(&[extent(2, 2), extent(6, 3), extent(4, 1)]).unwrap();
        assert_eq!(f.runs(), vec![extent(2, 3), extent(6, 3)], "4 joined 2-3; 5 still in use");
        f.free(&[extent(5, 1)]).unwrap();
        assert_eq!(f.runs(), vec![extent(2, 7)], "5 joined both");
        f.free(&[extent(0, 2)]).unwrap();
        assert_eq!((f.runs(), f.free_pages()), (vec![extent(0, 9)], 9));
        let mut g = FreeList::new(20);
        g.free(&[extent(0, 5), extent(10, 3)]).unwrap();
        assert_eq!(g.allocate(3), vec![extent(10, 3)], "the smallest run that fits");
        assert_eq!(g.allocate(2), vec![extent(0, 2)]);
        assert_eq!(g.runs(), vec![extent(2, 3)]);
        assert!(g.is_free(3) && !g.is_free(5) && !g.is_free(0));
    }

    /// Verifies: [DESIGN] RULES.md D30, free list: a request no run holds is
    /// made of free runs, largest first, while the free pages hold it, even
    /// with a free run at the end of the file; when they do not, every free
    /// run is used, the one at the end last and grown, so the file grows
    /// only by what no free page could hold.
    #[test]
    fn large_requests_grow_the_last_run_or_gather_large_runs() {
        let mut f = FreeList::new(30);
        f.free(&[extent(26, 4)]).unwrap();
        assert_eq!(f.allocate(6), vec![extent(26, 6)], "the run at the end, grown");
        assert_eq!(f.pages(), 32);
        let mut h = FreeList::new(30);
        h.free(&[extent(0, 3), extent(10, 3), extent(29, 1)]).unwrap();
        assert_eq!(h.allocate(5), vec![extent(0, 3), extent(10, 2)], "enough free pages: the file does not grow");
        assert_eq!(h.allocate(4), vec![extent(12, 1), extent(29, 3)], "not enough: the run at the end, last, grown");
        assert_eq!((h.pages(), h.free_pages()), (32, 0));
        let mut g = FreeList::new(50);
        g.free(&[extent(0, 8), extent(10, 12), extent(30, 2)]).unwrap();
        assert_eq!(g.allocate(25), vec![extent(10, 12), extent(0, 8), extent(30, 2), extent(50, 3)]);
        assert_eq!((g.runs(), g.pages()), (vec![], 53), "every free page used first");
    }

    /// Verifies: [DESIGN] RULES.md D30, free list: freeing an empty extent,
    /// one past the end of the file, or one overlapping a free page (a double
    /// free, from before or after) is refused and frees nothing; the free
    /// run that ends the file is taken while the file is cut, then the file
    /// ends at its start, unless pages were allocated past it meanwhile, when
    /// it is free again; growing adds free pages.
    #[test]
    fn invalid_frees_are_refused_and_the_end_is_trimmed() {
        let mut f = FreeList::new(10);
        f.free(&[extent(4, 2)]).unwrap();
        for bad in [extent(1, 0), extent(9, 2), extent(5, 1), extent(3, 2), extent(0, 10)] {
            assert!(f.free(&[extent(0, 1), bad]).is_err(), "{bad:?}");
            assert_eq!(f.runs(), vec![extent(4, 2)], "{bad:?}: nothing freed");
        }
        f.free(&[extent(6, 4)]).unwrap();
        let trailing = f.take_trailing().unwrap();
        assert_eq!((trailing, f.runs(), f.pages()), (extent(4, 6), vec![], 10));
        assert_eq!(f.allocate(2), vec![extent(10, 2)], "the taken run is not allocated");
        f.release_trailing(trailing);
        assert_eq!((f.runs(), f.pages()), (vec![extent(4, 6)], 12), "free again: pages were allocated past it");
        f.free(&[extent(10, 2)]).unwrap();
        let last = f.take_trailing().unwrap();
        f.release_trailing(last);
        assert_eq!((f.runs(), f.pages(), f.take_trailing()), (vec![], 4, None));
        f.grow_free(7).unwrap();
        assert_eq!((f.pages(), f.runs()), (7, vec![extent(4, 3)]));
        f.grow_free(5).unwrap();
        assert_eq!(f.pages(), 7, "never shrinks");
    }

    /// Verifies: [DESIGN] RULES.md D30, free list: extents of one call
    /// that overlap one another, either first, are refused as a double free
    /// is, with the same reason, and nothing is freed; ones that touch are
    /// freed and merged; checking alone frees nothing. MC/DC of the overlap
    /// decision (`runs || before`): (free run, none before) refused; (no
    /// free run, one before) refused; (neither) freed.
    #[test]
    fn extents_overlapping_within_one_call_are_refused() {
        let mut f = FreeList::new(20);
        f.free(&[extent(10, 2)]).unwrap();
        for (bad, why) in [
            (vec![extent(0, 3), extent(2, 2)], "pages from 2 to 3 include a free page"),
            (vec![extent(2, 2), extent(0, 3)], "pages from 0 to 2 include a free page"),
            (vec![extent(0, 1), extent(5, 1), extent(5, 1)], "pages from 5 to 5 include a free page"),
            (vec![extent(0, 1), extent(11, 3)], "pages from 11 to 13 include a free page"),
            (vec![extent(0, 1), extent(19, 2)], "2 pages from page 19 are not within the 20 pages of the file"),
        ] {
            assert_eq!(f.check_free(&bad), Err(why.to_owned()), "{bad:?}");
            assert_eq!(f.free(&bad), Err(why.to_owned()), "{bad:?}");
            assert_eq!(f.runs(), vec![extent(10, 2)], "{bad:?}: nothing freed");
        }
        f.check_free(&[extent(0, 2), extent(2, 2)]).unwrap();
        assert_eq!(f.runs(), vec![extent(10, 2)], "checking frees nothing");
        f.free(&[extent(2, 2), extent(0, 2), extent(12, 1)]).unwrap();
        assert_eq!(f.runs(), vec![extent(0, 4), extent(10, 3)]);
        assert!(f.free(&[]).is_ok());
    }
}
