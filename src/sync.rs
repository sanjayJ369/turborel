//! \[DESIGN\] Locks on state that threads share (RULES.md D27, R-ERR-4).
//!
//! A `std` lock is poisoned when a thread panics while holding it: the data
//! it guards may be half changed. Production code does not panic (R-ERR-1),
//! so poisoning means a plug-in panicked. Taking a poisoned lock is reported
//! as SQLCODE -910 rather than unwrapped, so the database refuses to go on
//! with state it cannot trust.
//!
//! And a condition variable that skips the wake-up when no thread waits
//! (`Signal`), the gate checkpoints shut (`Gate`), and the stripe of each
//! thread (`thread_stripe`).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, LockResult, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard, WaitTimeoutResult};
use std::time::Duration;

use crate::error::{Code, SqlError};

/// The error for a lock whose holder panicked.
fn poisoned<T>(_: PoisonError<T>) -> SqlError {
    SqlError::new(Code::Api, "[DESIGN] a thread panicked while it held shared state, which may be inconsistent (RULES.md D27)")
}

/// Locks `mutex`.
///
/// # Errors
///
/// SQLCODE -910 if a thread panicked while holding it.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, SqlError> {
    mutex.lock().map_err(poisoned)
}

/// Takes `lock` for reading.
///
/// # Errors
///
/// SQLCODE -910 if a thread panicked while writing it.
pub(crate) fn read<T>(lock: &RwLock<T>) -> Result<RwLockReadGuard<'_, T>, SqlError> {
    lock.read().map_err(poisoned)
}

/// Takes `lock` for writing.
///
/// # Errors
///
/// SQLCODE -910 if a thread panicked while writing it.
pub(crate) fn write<T>(lock: &RwLock<T>) -> Result<RwLockWriteGuard<'_, T>, SqlError> {
    lock.write().map_err(poisoned)
}

/// Locks a mutex of a built-in engine or index that runs no plug-in code
/// while holding it, so no panic can poison it; if one somehow did, the data
/// is used as it is. For methods whose signature cannot report an error.
pub(crate) fn lock_builtin<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// \[DESIGN\] A condition variable that knows whether a thread waits on it
/// (RULES.md D27). `std`'s `Condvar::notify_all` and `notify_one` make a
/// system call every time, though most notifications here (a node unlocked,
/// a log append finished, a table lock released) find no thread waiting:
/// a `Signal` counts its waiters, and a notification with none returns at
/// once.
///
/// No notification is lost, by this invariant: a waiter raises the count
/// while it holds the mutex it waits with, after checking the state it
/// waits for and before `Condvar::wait` releases the mutex; and a notifier
/// changes that state under the same mutex, or takes the mutex after
/// changing it, before it reads the count. So either the notifier's
/// critical section comes first, and the waiter sees the state changed
/// and does not wait; or the waiter's comes first, and its count, raised
/// before it released the mutex, is seen by the notifier, which then makes
/// the call. A waiter lowers the count once it has the mutex back.
#[derive(Default)]
pub(crate) struct Signal {
    condvar: Condvar,
    /// Threads in `wait`, `wait_while` or `wait_timeout`.
    waiters: AtomicUsize,
}

impl Signal {
    /// A signal no thread waits on.
    pub(crate) const fn new() -> Signal {
        Signal { condvar: Condvar::new(), waiters: AtomicUsize::new(0) }
    }

    /// Counts the calling thread as waiting, for the time it waits. It holds
    /// the mutex, so the count is raised before the mutex is released.
    fn waiting<R>(&self, wait: impl FnOnce(&Condvar) -> R) -> R {
        // Bounded by the threads waiting at once, so it cannot wrap.
        self.waiters.fetch_add(1, Ordering::SeqCst);
        let woken = wait(&self.condvar);
        // Lowers only what this thread raised, so it cannot wrap either.
        self.waiters.fetch_sub(1, Ordering::SeqCst);
        woken
    }

