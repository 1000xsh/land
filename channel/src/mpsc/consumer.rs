//! consumer implementation for MPSC channel.

use super::shared::Shared;
use crate::common::wait_backoff;
use crate::error::{RecvError, TryRecvError};
use land_cpu::{SpinLoopHintWait, WaitStrategy, fence_acquire};
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// consumer handle for MPSC channel.
///
/// the consumer receives events from the channel. there can only be one
/// consumer for an MPSC channel.
pub struct Consumer<T, W: WaitStrategy = SpinLoopHintWait> {
    pub(super) shared: Arc<Shared<T>>,
    /// next sequence to consume.
    pub(super) next_sequence: i64,
    /// wait strategy for blocking receives.
    pub(super) _wait_strategy: W,
}

impl<T, W: WaitStrategy> Consumer<T, W> {
    /// receive the next event from the channel.
    ///
    /// blocks until an event is available.
    ///
    /// # errors
    ///
    /// returns `RecvError` if all producers have been dropped and buffer is empty.
    pub fn recv(&mut self) -> Result<T, RecvError> {
        let mut iteration = 0u32;
        loop {
            // scan bitmap directly to check if our sequence is published
            if self.shared.is_published(self.next_sequence) {
                // fence ensures we see the buffer write
                fence_acquire();
                // event is available
                let event = unsafe { self.shared.buffer.read(self.next_sequence) };
                self.shared.consumer_cursor.set_relaxed(self.next_sequence);
                self.next_sequence += 1;
                return Ok(event);
            }

            // check for disconnection
            if self.is_disconnected() {
                // final check - maybe a producer published just before closing
                if self.shared.is_published(self.next_sequence) {
                    continue;
                }
                return Err(RecvError);
            }

            wait_backoff(&mut iteration);
        }
    }

    /// try to receive an event without blocking.
    ///
    /// # errors
    ///
    /// - `TryRecvError::Empty` if no events are available
    /// - `TryRecvError::Disconnected` if all producers dropped and buffer empty
    pub fn try_recv(&mut self) -> Result<T, TryRecvError> {
        // check bitmap directly
        if !self.shared.is_published(self.next_sequence) {
            if self.is_disconnected() {
                return Err(TryRecvError::Disconnected);
            }
            return Err(TryRecvError::Empty);
        }

        // fence ensures we see the buffer write
        fence_acquire();
        let event = unsafe { self.shared.buffer.read(self.next_sequence) };
        self.shared.consumer_cursor.set_relaxed(self.next_sequence);
        self.next_sequence += 1;
        Ok(event)
    }

    /// peek at the next event without consuming it.
    pub fn peek(&self) -> Result<&T, TryRecvError> {
        // check bitmap directly
        if !self.shared.is_published(self.next_sequence) {
            if self.is_disconnected() {
                return Err(TryRecvError::Disconnected);
            }
            return Err(TryRecvError::Empty);
        }

        Ok(unsafe { self.shared.buffer.get(self.next_sequence) })
    }

    /// close the channel from the consumer side.
    pub fn close(&self) {
        self.shared.closed.store(true, Ordering::Release);
    }

    /// check if all producers have disconnected.
    #[inline]
    pub fn is_disconnected(&self) -> bool {
        self.shared.closed.load(Ordering::Acquire)
    }

    /// get the number of pending events.
    ///
    /// scans the bitmap to find how many contiguous events are available.
    #[inline]
    pub fn pending(&self) -> usize {
        let highest = self.shared.highest_published(self.next_sequence);
        let diff = highest - self.next_sequence + 1;
        if diff < 0 { 0 } else { diff as usize }
    }

