//! pre-allocated ring buffer storage.
//!
//! the ring buffer is the core data structure for storing events in the channel.
//! it uses a fixed-size circular array with power-of-2 capacity for efficient
//! index calculations using bitwise AND instead of modulo.
//!
//! # design
//!
//! - pre-allocated slots to avoid allocation in hot path
//! - power-of-2 capacity for O(1) index calculation
//! - uses `UnsafeCell<MaybeUninit<T>>` for interior mutability and uninitialized storage
//! - zero-copy access via raw pointer operations
//!
//! # safety
//!
//! the ring buffer provides raw access methods that are unsafe. users must ensure:
//! - only one writer accesses a slot at a time
//! - readers only access slots that have been initialized
//! - proper synchronization between producers and consumers
//!
//! # example
//!
//! ```
//! use land_channel::ringbuffer::RingBuffer;
//!
//! let buffer: RingBuffer<u64> = RingBuffer::new(1024);
//! assert_eq!(buffer.capacity(), 1024);
//!
//! // write at sequence 0
//! unsafe {
//!     buffer.write(0, 42);
//!     assert_eq!(*buffer.get(0), 42);
//! }
//! ```

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;

/// check if a number is a power of 2.
#[inline]
const fn is_power_of_two(n: usize) -> bool {
    n != 0 && (n & (n - 1)) == 0
}

/// pre-allocated ring buffer with runtime-configurable capacity.
///
/// the buffer stores events in a circular array. capacity must be a power of 2,
/// which allows using bitwise AND instead of modulo for index calculations.
///
/// # type parameters
///
/// * `T` - the event type to store
///
/// # example
///
/// ```
/// use land_channel::ringbuffer::RingBuffer;
///
/// // create a buffer for 1024 events
/// let buffer: RingBuffer<String> = RingBuffer::new(1024);
///
/// // with factory for pre-initialization
/// let buffer = RingBuffer::with_factory(1024, || String::with_capacity(256));
/// ```
pub struct RingBuffer<T> {
    /// pre-allocated event slots.
    /// UnsafeCell allows interior mutability for zero-copy writes.
    /// MaybeUninit allows uninitialized storage.
    slots: Box<[UnsafeCell<MaybeUninit<T>>]>,

    /// bitmask for efficient modulo operation: `sequence & mask` == `sequence % capacity`
    mask: usize,

    /// number of slots in the buffer (always power of 2)
    capacity: usize,
}

impl<T> RingBuffer<T> {
    /// create a new ring buffer with the given capacity.
    ///
    /// # arguments
    ///
    /// * `capacity` - number of slots (must be power of 2)
    ///
    /// # panics
    ///
    /// panics if `capacity` is not a power of 2 or is zero.
    ///
    /// # example
    ///
    /// ```
    /// use land_channel::ringbuffer::RingBuffer;
    ///
    /// let buffer: RingBuffer<u64> = RingBuffer::new(1024);
    /// assert_eq!(buffer.capacity(), 1024);
    /// ```
    ///
    /// ```should_panic
    /// use land_channel::ringbuffer::RingBuffer;
    ///
    /// // this will panic - 100 is not a power of 2
    /// let buffer: RingBuffer<u64> = RingBuffer::new(100);
    /// ```
    pub fn new(capacity: usize) -> Self {
        assert!(
            is_power_of_two(capacity),
            "Ring buffer capacity must be a power of 2, got {}",
            capacity
        );

        let mut slots = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            slots.push(UnsafeCell::new(MaybeUninit::uninit()));
        }

