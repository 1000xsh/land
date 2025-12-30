//! SeqLock (sequence lock) provides low-latency reads with single-writer
//! semantics. readers never block and can detect when they have read inconsistent data
//! by checking version numbers.
//!
//! # characteristics
//!
//! - **single writer**: only one thread may write at a time (not enforced, caller responsibility)
//! - **multiple readers**: any number of threads can read concurrently
//! - **lock-free reads**: readers never block, they retry on conflict
//! - **writer priority**: writers are never blocked by readers
//!
//! # how it works
//!
//! - version counter starts at 0 (even)
//! - writer increments version to odd before write, even after write
//! - reader checks version before and after read
//! - if version changed or is odd, reader retries
//!
//! # example
//!
//! ```
//! use land_sync::SeqLock;
//!
//! let lock = SeqLock::new([0u64; 4]);
//!
//! // reader (can be called from any thread)
//! let data = lock.read();
//!
//! // writer (must be single-threaded)
//! lock.write([1, 2, 3, 4]);
//! ```

use land_cpu::CachePadded;
use std::cell::UnsafeCell;
use std::sync::atomic::{fence, AtomicU64, Ordering};

/// seqLock version counter, cache-line padded to prevent false sharing.
#[repr(C)]
struct Version {
    value: CachePadded<AtomicU64>,
}

impl Version {
    const fn new() -> Self {
        Self {
            value: CachePadded::new(AtomicU64::new(0)),
        }
    }

    #[inline(always)]
    fn load(&self) -> u64 {
        self.value.load(Ordering::Acquire)
    }

    #[inline(always)]
    fn increment(&self) {
        let v = self.value.load(Ordering::Relaxed);
        self.value.store(v.wrapping_add(1), Ordering::Release);
    }
}

/// a sequence lock for single-writer, multi-reader scenarios.
///
/// `T` must be `Copy` because:
/// - readers copy the data out (to avoid holding references during retry)
/// - writers copy the data in
///
/// # safety
///
/// this type assumes single-writer semantics. The caller must ensure that
/// only one thread calls write methods at a time.
#[repr(C)]
pub struct SeqLock<T> {
    version: Version,
    data: UnsafeCell<T>,
}

// safety: SeqLock is safe to share between threads because:
// - reads use SeqLock protocol (version check before/after)
// - single-writer assumption is documented (caller responsibility)
unsafe impl<T: Send> Send for SeqLock<T> {}
unsafe impl<T: Send + Sync> Sync for SeqLock<T> {}

impl<T: Copy> SeqLock<T> {
    /// create a new SeqLock with the given initial value.
    #[inline]
    pub const fn new(value: T) -> Self {
        Self {
            version: Version::new(),
            data: UnsafeCell::new(value),
        }
    }

    /// try to read the value without blocking.
    ///
    /// returns `None` if a write is in progress or occurred during the read.
    /// The caller should retry in a loop.
    ///
    /// # example
    ///
    /// ```
    /// use land_sync::SeqLock;
    ///
    /// let lock = SeqLock::new(42u64);
    ///
    /// // non-blocking read attempt
    /// if let Some(value) = lock.try_read() {
    ///     assert_eq!(value, 42);
    /// }
    /// ```
    /// note on compiler optimization:
    /// in real concurrent scenarios, atomic fences prevent read elimination.
    /// in single-threaded benchmarks, use std::hint::black_box() to prevent
    /// the compiler from optimizing away reads.
    #[inline]
    pub fn try_read(&self) -> Option<T> {
        let v1 = self.version.load();
        if v1 & 1 == 1 {
            return None;
        }

        // safety: version check after ensures consistency
        let value = unsafe { *self.data.get() };

        fence(Ordering::Acquire);

        let v2 = self.version.load();
        if v1 != v2 {
            return None;
        }

        Some(value)
    }

    /// read the value, spinning until a consistent read is achieved.
    ///
    /// this will retry indefinitely until it can read consistent data.
    ///
    /// # example
    ///
    /// ```
    /// use land_sync::SeqLock;
    ///
    /// let lock = SeqLock::new(42u64);
    /// let value = lock.read();
    /// assert_eq!(value, 42);
    /// ```
    #[inline]
    pub fn read(&self) -> T {
        loop {
            if let Some(value) = self.try_read() {
                return value;
            }
            std::hint::spin_loop();
        }
    }

