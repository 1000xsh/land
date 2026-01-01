// wait strategies for inter-thread coordination
// determines how consumer waits for producer to make data available
// each strategy trades off between latency and cpu usage
//
// | strategy             | latency  | cpu      | use case                       |
// |----------------------|----------|----------|--------------------------------|
// | BusySpinWait         | lowest   | highest  | isolated cores, low-latency    |
// | SpinLoopHintWait     | low      | high     | general low-latency            |
// | YieldingWait         | moderate | low      | shared cores                   |
// | BackoffWait          | variable | adaptive | variable load scenarios        |

use core::hint;
use std::sync::atomic::{AtomicI64, Ordering};

pub const INITIAL_SEQUENCE: i64 = -1;

// strategy for waiting on sequence cursor to reach target value
// implementations determine trade-off between latency and cpu usage
pub trait WaitStrategy: Send + Sync {
    // wait until cursor reaches or exceeds target sequence
    // returns current sequence value when >= target
    fn wait_for(&self, target: i64, cursor: &AtomicI64) -> i64;

    // optional signal that producer made progress
    // used by blocking strategies to wake waiters (default no-op)
    #[inline]
    fn signal(&self) {}
}

// busy-spin wait - lowest latency, highest cpu usage
// continuously polls cursor without yielding or hinting
// achieves absolute lowest latency but consumes 100% cpu
//
// use cases:
// - isolated cpu cores dedicated to application
// - extreme low-latency requirements (sub-microsecond)
// - when cpu usage not a concern
#[derive(Debug, Clone, Copy, Default)]
pub struct BusySpinWait;

impl WaitStrategy for BusySpinWait {
    #[inline]
    fn wait_for(&self, target: i64, cursor: &AtomicI64) -> i64 {
        loop {
            let current = cursor.load(Ordering::Acquire);
            if current >= target {
                return current;
            }
            // pure busy spin - no hint, no yield
        }
    }
}

// spin-loop hint wait - low latency with cpu power hints
// uses core::hint::spin_loop() to hint cpu we're spin-waiting
// reduces power consumption and improves performance on hyperthreaded systems
// by allowing sibling thread to use more resources
//
// use cases:
// - general low-latency scenarios
// - running on non-isolated cores
// - good default choice for most applications
#[derive(Debug, Clone, Copy, Default)]
pub struct SpinLoopHintWait;

impl WaitStrategy for SpinLoopHintWait {
    #[inline]
    fn wait_for(&self, target: i64, cursor: &AtomicI64) -> i64 {
        loop {
            let current = cursor.load(Ordering::Acquire);
            if current >= target {
                return current;
            }
            hint::spin_loop();
        }
    }
}

// yielding wait - moderate latency, lower cpu usage
// spins for configurable iterations before yielding thread
// reduces cpu usage vs pure spinning while maintaining reasonable latency
//
// use cases:
// - shared cores with other applications
// - when cpu efficiency matters more than absolute latency
// - variable or bursty workloads
#[derive(Debug, Clone, Copy)]
pub struct YieldingWait {
    // number of spin iterations before yielding
    spin_tries: u32,
}

impl YieldingWait {
    // create new yielding wait strategy
    // spin_tries: number of spin iterations before yielding to os
    #[inline]
    pub const fn new(spin_tries: u32) -> Self {
        Self { spin_tries }
    }
}

impl Default for YieldingWait {
    // default to 100 spin iterations before yielding
    fn default() -> Self {
        Self::new(100)
    }
}

impl WaitStrategy for YieldingWait {
    fn wait_for(&self, target: i64, cursor: &AtomicI64) -> i64 {
        let mut spins = 0u32;
        loop {
            let current = cursor.load(Ordering::Acquire);
            if current >= target {
                return current;
            }

            if spins < self.spin_tries {
                hint::spin_loop();
                spins += 1;
            } else {
                std::thread::yield_now();
                spins = 0;
            }
        }
    }
}

// exponential backoff wait - adaptive cpu usage
// starts with small number of spins and exponentially increases up to maximum, then yields
// adapts well to varying load patterns
//
// use cases:
// - unknown or highly variable workload patterns
// - when you need automatic adaptation
// - production systems with varying load
#[derive(Debug, Clone, Copy)]
pub struct BackoffWait {
    // initial number of spin iterations
    initial_spins: u32,
    // maximum number of spin iterations before yielding
    max_spins: u32,
}

impl BackoffWait {
    // create new backoff wait strategy
    // initial_spins: starting number of spin iterations
    // max_spins: maximum spins before yielding (exponentially approached)
    #[inline]
    pub const fn new(initial_spins: u32, max_spins: u32) -> Self {
        Self {
            initial_spins,
            max_spins,
        }
    }
}

impl Default for BackoffWait {
    // default to 10 initial spins, 1000 max spins
    fn default() -> Self {
        Self::new(10, 1000)
    }
}

impl WaitStrategy for BackoffWait {
    fn wait_for(&self, target: i64, cursor: &AtomicI64) -> i64 {
        let mut spins = self.initial_spins;

        loop {
            // spin for current iteration count
            for _ in 0..spins {
                let current = cursor.load(Ordering::Acquire);
                if current >= target {
                    return current;
                }
                hint::spin_loop();
            }

            // exponential backoff
            spins = (spins.saturating_mul(2)).min(self.max_spins);

            // yield after max spins reached
            if spins >= self.max_spins {
                std::thread::yield_now();
            }
        }
    }
}
