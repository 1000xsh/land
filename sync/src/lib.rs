//! lock-free synchronization primitives.
//!
//! this crate provides synchronization primitives optimized for scenarios where:
//! - read operations vastly outnumber writes
//! - latency is critical (nanosecond-level reads)
//! - single-writer semantics are acceptable
//!
//! # available primitives
//!
//! - [`SeqLock`]: sequence lock for single-writer, multi-reader scenarios
//!
//! # example
//!
//! ```
//! use land_sync::SeqLock;
//!
//! // create a SeqLock with initial data
//! let lock = SeqLock::new([0u64; 4]);
//!
//! // write (single-threaded only)
//! lock.write([1, 2, 3, 4]);
//!
//! // read (from any thread, lock-free)
//! let data = lock.read();
//! assert_eq!(data, [1, 2, 3, 4]);
//! ```

mod seqlock;

pub use seqlock::SeqLock;
