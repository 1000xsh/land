//! shared state between SPSC producer and consumer.

use crate::ringbuffer::RingBuffer;
use land_cpu::{CachePadded, Cursor};
use std::sync::atomic::AtomicBool;

/// shared state between producer and consumer.
pub(super) struct Shared<T> {
    /// the ring buffer storing events.
    pub(super) buffer: RingBuffer<T>,
    /// producer's published sequence (what's available to consume).
    pub(super) producer_cursor: Cursor,
    /// consumer's processed sequence (what's been consumed).
    pub(super) consumer_cursor: Cursor,
    /// channel closed flag.
    pub(super) closed: CachePadded<AtomicBool>,
}

impl<T> Drop for Shared<T> {
    fn drop(&mut self) {
        // drop any unconsumed events to prevent memory leaks for types with Drop.
        // events from (consumer_seq + 1) to producer_seq are unconsumed.
        let consumer_seq = self.consumer_cursor.value();
        let producer_seq = self.producer_cursor.value();

        // only drop if there are unconsumed events
        if producer_seq > consumer_seq {
            for seq in (consumer_seq + 1)..=producer_seq {
                // safety: these slots were written by the producer and not yet consumed.
                // we have exclusive access since we're in Drop.
                unsafe {
                    let ptr = self.buffer.get_ptr_mut(seq);
                    core::ptr::drop_in_place(ptr);
                }
            }
        }
    }
}