    /// `Condvar::wait`, counted: waits on `guard`'s mutex until notified,
    /// or spuriously; the caller checks its state again.
    ///
    /// # Errors
    ///
    /// The guard, poisoned, if a thread panicked while holding the mutex,
    /// as `Condvar::wait` gives it.
    pub(crate) fn wait<'a, T>(&self, guard: MutexGuard<'a, T>) -> LockResult<MutexGuard<'a, T>> {
        self.waiting(|condvar| condvar.wait(guard))
    }

    /// `Condvar::wait_while`, counted: waits while `condition` holds.
    ///
    /// # Errors
    ///
    /// The guard, poisoned, if a thread panicked while holding the mutex,
    /// as `Condvar::wait_while` gives it.
    pub(crate) fn wait_while<'a, T, F>(&self, guard: MutexGuard<'a, T>, condition: F) -> LockResult<MutexGuard<'a, T>>
    where
        F: FnMut(&mut T) -> bool,
    {
        self.waiting(|condvar| condvar.wait_while(guard, condition))
    }

    /// `Condvar::wait_timeout`, counted: waits until notified, spuriously,
    /// or `most` has passed.
    ///
    /// # Errors
    ///
    /// The guard and whether it timed out, poisoned, if a thread panicked
    /// while holding the mutex, as `Condvar::wait_timeout` gives them.
    pub(crate) fn wait_timeout<'a, T>(
        &self,
        guard: MutexGuard<'a, T>,
        most: Duration,
    ) -> LockResult<(MutexGuard<'a, T>, WaitTimeoutResult)> {
        self.waiting(|condvar| condvar.wait_timeout(guard, most))
    }

    /// Whether a thread waits: read after the state changed (the type's
    /// invariant), so a waiter that saw the state unchanged is counted.
    fn awaited(&self) -> bool {
        self.waiters.load(Ordering::SeqCst) > 0
    }

    /// Wakes every waiting thread; with none waiting, makes no system call.
    pub(crate) fn notify_all(&self) {
        if self.awaited() {
            self.condvar.notify_all();
        }
    }

    /// Wakes one waiting thread; with none waiting, makes no system call.
    pub(crate) fn notify_one(&self) {
        if self.awaited() {
            self.condvar.notify_one();
        }
    }
}

// Unit tests: a lock poisoned by a panicking thread is reported, not
// unwrapped (RULES.md R-ERR-1, R-TEST-2 robustness); a signal wakes only
// threads waiting on it.
#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, RwLock};
    use std::thread;

    use super::*;

    /// Runs `hold`, which panics while holding a lock, on another thread.
    fn poison(hold: impl FnOnce() + Send + 'static) {
        assert!(thread::spawn(hold).join().is_err(), "the holder panicked");
    }

    /// Verifies: [DESIGN] RULES.md D27: taking a mutex or a read-write lock
    /// whose holder panicked gives SQLCODE -910; a built-in's own lock is used
    /// as it is.
    #[test]
    fn poisoned_locks_are_reported() {
        let mutex = Arc::new(Mutex::new(0_u8));
        let held = Arc::clone(&mutex);
        poison(move || drop((held.lock(), panic_now())));
        assert_eq!(lock(&mutex).err().map(|e| e.code), Some(Code::Api));
        assert_eq!(*lock_builtin(&mutex), 0);

        let rw = Arc::new(RwLock::new(0_u8));
        let writer = Arc::clone(&rw);
        poison(move || drop((writer.write(), panic_now())));
        assert_eq!(read(&rw).err().map(|e| e.code), Some(Code::Api));
        assert_eq!(write(&rw).err().map(|e| e.code), Some(Code::Api));
    }

    /// Panics, as a plug-in might.
    fn panic_now() {
        panic!("a plug-in panicked");
    }

    /// Waits on `signal` until `flag` is set, on another thread, and returns
    /// once that thread is counted as waiting.
    fn waiter(signal: &Arc<Signal>, flag: &Arc<Mutex<bool>>, timed: bool) -> thread::JoinHandle<()> {
        let (s, f) = (Arc::clone(signal), Arc::clone(flag));
        let handle = thread::spawn(move || {
            let mut set = f.lock().unwrap();
            while !*set {
                set = if timed { s.wait_timeout(set, Duration::from_secs(3600)).unwrap().0 } else { s.wait(set).unwrap() };
            }
        });
        while !signal.awaited() {
            thread::yield_now();
        }
        handle
    }

    /// Verifies: [DESIGN] RULES.md D27: a `Signal` notified with no thread
    /// waiting makes no wake-up (the MC/DC pair of "waiters > 0": false, the
    /// condition variable is not called; true, it is), and a waiter is still
    /// woken by a later notification, by `notify_all` and by `notify_one`;
    /// the count falls back to 0 once each waiter is woken.
    #[test]
    fn a_signal_wakes_only_waiters() {
        let signal = Arc::new(Signal::new());
        signal.notify_all();
        signal.notify_one();
        assert_eq!(signal.waiters.load(Ordering::SeqCst), 0, "no waiter: nothing to wake");
        assert!(!signal.awaited());

        for (timed, one) in [(false, false), (true, true)] {
            let flag = Arc::new(Mutex::new(false));
            let woken = waiter(&signal, &flag, timed);
            assert!(signal.awaited(), "a waiter is counted while it waits");
            *flag.lock().unwrap() = true;
            if one {
                signal.notify_one();
            } else {
                signal.notify_all();
            }
            woken.join().unwrap();
            assert_eq!(signal.waiters.load(Ordering::SeqCst), 0, "the waiter left");
        }
    }

    /// Verifies: [DESIGN] RULES.md D27: `Signal::wait_while` returns at once
    /// when its condition is false, uncounted after, and waits while it
    /// holds until a notification after the state changed.
    #[test]
    fn a_signal_waits_while_a_condition_holds() {
        let signal = Arc::new(Signal::default());
        let flag = Arc::new(Mutex::new(true));
        drop(signal.wait_while(flag.lock().unwrap(), |set| !*set).unwrap());
        assert_eq!(signal.waiters.load(Ordering::SeqCst), 0);

        *flag.lock().unwrap() = false;
        let (s, f) = (Arc::clone(&signal), Arc::clone(&flag));
        let woken = thread::spawn(move || drop(s.wait_while(f.lock().unwrap(), |set| !*set).unwrap()));
        while !signal.awaited() {
            thread::yield_now();
        }
        *flag.lock().unwrap() = true;
        signal.notify_all();
        woken.join().unwrap();
        assert_eq!(signal.waiters.load(Ordering::SeqCst), 0);
    }
}

