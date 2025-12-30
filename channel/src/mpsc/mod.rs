//! multi producer single consumer (MPSC) channel.
//!
//! this module provides a high-performance, lock-free MPSC channel inspired by
//! the lmax disruptor pattern. multiple producers can send events concurrently
//! using atomic compare-and-swap operations.
//!
//! # key differences from SPSC
//!
//! - producers are `Clone` - multiple can exist simultaneously
//! - producers use `&self` (not `&mut self`) for send operations
//! - uses CAS for sequence claiming
//! - uses availability bitmap to track published sequences
//!
//! # example
//!
//! ```
//! use land_channel::mpsc;
//! use std::thread;
//!
//! let (tx, mut rx) = mpsc::channel::<u64>(1024);
//!
//! // clone for multiple producers
//! let tx1 = tx.clone();
//! let tx2 = tx.clone();
//!
//! let h1 = thread::spawn(move || {
//!     for i in 0..100 {
//!         tx1.send(i).unwrap();
//!     }
//! });
//!
//! let h2 = thread::spawn(move || {
//!     for i in 100..200 {
//!         tx2.send(i).unwrap();
//!     }
//! });
//!
//! h1.join().unwrap();
//! h2.join().unwrap();
//! drop(tx);  // drop original to signal completion
//!
//! // receive all 200 events (order not guaranteed)
//! let mut received = 0;
//! while let Ok(_) = rx.try_recv() {
//!     received += 1;
//! }
//! assert_eq!(received, 200);
//! ```

mod batch;
mod consumer;
mod producer;
mod shared;

pub use batch::MpscBatchSlots;
pub use consumer::Consumer;
pub use producer::Producer;

use crate::ringbuffer::RingBuffer;
use land_cpu::{CachePadded, Cursor, SpinLoopHintWait, WaitStrategy};
use shared::{Shared, BITS_PER_WORD};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
use std::sync::Arc;

/// create a new MPSC channel with the given capacity.
///
/// the capacity must be a power of 2. returns a (producer, consumer) pair.
///
/// # arguments
///
/// * `capacity` - number of slots in the ring buffer (must be power of 2)
///
/// # panics
///
/// panics if `capacity` is not a power of 2 or is zero.
///
/// # example
///
/// ```
/// use land_channel::mpsc;
///
/// let (tx, rx) = mpsc::channel::<String>(1024);
/// ```
pub fn channel<T>(capacity: usize) -> (Producer<T>, Consumer<T, SpinLoopHintWait>) {
    channel_with_wait(capacity, SpinLoopHintWait)
}

/// create a new MPSC channel with a custom wait strategy.
pub fn channel_with_wait<T, W: WaitStrategy>(
    capacity: usize,
    wait_strategy: W,
) -> (Producer<T>, Consumer<T, W>) {
    let (shared, wait_strategy) = create_shared(capacity, wait_strategy, None::<fn() -> T>);

    let producer = Producer {
        shared: Arc::clone(&shared),
    };

    let consumer = Consumer {
        shared,
        next_sequence: 0,
        _wait_strategy: wait_strategy,
    };

    (producer, consumer)
}

/// create a new MPSC channel with pre-initialized slots.
///
/// the factory function is called once for each slot. useful for zero-copy
/// patterns with pre-allocated buffers.
pub fn channel_with_factory<T, F, W>(
    capacity: usize,
    factory: F,
    wait_strategy: W,
) -> (Producer<T>, Consumer<T, W>)
where
    F: Fn() -> T,
    W: WaitStrategy,
{
    let (shared, wait_strategy) = create_shared(capacity, wait_strategy, Some(factory));

    let producer = Producer {
        shared: Arc::clone(&shared),
    };

    let consumer = Consumer {
        shared,
        next_sequence: 0,
        _wait_strategy: wait_strategy,
    };

    (producer, consumer)
}

