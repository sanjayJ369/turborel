//! Unit tests of the publication of appended blocks (RULES.md R-TEST-1):
//! an appender whose write finishes before an earlier one's waits for it,
//! through its checks, its yields and its park, and publishes after it.
//! Split from publish.rs (RULES.md R-SIZE-2).

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use super::super::{BlockFile, BlockOptions};
use crate::disk::encoding::Pipeline;
use crate::disk::io::{DiskFile, Durability, FileLock, FileSystem, OpenMode};
use crate::disk::memory::MemoryFileSystem;
use crate::error::Result;
use crate::hash::HashFunction;
use crate::hash::crc::{CRC32_ISCSI, Crc};

/// The write held, and whether it has begun or may go on.
#[derive(Default)]
struct Hold {
    /// The offset of the write to hold, once set.
    at: Option<u64>,
    /// Whether the held write has begun.
    begun: bool,
    /// Whether it may go on.
    released: bool,
}

/// A gate holding one write until it is released.
#[derive(Default)]
struct Gate {
    hold: Mutex<Hold>,
    changed: Condvar,
}

impl Gate {
    /// Holds the write at `at` once it begins.
    fn hold_at(&self, at: u64) {
        self.hold.lock().unwrap().at = Some(at);
    }

    /// Waits until the held write has begun.
    fn until_begun(&self) {
        let mut hold = self.hold.lock().unwrap();
        while !hold.begun {
            hold = self.changed.wait(hold).unwrap();
        }
    }

    /// Lets the held write go on.
    fn release(&self) {
        self.hold.lock().unwrap().released = true;
        self.changed.notify_all();
    }

    /// Waits, if the write at `at` is the one held, until it is released.
    fn pass(&self, at: u64) {
        let mut hold = self.hold.lock().unwrap();
        if hold.at != Some(at) {
            return;
        }
        hold.begun = true;
        self.changed.notify_all();
        while !hold.released {
            hold = self.changed.wait(hold).unwrap();
        }
    }
}

/// A file system in memory whose files hold the write the gate says.
struct Holding {
    inner: MemoryFileSystem,
    gate: Arc<Gate>,
}

impl FileSystem for Holding {
    fn open(&self, path: &Path, mode: OpenMode) -> Result<Box<dyn DiskFile>> {
        Ok(Box::new(HoldingFile { inner: self.inner.open(path, mode)?, gate: Arc::clone(&self.gate) }))
    }
    fn remove(&self, path: &Path) -> Result<()> {
        self.inner.remove(path)
    }
    fn exists(&self, path: &Path) -> Result<bool> {
        self.inner.exists(path)
    }
    fn create_dir_all(&self, path: &Path) -> Result<()> {
        self.inner.create_dir_all(path)
    }
    fn read_dir(&self, path: &Path) -> Result<Vec<PathBuf>> {
        self.inner.read_dir(path)
    }
    fn sync_dir(&self, path: &Path) -> Result<()> {
        self.inner.sync_dir(path)
    }
    fn lock(&self, path: &Path) -> Result<FileLock> {
        self.inner.lock(path)
    }
}

/// A file of a `Holding` file system.
struct HoldingFile {
    inner: Box<dyn DiskFile>,
    gate: Arc<Gate>,
}

impl DiskFile for HoldingFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<usize> {
        self.inner.read_at(buf, offset)
    }
    fn write_all_at(&self, data: &[u8], offset: u64) -> Result<()> {
        self.gate.pass(offset);
        self.inner.write_all_at(data, offset)
    }
    fn size(&self) -> Result<u64> {
        self.inner.size()
    }
    fn set_size(&self, size: u64) -> Result<()> {
        self.inner.set_size(size)
    }
    fn sync(&self, durability: Durability) -> Result<()> {
        self.inner.sync(durability)
    }
}

/// Verifies: [DESIGN] RULES.md D30, block manager: an appender whose write
/// finishes while an earlier reservation's is still being written does
/// not publish its block across the gap: it checks, yields its bounded
/// rounds and parks, the end staying at the earlier block. Once that write
/// finishes, both are published in the order reserved, the end after the
/// later one, and both read back in that order.
#[test]
fn an_appender_finishing_first_publishes_after_the_earlier_one() {
    let gate = Arc::new(Gate::default());
    let fs = Holding { inner: MemoryFileSystem::new(), gate: Arc::clone(&gate) };
    let hash: Arc<dyn HashFunction> = Arc::new(Crc::new(CRC32_ISCSI).unwrap());
    let file = Arc::new(BlockFile::create(&fs, Path::new("/b"), hash, Pipeline::none(), BlockOptions::default()).unwrap());
    let first = file.end();
    gate.hold_at(first.0);
    let earlier = thread::spawn({
        let file = Arc::clone(&file);
        move || file.append(b"earlier").unwrap()
    });
    gate.until_begun();
    let later = thread::spawn({
        let file = Arc::clone(&file);
        move || file.append(b"later, written first").unwrap()
    });
    // The later appender has written its block, checked and yielded its
    // rounds, and parked: only then is the earlier write let go.
    while file.parked.load(Ordering::SeqCst) == 0 {
        thread::yield_now();
    }
    assert_eq!(file.end(), first, "nothing published across the gap");
    gate.release();
    let (earlier_id, later_id) = (earlier.join().unwrap(), later.join().unwrap());
    assert_eq!(earlier_id, first);
    assert!(later_id > earlier_id);
    let mut cursor = file.cursor(first);
    let mut payload = Vec::new();
    assert_eq!(cursor.next(&mut payload).unwrap(), Some(earlier_id));
    assert_eq!(payload, b"earlier");
    assert_eq!(cursor.next(&mut payload).unwrap(), Some(later_id));
    assert_eq!(payload, b"later, written first");
    assert_eq!(cursor.next(&mut payload).unwrap(), None);
    assert_eq!(cursor.position(), file.end(), "the end is after the later block");
    assert_eq!(file.parked.load(Ordering::SeqCst), 0, "no appender left parked");
}
