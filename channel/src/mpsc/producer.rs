//! producer implementation for MPSC channel.

use super::batch::MpscBatchSlots;
use super::shared::Shared;
use crate::common::wait_backoff;
use crate::error::{SendError, TrySendError};
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// producer handle for MPSC channel.
///
/// the producer can send events into the channel. unlike SPSC, multiple
/// producers can exist by cloning this handle.
///
/// # thread safety
///
/// `Producer` implements `Clone`, allowing multiple threads to send
/// concurrently. all operations use `&self` (not `&mut self`).
pub struct Producer<T> {
    pub(super) shared: Arc<Shared<T>>,
}

impl<T> Producer<T> {
    /// send an event to the channel.
    ///
    /// this may block if the buffer is full, waiting for the consumer.
    ///
    /// # errors
    ///
    /// returns `SendError` if the consumer has been dropped.
    pub fn send(&self, event: T) -> Result<(), SendError<T>> {
        if self.is_disconnected() {
            return Err(SendError(event));
        }

        // wait-free claim: use fetch_add instead of CAS loop
        // eliminates CAS retry contention among producers
        let sequence = self.shared.claim_cursor.increment();

        // now wait for space (spin on consumer cursor, not CAS)
        let wrap_point = sequence - self.shared.capacity as i64;
        let mut iteration = 0u32;
        while self.shared.consumer_cursor.value_relaxed() < wrap_point {
            if self.is_disconnected() {
                // note: we have claimed this sequence but can not use it.
                // the sequence is "leaked" but this is acceptable for disconnection.
                return Err(SendError(event));
            }
            // progressive backoff to reduce cache contention
            wait_backoff(&mut iteration);
        }

        // write event to buffer
        // safety: we have exclusive access to this sequence
        unsafe {
            self.shared.buffer.write(sequence, event);
        }

        // publish - mark as available in bitmap
        self.publish_sequence(sequence);

        Ok(())
    }

    /// try to send an event without blocking.
    ///
    /// # errors
    ///
    /// - `TrySendError::Full` if the buffer is full
    /// - `TrySendError::Disconnected` if the consumer has been dropped
    pub fn try_send(&self, event: T) -> Result<(), TrySendError<T>> {
        if self.is_disconnected() {
            return Err(TrySendError::Disconnected(event));
        }

        // try to claim a sequence
        let sequence = match self.try_claim_sequence() {
            Ok(seq) => seq,
            Err(()) => return Err(TrySendError::Full(event)),
        };

        // write event to buffer
        unsafe {
            self.shared.buffer.write(sequence, event);
        }

        // publish
        self.publish_sequence(sequence);

        Ok(())
    }

    /// try to claim a sequence without blocking.
    #[inline]
    fn try_claim_sequence(&self) -> Result<i64, ()> {
        // limited retries for non-blocking operation
        for _ in 0..8 {
            let current = self.shared.claim_cursor.value_relaxed();
            let next = current + 1;

            // check for space
            let wrap_point = next - self.shared.capacity as i64;
            let consumer_seq = self.shared.consumer_cursor.value_relaxed();

            if consumer_seq < wrap_point {
                return Err(());
            }

            // try to claim
            if self
                .shared
                .claim_cursor
                .compare_exchange(current, next)
                .is_ok()
            {
                return Ok(next);
            }
            // CAS failed, retry
            core::hint::spin_loop();
        }
        Err(())
    }

    /// publish a sequence after writing.
    ///
    /// simply marks the sequence as published in the bitmap.
    /// consumer scans the bitmap directly to find available sequences.
    #[inline]
    fn publish_sequence(&self, sequence: i64) {
        self.shared.mark_published(sequence);
    }