fn create_shared<T, F, W>(
    capacity: usize,
    wait_strategy: W,
    factory: Option<F>,
) -> (Arc<Shared<T>>, W)
where
    F: Fn() -> T,
    W: WaitStrategy,
{
    // size availability bitmap to match capacity: one bit per slot
    let availability_words = (capacity + BITS_PER_WORD - 1) / BITS_PER_WORD;
    let availability: Box<[CachePadded<AtomicU64>]> = {
        let mut v = Vec::with_capacity(availability_words);
        for _ in 0..availability_words {
            // initialize to all 1s; for round 0, expected bit is 0, so unpublished
            v.push(CachePadded::new(AtomicU64::new(!0u64)));
        }
        v.into_boxed_slice()
    };

    let buffer = match factory {
        Some(f) => RingBuffer::with_factory(capacity, f),
        None => RingBuffer::new(capacity),
    };

    let shared = Arc::new(Shared {
        buffer,
        claim_cursor: Cursor::new(),
        consumer_cursor: Cursor::new(),
        availability,
        availability_words,
        producer_count: CachePadded::new(AtomicUsize::new(1)),
        closed: CachePadded::new(AtomicBool::new(false)),
        capacity,
    });

    (shared, wait_strategy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{RecvError, SendError, TryRecvError};
    use std::collections::HashSet;
    use std::thread;

    #[test]
    fn test_basic_send_recv() {
        let (tx, mut rx) = channel::<u64>(64);

        tx.send(42).unwrap();
        assert_eq!(rx.recv().unwrap(), 42);
    }

    #[test]
    fn test_multiple_sends() {
        let (tx, mut rx) = channel::<u64>(64);

        for i in 0..10 {
            tx.send(i).unwrap();
        }

        for i in 0..10 {
            assert_eq!(rx.recv().unwrap(), i);
        }
    }

    #[test]
    fn test_try_recv_empty() {
        let (_tx, mut rx) = channel::<u64>(64);

        match rx.try_recv() {
            Err(TryRecvError::Empty) => {}
            _ => panic!("expected Empty error"),
        }
    }

    #[test]
    fn test_producer_dropped() {
        let (tx, mut rx) = channel::<u64>(64);

        drop(tx);

        match rx.recv() {
            Err(RecvError) => {}
            _ => panic!("expected RecvError"),
        }
    }

    #[test]
    fn test_consumer_dropped() {
        let (tx, rx) = channel::<u64>(64);

        drop(rx);

        match tx.send(42) {
            Err(SendError(42)) => {}
            _ => panic!("expected SendError"),
        }
    }

    #[test]
    fn test_multiple_producers() {
        let (tx, mut rx) = channel::<u64>(1024);

        let tx1 = tx.clone();
        let tx2 = tx.clone();

        let h1 = thread::spawn(move || {
            for i in 0..100 {
                tx1.send(i).unwrap();
            }
        });

        let h2 = thread::spawn(move || {
            for i in 100..200 {
                tx2.send(i).unwrap();
            }
        });

        h1.join().unwrap();
        h2.join().unwrap();
        drop(tx);

        // collect all received values
        let mut received = HashSet::new();
        while let Ok(v) = rx.try_recv() {
            received.insert(v);
        }

        assert_eq!(received.len(), 200);
        for i in 0..200 {
            assert!(received.contains(&i), "missing {}", i);
        }
    }

    #[test]
    fn test_clone_producer() {
        let (tx, _rx) = channel::<u64>(64);

        let tx2 = tx.clone();
        let tx3 = tx.clone();

        tx.send(1).unwrap();
        tx2.send(2).unwrap();
        tx3.send(3).unwrap();
    }

    #[test]
    fn test_peek() {
        let (tx, rx) = channel::<u64>(64);

        tx.send(42).unwrap();

        assert_eq!(*rx.peek().unwrap(), 42);
        assert_eq!(*rx.peek().unwrap(), 42); // does not consume
    }

    #[test]
    fn test_pending() {
        let (tx, rx) = channel::<u64>(64);

        assert_eq!(rx.pending(), 0);

        tx.send(1).unwrap();
        tx.send(2).unwrap();

        assert_eq!(rx.pending(), 2);
    }

    #[test]
    fn test_close() {
        let (tx, mut rx) = channel::<u64>(64);

        tx.send(42).unwrap();
        tx.close();

        // can still receive buffered
        assert_eq!(rx.recv().unwrap(), 42);

        // but channel is closed
        match rx.recv() {
            Err(RecvError) => {}
            _ => panic!("expected RecvError"),
        }
    }

    #[test]
    fn test_debug() {
        let (tx, rx) = channel::<u64>(64);
        let _ = format!("{:?}", tx);
        let _ = format!("{:?}", rx);
    }
}
