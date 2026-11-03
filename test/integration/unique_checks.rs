//! UNIQUE checked after a statement [X3.135-1986 §4.5, §6.4 GR2, §8.12]:
//! the checks an UPDATE makes, counted through an index method; and the
//! planner's estimates of a unique key's probes, counted likewise
//! (RULES.md D35).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use turborel::db::Database;
use turborel::error::{self, Code};
use turborel::index::{BTreeMethod, Entries, Index, IndexMethod, Key, KeyOrder, Probe};
use turborel::params::Params;
use turborel::storage::RowId;
use turborel::value::{CompOp, DataType};

use crate::common::{exec, rows};

/// The built-in ordered index method, counting the UNIQUE checks made
/// through its indexes (`Index::rows`), and the planner's estimates asked
/// of them (`Index::estimate`).
struct Counting {
    inner: BTreeMethod,
    checks: Arc<AtomicUsize>,
    estimates: Arc<AtomicUsize>,
}

/// An index of `Counting`.
struct CountingIndex {
    inner: Arc<dyn Index>,
    checks: Arc<AtomicUsize>,
    estimates: Arc<AtomicUsize>,
}

impl IndexMethod for Counting {
    fn name(&self) -> &'static str {
        self.inner.name()
    }
    fn supports(&self, op: CompOp) -> bool {
        self.inner.supports(op)
    }
    fn ordered(&self) -> bool {
        self.inner.ordered()
    }
    fn create(&self, columns: &[DataType]) -> error::Result<Box<dyn Index>> {
        let (checks, estimates) = (Arc::clone(&self.checks), Arc::clone(&self.estimates));
        Ok(Box::new(CountingIndex { inner: Arc::from(self.inner.create(columns)?), checks, estimates }))
    }
}

impl Index for CountingIndex {
    fn insert(&self, key: Key, row: RowId) -> error::Result<()> {
        self.inner.insert(key, row)
    }
    fn insert_alone(&self, key: Key, row: RowId) -> error::Result<bool> {
        self.inner.insert_alone(key, row)
    }
    fn remove(&self, key: &Key, row: RowId) -> error::Result<()> {
        self.inner.remove(key, row)
    }
    fn rows(&self, key: &Key) -> error::Result<Vec<RowId>> {
        self.checks.fetch_add(1, Ordering::Relaxed);
        self.inner.rows(key)
    }
    fn estimate(&self, probe: &Probe) -> Option<usize> {
        self.estimates.fetch_add(1, Ordering::Relaxed);
        self.inner.estimate(probe)
    }
    fn search(self: Arc<Self>, probe: &Probe, order: KeyOrder) -> Entries {
        Arc::clone(&self.inner).search(probe, order)
    }
}

/// Verifies: [X3.135-1986 §4.5, §6.4 GR2, §8.12] UPDATE enforces
/// UNIQUE after the statement: keys may pass through duplicates (SET K =
/// K + 1) but not end with them, a constraint of two columns checked when
/// either is assigned; [DESIGN] an UPDATE that assigns no column of a
/// constraint does not check it, as it leaves every key as it was, nor a
/// key whose entry was its only one as it was inserted
/// (`Index::insert_alone`).
#[test]
fn update_checks_the_unique_constraints_it_could_break_8_12() {
    let checks = Arc::new(AtomicUsize::new(0));
    let db = Database::new();
    let counting = Counting { inner: BTreeMethod::in_memory(), checks: Arc::clone(&checks), estimates: Arc::default() };
    db.register_index_method(Box::new(counting)).unwrap();
    db.schema(
        "CREATE SCHEMA AUTHORIZATION APP
           CREATE TABLE T (K INTEGER NOT NULL UNIQUE, V INTEGER)
           CREATE TABLE U (A INTEGER NOT NULL, B INTEGER NOT NULL, V INTEGER, UNIQUE (A, B))",
    )
    .unwrap();
    let s = db.session("APP");
    for k in 1..=3 {
        s.run(&format!("INSERT INTO T VALUES ({k}, 0)")).unwrap();
        s.run(&format!("INSERT INTO U VALUES ({k}, {k}, 0)")).unwrap();
    }
    let made = |sql: &str| {
        let before = checks.load(Ordering::Relaxed);
        let outcome = exec(&s, sql).map(|_| ());
        (outcome, checks.load(Ordering::Relaxed) - before)
    };
    assert_eq!(made("UPDATE T SET V = V + 1"), (Ok(()), 0), "no key assigned");
    assert_eq!(made("UPDATE U SET V = 7 WHERE A = 2"), (Ok(()), 0));
    assert_eq!(made("UPDATE T SET K = K + 1").0, Ok(()), "through duplicates, ending with none");
    // 2, 3 and 4 to 3, 4 and 5: the entries of 3 and 4 not their keys'
    // only ones, the old versions' there; 5's is, and needs no check.
    assert!(made("UPDATE T SET K = K + 1").1 >= 2, "each key of another row's entry checked");
    assert_eq!(made("UPDATE T SET K = 9").0, Err(Code::Constraint));
    let (outcome, made_checks) = made("UPDATE U SET A = 1 WHERE B = 2");
    assert_eq!((outcome, made_checks), (Ok(()), 0), "(2, 2) to (1, 2): one column assigned, its entry its key's only one");
    assert_eq!(made("UPDATE U SET B = 1 WHERE A = 1 AND B = 2").0, Err(Code::Constraint), "(1, 2) to (1, 1), a duplicate");
    assert_eq!(rows(&s, "SELECT K FROM T ORDER BY K"), Ok(vec!["3".into(), "4".into(), "5".into()]));
}