    /// claim multiple slots for zero-copy batch writing.
    ///
    /// returns a handle that provides mutable access to N consecutive slots.
    /// write directly into the slots, then call `publish()`.
    ///
    /// # errors
    ///
    /// returns `SendError` if the consumer has been dropped.
    ///
    /// # example
    ///
    /// ```ignore
    /// let (tx, mut rx) = mpsc::channel_with_factory(64, || 0u64, SpinLoopHintWait);
    ///
    /// // zero-copy batch write
    /// let mut batch = tx.claim_batch(4).unwrap();
    /// for i in 0..4 {
    ///     *batch.get_mut(i) = i as u64 * 10;
    /// }
    /// batch.publish();
    /// ```
    pub fn claim_batch(&self, n: usize) -> Result<MpscBatchSlots<'_, T>, SendError<()>> {
        assert!(n > 0, "batch size must be > 0");
        assert!(n <= self.shared.capacity, "batch size exceeds capacity");

        if self.is_disconnected() {
            return Err(SendError(()));
        }

        let n_i64 = n as i64;

        // wait-free claim: use fetch_add instead of CAS loop
        // add() returns the NEW value, so start = end - n + 1
        let end_sequence = self.shared.claim_cursor.add(n_i64);
        let start = end_sequence - n_i64 + 1;

        // wait for space (spin on consumer cursor)
        let wrap_point = end_sequence - self.shared.capacity as i64;
        let mut iteration = 0u32;
        while self.shared.consumer_cursor.value_relaxed() < wrap_point {
            if self.is_disconnected() {
                return Err(SendError(()));
            }
            wait_backoff(&mut iteration);
        }

        Ok(MpscBatchSlots {
            shared: &self.shared,
            start,
            count: n,
            published: false,
        })
    }

    /// try to claim multiple slots without blocking.
    ///
    /// # errors
    ///
    /// - `TrySendError::Full` if not enough space
    /// - `TrySendError::Disconnected` if consumer dropped
    pub fn try_claim_batch(&self, n: usize) -> Result<MpscBatchSlots<'_, T>, TrySendError<()>> {
        assert!(n > 0, "batch size must be > 0");
        assert!(n <= self.shared.capacity, "batch size exceeds capacity");

        if self.is_disconnected() {
            return Err(TrySendError::Disconnected(()));
        }

        let n_i64 = n as i64;

        // limited retries
        for _ in 0..8 {
            let current = self.shared.claim_cursor.value_relaxed();
            let next = current + n_i64;

            let wrap_point = next - self.shared.capacity as i64;
            let consumer_seq = self.shared.consumer_cursor.value_relaxed();

            if consumer_seq < wrap_point {
                return Err(TrySendError::Full(()));
            }

            if self
                .shared
                .claim_cursor
                .compare_exchange(current, next)
                .is_ok()
            {
                return Ok(MpscBatchSlots {
                    shared: &self.shared,
                    start: current + 1,
                    count: n,
                    published: false,
                });
            }
            core::hint::spin_loop();
        }
        Err(TrySendError::Full(()))
    }

    /// close the channel from the producer side.
    pub fn close(&self) {
        self.shared.closed.store(true, Ordering::Release);
    }

    /// check if the consumer has disconnected.
    #[inline]
    pub fn is_disconnected(&self) -> bool {
        self.shared.closed.load(Ordering::Acquire)
            || (self.shared.producer_count.load(Ordering::Acquire) == 0
                && Arc::strong_count(&self.shared) == 1)
    }
}

impl<T> Drop for Producer<T> {
    fn drop(&mut self) {
        let prev = self.shared.producer_count.fetch_sub(1, Ordering::AcqRel);
        if prev == 1 {
            // last producer dropped
            self.shared.closed.store(true, Ordering::Release);
        }
    }
}

impl<T> Clone for Producer<T> {
    fn clone(&self) -> Self {
        self.shared.producer_count.fetch_add(1, Ordering::AcqRel);
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

// safety: producer can be sent between threads if T is Send
unsafe impl<T: Send> Send for Producer<T> {}
// safety: producer can be shared between threads if T is Send
unsafe impl<T: Send> Sync for Producer<T> {}

impl<T> core::fmt::Debug for Producer<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("mpsc::Producer")
            .field(
                "producer_count",
                &self.shared.producer_count.load(Ordering::Relaxed),
            )
            .field("disconnected", &self.is_disconnected())
            .finish()
    }
}
