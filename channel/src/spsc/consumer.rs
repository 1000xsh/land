//! consumer implementation for SPSC channel.

use super::shared::Shared;
use crate::barrier::SequenceBarrier;
use crate::error::{RecvError, TryRecvError};
use land_cpu::{fence_acquire, fence_release, SpinLoopHintWait, WaitStrategy};
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// consumer handle for SPSC channel.
///
/// the consumer receives events from the channel. there can only be one
/// consumer for an SPSC channel.
///
/// # type parameters
///
/// * `T` - event type
/// * `W` - wait strategy for blocking receives
///
/// # example
///
/// ```
/// use land_channel::spsc;
///
/// let (mut tx, mut rx) = spsc::channel::<i32>(64);
///
/// tx.send(42).unwrap();
///
/// // blocking receive
/// let value = rx.recv().unwrap();
/// assert_eq!(value, 42);
///
/// // non-blocking receive
/// match rx.try_recv() {
///     Ok(v) => println!("got {}", v),
///     Err(e) => println!("empty or closed: {}", e),
/// }
/// ```
pub struct Consumer<T, W: WaitStrategy = SpinLoopHintWait> {
    pub(super) shared: Arc<Shared<T>>,
    /// next sequence to consume.
    pub(super) next_sequence: i64,
    /// barrier for waiting on producer.
    pub(super) barrier: SequenceBarrier<W>,
    /// cached disconnection state - once true, stays true.
    /// avoids Arc::strong_count() atomic load in hot path.
    pub(super) cached_disconnected: bool,
}

impl<T, W: WaitStrategy> Consumer<T, W> {
    /// receive the next event from the channel.
    ///
    /// blocks until an event is available using the configured wait strategy.
    ///
    /// # errors
    ///
    /// returns `RecvError` if the producer has been dropped and there are
    /// no more events in the buffer.
    ///
    /// # example
    ///
    /// ```
    /// use land_channel::spsc;
    ///
    /// let (mut tx, mut rx) = spsc::channel::<i32>(64);
    /// tx.send(42).unwrap();
    ///
    /// let value = rx.recv().unwrap();
    /// assert_eq!(value, 42);
    /// ```
    pub fn recv(&mut self) -> Result<T, RecvError> {
        loop {
            // fast path: check with relaxed ordering first
            let available = self.barrier.get_cursor_relaxed();

            if available >= self.next_sequence {
                // prefetch disabled
                // // prefetch next slot while we read current - hides memory latency
                // // safety: prefetch is safe even for invalid addresses on x86_64
                // unsafe {
                //     let next_ptr = self.shared.buffer.get_ptr(self.next_sequence + 1);
                //     prefetch_read(next_ptr);
                // }

                // event is available - read it
                // safety: producer has published this sequence, and we'll do acquire
                // through the cursor read which ensures visibility
                let event = unsafe { self.shared.buffer.read(self.next_sequence) };

                // update consumer cursor with relaxed ordering
                // producer polls with relaxed anyway, so no need for Release here
                self.shared.consumer_cursor.set_relaxed(self.next_sequence);
                self.next_sequence += 1;

                return Ok(event);
            }

            // check for disconnection before waiting
            if self.is_disconnected() {
                // final check for pending events with acquire ordering
                if self.barrier.get_cursor() >= self.next_sequence {
                    continue;
                }
                return Err(RecvError);
            }

            // use the configured wait strategy
            let _ = self.barrier.wait_for(self.next_sequence);
        }
    }

