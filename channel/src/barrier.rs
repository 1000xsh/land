//! sequence barriers for coordinating producer-consumer dependencies.
//!
//! barriers track the progress of producers and consumers to ensure correct
//! ordering of reads and writes. a consumer uses a barrier to wait until
//! the producer has published events up to a certain sequence.
//!
//! # barrier Types
//!
//! - [`SequenceBarrier`]: waits on a single cursor (used by consumers)
//! - [`MultiSequenceBarrier`]: waits on multiple cursors (for multi-consumer)
//!
//! # example
//!
//! ```ignore
//! use land_channel::barrier::SequenceBarrier;
//! use land_channel::{Cursor, SpinLoopHintWait};
//!
//! let producer_cursor = Cursor::new();
//! let barrier = SequenceBarrier::new(&producer_cursor, SpinLoopHintWait);
//!
//! // wait for sequence 5 to be available
//! let available = barrier.wait_for(5);
//! ```

use land_cpu::{Cursor, WaitStrategy};

/// a barrier that tracks a single cursor.
///
/// used by consumers to wait until the producer has published events
/// up to a certain sequence number.
///
/// # type parameters
///
/// * `W` - the wait strategy to use when blocking
pub struct SequenceBarrier<W: WaitStrategy> {
    /// pointer to the cursor being tracked (producer's published sequence).
    cursor: *const Cursor,
    /// wait strategy for blocking operations.
    wait_strategy: W,
}

// safety: the cursor pointer is valid for the lifetime of the channel,
// and wait strategies are Send + Sync
unsafe impl<W: WaitStrategy> Send for SequenceBarrier<W> {}
unsafe impl<W: WaitStrategy> Sync for SequenceBarrier<W> {}

impl<W: WaitStrategy> SequenceBarrier<W> {
    /// create a new barrier tracking the given cursor.
    ///
    /// # safety
    ///
    /// the cursor must outlive this barrier. the barrier holds a raw pointer
    /// to the cursor, so the cursor must not be moved or dropped while the
    /// barrier exists.
    ///
    /// # arguments
    ///
    /// * `cursor` - the cursor to track
    /// * `wait_strategy` - strategy for waiting when blocking
    #[inline]
    pub unsafe fn new(cursor: *const Cursor, wait_strategy: W) -> Self {
        Self {
            cursor,
            wait_strategy,
        }
    }

    /// wait until the tracked cursor reaches or exceeds the given sequence.
    ///
    /// this blocks using the configured wait strategy until the producer
    /// has published the requested sequence.
    ///
    /// # arguments
    ///
    /// * `sequence` - the minimum sequence to wait for
    ///
    /// # returns
    ///
    /// the current cursor value (>= sequence)
    #[inline]
    pub fn wait_for(&self, sequence: i64) -> i64 {
        // safety: cursor pointer is valid per construction contract
        let cursor = unsafe { &*self.cursor };
        self.wait_strategy.wait_for(sequence, cursor.as_atomic())
    }

    /// check if the given sequence is currently available.
    ///
    /// this is a non-blocking check that returns immediately.
    ///
    /// # arguments
    ///
    /// * `sequence` - the sequence to check
    ///
    /// # returns
    ///
    /// `true` if the cursor has reached or exceeded the sequence
    #[inline]
    pub fn is_available(&self, sequence: i64) -> bool {
        // safety: cursor pointer is valid per construction contract
        let current = unsafe { (*self.cursor).value() };
        current >= sequence
    }

    /// get the current cursor value without waiting.
    ///
    /// uses Acquire ordering to ensure visibility of prior writes.
    #[inline]
    pub fn get_cursor(&self) -> i64 {
        // safety: cursor pointer is valid per construction contract
        unsafe { (*self.cursor).value() }
    }

    /// get the current cursor value with relaxed ordering.
    ///
    /// faster but may see stale values.
    #[inline]
    pub fn get_cursor_relaxed(&self) -> i64 {
        // safety: cursor pointer is valid per construction contract
        unsafe { (*self.cursor).value_relaxed() }
    }

    /// signal that progress has been made (for blocking strategies).
    #[inline]
    pub fn signal(&self) {
        self.wait_strategy.signal();
    }
}

impl<W: WaitStrategy + Clone> Clone for SequenceBarrier<W> {
    fn clone(&self) -> Self {
        Self {
            cursor: self.cursor,
            wait_strategy: self.wait_strategy.clone(),
        }
    }
}

impl<W: WaitStrategy + core::fmt::Debug> core::fmt::Debug for SequenceBarrier<W> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SequenceBarrier")
            .field("cursor_value", &self.get_cursor_relaxed())
            .field("wait_strategy", &self.wait_strategy)
            .finish()
    }
}

/// a barrier that tracks multiple cursors.
///
/// returns the minimum sequence across all tracked cursors.
/// used when a consumer depends on multiple producers or other consumers.
pub struct MultiSequenceBarrier<W: WaitStrategy> {
    /// pointers to cursors being tracked.
    cursors: Vec<*const Cursor>,
    /// wait strategy for blocking operations.
    wait_strategy: W,
}

// safety: same reasoning as SequenceBarrier
unsafe impl<W: WaitStrategy> Send for MultiSequenceBarrier<W> {}
unsafe impl<W: WaitStrategy> Sync for MultiSequenceBarrier<W> {}

