//! producer implementation for SPSC channel.

use super::batch::BatchSlots;
use super::shared::Shared;
use crate::common::wait_backoff;
use crate::error::{SendError, TrySendError};
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// producer handle for SPSC channel.
///
/// the producer can send events into the channel. there can only be one
/// producer for an SPSC channel.
///
/// # example
///
/// ```
/// use land_channel::spsc;
///
/// let (mut tx, _rx) = spsc::channel::<i32>(64);
///
/// // send values
/// tx.send(1).unwrap();
/// tx.send(2).unwrap();
///
/// // try send (non-blocking)
/// match tx.try_send(3) {
///     Ok(()) => println!("sent"),
///     Err(e) => println!("failed: {}", e),
/// }
/// ```
pub struct Producer<T> {
    pub(super) shared: Arc<Shared<T>>,
    /// cached consumer position for wraparound checks.
    /// this avoids reading the consumer cursor on every send.
    pub(super) cached_consumer: i64,
    /// next sequence to claim.
    pub(super) next_sequence: i64,
    /// capacity of the buffer (cached for performance).
    pub(super) capacity: i64,
    /// currently claimed slot (for claim/publish pattern).
    pub(super) claimed: bool,
    /// cached disconnection state - once true, stays true.
    /// avoids Arc::strong_count() atomic load in hot path.
    pub(super) cached_disconnected: bool,
}

impl<T> Producer<T> {
    /// send an event to the channel.
    ///
    /// blocks if the buffer is full, waiting for the consumer to make space.
    ///
    /// # errors
    ///
    /// returns `SendError` if the consumer has been dropped.
    ///
    /// # example
    ///
    /// ```
    /// use land_channel::spsc;
    ///
    /// let (mut tx, rx) = spsc::channel::<i32>(64);
    /// tx.send(42).unwrap();
    /// ```
    pub fn send(&mut self, event: T) -> Result<(), SendError<T>> {
        assert!(!self.claimed, "must call publish() after claim()");

        // check if consumer has closed
        if self.is_disconnected() {
            return Err(SendError(event));
        }

        // wait for space in buffer
        if self.wait_for_space().is_err() {
            return Err(SendError(event));
        }

        // write event to buffer
        // safety: we have exclusive access as the only producer and we
        // confirmed space is available
        unsafe {
            self.shared.buffer.write(self.next_sequence, event);
        }

        // publish (make visible to consumer)
        self.shared.producer_cursor.set(self.next_sequence);
        self.next_sequence += 1;

        Ok(())
    }

    /// try to send an event without blocking.
    ///
    /// returns immediately if the buffer is full.
    ///
    /// # errors
    ///
    /// - `TrySendError::Full` if the buffer is full
    /// - `TrySendError::Disconnected` if the consumer has been dropped
    ///
    /// # example
    ///
    /// ```
    /// use land_channel::spsc;
    /// use land_channel::error::TrySendError;
    ///
    /// let (mut tx, rx) = spsc::channel::<i32>(2);
    ///
    /// tx.try_send(1).unwrap();
    /// tx.try_send(2).unwrap();
    ///
    /// // buffer is now full
    /// match tx.try_send(3) {
    ///     Err(TrySendError::Full(_)) => println!("buffer full"),
    ///     _ => {}
    /// }
    /// ```
    pub fn try_send(&mut self, event: T) -> Result<(), TrySendError<T>> {
        assert!(!self.claimed, "must call publish() after claim()");

        if self.is_disconnected() {
            return Err(TrySendError::Disconnected(event));
        }

        // double-check pattern - only reload remote cursor when cached says full
        let wrap_point = self.next_sequence - self.capacity;

        // fast path: check cached value first (no atomic load)
        if self.cached_consumer < wrap_point {
            // cached says might be full - reload actual consumer position
            self.cached_consumer = self.shared.consumer_cursor.value_relaxed();
            if self.cached_consumer < wrap_point {
                // actually full
                return Err(TrySendError::Full(event));
            }
        }

        // safety: we have exclusive access and confirmed space
        unsafe {
            self.shared.buffer.write(self.next_sequence, event);
        }

        self.shared.producer_cursor.set(self.next_sequence);
        self.next_sequence += 1;

        Ok(())
    }

