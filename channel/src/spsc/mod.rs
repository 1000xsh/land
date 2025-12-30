//! single producer single consumer (SPSC) channel.
//!
//! this module provides a high-performance, lock-free SPSC channel inspired by
//! the LMAX disruptor pattern. it achieves low latency through:
//!
//! - pre-allocated ring buffer (no allocation in hot path)
//! - sequence-based coordination using atomics (no locks)
//! - cache-line padding to prevent false sharing
//! - zero-copy in-place mutation support
//!
//! # example
//!
//! ```
//! use land_channel::spsc;
//!
//! // create a channel with 1024 slots
//! let (mut tx, mut rx) = spsc::channel::<u64>(1024);
//!
//! // send a value
//! tx.send(42).unwrap();
//!
//! // receive the value
//! let value = rx.recv().unwrap();
//! assert_eq!(value, 42);
//! ```
//!
//! # zero-copy pattern
//!
//! for maximum performance with large types, use the claim/publish pattern:
//!
//! ```
//! use land_channel::spsc;
//!
//! let (mut tx, mut rx) = spsc::channel::<Vec<u8>>(1024);
//!
//! // zero-copy send: write directly into the ring buffer
//! {
//!     let slot = tx.claim().unwrap();
//!     slot.clear();
//!     slot.extend_from_slice(b"hello world");
//! }
//! tx.publish();
//!
//! // receive
//! let data = rx.recv().unwrap();
//! assert_eq!(&data[..], b"hello world");
//! ```

mod batch;
mod consumer;
mod producer;
mod shared;

pub use batch::{BatchSlotIterMut, BatchSlots};
pub use consumer::Consumer;
pub use producer::Producer;

use crate::barrier::SequenceBarrier;
use crate::ringbuffer::RingBuffer;
use land_cpu::{CachePadded, Cursor, SpinLoopHintWait, WaitStrategy, INITIAL_CURSOR_VALUE};
use shared::Shared;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// create a new SPSC channel with the given capacity.
///
/// the capacity must be a power of 2. returns a (producer, consumer) pair.
///
/// uses `SpinLoopHintWait` as the default wait strategy.
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
/// use land_channel::spsc;
///
/// let (tx, rx) = spsc::channel::<String>(1024);
/// ```
pub fn channel<T>(capacity: usize) -> (Producer<T>, Consumer<T, SpinLoopHintWait>) {
    channel_with_wait(capacity, SpinLoopHintWait)
}

/// create a new SPSC channel with a custom wait strategy.
///
/// # arguments
///
/// * `capacity` - number of slots in the ring buffer (must be power of 2)
/// * `wait_strategy` - strategy for blocking receives
///
/// # example
///
/// ```
/// use land_channel::spsc;
/// use land_channel::BusySpinWait;
///
/// // use busy spin for lowest latency
/// let (tx, rx) = spsc::channel_with_wait::<u64, _>(1024, BusySpinWait);
/// ```
pub fn channel_with_wait<T, W: WaitStrategy>(
    capacity: usize,
    wait_strategy: W,
) -> (Producer<T>, Consumer<T, W>) {
    let shared = Arc::new(Shared {
        buffer: RingBuffer::new(capacity),
        producer_cursor: Cursor::new(),
        consumer_cursor: Cursor::new(),
        closed: CachePadded::new(AtomicBool::new(false)),
    });

    // create barrier pointing to producer cursor
    // safety: the shared Arc keeps the cursor alive as long as the consumer exists
    let barrier =
        unsafe { SequenceBarrier::new(&shared.producer_cursor as *const Cursor, wait_strategy) };

    let producer = Producer {
        shared: Arc::clone(&shared),
        cached_consumer: INITIAL_CURSOR_VALUE,
        next_sequence: 0,
        capacity: capacity as i64,
        claimed: false,
        cached_disconnected: false,
    };

    let consumer = Consumer {
        shared,
        next_sequence: 0,
        barrier,
        cached_disconnected: false,
    };

    (producer, consumer)
}

