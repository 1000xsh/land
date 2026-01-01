// cache-line padding to reduce false sharing
//
// false sharing occurs when different threads write to distinct variables on same cache line
// cache coherence operates at cache-line granularity causing unnecessary invalidation traffic
//
// CachePadded<T> aligns value to 64-byte boundary and ensures size is multiple of 64
// so adjacent instances dont share cache lines

use core::fmt;
use core::ops::{Deref, DerefMut};

pub const CACHE_LINE_SIZE: usize = 64;

// pads and aligns value to 64-byte boundary
// reduces false sharing by ensuring adjacent CachePadded<T> values don't share cache lines
//
// guarantees:
// - alignment 64
// - value field at offset 0
// - size rounded up to multiple of 64
#[repr(C, align(64))]
pub struct CachePadded<T> {
    value: T,
}

impl<T> CachePadded<T> {
    #[inline]
    pub const fn new(value: T) -> Self {
        Self { value }
    }

    #[inline]
    pub fn into_inner(self) -> T {
        self.value
    }

    #[inline]
    pub fn get_mut(&mut self) -> &mut T {
        &mut self.value
    }
}

impl<T> Deref for CachePadded<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &self.value
    }
}

impl<T> DerefMut for CachePadded<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        &mut self.value
    }
}

impl<T: Default> Default for CachePadded<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: Clone> Clone for CachePadded<T> {
    fn clone(&self) -> Self {
        Self::new(self.value.clone())
    }
}

impl<T: Copy> Copy for CachePadded<T> {}

impl<T: fmt::Debug> fmt::Debug for CachePadded<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CachePadded")
            .field("value", &self.value)
            .finish()
    }
}

impl<T: PartialEq> PartialEq for CachePadded<T> {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl<T: Eq> Eq for CachePadded<T> {}

impl<T> From<T> for CachePadded<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}
