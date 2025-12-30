//! common utilities shared across channel implementations.
//!
//! this module contains performance-critical helper functions reused across
//! SPSC and MPSC implementations. all functions are designed for zero-cost
//! abstraction with aggressive inlining.
//!
//! # modules
//!
//! - [`backoff`]: progressive backoff for spin-wait loops
//! - [`disconnect`]: cached disconnection detection

pub mod backoff;

#[allow(dead_code)]
pub mod disconnect;

pub use backoff::wait_backoff;
