// land-cpu

mod affinity;
mod cache_padded;
mod cursor;
mod error;
pub mod fence;
mod prefetch;
mod topology;
pub mod wait_strategy;

pub use {
    affinity::{cpu_count, set_cpu_affinity},
    cache_padded::{CachePadded, CACHE_LINE_SIZE},
    cursor::{Cursor, INITIAL_CURSOR_VALUE},
    fence::{cpu_pause, fence_acq_rel, fence_acquire, fence_release},
    prefetch::prefetch_read,
    topology::physical_core_count,
    wait_strategy::{BackoffWait, BusySpinWait, SpinLoopHintWait, WaitStrategy, YieldingWait},
};