    /// write a new value.
    ///
    /// # safety (contract)
    ///
    /// this method must only be called from a single writer thread.
    /// concurrent calls to `write` will result in undefined behavior.
    ///
    /// # example
    ///
    /// ```
    /// use land_sync::SeqLock;
    ///
    /// let lock = SeqLock::new(0u64);
    /// lock.write(42);
    /// assert_eq!(lock.read(), 42);
    /// ```
    #[inline]
    pub fn write(&self, value: T) {
        // begin write: increment version to odd
        self.version.increment();

        // write the data
        // safety: single-writer assumption - no concurrent writes
        unsafe {
            *self.data.get() = value;
        }

        // end write: increment version to even
        self.version.increment();
    }

    /// mutate the value in place using a closure.
    ///
    /// # safety (contract)
    ///
    /// this method must only be called from a single writer thread.
    ///
    /// # example
    ///
    /// ```
    /// use land_sync::SeqLock;
    ///
    /// let lock = SeqLock::new([0u64; 4]);
    /// lock.update(|data| {
    ///     data[0] = 1;
    ///     data[1] = 2;
    /// });
    /// ```
    #[inline]
    pub fn update<F>(&self, f: F)
    where
        F: FnOnce(&mut T),
    {
        // begin write: increment version to odd
        self.version.increment();

        // mutate the data
        // safety: single-writer assumption - no concurrent writes
        unsafe {
            f(&mut *self.data.get());
        }

        // end write: increment version to even
        self.version.increment();
    }

    /// get the current version number.
    ///
    /// useful for debugging or advanced synchronization patterns.
    #[inline]
    pub fn version(&self) -> u64 {
        self.version.load()
    }

    /// zero-copy read: access data in-place via closure.
    ///
    /// the closure receives a reference to the data and can extract only
    /// what it needs, avoiding copying the entire `T`.
    ///
    /// returns `None` if a write is in progress or occurred during the read.
    ///
    /// # example
    ///
    /// ```
    /// use land_sync::SeqLock;
    ///
    /// #[derive(Copy, Clone)]
    /// struct LargeData {
    ///     field1: u64,
    ///     field2: [u8; 1024],
    /// }
    ///
    /// let lock = SeqLock::new(LargeData { field1: 42, field2: [0; 1024] });
    ///
    /// // zero-copy: only extract field1, don't copy the entire 1KB struct
    /// if let Some(value) = lock.try_read_with(|data| data.field1) {
    ///     assert_eq!(value, 42);
    /// }
    /// ```
    #[inline]
    pub fn try_read_with<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&T) -> R,
    {
        let v1 = self.version.load();
        if v1 & 1 == 1 {
            return None;
        }

        // access data in-place, closure extracts what it needs
        // safety: version check after ensures consistency
        let result = f(unsafe { &*self.data.get() });

        fence(Ordering::Acquire);

        let v2 = self.version.load();
        if v1 != v2 {
            return None;
        }

        Some(result)
    }

    /// zero-copy read with spin-wait.
    ///
    /// spins until a consistent read is achieved.
    ///
    /// # example
    ///
    /// ```
    /// use land_sync::SeqLock;
    ///
    /// let lock = SeqLock::new([0u64; 100]);
    ///
    /// // zero-copy: only read index 5
    /// let value = lock.read_with(|data| data[5]);
    /// ```
    #[inline]
    pub fn read_with<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&T) -> R + Copy,
    {
        loop {
            if let Some(result) = self.try_read_with(f) {
                return result;
            }
            std::hint::spin_loop();
        }
    }
}

