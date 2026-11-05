//! How fast the maintenance thread sums up a row store table's zones
//! (RULES.md D31, `engine/rows/zones.rs`): after rows are inserted, which
//! leaves each zone unsettled, its rows' visibility to settle, and after
//! every row is updated, which leaves each zone stale, to be narrowed from
//! its rows. The timed part is `Database::maintain`, its jobs run by the
//! program alone (JOBS.POLICY MANUAL), so no background round does the
//! work before it. Run with `cargo xtask bench`, on the harness (RULES.md
//! R-BENCH-1).

#![allow(
    clippy::unwrap_used,
    clippy::as_conversions,
    clippy::arithmetic_side_effects,
    clippy::integer_division,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    missing_docs,
    reason = "benchmark code: a failure ends the run (RULES.md R-DEV-3)"
)]

mod harness;

use harness::{Bench, Case, scatter};
use turborel::db::{Database, Session};
use turborel::threads::JobPolicy;

/// Rows of the table, and those each INSERT of its setup writes.
const ROWS: u64 = 200_000;
const PER_INSERT: u64 = 1_000;

fn main() {
    let bench = Bench::new("maintenance");
    bench.measure_fresh(
        Case::new("sum up zones after inserting 200,000 rows", "rows"),
        || inserted().0,
        |db| {
            db.maintain().unwrap();
            ROWS
        },
    );
    bench.measure_fresh(
        Case::new("sum up zones after updating every row", "rows"),
        || {
            let (db, s) = inserted();
            s.run("UPDATE T SET N = N + 1").unwrap();
            s.commit().unwrap();
            db
        },
        |db| {
            db.maintain().unwrap();
            ROWS
        },
    );
}

/// A database of table T, of an INTEGER, a CHAR(20) and a DOUBLE
/// PRECISION, of `ROWS` rows of scattered keys inserted in one transaction
/// and committed, its maintenance not yet done; and the session that
/// inserted them.
fn inserted() -> (Database, Session) {
    let db = Database::new();
    db.set_job_policy(JobPolicy::Manual).unwrap();
    db.schema("CREATE SCHEMA AUTHORIZATION B CREATE TABLE T (K INTEGER, NAME CHAR(20), N DOUBLE PRECISION)").unwrap();
    let s = db.session("B");
    for first in (0..ROWS).step_by(PER_INSERT as usize) {
        let rows: Vec<String> = (first..first + PER_INSERT)
            .map(|n| {
                let k = scatter(n, ROWS);
                format!("({k}, 'name {k}', {k}.5)")
            })
            .collect();
        s.run(&format!("INSERT INTO T VALUES {}", rows.join(", "))).unwrap();
    }
    s.commit().unwrap();
    (db, s)
}