    /// try to receive an event without blocking.
    ///
    /// returns immediately if no event is available.
    ///
    /// # errors
    ///
    /// - `TryRecvError::Empty` if no events are available
    /// - `TryRecvError::Disconnected` if the producer was dropped and buffer is empty
    ///
    /// # example
    ///
    /// ```
    /// use land_channel::spsc;
    /// use land_channel::error::TryRecvError;
    ///
    /// let (tx, mut rx) = spsc::channel::<i32>(64);
    ///
    /// match rx.try_recv() {
    ///     Ok(v) => println!("got {}", v),
    ///     Err(TryRecvError::Empty) => println!("empty"),
    ///     Err(TryRecvError::Disconnected) => println!("closed"),
    /// }
    /// ```
    pub fn try_recv(&mut self) -> Result<T, TryRecvError> {
        if !self.barrier.is_available(self.next_sequence) {
            if self.is_disconnected() {
                return Err(TryRecvError::Disconnected);
            }
            return Err(TryRecvError::Empty);
        }

        // prefetch disabled
        // // prefetch next slot while we read current
        // // safety: prefetch is safe even for invalid addresses on x86_64
        // unsafe {
        //     let next_ptr = self.shared.buffer.get_ptr(self.next_sequence + 1);
        //     prefetch_read(next_ptr);
        // }

        // safety: producer has published this sequence
        let event = unsafe { self.shared.buffer.read(self.next_sequence) };

        // update consumer cursor with relaxed ordering
        // producer polls with relaxed anyway, so no need for Release here
        self.shared.consumer_cursor.set_relaxed(self.next_sequence);
        self.next_sequence += 1;

        Ok(event)
    }

    /// peek at the next event without consuming it.
    ///
    /// returns a reference to the next event if available.
    ///
    /// # errors
    ///
    /// - `TryRecvError::Empty` if no events are available
    /// - `TryRecvError::Disconnected` if the channel is closed
    ///
    /// # example
    ///
    /// ```
    /// use land_channel::spsc;
    ///
    /// let (mut tx, mut rx) = spsc::channel::<i32>(64);
    /// tx.send(42).unwrap();
    ///
    /// // peek doesn't consume
    /// assert_eq!(*rx.peek().unwrap(), 42);
    /// assert_eq!(*rx.peek().unwrap(), 42);
    ///
    /// // recv consumes
    /// assert_eq!(rx.recv().unwrap(), 42);
    /// ```
    pub fn peek(&mut self) -> Result<&T, TryRecvError> {
        if !self.barrier.is_available(self.next_sequence) {
            if self.is_disconnected() {
                return Err(TryRecvError::Disconnected);
            }
            return Err(TryRecvError::Empty);
        }

        // safety: producer has published this sequence
        Ok(unsafe { self.shared.buffer.get(self.next_sequence) })
    }

    /// close the channel from the consumer side.
    ///
    /// after closing, the producer will receive `SendError` on subsequent sends.
    pub fn close(&self) {
        self.shared.closed.store(true, Ordering::Release);
    }

    /// check if the producer has disconnected.
    ///
    /// returns `true` if the producer has been dropped.
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

    /// get the number of pending events waiting to be consumed.
    #[inline]
    pub fn pending(&self) -> usize {
        let producer_seq = self.barrier.get_cursor();
        let diff = producer_seq - self.next_sequence + 1;
        if diff < 0 {
            0
        } else {
            diff as usize
        }
    }

    /// check if there are events available to receive.
    #[inline]
    pub fn has_pending(&self) -> bool {
        self.barrier.is_available(self.next_sequence)
    }

    /// poll and process all available events with a callback.
    ///
    /// this is the recommended API for event-driven consumption patterns.
    /// the callback receives:
    /// - `&T`: reference to the event
    /// - `i64`: the sequence number
    /// - `bool`: true if this is the last event in the current batch (end of batch)
    ///
    /// returns the number of events processed (0 if none available).
    ///
    /// # example
    ///
    /// ```
    /// use land_channel::spsc;
    ///
    /// let (mut tx, mut rx) = spsc::channel::<u64>(64);
    ///
    /// tx.send(1).unwrap();
    /// tx.send(2).unwrap();
    /// tx.send(3).unwrap();
    ///
    /// let count = rx.poll(|event, seq, end_of_batch| {
    ///     println!("Event {} at seq {}, eob={}", event, seq, end_of_batch);
    /// });
    /// assert_eq!(count, 3);
    /// ```
    #[inline]
    pub fn poll<F>(&mut self, mut handler: F) -> usize
    where
        F: FnMut(&T, i64, bool),
    {
        let available = self.barrier.get_cursor_relaxed();
        if available < self.next_sequence {
            return 0;
        }

        let count = (available - self.next_sequence + 1) as usize;

        // prefetch disabled
        // // process events with prefetch lookahead
        // const PREFETCH_DISTANCE: usize = 2;
        // for i in 0..count {
        //     if i + PREFETCH_DISTANCE < count {
        //         unsafe {
        //             let prefetch_ptr = self.shared.buffer.get_ptr(
        //                 self.next_sequence + (i + PREFETCH_DISTANCE) as i64
        //             );
        //             prefetch_read(prefetch_ptr);
        //         }
        //     }

        for i in 0..count {
            let seq = self.next_sequence + i as i64;
            let end_of_batch = i == count - 1;

            // safety: producer has published this sequence
            let event = unsafe { self.shared.buffer.get(seq) };
            handler(event, seq, end_of_batch);
        }

        // update cursor once for entire batch
        let last_seq = self.next_sequence + count as i64 - 1;
        self.next_sequence += count as i64;

        // single fence after batch, relaxed cursor update
        fence_release();
        self.shared.consumer_cursor.set_relaxed(last_seq);

        count
    }