/// Stripes of a `Gate`: threads taking it shared rarely share one.
const GATE_STRIPES: u64 = 16;

/// The stripe the next thread to take a gate is given.
static NEXT_STRIPE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

std::thread_local! {
    /// The gates this thread holds, by address: taking one again is free.
    static HELD: std::cell::RefCell<Vec<usize>> = const { std::cell::RefCell::new(Vec::new()) };
    /// This thread's stripe of every gate: given in turn, so threads at
    /// once take different stripes.
    static STRIPE: u64 = NEXT_STRIPE.fetch_add(1, Ordering::Relaxed) % GATE_STRIPES;
}

/// A stripe, alone on its cache line, so taking one never slows a thread
/// taking another.
#[repr(align(128))]
#[derive(Default)]
struct Stripe(RwLock<()>);

/// \[DESIGN\] A lock taken shared by every operation that changes what a
/// checkpoint writes, and exclusively by a checkpoint (`db/checkpoint.rs`)
/// and an index build as it publishes (`db/indexes.rs`), so neither sees
/// an operation half done. A shared holder takes one of
/// several stripes, given to its thread in turn, each on a cache line of
/// its own, so holders on different threads rarely meet; the exclusive holder takes them all, in order. A thread
/// holding the gate takes it again without waiting, as a checkpoint
/// waiting for it would otherwise wait for itself. It guards no data, so a
/// panic while holding it poisons nothing that matters.
pub(crate) struct Gate {
    stripes: Box<[Stripe]>,
}

impl Default for Gate {
    fn default() -> Self {
        Gate { stripes: (0..GATE_STRIPES).map(|_| Stripe::default()).collect() }
    }
}

/// The gate held shared, until dropped.
pub(crate) struct Entered<'a> {
    held: Option<(usize, RwLockReadGuard<'a, ()>)>,
}

impl Drop for Entered<'_> {
    fn drop(&mut self) {
        if let Some((gate, _)) = &self.held {
            HELD.with(|held| {
                let mut held = held.borrow_mut();
                if let Some(at) = held.iter().rposition(|g| g == gate) {
                    held.swap_remove(at);
                }
            });
        }
    }
}

/// The gate held exclusively, until dropped. Its thread counts as holding
/// it, so taking it shared there does not wait.
pub(crate) struct Shut<'a> {
    gate: usize,
    _stripes: Vec<RwLockWriteGuard<'a, ()>>,
}

impl Drop for Shut<'_> {
    fn drop(&mut self) {
        HELD.with(|held| {
            let mut held = held.borrow_mut();
            if let Some(at) = held.iter().rposition(|g| *g == self.gate) {
                held.swap_remove(at);
            }
        });
    }
}

impl Gate {
    fn address(&self) -> usize {
        std::ptr::from_ref(self).addr()
    }

    /// Takes the gate shared, unless this thread holds it already.
    pub(crate) fn enter(&self) -> Entered<'_> {
        let me = self.address();
        if HELD.with(|held| held.borrow().contains(&me)) {
            return Entered { held: None };
        }
        let stripe = usize::try_from(STRIPE.with(|s| *s)).ok().and_then(|i| self.stripes.get(i));
        let Some(stripe) = stripe else { return Entered { held: None } };
        let guard = stripe.0.read().unwrap_or_else(PoisonError::into_inner);
        HELD.with(|held| held.borrow_mut().push(me));
        Entered { held: Some((me, guard)) }
    }

    /// Takes the gate exclusively, waiting for every shared holder. The
    /// thread must not hold it shared.
    pub(crate) fn shut(&self) -> Shut<'_> {
        let stripes = self.stripes.iter().map(|s| s.0.write().unwrap_or_else(PoisonError::into_inner)).collect();
        let gate = self.address();
        HELD.with(|held| held.borrow_mut().push(gate));
        Shut { gate, _stripes: stripes }
    }
}

/// \[DESIGN\] The thread's stripe of `stripes`: given in turn as threads
/// first ask, so threads working at once seldom share one, for counters
/// and marks kept by stripe, each on cache lines of its own.
pub(crate) fn thread_stripe(stripes: usize) -> usize {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    thread_local! {
        static GIVEN: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    }
    let given = GIVEN.with(|given| {
        given.get().unwrap_or_else(|| {
            let next = NEXT.fetch_add(1, Ordering::Relaxed);
            given.set(Some(next));
            next
        })
    });
    given.checked_rem(stripes).unwrap_or_default()
}
