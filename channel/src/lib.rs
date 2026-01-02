//! low-latency inter-thread communication inspired by lmax disruptor.
//!
//! this crate provides zero-copy, lock-free channels for high-performance
//! applications.
//!
//! # features
//!
//! - pre-allocated ring buffers (no allocation in hot path)
//! - sequence-based coordination (no locks)
//! - multiple wait strategies for latency/CPU trade-offs
//! - zero-copy in-place mutation support
//! - cache-line padding to prevent false sharing
//!
//! # channel types
//!
//! - [`spsc`]: single producer single consumer - lowest latency
//! - [`mpsc`]: multi producer single consumer - multiple senders
//!
//! # example
//!
//! ```ignore
//! use land_channel::spsc;
//!
//! // create channel with 1024-slot buffer
//! let (mut producer, mut consumer) = spsc::channel::<u64>(1024);
//!
//! // producer thread
//! std::thread::spawn(move || {
//!     for i in 0..1000 {
//!         producer.send(i).unwrap();
//!     }
//! });
//!
//! // consumer
//! for _ in 0..1000 {
//!     let value = consumer.recv().unwrap();
//! }
//! ```

#![warn(missing_docs)]
#![warn(rust_2018_idioms)]

pub mod barrier;
pub mod error;
pub mod mpsc;
pub mod ringbuffer;
pub mod spsc;

pub(crate) mod common;

pub use error::{RecvError, SendError, TryRecvError, TrySendError};
pub use land_cpu::{CachePadded, Cursor, CACHE_LINE_SIZE, INITIAL_CURSOR_VALUE};
pub use ringbuffer::RingBuffer;

pub use land_cpu::{
    wait_strategy, BackoffWait, BusySpinWait, SpinLoopHintWait, WaitStrategy, YieldingWait,
};

// re-export fence helpers for architecture-aware memory ordering
pub use land_cpu::{
    cpu_pause, fence_acq_rel, fence_acquire, fence_release,
};