    /// receive up to `max_count` events into the provided slice.
    ///
    /// this is more efficient than multiple `recv()` calls because:
    /// - single wait for available events
    /// - single memory fence after batch
    /// - batch cursor update
    ///
    /// returns the number of events actually received (may be less than `max_count`).
    ///
    /// # errors
    ///
    /// returns `RecvError` if the producer has disconnected and no events are available.
    ///
    /// # example
    ///
    /// ```
    /// use land_channel::spsc;
    ///
    /// let (mut tx, mut rx) = spsc::channel::<u64>(64);
    ///
    /// // send some values
    /// for i in 0..5 {
    ///     tx.send(i).unwrap();
    /// }
    ///
    /// // receive in batch
    /// let mut buf = [0u64; 10];
    /// let count = rx.recv_batch(&mut buf).unwrap();
    /// assert_eq!(count, 5);
    /// ```
    pub fn recv_batch(&mut self, out: &mut [T]) -> Result<usize, RecvError> {
        if out.is_empty() {
            return Ok(0);
        }

        // wait for at least one event
        loop {
            let available = self.barrier.get_cursor_relaxed();

            if available >= self.next_sequence {
                // acquire fence ensures we see buffer writes (required for ARM, free on x86)
                fence_acquire();

                // calculate how many we can receive
                let available_count = (available - self.next_sequence + 1) as usize;
                let count = core::cmp::min(out.len(), available_count);

                // prefetch disabled
                // // read events with prefetch lookahead
                // // prefetch 2 cache lines ahead (typically 128 bytes)
                // const PREFETCH_DISTANCE: usize = 2;
                // for i in 0..count {
                //     // prefetch ahead while reading current
                //     if i + PREFETCH_DISTANCE < count {
                //         unsafe {
                //             let prefetch_ptr = self.shared.buffer.get_ptr(
                //                 self.next_sequence + (i + PREFETCH_DISTANCE) as i64
                //             );
                //             prefetch_read(prefetch_ptr);
                //         }
                //     }
                //     out[i] = unsafe { self.shared.buffer.read(self.next_sequence + i as i64) };
                // }

                for i in 0..count {
                    out[i] = unsafe { self.shared.buffer.read(self.next_sequence + i as i64) };
                }

                // update cursor once for entire batch
                let last_seq = self.next_sequence + count as i64 - 1;
                self.next_sequence += count as i64;
                self.shared.consumer_cursor.set_relaxed(last_seq);

                return Ok(count);
            }

            // check for disconnection
            if self.is_disconnected() {
                if self.barrier.get_cursor() >= self.next_sequence {
                    continue;
                }
                return Err(RecvError);
            }

            // use the configured wait strategy
            let _ = self.barrier.wait_for(self.next_sequence);
        }
    }

