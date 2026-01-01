// cpu prefetch intrinsics for x86_64
// use for optimizing memory access patterns in latency-sensitive code

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
use std::arch::x86_64::{_mm_prefetch, _MM_HINT_T0};

// prefetch for read access into L1 cache
// use for data that will be read soon and accessed multiple times
//
// # safety
// pointer should point to readable memory
// on x86_64, prefetching invalid address is no-op and does not fault
#[inline(always)]
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub unsafe fn prefetch_read<T>(ptr: *const T) {
    unsafe { _mm_prefetch(ptr as *const i8, _MM_HINT_T0) }
}

#[inline(always)]
#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
pub unsafe fn prefetch_read<T>(_ptr: *const T) {
    // no-op on unsupported platforms
}