impl<T: Copy + Default> Default for SeqLock<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: Copy + std::fmt::Debug> std::fmt::Debug for SeqLock<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.try_read() {
            Some(value) => f.debug_struct("SeqLock").field("value", &value).finish(),
            None => f
                .debug_struct("SeqLock")
                .field("value", &"<write in progress>")
                .finish(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_basic_read_write() {
        let lock = SeqLock::new(42u64);
        assert_eq!(lock.read(), 42);

        lock.write(100);
        assert_eq!(lock.read(), 100);
    }

    #[test]
    fn test_try_read() {
        let lock = SeqLock::new(42u64);
        assert_eq!(lock.try_read(), Some(42));
    }

    #[test]
    fn test_update() {
        let lock = SeqLock::new([0u64; 4]);
        lock.update(|data| {
            data[0] = 1;
            data[1] = 2;
            data[2] = 3;
            data[3] = 4;
        });

        let data = lock.read();
        assert_eq!(data, [1, 2, 3, 4]);
    }

    #[test]
    fn test_concurrent_reads() {
        let lock = Arc::new(SeqLock::new(42u64));

        let readers: Vec<_> = (0..4)
            .map(|_| {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    for _ in 0..1000 {
                        let _ = lock.read();
                    }
                })
            })
            .collect();

        for r in readers {
            r.join().unwrap();
        }
    }

    #[test]
    fn test_writer_reader_contention() {
        let lock = Arc::new(SeqLock::new(0u64));

        // writer thread
        let writer_lock = Arc::clone(&lock);
        let writer = thread::spawn(move || {
            for i in 0..1000u64 {
                writer_lock.write(i);
            }
        });

        // reader threads
        let readers: Vec<_> = (0..4)
            .map(|_| {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    let mut last_seen = 0u64;
                    for _ in 0..10000 {
                        let value = lock.read();
                        // values should be monotonically increasing or same
                        assert!(value >= last_seen || value == 0);
                        last_seen = value;
                    }
                })
            })
            .collect();

        writer.join().unwrap();
        for r in readers {
            r.join().unwrap();
        }
    }

    #[test]
    fn test_array_type() {
        #[derive(Copy, Clone, Debug, PartialEq)]
        struct Entry {
            id: u64,
            value: u64,
        }

        let lock = SeqLock::new([Entry { id: 0, value: 0 }; 4]);

        lock.update(|entries| {
            for (i, entry) in entries.iter_mut().enumerate() {
                entry.id = i as u64;
                entry.value = i as u64 * 10;
            }
        });

        let entries = lock.read();
        for (i, entry) in entries.iter().enumerate() {
            assert_eq!(entry.id, i as u64);
            assert_eq!(entry.value, i as u64 * 10);
        }
    }

    #[test]
    fn test_version_increments() {
        let lock = SeqLock::new(0u64);
        assert_eq!(lock.version(), 0);

        lock.write(1);
        assert_eq!(lock.version(), 2); // 0 -> 1 -> 2

        lock.write(2);
        assert_eq!(lock.version(), 4); // 2 -> 3 -> 4
    }

    #[test]
    fn test_default() {
        let lock: SeqLock<u64> = SeqLock::default();
        assert_eq!(lock.read(), 0);
    }

    #[test]
    fn test_debug() {
        let lock = SeqLock::new(42u64);
        let debug = format!("{:?}", lock);
        assert!(debug.contains("SeqLock"));
        assert!(debug.contains("42"));
    }

    #[test]
    fn test_zero_copy_read_with() {
        #[derive(Copy, Clone)]
        struct LargeData {
            field1: u64,
            field2: u64,
            _padding: [u8; 1024],
        }

        let lock = SeqLock::new(LargeData {
            field1: 42,
            field2: 100,
            _padding: [0; 1024],
        });

        // zero-copy: only extract field1
        let value = lock.read_with(|data| data.field1);
        assert_eq!(value, 42);

        // zero-copy: only extract field2
        let value = lock.read_with(|data| data.field2);
        assert_eq!(value, 100);
    }

    #[test]
    fn test_try_read_with() {
        let lock = SeqLock::new([1u64, 2, 3, 4, 5]);

        // extract single element
        assert_eq!(lock.try_read_with(|arr| arr[2]), Some(3));

        // extract sum
        assert_eq!(lock.try_read_with(|arr| arr.iter().sum::<u64>()), Some(15));
    }
}
