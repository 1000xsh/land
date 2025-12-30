//! batch slot operations for MPSC zero-copy writes.

use super::shared::Shared;
use land_cpu::fence_release;
use std::sync::Arc;

/// RAII handle for MPSC batch slot operations.
///
/// provides zero-copy access to multiple consecutive slots.
/// publishes on drop or explicit `publish()` call.
pub struct MpscBatchSlots<'a, T> {
    pub(super) shared: &'a Arc<Shared<T>>,
    pub(super) start: i64,
    pub(super) count: usize,
    pub(super) published: bool,
}

impl<'a, T> MpscBatchSlots<'a, T> {
    /// get mutable reference to slot at index.
    #[inline]
    pub fn get_mut(&mut self, index: usize) -> &mut T {
        assert!(index < self.count);
        unsafe { self.shared.buffer.get_mut(self.start + index as i64) }
    }

    /// get immutable reference to slot at index.
    #[inline]
    pub fn get(&self, index: usize) -> &T {
        assert!(index < self.count);
        unsafe { self.shared.buffer.get(self.start + index as i64) }
    }

    /// number of slots in batch.
    #[inline]
    pub fn len(&self) -> usize {
        self.count
    }

    /// returns true if batch is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// publish all slots, making them visible to consumer.
    #[inline]
    pub fn publish(mut self) {
        self.do_publish();
    }

    fn do_publish(&mut self) {
        if self.published {
            return;
        }

        // single release fence for entire batch
        fence_release();

        // mark all sequences using batched XOR - consumer scans bitmap directly
        self.shared.mark_published_batch(self.start, self.count);

        self.published = true;
    }
}

impl<'a, T> Drop for MpscBatchSlots<'a, T> {
    fn drop(&mut self) {
        self.do_publish();
    }
}
