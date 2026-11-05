//! Unit tests of a page read into the caller's buffer (RULES.md R-TEST-1,
//! R-TEST-2). Split from pager/sealed.rs (RULES.md R-SIZE-2).

use std::path::Path;
use std::sync::Arc;

use super::super::named::tests::shift;
use super::super::{PageId, Pager};
use crate::disk::encoding::Pipeline;
use crate::disk::memory::MemoryFileSystem;
use crate::hash::crc::{CRC32_ISCSI, Crc};

/// Verifies: [DESIGN] RULES.md D30, pager reads: a page read into a buffer
/// already holding other bytes, fewer than the page or more, leaves in it
/// exactly the page's content: short, filling the page, overflowing it,
/// none for a page never written, and decoded for a page written through
/// an encoding; a buffer of a page's size holding the page read is the
/// one the content is left in, with no other allocated.
#[test]
fn a_read_leaves_exactly_the_content_in_the_buffer() {
    let fs = MemoryFileSystem::new();
    let pager = Pager::create(&fs, Path::new("/read"), Arc::new(Crc::new(CRC32_ISCSI).unwrap()), Pipeline::none(), 4096).unwrap();
    pager.allocate(5);
    let full = vec![7_u8; pager.inline_capacity()];
    let overflowing: Vec<u8> = (0..20_000_u32).map(|n| u8::try_from(n % 251).unwrap()).collect();
    // Page 3 is never written; page 4 is written through an encoding.
    let expected: [&[u8]; 5] = [b"short", &full, &overflowing, b"", b"shifted"];
    for (id, content) in (0..).zip(expected.iter().take(3)) {
        pager.write(PageId(id), content).unwrap();
    }
    pager.set_pipeline(shift(5)).unwrap();
    pager.write(PageId(4), expected[4]).unwrap();
    for held in [Vec::new(), vec![0xEE; 10], vec![0xEE; 70_000]] {
        for (id, content) in (0..).zip(&expected) {
            let mut buffer = held.clone();
            pager.read(PageId(id), &mut buffer).unwrap();
            assert_eq!(buffer, *content, "page {id}, the buffer holding {} bytes", held.len());
        }
    }
    let mut buffer = Vec::with_capacity(pager.page_size());
    buffer.extend_from_slice(&[0xEE; 100]);
    let at = buffer.as_ptr();
    pager.read(PageId(1), &mut buffer).unwrap();
    assert_eq!((buffer.as_ptr(), buffer.len()), (at, full.len()), "read where it lies");
}