        Self {
            slots: slots.into_boxed_slice(),
            mask: capacity - 1,
            capacity,
        }
    }

    /// create a new ring buffer with pre-initialized slots.
    ///
    /// the factory function is called once for each slot to initialize it.
    /// this is useful for pre-allocating resources (e.g. `Vec` with capacity).
    ///
    /// # arguments
    ///
    /// * `capacity` - number of slots (must be power of 2)
    /// * `factory` - function to create initial values
    ///
    /// # panics
    ///
    /// panics if `capacity` is not a power of 2 or is zero.
    ///
    /// # example
    ///
    /// ```
    /// use land_channel::ringbuffer::RingBuffer;
    ///
    /// // Pre-allocate strings with capacity
    /// let buffer = RingBuffer::with_factory(1024, || String::with_capacity(256));
    /// ```
    pub fn with_factory<F>(capacity: usize, factory: F) -> Self
    where
        F: Fn() -> T,
    {
        assert!(
            is_power_of_two(capacity),
            "Ring buffer capacity must be a power of 2, got {}",
            capacity
        );

        let mut slots = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            slots.push(UnsafeCell::new(MaybeUninit::new(factory())));
        }

        Self {
            slots: slots.into_boxed_slice(),
            mask: capacity - 1,
            capacity,
        }
    }

    /// get the buffer capacity (number of slots).
    #[inline]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// get the index mask for modulo operations.
    #[inline]
    pub fn mask(&self) -> usize {
        self.mask
    }

    /// convert a sequence number to a slot index.
    ///
    /// uses bitwise AND which is faster than modulo for power-of-2 sizes.
    ///
    /// # example
    ///
    /// ```
    /// use land_channel::ringbuffer::RingBuffer;
    ///
    /// let buffer: RingBuffer<u64> = RingBuffer::new(8);
    /// assert_eq!(buffer.index(0), 0);
    /// assert_eq!(buffer.index(7), 7);
    /// assert_eq!(buffer.index(8), 0);  // wraps around
    /// assert_eq!(buffer.index(9), 1);
    /// ```
    #[inline(always)]
    pub fn index(&self, sequence: i64) -> usize {
        (sequence as usize) & self.mask
    }

    /// get a raw mutable pointer to the slot at the given sequence.
    ///
    /// # safety
    ///
    /// the caller must ensure:
    /// - exclusive access to this slot (no concurrent reads or writes)
    /// - the slot index is valid
    ///
    /// # example
    ///
    /// ```
    /// use land_channel::ringbuffer::RingBuffer;
    ///
    /// let buffer: RingBuffer<u64> = RingBuffer::new(8);
    /// unsafe {
    ///     let ptr = buffer.get_ptr_mut(0);
    ///     ptr.write(42);
    /// }
    /// ```
    #[inline(always)]
    pub unsafe fn get_ptr_mut(&self, sequence: i64) -> *mut T {
        let idx = self.index(sequence);
        // safety: idx is always < capacity due to mask
        unsafe { (*self.slots.get_unchecked(idx).get()).as_mut_ptr() }
    }

    /// get a raw const pointer to the slot at the given sequence.
    ///
    /// # safety
    ///
    /// the caller must ensure:
    /// - the slot has been initialized (written to)
    /// - no concurrent writes to this slot
    ///
    /// # example
    ///
    /// ```
    /// use land_channel::ringbuffer::RingBuffer;
    ///
    /// let buffer: RingBuffer<u64> = RingBuffer::new(8);
    /// unsafe {
    ///     buffer.write(0, 42);
    ///     let ptr = buffer.get_ptr(0);
    ///     assert_eq!(*ptr, 42);
    /// }
    /// ```
    #[inline(always)]
    pub unsafe fn get_ptr(&self, sequence: i64) -> *const T {
        let idx = self.index(sequence);
        // safety: idx is always < capacity due to mask
        unsafe { (*self.slots.get_unchecked(idx).get()).as_ptr() }
    }

    /// write an event to the slot at the given sequence.
    ///
    /// this overwrites any existing value without dropping it.
    /// for types that implement `Drop`, use [`write_and_drop`](Self::write_and_drop).
    ///
    /// # safety
    ///
    /// the caller must ensure:
    /// - exclusive access to this slot
    /// - no concurrent reads while writing
    ///
    /// # example
    ///
    /// ```
    /// use land_channel::ringbuffer::RingBuffer;
    ///
    /// let buffer: RingBuffer<u64> = RingBuffer::new(8);
    /// unsafe {
    ///     buffer.write(0, 100);
    ///     buffer.write(1, 200);
    /// }
    /// ```
    #[inline(always)]
    pub unsafe fn write(&self, sequence: i64, event: T) {
        let ptr = unsafe { self.get_ptr_mut(sequence) };
        unsafe { ptr.write(event) };
    }

    /// write an event, dropping any existing value first.
    ///
    /// use this for types that implement `Drop` when reusing slots.
    ///
    /// # safety
    ///
    /// the caller must ensure:
    /// - exclusive access to this slot
    /// - the slot was previously initialized (has a valid value to drop)
    #[inline(always)]
    pub unsafe fn write_and_drop(&self, sequence: i64, event: T) {
        let ptr = unsafe { self.get_ptr_mut(sequence) };
        // drop the old value
        unsafe { core::ptr::drop_in_place(ptr) };
        // write the new value
        unsafe { ptr.write(event) };
    }

    /// read a reference to the event at the given sequence.
    ///
    /// # safety
    ///
    /// the caller must ensure:
    /// - the slot has been initialized
    /// - no concurrent writes to this slot
    /// - the returned reference does not outlive concurrent access constraints
    ///
    /// # example
    ///
    /// ```
    /// use land_channel::ringbuffer::RingBuffer;
    ///
    /// let buffer: RingBuffer<u64> = RingBuffer::new(8);
    /// unsafe {
    ///     buffer.write(0, 42);
    ///     let value = buffer.get(0);
    ///     assert_eq!(*value, 42);
    /// }
    /// ```
    #[inline(always)]
    pub unsafe fn get(&self, sequence: i64) -> &T {
        unsafe { &*self.get_ptr(sequence) }
    }

    /// get a mutable reference to the event at the given sequence.
    ///
    /// this enables zero-copy in-place mutation.
    ///
    /// # safety
    ///
    /// the caller must ensure:
    /// - exclusive access to this slot
    /// - the slot has been initialized
    /// - no concurrent reads or writes
    ///
    /// # example
    ///
    /// ```
    /// use land_channel::ringbuffer::RingBuffer;
    ///
    /// let buffer = RingBuffer::with_factory(8, || String::new());
    /// unsafe {
    ///     // zero-copy: modify the event in place
    ///     let event = buffer.get_mut(0);
    ///     event.push_str("hello");
    ///     assert_eq!(buffer.get(0), "hello");
    /// }
    /// ```
    #[inline(always)]
    pub unsafe fn get_mut(&self, sequence: i64) -> &mut T {
        unsafe { &mut *self.get_ptr_mut(sequence) }
    }

    /// read and move the event out of the slot.
    ///
    /// this performs a `ptr::read` which moves ownership to the caller.
    /// the slot is left in an uninitialized state.
    ///
    /// # safety
    ///
    /// the caller must ensure:
    /// - she slot has been initialized
    /// - no concurrent access to this slot
    /// - the slot wont be read again without being rewritten
    #[inline(always)]
    pub unsafe fn read(&self, sequence: i64) -> T {
        unsafe { core::ptr::read(self.get_ptr(sequence)) }
    }

    /// calculate free slots available for writing.
    ///
    /// # arguments
    ///
    /// * `producer_seq` - current producer sequence (last written)
    /// * `consumer_seq` - current consumer sequence (last consumed)
    ///
    /// # returns
    ///
    /// number of slots available for the producer to write.
    #[inline]
    pub fn free_slots(&self, producer_seq: i64, consumer_seq: i64) -> usize {
        let pending = producer_seq - consumer_seq;
        if pending < 0 {
            self.capacity
        } else {
            self.capacity - pending as usize
        }
    }
}

