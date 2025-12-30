//! batch slot operations for zero-copy writes.

use super::shared::Shared;
use land_cpu::fence_release;
use std::sync::Arc;

/// RAII handle for batch slot operations.
///
/// provides access to multiple consecutive slots in the ring buffer.
/// the slots are published when [`publish()`](Self::publish) is called
/// or when the handle is dropped.
///
/// # performance
///
/// using batch operations is more efficient than individual operations:
/// - single space check for all slots
/// - single memory fence for the entire batch (vs. per-slot fences)
///
/// # example
///
/// ```
/// use land_channel::spsc;
///
/// let (mut tx, mut rx) = spsc::channel_with_factory(64, || 0u64, land_channel::SpinLoopHintWait);
///
/// // claim and write batch
/// let mut batch = tx.claim_batch(3).unwrap();
/// *batch.get_mut(0) = 100;
/// *batch.get_mut(1) = 200;
/// *batch.get_mut(2) = 300;
/// batch.publish();
///
/// // all values are now visible to consumer
/// assert_eq!(rx.recv().unwrap(), 100);
/// ```
pub struct BatchSlots<'a, T> {
    pub(super) shared: &'a Arc<Shared<T>>,
    pub(super) start: i64,
    pub(super) count: usize,
    pub(super) published: bool,
}

impl<'a, T> BatchSlots<'a, T> {
    /// get a mutable reference to the slot at the given index.
    ///
    /// # panics
    ///
    /// panics if `index >= count`.
    ///
    /// # example
    ///
    /// ```
    /// use land_channel::spsc;
    ///
    /// let (mut tx, _rx) = spsc::channel_with_factory(64, || 0u64, land_channel::SpinLoopHintWait);
    ///
    /// let mut batch = tx.claim_batch(2).unwrap();
    /// *batch.get_mut(0) = 10;
    /// *batch.get_mut(1) = 20;
    /// batch.publish();
    /// ```
    #[inline]
    pub fn get_mut(&mut self, index: usize) -> &mut T {
        assert!(
            index < self.count,
            "index {} out of bounds for batch of {}",
            index,
            self.count
        );
        // safety: we have exclusive access to these slots
        unsafe { self.shared.buffer.get_mut(self.start + index as i64) }
    }

    /// get an immutable reference to the slot at the given index.
    #[inline]
    pub fn get(&self, index: usize) -> &T {
        assert!(
            index < self.count,
            "index {} out of bounds for batch of {}",
            index,
            self.count
        );
        // safety: we have exclusive access to these slots
        unsafe { self.shared.buffer.get(self.start + index as i64) }
    }

    /// returns the number of slots in this batch.
    #[inline]
    pub fn len(&self) -> usize {
        self.count
    }

    /// returns true if this batch contains no slots.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// publish all slots, making them visible to the consumer.
    ///
    /// this uses a single memory fence for the entire batch, which is
    /// more efficient than publishing slots individually.
    ///
    /// after calling this, the batch handle cannot be used.
    #[inline]
    pub fn publish(mut self) {
        self.do_publish();
    }

    /// internal publish logic
    #[inline]
    pub(super) fn do_publish(&mut self) {
        if !self.published {
            // single release fence for entire batch
            fence_release();
            // publish the last sequence in the batch
            self.shared
                .producer_cursor
                .set_relaxed(self.start + self.count as i64 - 1);
            self.published = true;
        }
    }

    /// returns an iterator over mutable references to all slots.
    #[inline]
    pub fn iter_mut(&mut self) -> BatchSlotIterMut<'_, T> {
        BatchSlotIterMut {
            shared: self.shared,
            current: self.start,
            end: self.start + self.count as i64,
        }
    }
}

impl<'a, T> Drop for BatchSlots<'a, T> {
    fn drop(&mut self) {
        // auto-publish on drop if not already published
        self.do_publish();
    }
}

/// iterator over mutable references to batch slots.
pub struct BatchSlotIterMut<'a, T> {
    shared: &'a Arc<Shared<T>>,
    current: i64,
    end: i64,
}

impl<'a, T> Iterator for BatchSlotIterMut<'a, T> {
    type Item = &'a mut T;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        if self.current < self.end {
            let seq = self.current;
            self.current += 1;
            // safety: we have exclusive access to these slots from the batch
            Some(unsafe { &mut *self.shared.buffer.get_ptr_mut(seq) })
        } else {
            None
        }
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = (self.end - self.current) as usize;
        (remaining, Some(remaining))
    }
}

impl<'a, T> ExactSizeIterator for BatchSlotIterMut<'a, T> {}
