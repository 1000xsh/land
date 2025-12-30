//! disconnection detection helpers.
//!
//! provides cached disconnection state to avoid repeated expensive atomic operations.
//! once disconnected, the state is immutable (never reconnects), allowing safe caching.
//!
//! # note
//!
//! currently unused - SPSC uses inline caching, MPSC uses no caching.
//! consider replacing inline implementations with this helper to:
//! - reduce code duplication (9 lines → 4 lines per use)
//! - add caching to MPSC (currently does 2-3 atomic loads per check)
//! - improve maintainability (single implementation to optimize)

#[allow(dead_code)]
/// cached disconnection state to avoid repeated atomic loads.
///
/// once disconnected, stays disconnected (immutable state transition).
/// this avoids calling `Arc::strong_count()` and other atomic operations
/// on every `is_disconnected()` call, which would add latency to the hot path.
///
/// # performance
///
/// the caching optimization is critical because `is_disconnected()` is called:
/// - in every send/recv loop iteration when waiting
/// - in every try_send/try_recv operation
///
/// without caching, this would add an atomic load to every check.
///
/// # example
///
/// ```ignore
/// struct Producer {
///     cached_disconnected: CachedDisconnection,
///     // ...
/// }
///
/// impl Producer {
///     fn is_disconnected(&mut self) -> bool {
///         self.cached_disconnected.check(|| {
///             self.shared.closed.load(Ordering::Relaxed)
///                 || Arc::strong_count(&self.shared) == 1
///         })
///     }
/// }
/// ```
pub struct CachedDisconnection {
    cached: bool,
}

impl CachedDisconnection {
    /// create a new disconnection state (initially not disconnected).
    #[inline]
    #[allow(dead_code)]
    pub const fn new() -> Self {
        Self { cached: false }
    }

    /// check disconnection status with caching.
    ///
    /// once true, always returns true without calling check_fn.
    /// this is safe because disconnection is a one-way state transition.
    ///
    /// # arguments
    ///
    /// * `check_fn` - closure that performs the actual disconnection check
    ///
    /// # returns
    ///
    /// `true` if disconnected, `false` otherwise
    #[inline]
    #[allow(dead_code)]
    pub fn check<F>(&mut self, check_fn: F) -> bool
    where
        F: FnOnce() -> bool,
    {
        // fast path: return cached value (no atomic load)
        if self.cached {
            return true;
        }

        // slow path: perform actual check
        let disconnected = check_fn();
        if disconnected {
            self.cached = true;
        }
        disconnected
    }
}

impl Default for CachedDisconnection {
    #[allow(dead_code)]
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[allow(dead_code)]
mod tests {
    use super::*;

    #[test]
    fn test_cached_disconnection_initially_false() {
        let mut cached = CachedDisconnection::new();
        let result = cached.check(|| false);
        assert!(!result);
    }

    #[test]
    fn test_cached_disconnection_caches_true() {
        let mut cached = CachedDisconnection::new();
        let mut call_count = 0;

        // first call - returns true
        let result = cached.check(|| {
            call_count += 1;
            true
        });
        assert!(result);
        assert_eq!(call_count, 1);

        // second call - should not invoke check_fn (cached)
        let result = cached.check(|| {
            call_count += 1;
            false // would return false if called, but shouldn't be called
        });
        assert!(result); // still true from cache
        assert_eq!(call_count, 1); // not incremented
    }

    #[test]
    fn test_cached_disconnection_multiple_false() {
        let mut cached = CachedDisconnection::new();
        let mut call_count = 0;

        // multiple false calls - always invokes check_fn
        for _ in 0..5 {
            let result = cached.check(|| {
                call_count += 1;
                false
            });
            assert!(!result);
        }
        assert_eq!(call_count, 5); // called every time
    }

    #[test]
    fn test_default() {
        let mut cached = CachedDisconnection::default();
        let result = cached.check(|| false);
        assert!(!result);
    }
}