    /// receive multiple events into a buffer (blocking).
    ///
    /// waits for at least one event, then receives as many contiguous events
    /// as are available (up to buffer capacity).
    ///
    /// # returns
    ///
    /// number of events received (always >= 1 on success).
    ///
    /// # errors
    ///
    /// returns `RecvError` if all producers dropped and no events available.
    pub fn recv_batch(&mut self, out: &mut [T]) -> Result<usize, RecvError> {
        if out.is_empty() {
            return Ok(0);
        }

        // wait for at least one event
        let mut iteration = 0u32;
        loop {
            let highest = self.shared.highest_published(self.next_sequence);
            if highest >= self.next_sequence {
                // calculate how many we can receive
                let available = (highest - self.next_sequence + 1) as usize;
                let count = available.min(out.len());

                // read events into output buffer
                for i in 0..count {
                    let seq = self.next_sequence + i as i64;
                    out[i] = unsafe { self.shared.buffer.read(seq) };
                }

                // update cursor once for entire batch
                let last_seq = self.next_sequence + count as i64 - 1;
                self.next_sequence += count as i64;
                self.shared.consumer_cursor.set_relaxed(last_seq);

                return Ok(count);
            }

            if self.is_disconnected() {
                return Err(RecvError);
            }

            // progressive backoff
            wait_backoff(&mut iteration);
        }
    }

    /// try to receive multiple events without blocking.
    ///
    /// # returns
    ///
    /// number of events received (may be 0 if none available).
    ///
    /// # errors
    ///
    /// returns `TryRecvError::Disconnected` if all producers dropped.
    pub fn try_recv_batch(&mut self, out: &mut [T]) -> Result<usize, TryRecvError> {
        if out.is_empty() {
            return Ok(0);
        }

        let highest = self.shared.highest_published(self.next_sequence);
        if highest < self.next_sequence {
            if self.is_disconnected() {
                return Err(TryRecvError::Disconnected);
            }
            return Err(TryRecvError::Empty);
        }

        // calculate how many we can receive
        let available = (highest - self.next_sequence + 1) as usize;
        let count = available.min(out.len());

        // read events into output buffer
        for i in 0..count {
            let seq = self.next_sequence + i as i64;
            out[i] = unsafe { self.shared.buffer.read(seq) };
        }

        // update cursor once for entire batch
        let last_seq = self.next_sequence + count as i64 - 1;
        self.next_sequence += count as i64;
        self.shared.consumer_cursor.set_relaxed(last_seq);

        Ok(count)
    }

    /// poll and process all available contiguous events with a callback.
    ///
    /// this is the recommended API for event-driven consumption patterns.
    /// the callback receives:
    /// - `&T`: reference to the event
    /// - `i64`: the sequence number
    /// - `bool`: true if this is the last event in the current batch (end of batch)
    ///
    /// returns the number of events processed (0 if none available).
    ///
    /// # note
    ///
    /// in MPSC, producers can publish out-of-order, so this processes only
    /// contiguous published sequences (no gaps).
    ///
    /// # example
    ///
    /// ```
    /// use land_channel::mpsc;
    ///
    /// let (tx, mut rx) = mpsc::channel::<u64>(64);
    ///
    /// tx.send(1).unwrap();
    /// tx.send(2).unwrap();
    ///
    /// let count = rx.poll(|event, seq, end_of_batch| {
    ///     println!("Event {} at seq {}, eob={}", event, seq, end_of_batch);
    /// });
    /// assert_eq!(count, 2);
    /// ```
    #[inline]
    pub fn poll<F>(&mut self, mut handler: F) -> usize
    where
        F: FnMut(&T, i64, bool),
    {
        // find highest contiguous published sequence
        let highest = self.shared.highest_published(self.next_sequence);
        if highest < self.next_sequence {
            return 0;
        }

        let count = (highest - self.next_sequence + 1) as usize;

        // process events - no prefetch in MPSC due to potential cache thrashing
        // from multiple producer writes
        for i in 0..count {
            let seq = self.next_sequence + i as i64;
            let end_of_batch = i == count - 1;

            // safety: bitmap guarantees this sequence is published
            let event = unsafe { self.shared.buffer.get(seq) };
            handler(event, seq, end_of_batch);
        }

        // update cursor once for entire batch
        let last_seq = self.next_sequence + count as i64 - 1;
        self.next_sequence += count as i64;
        self.shared.consumer_cursor.set_relaxed(last_seq);

        count
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
        f.debug_struct("mpsc::Consumer")
            .field("next_sequence", &self.next_sequence)
            .field("pending", &self.pending())
            .field("disconnected", &self.is_disconnected())
            .finish()
    }
}
