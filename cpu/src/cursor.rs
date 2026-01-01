// atomic sequence cursor for ring buffer coordination
//
// tracks position in ring buffer using monotonically increasing sequence number
// used by producers to publish events and consumers to track consumption
//
// sequence numbers:
// - initial: -1 (nothing published)
// - first event: 0
// - monotonically increasing
//
// memory ordering:
// - value(): acquire - ensures visibility of writes before this sequence
// - set(): release - ensures our writes are visible before this sequence
// - value_relaxed(): no ordering - for polling where eventual consistency ok

use crate::cache_padded::CachePadded;
use core::sync::atomic::{AtomicI64, Ordering};

pub const INITIAL_CURSOR_VALUE: i64 = -1;

// atomic sequence cursor with cache-line padding
// represents last sequence processed (published for producers, consumed for consumers)
// cache-padded to prevent false sharing
#[repr(C)]
pub struct Cursor {
    sequence: CachePadded<AtomicI64>,
}

impl Cursor {
    // create cursor at initial position (-1)
    #[inline]
    pub const fn new() -> Self {
        Self {
            sequence: CachePadded::new(AtomicI64::new(INITIAL_CURSOR_VALUE)),
        }
    }

    // create cursor starting at specific sequence
    #[inline]
    pub const fn with_value(sequence: i64) -> Self {
        Self {
            sequence: CachePadded::new(AtomicI64::new(sequence)),
        }
    }

    // get current sequence with acquire ordering
    // ensures all writes by thread that set this sequence are visible
    #[inline(always)]
    pub fn value(&self) -> i64 {
        self.sequence.load(Ordering::Acquire)
    }

    // set sequence with release ordering
    // ensures all prior writes are visible to threads that read with acquire
    // use when publishing data - write data first, then set cursor
    #[inline(always)]
    pub fn set(&self, sequence: i64) {
        self.sequence.store(sequence, Ordering::Release);
    }

    // atomically increment cursor and return new value
    // uses acqrel ordering for visibility of prior writes and increment
    // used by producers to claim next sequence
    #[inline(always)]
    pub fn increment(&self) -> i64 {
        self.sequence.fetch_add(1, Ordering::AcqRel) + 1
    }

    // atomically add value and return new sequence
    // uses acqrel ordering, useful for batch claims
    #[inline(always)]
    pub fn add(&self, delta: i64) -> i64 {
        self.sequence.fetch_add(delta, Ordering::AcqRel) + delta
    }

    // compare-and-swap for multi-producer coordination
    // atomically updates cursor from current to new
    // returns Ok(current) on success, Err(actual) on failure
    // uses acqrel/acquire ordering
    #[inline(always)]
    pub fn compare_exchange(&self, current: i64, new: i64) -> Result<i64, i64> {
        self.sequence.compare_exchange(
            current,
            new,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
    }

    // compare-and-swap with weak semantics (may spuriously fail)
    // more efficient in retry loops on some architectures
    #[inline(always)]
    pub fn compare_exchange_weak(&self, current: i64, new: i64) -> Result<i64, i64> {
        self.sequence.compare_exchange_weak(
            current,
            new,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
    }

    // get sequence with relaxed ordering - no memory ordering guarantees
    // use only when:
    // - polling and don't need immediate visibility
    // - will verify with acquire load later
    // - performance critical and eventual consistency ok
    #[inline(always)]
    pub fn value_relaxed(&self) -> i64 {
        self.sequence.load(Ordering::Relaxed)
    }

    // set sequence with relaxed ordering - no memory ordering guarantees
    #[inline(always)]
    pub fn set_relaxed(&self, sequence: i64) {
        self.sequence.store(sequence, Ordering::Relaxed);
    }

    // get reference to underlying AtomicI64
    // useful for custom atomic operations or wait strategies
    #[inline]
    pub fn as_atomic(&self) -> &AtomicI64 {
        &self.sequence
    }
}

impl Default for Cursor {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Debug for Cursor {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Cursor")
            .field("sequence", &self.value_relaxed())
            .finish()
    }
}

// safety: cursor safe to send between threads
unsafe impl Send for Cursor {}
// safety: cursor uses atomic operations for all mutations
unsafe impl Sync for Cursor {}