// safety: RingBuffer is Send if T is Send - can be transferred between threads
unsafe impl<T: Send> Send for RingBuffer<T> {}

// safety: RingBuffer is Sync if T is Send - can be shared between threads
// (actual synchronization is done by the channel using the buffer)
unsafe impl<T: Send> Sync for RingBuffer<T> {}

impl<T> Drop for RingBuffer<T> {
    fn drop(&mut self) {
        // we dont know which slots were initialized, so we can not safely drop them.
        // for types that do not need dropping (primitives), this is fine.
        // for types with drop, the channel must ensure all written values are
        // either consumed or explicitly dropped before the buffer is dropped.
        //
        // if T needs drop, the channel implementation should track which
        // sequences have been written but not consumed, and drop them.
    }
}

impl<T: core::fmt::Debug> core::fmt::Debug for RingBuffer<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RingBuffer")
            .field("capacity", &self.capacity)
            .field("mask", &self.mask)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new() {
        let buffer: RingBuffer<u64> = RingBuffer::new(1024);
        assert_eq!(buffer.capacity(), 1024);
        assert_eq!(buffer.mask(), 1023);
    }

    #[test]
    #[should_panic(expected = "power of 2")]
    fn test_new_non_power_of_2() {
        let _: RingBuffer<u64> = RingBuffer::new(100);
    }

    #[test]
    #[should_panic(expected = "power of 2")]
    fn test_new_zero() {
        let _: RingBuffer<u64> = RingBuffer::new(0);
    }

    #[test]
    fn test_with_factory() {
        let buffer = RingBuffer::with_factory(8, || String::with_capacity(100));
        assert_eq!(buffer.capacity(), 8);

        // verify factory was called
        unsafe {
            let s = buffer.get_mut(0);
            assert!(s.capacity() >= 100);
        }
    }

    #[test]
    fn test_index() {
        let buffer: RingBuffer<u64> = RingBuffer::new(8);

        assert_eq!(buffer.index(0), 0);
        assert_eq!(buffer.index(1), 1);
        assert_eq!(buffer.index(7), 7);
        assert_eq!(buffer.index(8), 0); // wrap
        assert_eq!(buffer.index(9), 1);
        assert_eq!(buffer.index(15), 7);
        assert_eq!(buffer.index(16), 0);
    }

    #[test]
    fn test_write_and_read() {
        let buffer: RingBuffer<u64> = RingBuffer::new(8);

        unsafe {
            buffer.write(0, 100);
            buffer.write(1, 200);
            buffer.write(7, 700);

            assert_eq!(*buffer.get(0), 100);
            assert_eq!(*buffer.get(1), 200);
            assert_eq!(*buffer.get(7), 700);
        }
    }

    #[test]
    fn test_wrap_around() {
        let buffer: RingBuffer<u64> = RingBuffer::new(4);

        unsafe {
            // fill buffer
            for i in 0..4 {
                buffer.write(i, i as u64 * 10);
            }

            // verify
            for i in 0..4 {
                assert_eq!(*buffer.get(i), i as u64 * 10);
            }

            // wrap around - write at sequence 4 overwrites sequence 0
            buffer.write(4, 40);
            assert_eq!(*buffer.get(4), 40); // same slot as 0
            assert_eq!(*buffer.get(0), 40); // same content
        }
    }

    #[test]
    fn test_get_mut_zero_copy() {
        let buffer = RingBuffer::with_factory(8, String::new);

        unsafe {
            // modify in place
            let s = buffer.get_mut(0);
            s.push_str("hello");
            s.push_str(" world");

            assert_eq!(buffer.get(0), "hello world");
        }
    }

    #[test]
    fn test_read_moves_ownership() {
        let buffer: RingBuffer<String> = RingBuffer::new(4);

        unsafe {
            buffer.write(0, String::from("test"));
            let owned: String = buffer.read(0);
            assert_eq!(owned, "test");
            // slot is now uninitialized
        }
    }

    #[test]
    fn test_free_slots() {
        let buffer: RingBuffer<u64> = RingBuffer::new(8);

        // empty buffer
        assert_eq!(buffer.free_slots(-1, -1), 8);

        // producer ahead of consumer
        assert_eq!(buffer.free_slots(3, -1), 4);
        assert_eq!(buffer.free_slots(7, -1), 0); // Full

        // consumer catching up
        assert_eq!(buffer.free_slots(7, 3), 4);
        assert_eq!(buffer.free_slots(7, 6), 7);
    }

    #[test]
    fn test_large_sequences() {
        let buffer: RingBuffer<u64> = RingBuffer::new(8);

        // test with large sequence numbers
        let large_seq: i64 = 1_000_000_000;

        unsafe {
            buffer.write(large_seq, 42);
            assert_eq!(*buffer.get(large_seq), 42);
        }

        // verify index calculation
        let expected_idx = (large_seq as usize) & 7;
        assert_eq!(buffer.index(large_seq), expected_idx);
    }

    #[test]
    fn test_debug() {
        let buffer: RingBuffer<u64> = RingBuffer::new(8);
        let debug = format!("{:?}", buffer);
        assert!(debug.contains("RingBuffer"));
        assert!(debug.contains("capacity: 8"));
    }
}