    /// claim the next slot for in-place writing (zero-copy pattern).
    ///
    /// this returns a mutable reference to the next slot in the ring buffer.
    /// after writing to the slot, call `publish()` to make it visible to
    /// the consumer.
    ///
    /// # errors
    ///
    /// returns `SendError` if the consumer has been dropped.
    ///
    /// # example
    ///
    /// ```
    /// use land_channel::spsc;
    ///
    /// let (mut tx, mut rx) = spsc::channel_with_factory(64, Vec::new, land_channel::SpinLoopHintWait);
    ///
    /// // zero-copy: write directly into ring buffer
    /// {
    ///     let slot: &mut Vec<u8> = tx.claim().unwrap();
    ///     slot.clear();
    ///     slot.push(1);
    ///     slot.push(2);
    ///     slot.push(3);
    /// }
    /// tx.publish();
    ///
    /// let data = rx.recv().unwrap();
    /// assert_eq!(data, vec![1, 2, 3]);
    /// ```
    pub fn claim(&mut self) -> Result<&mut T, SendError<()>> {
        assert!(!self.claimed, "already claimed, must call publish() first");

        if self.is_disconnected() {
            return Err(SendError(()));
        }

        // wait for space
        if self.wait_for_space().is_err() {
            return Err(SendError(()));
        }

        self.claimed = true;

        // safety: we have exclusive access and confirmed space
        Ok(unsafe { self.shared.buffer.get_mut(self.next_sequence) })
    }

    /// try to claim the next slot without blocking.
    ///
    /// # errors
    ///
    /// - `TrySendError::Full` if the buffer is full
    /// - `TrySendError::Disconnected` if the consumer has been dropped
    pub fn try_claim(&mut self) -> Result<&mut T, TrySendError<()>> {
        assert!(!self.claimed, "already claimed, must call publish() first");

        if self.is_disconnected() {
            return Err(TrySendError::Disconnected(()));
        }

        // double-check pattern - only reload remote cursor when cached says full
        let wrap_point = self.next_sequence - self.capacity;

        // fast path: check cached value first (no atomic load)
        if self.cached_consumer < wrap_point {
            // cached says might be full - reload actual consumer position
            self.cached_consumer = self.shared.consumer_cursor.value_relaxed();
            if self.cached_consumer < wrap_point {
                // actually full
                return Err(TrySendError::Full(()));
            }
        }

        self.claimed = true;

        Ok(unsafe { self.shared.buffer.get_mut(self.next_sequence) })
    }

    /// publish a previously claimed slot.
    ///
    /// this makes the event visible to the consumer. must be called after
    /// `claim()` or `try_claim()`.
    ///
    /// # panics
    ///
    /// panics if called without a prior `claim()` or `try_claim()`.
    ///
    /// # example
    ///
    /// ```
    /// use land_channel::spsc;
    ///
    /// let (mut tx, _rx) = spsc::channel_with_factory(64, || 0i32, land_channel::SpinLoopHintWait);
    ///
    /// {
    ///     let slot = tx.claim().unwrap();
    ///     *slot = 42;
    /// }
    /// tx.publish();  // now visible to consumer
    /// ```
    pub fn publish(&mut self) {
        assert!(self.claimed, "must call claim() before publish()");
        self.shared.producer_cursor.set(self.next_sequence);
        self.next_sequence += 1;
        self.claimed = false;
    }

    /// claim multiple slots for batch writing (zero-copy pattern).
    ///
    /// returns a [`BatchSlots`] handle that provides access to `n` consecutive
    /// slots. the batch is published when [`BatchSlots::publish()`] is called
    /// or when the handle is dropped.
    ///
    /// this is more efficient than multiple `claim()`/`publish()` calls because:
    /// - single space check for all slots
    /// - single memory fence for the entire batch
    ///
    /// # errors
    ///
    /// returns `SendError` if the consumer has been dropped.
    ///
    /// # example
    ///
    /// ```
    /// use land_channel::spsc;
    ///
    /// let (mut tx, mut rx) = spsc::channel_with_factory(64, || 0u64, land_channel::SpinLoopHintWait);
    ///
    /// // batch write 4 values
    /// {
    ///     let mut batch = tx.claim_batch(4).unwrap();
    ///     for i in 0..4 {
    ///         *batch.get_mut(i) = i as u64 * 10;
    ///     }
    ///     batch.publish();
    /// }
    ///
    /// // receive all 4
    /// for i in 0..4 {
    ///     assert_eq!(rx.recv().unwrap(), i as u64 * 10);
    /// }
    /// ```
    pub fn claim_batch(&mut self, n: usize) -> Result<BatchSlots<'_, T>, SendError<()>> {
        assert!(!self.claimed, "already claimed, must call publish() first");
        assert!(n > 0, "batch size must be > 0");
        assert!(
            n <= self.capacity as usize,
            "batch size exceeds buffer capacity"
        );

