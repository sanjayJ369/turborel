//! \[DESIGN\] Of the zones (see the module's documentation), how the
//! maintenance thread sums them up again (RULES.md D31): from the versions
//! of their rows a reader at or after the horizon may read, a write
//! meanwhile throwing the sum away. Split from zones.rs (RULES.md
//! R-SIZE-2).

use std::ops::Bound;
use std::sync::atomic::Ordering as AtomicOrdering;

use super::super::RowTable;
use super::super::version::{Maker, Version};
use super::{MOST_SUMMED, ZONE_ROWS, Zone};
use crate::engine::rows::{key, row_id};
use crate::engine::tree::Direction;
use crate::error::Result;
use crate::storage::RowId;
use crate::sync::{lock_builtin, read};
use crate::txn::Timestamp;
use crate::value::Value;

impl RowTable {
    /// Sums up again some of the zones stale or not known, or unsettled,
    /// of the versions of their rows a reader at or after `horizon` may
    /// read, from where the last left off: whether one is now up to date,
    /// or settled. One whose rows have versions a reader may still read
    /// besides their newest stays stale, to be summed up again once none
    /// does; one of a commit too recent stays unsettled.
    pub(in crate::engine::rows) fn sum_zones(&self, horizon: Timestamp) -> Result<bool> {
        let due: Vec<(usize, u64)> = {
            let map = read(&self.zones.map)?;
            let from = self.zones.next_sum.load(AtomicOrdering::Acquire).min(map.zones.len());
            let order = (from..map.zones.len()).chain(0..from);
            let stale = order.filter_map(|at| {
                let zone = lock_builtin(map.zones.get(at)?);
                (zone.stale || zone.unsettled).then_some((at, zone.changes))
            });
            stale.take(MOST_SUMMED).collect()
        };
        let mut done = false;
        for &(at, changes) in &due {
            let summed = self.zone_of(at, horizon)?;
            let map = read(&self.zones.map)?;
            let Some(slot) = map.zones.get(at) else { continue };
            let mut zone = lock_builtin(slot);
            // A write meanwhile: summed again later.
            if zone.changes != changes {
                continue;
            }
            let (was_stale, unsettled) = (zone.stale, zone.unsettled);
            *zone = Zone { changes, unsettled, ..summed };
            map.changed(at);
            if was_stale && !summed.stale {
                self.zones.stale.fetch_sub(1, AtomicOrdering::AcqRel);
                done = true;
            }
            if unsettled && self.settle((&map, at), &mut zone, summed.seen) {
                done = true;
            }
        }
        if let Some(&(last, _)) = due.last() {
            self.zones.next_sum.store(last.saturating_add(1), AtomicOrdering::Release);
        }
        Ok(done)
    }

    /// The zone of the rows of zone `at`, summed up from their versions a
    /// reader at or after `horizon` may read: stale still if any is not
    /// its row's newest committed, as a reader of it may end. A row read
    /// again (`RowTable::unpinned`) may leave it wider, never narrower.
    /// What its rows' newest versions show of their visibility, too.
    fn zone_of(&self, at: usize, horizon: Timestamp) -> Result<Zone> {
        let first = u64::try_from(at).unwrap_or(u64::MAX).saturating_mul(ZONE_ROWS);
        let (low, high) = (key(first), key(first.saturating_add(ZONE_ROWS)));
        let kept = &self.zones.kept;
        let mut zone = Zone::empty(kept.len(), true);
        zone.stale = false;
        for entry in self.tree.scan(Bound::Included(&low[..]), Bound::Excluded(&high[..]), Direction::Forward) {
            let (k, newest) = entry?;
            let id = row_id(&k)?;
            zone.seen = zone.seen.and(self.seen_of(&newest, horizon)?);
            let mut held = |bytes: Vec<u8>, reading_at: Timestamp| {
                let live = !Version::parse(&bytes)?.deleted;
                self.hold_versions(&mut zone, id, bytes, reading_at)?;
                Ok(live)
            };
            if self.unpinned((&k, newest), horizon, &mut held)? == Some(true) {
                zone.rows = zone.rows.saturating_add(1);
            }
        }
        Ok(zone)
    }

    /// Widens `zone` to hold each version of row `id`, whose newest is
    /// `newest`, a reader at or after `horizon` may read: stale if one is
    /// not its newest.
    fn hold_versions(&self, zone: &mut Zone, id: RowId, newest: Vec<u8>, horizon: Timestamp) -> Result<()> {
        let kept = &self.zones.kept;
        let mut bytes = newest;
        loop {
            let older = {
                let version = Version::parse(&bytes)?;
                if !version.deleted {
                    let row = self.decode(id, version.record, Some(kept))?;
                    zone.hold(kept.iter().map(|&c| row.get(c).unwrap_or(&Value::Null)));
                }
                // Versions before one committed at or before the horizon
                // no reader reads.
                match self.maker(&version) {
                    Maker::Committed(stamp) => version.older.filter(|_| stamp > horizon),
                    Maker::Open(_) => version.older,
                }
            };
            zone.stale |= older.is_some();
            let Some(older) = older else { return Ok(()) };
            bytes = self.undo.read(older)?;
        }
    }
}
