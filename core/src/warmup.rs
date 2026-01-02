//! warmup protocol for cache priming and connection prewarming.

use land_leader::LeaderBuffer;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct WarmupConfig {
    /// number of leaders to prewarm connections to.
    pub prewarm_leaders: usize,
    /// cache prime iterations.
    pub cache_prime_iterations: usize,
    /// stabilization delay after warmup.
    pub stabilization_delay: Duration,
}

impl Default for WarmupConfig {
    fn default() -> Self {
        Self {
            prewarm_leaders: 10,
            cache_prime_iterations: 1000,
            stabilization_delay: Duration::from_millis(100),
        }
    }
}

/// warmup the system.
///
/// prime caches with buffer reads
/// wait for stabilization
pub fn warmup(buffer: &Arc<LeaderBuffer>, config: &WarmupConfig) {
    eprintln!("warming up...");

    // prime caches with buffer reads
    prime_caches(buffer, config.cache_prime_iterations);

    // stabilization delay
    std::thread::sleep(config.stabilization_delay);

    eprintln!("warmup complete");
}

/// prime cpu caches with buffer reads.
#[inline(never)]
fn prime_caches(buffer: &Arc<LeaderBuffer>, iterations: usize) {
    for _ in 0..iterations {
        // read all entries to prime L1/L2 cache
        let _ = std::hint::black_box(buffer.read_all());
        // small spin to let cache settle
        for _ in 0..10 {
            std::hint::spin_loop();
        }
    }
}
