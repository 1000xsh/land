//! # loop unroll - loop unrolling library
//!
//! zero-dependency declarative macros for compile-time loop unrolling.
//! provides **7.4x speedup** in the right scenarios.
//!
//! ## when to use (benchmark-proven)
//!
//! | scenario | speedup | use unroll!? |
//! |----------|---------|--------------|
//! | **dynamic bounds** (slice with known size) | **7.4x** | yes |
//! | **128+ iterations** | **2x** | yes |
//! | small fixed arrays (8-64) | 1x | no, LLVM handles it |
//!
//! ### use case: dynamic bounds
//!
//! when you know the size at compile time but LLVM doesn't:
//!
//! ```rust
//! use land_unroll::unroll;
//!
//! fn process_slice(data: &[u64]) {
//!     // you know it's always 8 elements, but LLVM sees runtime bounds
//!     unroll!(8, |i| {
//!         // 7.4x faster than: for i in 0..data.len()
//!         let _ = data[i];
//!     });
//! }
//! ```
//!
//! common in trading (order book levels), network protocols (fixed headers),
//! and simd operations (known vector widths).
//!
//! ### skip for small fixed arrays
//!
//! LLVM already unrolls small fixed-size loops. no benefit:
//!
//! ```rust
//! use land_unroll::unroll;
//!
//! let data = [1u64; 8];
//! let mut sum = 0u64;
//!
//! // these produce identical assembly:
//! unroll!(8, |i| sum += data[i]);  // unroll! version
//! for i in 0..8 { sum += data[i]; }  // regular loop - same speed
//! ```
//! ## usage
//!
//! ```rust
//! use land_unroll::unroll;
//!
//! let mut sum = 0;
//! unroll!(8, |i| sum += i);
//! assert_eq!(sum, 28);
//!
//! let mut powers = [0u64; 8];
//! unroll!(8, |i| powers[i] = 1 << i);
//! ```
//!
//! ## recursion limit
//!
//! for loop counts > 128, add to your crate root:
//! ```rust,ignore
//! #![recursion_limit = "256"]
//! ```

#![no_std]
#![recursion_limit = "256"]
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

// core unroll macro
#[macro_use]
mod unroll;

// tests
#[cfg(test)]
mod tests;

// macros are already exported at crate root via #[macro_export]