    /// try to receive up to `max_count` events without blocking.
    ///
    /// returns immediately with whatever events are available (may be 0).
    ///
    /// # errors
    ///
    /// - `TryRecvError::Empty` if no events are available
    /// - `TryRecvError::Disconnected` if the producer was dropped and buffer is empty
    pub fn try_recv_batch(&mut self, out: &mut [T]) -> Result<usize, TryRecvError> {
        if out.is_empty() {
            return Ok(0);
        }

        let available = self.barrier.get_cursor_relaxed();

        if available < self.next_sequence {
            if self.is_disconnected() {
                return Err(TryRecvError::Disconnected);
            }
            return Err(TryRecvError::Empty);
        }

        // acquire fence ensures we see buffer writes (required for ARM, free on x86)
        fence_acquire();

        // calculate how many we can receive
        let available_count = (available - self.next_sequence + 1) as usize;
        let count = core::cmp::min(out.len(), available_count);

        // prefetch disabled
        // // read events with prefetch lookahead
        // const PREFETCH_DISTANCE: usize = 2;
        // for i in 0..count {
        //     if i + PREFETCH_DISTANCE < count {
        //         unsafe {
        //             let prefetch_ptr = self.shared.buffer.get_ptr(
        //                 self.next_sequence + (i + PREFETCH_DISTANCE) as i64
        //             );
        //             prefetch_read(prefetch_ptr);
        //         }
        //     }
        //     out[i] = unsafe { self.shared.buffer.read(self.next_sequence + i as i64) };
        // }

        for i in 0..count {
            out[i] = unsafe { self.shared.buffer.read(self.next_sequence + i as i64) };
        }

        // update cursor once for entire batch
        let last_seq = self.next_sequence + count as i64 - 1;
        self.next_sequence += count as i64;
        self.shared.consumer_cursor.set_relaxed(last_seq);

        Ok(count)
    }

    /// receive all available events into a Vec.
    ///
    /// this is a convenience method that allocates a Vec and fills it with
    /// all available events. for zero-allocation receiving, use [`recv_batch`](Self::recv_batch).
    ///
    /// # errors
    ///
    /// returns `RecvError` if the producer has disconnected and no events are available.
    pub fn recv_all(&mut self) -> Result<Vec<T>, RecvError> {
        // first wait for at least one event
        loop {
            let available = self.barrier.get_cursor_relaxed();

            if available >= self.next_sequence {
                let count = (available - self.next_sequence + 1) as usize;
                let mut out = Vec::with_capacity(count);

                // prefetch disabled
                // // read events with prefetch lookahead
                // const PREFETCH_DISTANCE: usize = 2;
                // for i in 0..count {
                //     if i + PREFETCH_DISTANCE < count {
                //         unsafe {
                //             let prefetch_ptr = self.shared.buffer.get_ptr(
                //                 self.next_sequence + (i + PREFETCH_DISTANCE) as i64
                //             );
                //             prefetch_read(prefetch_ptr);
                //         }
                //     }
                //     out.push(unsafe { self.shared.buffer.read(self.next_sequence + i as i64) });
                // }

                for i in 0..count {
                    out.push(unsafe { self.shared.buffer.read(self.next_sequence + i as i64) });
                }

                let last_seq = self.next_sequence + count as i64 - 1;
                self.next_sequence += count as i64;

                fence_release();
                self.shared.consumer_cursor.set_relaxed(last_seq);

                return Ok(out);
            }

            if self.is_disconnected() {
                if self.barrier.get_cursor() >= self.next_sequence {
                    continue;
                }
                return Err(RecvError);
            }

            let _ = self.barrier.wait_for(self.next_sequence);
        }
    }
}

impl<T, W: WaitStrategy> Drop for Consumer<T, W> {
    fn drop(&mut self) {
        self.close();
    }
}

// safety: consumer can be sent between threads if T is Send
unsafe impl<T: Send, W: WaitStrategy> Send for Consumer<T, W> {}

impl<T, W: WaitStrategy> core::fmt::Debug for Consumer<T, W> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // use cached value or direct check (avoid &mut self requirement)
        let disconnected = self.cached_disconnected
            || self.shared.closed.load(Ordering::Relaxed)
            || Arc::strong_count(&self.shared) == 1;
        f.debug_struct("Consumer")
            .field("next_sequence", &self.next_sequence)
            .field("pending", &self.pending())
            .field("disconnected", &disconnected)
            .finish()
    }
}