        if self.is_disconnected() {
            return Err(SendError(()));
        }

        // wait for space for entire batch
        self.wait_for_space_n(n as i64)?;

        let start = self.next_sequence;
        self.next_sequence += n as i64;

        Ok(BatchSlots {
            shared: &self.shared,
            start,
            count: n,
            published: false,
        })
    }

    /// try to claim multiple slots without blocking.
    ///
    /// returns immediately if not enough space is available.
    ///
    /// # errors
    ///
    /// - `TrySendError::Full` if not enough space for the batch
    /// - `TrySendError::Disconnected` if the consumer has been dropped
    pub fn try_claim_batch(&mut self, n: usize) -> Result<BatchSlots<'_, T>, TrySendError<()>> {
        assert!(!self.claimed, "already claimed, must call publish() first");
        assert!(n > 0, "batch size must be > 0");
        assert!(
            n <= self.capacity as usize,
            "batch size exceeds buffer capacity"
        );

        if self.is_disconnected() {
            return Err(TrySendError::Disconnected(()));
        }

        // check for space without blocking
        let wrap_point = self.next_sequence + n as i64 - 1 - self.capacity;
        self.cached_consumer = self.shared.consumer_cursor.value_relaxed();

        if self.cached_consumer < wrap_point {
            return Err(TrySendError::Full(()));
        }

        let start = self.next_sequence;
        self.next_sequence += n as i64;

        Ok(BatchSlots {
            shared: &self.shared,
            start,
            count: n,
            published: false,
        })
    }

    /// close the channel from the producer side.
    ///
    /// after closing, the consumer will receive `RecvError::Disconnected`
    /// once all buffered events have been consumed.
    pub fn close(&self) {
        self.shared.closed.store(true, Ordering::Release);
    }

    /// check if the consumer has disconnected.
    ///
    /// returns `true` if the consumer has been dropped.
    #[inline]
    pub fn is_disconnected(&mut self) -> bool {
        // fast path: return cached value (once true, stays true)
        if self.cached_disconnected {
            return true;
        }
        // check closed flag first (cheaper than Arc::strong_count)
        let disconnected =
            self.shared.closed.load(Ordering::Relaxed) || Arc::strong_count(&self.shared) == 1;
        if disconnected {
            self.cached_disconnected = true;
        }
        disconnected
    }

    /// get the number of pending (unsent) slots available.
    #[inline]
    pub fn available_slots(&self) -> usize {
        let consumer_seq = self.shared.consumer_cursor.value();
        let pending = self.next_sequence - consumer_seq - 1;
        if pending < 0 {
            self.capacity as usize
        } else {
            (self.capacity as usize).saturating_sub(pending as usize)
        }
    }

    /// wait for space in the buffer.
    #[inline]
    fn wait_for_space(&mut self) -> Result<(), SendError<()>> {
        self.wait_for_space_n(1)
    }

    /// wait for n slots of space in the buffer.
    #[inline]
    fn wait_for_space_n(&mut self, n: i64) -> Result<(), SendError<()>> {
        let wrap_point = self.next_sequence + n - 1 - self.capacity;

        // fast path: cached value says we have space
        if self.cached_consumer >= wrap_point {
            return Ok(());
        }

        // slow path: need to wait - use progressive backoff
        let mut iteration = 0u32;
        loop {
            // reload consumer cursor
            self.cached_consumer = self.shared.consumer_cursor.value_relaxed();
            if self.cached_consumer >= wrap_point {
                return Ok(());
            }
            if self.is_disconnected() {
                return Err(SendError(()));
            }
            // progressive backoff to reduce cache contention
            wait_backoff(&mut iteration);
        }
    }
}

impl<T> Drop for Producer<T> {
    fn drop(&mut self) {
        // if we have a claimed but unpublished slot, we can't safely drop
        // the value as the consumer might try to read it. mark as closed instead.
        self.close();
    }
}

// safety: producer can be sent between threads if T is Send
unsafe impl<T: Send> Send for Producer<T> {}

impl<T> core::fmt::Debug for Producer<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // use cached value or direct check (avoid &mut self requirement)
        let disconnected = self.cached_disconnected
            || self.shared.closed.load(Ordering::Relaxed)
            || Arc::strong_count(&self.shared) == 1;
        f.debug_struct("Producer")
            .field("next_sequence", &self.next_sequence)
            .field("capacity", &self.capacity)
            .field("claimed", &self.claimed)
            .field("disconnected", &disconnected)
            .finish()
    }
}