impl<W: WaitStrategy> MultiSequenceBarrier<W> {
    /// create a new barrier tracking multiple cursors.
    ///
    /// # safety
    ///
    /// all cursors must outlive this barrier.
    ///
    /// # arguments
    ///
    /// * `cursors` - the cursors to track
    /// * `wait_strategy` - strategy for waiting when blocking
    #[inline]
    pub unsafe fn new(cursors: Vec<*const Cursor>, wait_strategy: W) -> Self {
        Self {
            cursors,
            wait_strategy,
        }
    }

    /// get the minimum sequence across all tracked cursors.
    #[inline]
    pub fn get_minimum_sequence(&self) -> i64 {
        let mut min = i64::MAX;
        for &cursor_ptr in &self.cursors {
            // safety: cursor pointers are valid per construction contract
            // use relaxed for polling - proper sync happens through wait strategy
            let value = unsafe { (*cursor_ptr).value_relaxed() };
            if value < min {
                min = value;
            }
        }
        min
    }

    /// wait until all tracked cursors reach or exceed the given sequence.
    ///
    /// # arguments
    ///
    /// * `sequence` - the minimum sequence to wait for
    ///
    /// # returns
    ///
    /// the minimum current cursor value across all cursors (>= sequence)
    pub fn wait_for(&self, sequence: i64) -> i64 {
        loop {
            let min = self.get_minimum_sequence();
            if min >= sequence {
                return min;
            }

            // for multiple cursors, use spin_loop hint
            // (wait_for requires a single cursor, but we track multiple)
            core::hint::spin_loop();
        }
    }

    /// check if all cursors have reached or exceeded the given sequence.
    #[inline]
    pub fn is_available(&self, sequence: i64) -> bool {
        self.get_minimum_sequence() >= sequence
    }

    /// get the number of cursors being tracked.
    #[inline]
    pub fn cursor_count(&self) -> usize {
        self.cursors.len()
    }

    /// signal that progress has been made (for blocking strategies).
    #[inline]
    pub fn signal(&self) {
        self.wait_strategy.signal();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use land_cpu::SpinLoopHintWait;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_sequence_barrier_immediate() {
        let cursor = Cursor::with_value(10);
        let barrier = unsafe { SequenceBarrier::new(&cursor, SpinLoopHintWait) };

        // should return immediately - cursor is already at 10
        let result = barrier.wait_for(5);
        assert_eq!(result, 10);
    }

    #[test]
    fn test_sequence_barrier_is_available() {
        let cursor = Cursor::with_value(10);
        let barrier = unsafe { SequenceBarrier::new(&cursor, SpinLoopHintWait) };

        assert!(barrier.is_available(5));
        assert!(barrier.is_available(10));
        assert!(!barrier.is_available(11));
    }

    #[test]
    fn test_sequence_barrier_get_cursor() {
        let cursor = Cursor::with_value(42);
        let barrier = unsafe { SequenceBarrier::new(&cursor, SpinLoopHintWait) };

        assert_eq!(barrier.get_cursor(), 42);
        assert_eq!(barrier.get_cursor_relaxed(), 42);
    }

    #[test]
    fn test_sequence_barrier_with_producer() {
        let cursor = Arc::new(Cursor::new()); // -1
        let cursor_ptr = Arc::as_ptr(&cursor) as *const Cursor;

        let cursor_clone = Arc::clone(&cursor);
        let producer = thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            cursor_clone.set(5);
        });

        let barrier = unsafe { SequenceBarrier::new(cursor_ptr, SpinLoopHintWait) };
        let result = barrier.wait_for(5);
        assert!(result >= 5);

        producer.join().unwrap();
    }

    #[test]
    fn test_multi_sequence_barrier() {
        let cursor1 = Cursor::with_value(10);
        let cursor2 = Cursor::with_value(5);
        let cursor3 = Cursor::with_value(15);

        let cursors = vec![
            &cursor1 as *const Cursor,
            &cursor2 as *const Cursor,
            &cursor3 as *const Cursor,
        ];

        let barrier = unsafe { MultiSequenceBarrier::new(cursors, SpinLoopHintWait) };

        // Minimum is 5
        assert_eq!(barrier.get_minimum_sequence(), 5);
        assert!(barrier.is_available(5));
        assert!(!barrier.is_available(6));
        assert_eq!(barrier.cursor_count(), 3);
    }

    #[test]
    fn test_multi_sequence_barrier_wait() {
        let cursor1 = Cursor::with_value(10);
        let cursor2 = Cursor::with_value(8);

        let cursors = vec![&cursor1 as *const Cursor, &cursor2 as *const Cursor];

        let barrier = unsafe { MultiSequenceBarrier::new(cursors, SpinLoopHintWait) };

        // should return immediately - minimum (8) >= 5
        let result = barrier.wait_for(5);
        assert_eq!(result, 8);
    }

    #[test]
    fn test_barrier_debug() {
        let cursor = Cursor::with_value(42);
        let barrier = unsafe { SequenceBarrier::new(&cursor, SpinLoopHintWait) };
        let debug = format!("{:?}", barrier);
        assert!(debug.contains("SequenceBarrier"));
        assert!(debug.contains("42"));
    }
}