/// create a new SPSC channel with pre-initialized slots.
///
/// the factory function is called once for each slot to initialize it.
/// this is useful for types that benefit from pre-allocation.
///
/// # arguments
///
/// * `capacity` - number of slots (must be power of 2)
/// * `factory` - function to create initial values
/// * `wait_strategy` - strategy for blocking receives
///
/// # example
///
/// ```
/// use land_channel::spsc;
/// use land_channel::SpinLoopHintWait;
///
/// // pre-allocate strings with capacity
/// let (tx, rx) = spsc::channel_with_factory(
///     1024,
///     || String::with_capacity(256),
///     SpinLoopHintWait,
/// );
/// ```
pub fn channel_with_factory<T, F, W>(
    capacity: usize,
    factory: F,
    wait_strategy: W,
) -> (Producer<T>, Consumer<T, W>)
where
    F: Fn() -> T,
    W: WaitStrategy,
{
    let shared = Arc::new(Shared {
        buffer: RingBuffer::with_factory(capacity, factory),
        producer_cursor: Cursor::new(),
        consumer_cursor: Cursor::new(),
        closed: CachePadded::new(AtomicBool::new(false)),
    });

    let barrier =
        unsafe { SequenceBarrier::new(&shared.producer_cursor as *const Cursor, wait_strategy) };

    let producer = Producer {
        shared: Arc::clone(&shared),
        cached_consumer: INITIAL_CURSOR_VALUE,
        next_sequence: 0,
        capacity: capacity as i64,
        claimed: false,
        cached_disconnected: false,
    };

    let consumer = Consumer {
        shared,
        next_sequence: 0,
        barrier,
        cached_disconnected: false,
    };

    (producer, consumer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{RecvError, SendError, TryRecvError, TrySendError};
    use std::thread;

    #[test]
    fn test_basic_send_recv() {
        let (mut tx, mut rx) = channel::<u64>(64);

        tx.send(42).unwrap();
        assert_eq!(rx.recv().unwrap(), 42);
    }

    #[test]
    fn test_multiple_sends() {
        let (mut tx, mut rx) = channel::<u64>(64);

        for i in 0..10 {
            tx.send(i).unwrap();
        }

        for i in 0..10 {
            assert_eq!(rx.recv().unwrap(), i);
        }
    }

    #[test]
    fn test_try_send_full() {
        let (mut tx, _rx) = channel::<u64>(4);

        // fill the buffer
        for i in 0..4 {
            assert!(tx.try_send(i).is_ok());
        }

        // should be full now
        match tx.try_send(100) {
            Err(TrySendError::Full(100)) => {}
            _ => panic!("expected Full error"),
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
        let (mut tx, rx) = channel::<u64>(64);

        drop(rx);

        match tx.send(42) {
            Err(SendError(42)) => {}
            _ => panic!("expected SendError"),
        }
    }

    #[test]
    fn test_peek() {
        let (mut tx, mut rx) = channel::<u64>(64);

        tx.send(42).unwrap();

        // peek doesn't consume
        assert_eq!(*rx.peek().unwrap(), 42);
        assert_eq!(*rx.peek().unwrap(), 42);

        // recv consumes
        assert_eq!(rx.recv().unwrap(), 42);

        // now empty
        assert!(rx.peek().is_err());
    }

    #[test]
    fn test_claim_publish() {
        let (mut tx, mut rx) = channel_with_factory(64, String::new, SpinLoopHintWait);

        {
            let slot = tx.claim().unwrap();
            slot.clear();
            slot.push_str("hello");
        }
        tx.publish();

        assert_eq!(rx.recv().unwrap(), "hello");
    }

    #[test]
    fn test_try_claim() {
        let (mut tx, mut rx) = channel_with_factory(2, || 0i32, SpinLoopHintWait);

        // claim and publish first
        {
            let slot = tx.try_claim().unwrap();
            *slot = 1;
        }
        tx.publish();

        {
            let slot = tx.try_claim().unwrap();
            *slot = 2;
        }
        tx.publish();

        // buffer full
        assert!(tx.try_claim().is_err());

        // consume one
        assert_eq!(rx.recv().unwrap(), 1);

        // can claim again
        {
            let slot = tx.try_claim().unwrap();
            *slot = 3;
        }
        tx.publish();

        assert_eq!(rx.recv().unwrap(), 2);
        assert_eq!(rx.recv().unwrap(), 3);
    }

    #[test]
    fn test_threaded() {
        let (mut tx, mut rx) = channel::<u64>(1024);

        let producer = thread::spawn(move || {
            for i in 0..10000 {
                tx.send(i).unwrap();
            }
        });

        let consumer = thread::spawn(move || {
            let mut sum = 0u64;
            for _ in 0..10000 {
                sum += rx.recv().unwrap();
            }
            sum
        });

        producer.join().unwrap();
        let sum = consumer.join().unwrap();

        // sum of 0..10000
        assert_eq!(sum, (0..10000u64).sum());
    }

    #[test]
    fn test_pending() {
        let (mut tx, rx) = channel::<u64>(64);

        assert_eq!(rx.pending(), 0);
        assert!(!rx.has_pending());

        tx.send(1).unwrap();
        tx.send(2).unwrap();

        assert_eq!(rx.pending(), 2);
        assert!(rx.has_pending());
    }

    #[test]
    fn test_available_slots() {
        let (mut tx, mut rx) = channel::<u64>(4);

        assert_eq!(tx.available_slots(), 4);

        tx.send(1).unwrap();
        assert_eq!(tx.available_slots(), 3);

        tx.send(2).unwrap();
        tx.send(3).unwrap();
        tx.send(4).unwrap();
        assert_eq!(tx.available_slots(), 0);

        rx.recv().unwrap();
        rx.recv().unwrap();
        // consumer cursor update happens on recv, so available slots should increase
        assert!(tx.available_slots() >= 1);
    }

    #[test]
    fn test_close() {
        let (mut tx, mut rx) = channel::<u64>(64);

        tx.send(42).unwrap();
        tx.close();

        // can still receive buffered
        assert_eq!(rx.recv().unwrap(), 42);

        // but producer is marked as closed
        match rx.recv() {
            Err(RecvError) => {}
            _ => panic!("expected RecvError"),
        }
    }

    #[test]
    fn test_with_wait_strategy() {
        use land_cpu::BusySpinWait;

        let (mut tx, mut rx) = channel_with_wait::<u64, _>(64, BusySpinWait);

        tx.send(42).unwrap();
        assert_eq!(rx.recv().unwrap(), 42);
    }

    #[test]
    fn test_wrap_around() {
        let (mut tx, mut rx) = channel::<u64>(4);

        // send more than capacity to test wrap-around
        for round in 0..5 {
            for i in 0..4 {
                tx.send(round * 4 + i).unwrap();
            }
            for i in 0..4 {
                assert_eq!(rx.recv().unwrap(), round * 4 + i);
            }
        }
    }

    #[test]
    fn test_debug() {
        let (tx, rx) = channel::<u64>(64);
        let _ = format!("{:?}", tx);
        let _ = format!("{:?}", rx);
    }
}