/// The plan of `sql`, its index's name put as `N_K`, and the estimates
/// asked of indexes as it was planned.
fn planned(s: &turborel::db::Session, estimates: &AtomicUsize, sql: &str) -> (String, usize) {
    let before = estimates.load(Ordering::Relaxed);
    let plan = s.physical_plan(sql, &Params::new()).unwrap();
    let asked = estimates.load(Ordering::Relaxed) - before;
    let name = plan.split("using ").nth(1).and_then(|rest| rest.split(' ').next()).unwrap_or_default().to_string();
    (plan.replace(&name, "N_K"), asked)
}

/// Verifies: [DESIGN] RULES.md D35, index costs: an equality on the key
/// of a UNIQUE constraint's index is priced at one row, the index's
/// estimate not asked, and planned as it was: the plan, its estimated
/// rows and cost those of the same query of a table of the same rows by
/// an index of the same column that is not unique, whose estimate is
/// asked and counts one. MC/DC pairs of the decision (unique, a whole-key
/// equality): the index not unique, the equality the same, asks; the
/// index unique, a range on it, asks. The rows are the same each way.
#[test]
fn an_equality_on_a_unique_key_is_priced_without_asking_its_index() {
    let estimates = Arc::new(AtomicUsize::new(0));
    let db = Database::new();
    let counting = Counting { inner: BTreeMethod::in_memory(), checks: Arc::default(), estimates: Arc::clone(&estimates) };
    db.register_index_method(Box::new(counting)).unwrap();
    db.schema(
        "CREATE SCHEMA AUTHORIZATION APP
           CREATE TABLE U (K INTEGER NOT NULL UNIQUE, V INTEGER)
           CREATE TABLE N (K INTEGER NOT NULL, V INTEGER)",
    )
    .unwrap();
    let s = db.session("APP");
    let values: Vec<String> = (0..1_000).map(|k| format!("({k}, {k})")).collect();
    for table in ["U", "N"] {
        s.run(&format!("INSERT INTO {table} VALUES {}", values.join(", "))).unwrap();
    }
    s.commit().unwrap();
    db.create_index("N_K", "APP.N", &["K"], "btree").unwrap();
    let (unique, unasked) = planned(&s, &estimates, "SELECT V FROM U WHERE K = 500");
    let (other, asked) = planned(&s, &estimates, "SELECT V FROM N WHERE K = 500");
    assert_eq!((unasked, asked), (0, 1), "the unique key's equality not asked; the other's asked");
    assert!(unique.contains("est. rows=1,"), "{unique}");
    assert_eq!(unique.replace("APP.U", "APP.N").replace("U.", "N."), other, "the same plan, rows and cost");
    let (_, ranged) = planned(&s, &estimates, "SELECT V FROM U WHERE K > 990");
    assert_eq!(ranged, 1, "a range on the unique key asked");
    for sql in ["SELECT V FROM U WHERE K = 500", "SELECT V FROM U WHERE K > 997 ORDER BY V"] {
        assert_eq!(rows(&s, sql), rows(&s, &sql.replace("FROM U", "FROM N")), "{sql}");
    }
    assert_eq!(rows(&s, "SELECT V FROM U WHERE K = 500"), Ok(vec!["500".into()]));
}
