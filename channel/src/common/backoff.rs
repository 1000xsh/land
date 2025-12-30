//! progressive backoff for spin-wait loops.
//!
//! this module provides a progressive backoff strategy used across SPSC and MPSC
//! implementations to reduce cache contention during wait loops while maintaining
//! low latency.

/// progressive backoff to reduce cache contention during waits.
///
/// three-phase strategy optimized for low-latency:
/// - **phase 1 (0-100)**: aggressive spinning for short waits
/// - **phase 2 (100-1000)**: CPU pause with increasing count to reduce power/contention
/// - **phase 3 (1000+)**: occasional yield to prevent CPU starvation
///
/// # arguments
///
/// * `iteration` - mutable counter tracking backoff progression
///
/// # performance
///
/// must be `#[inline]` for zero-cost abstraction.
///
/// # example
///
/// ```ignore
/// let mut iteration = 0u32;
/// loop {
///     if condition_met() {
///         break;
///     }
///     wait_backoff(&mut iteration);
/// }
/// ```
#[inline]
pub fn wait_backoff(iteration: &mut u32) {
    let i = *iteration;
    if i < 100 {
        // phase 1: aggressive spinning for very short waits
        core::hint::spin_loop();
    } else if i < 1000 {
        // phase 2: progressive pause - more pauses as contention increases
        let pauses = ((i - 100) >> 3) + 1;
        for _ in 0..pauses.min(32) {
            core::hint::spin_loop();
        }
    } else {
        // phase 3: occasional yield to prevent CPU starvation
        if i % 16 == 0 {
            std::thread::yield_now();
        } else {
            core::hint::spin_loop();
        }
    }
    *iteration = iteration.wrapping_add(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backoff_progression() {
        let mut iter = 0;

        // phase 1: 0-99
        for _ in 0..100 {
            wait_backoff(&mut iter);
        }
        assert_eq!(iter, 100);

        // phase 2: 100-999
        for _ in 0..900 {
            wait_backoff(&mut iter);
        }
        assert_eq!(iter, 1000);

        // phase 3: 1000+
        for _ in 0..100 {
            wait_backoff(&mut iter);
        }
        assert_eq!(iter, 1100);
    }

    #[test]
    fn test_wrapping() {
        let mut iter = u32::MAX;
        wait_backoff(&mut iter);
        assert_eq!(iter, 0); // wrapped
    }
}
