//! per-IP connection timing registry for adaptive connect
//!
//! this module provides lock-free timing storage for measuring and tracking
//! handshake latencies per validator IP address. used to adaptively determine
//! how many slots early to initiate connections.

use land_timing::Histogram;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

/// number of shards for lock-free parallel access (~600 IPs / 16 shards = ~38 IPs per shard)
const SHARDS: usize = 16;

/// timing category for handshake measurements
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimingCategory {
    /// full handshake (no cached session, first connection)
    Handshake,
    /// 0-RTT (successful session resumption with early data)
    ZeroRtt,
    /// 1-RTT (0-RTT rejected or unavailable, fallback to full handshake)
    OneRtt,
}

/// per-IP connection timing metrics (lock-free, zero allocation)
pub struct ConnectionTimings {
    /// full handshake timing histogram (no 0-RTT)
    pub handshake_histogram: Arc<Histogram>,
    /// 0-RTT timing histogram (successful session resumption)
    pub zero_rtt_histogram: Arc<Histogram>,
    /// 1-RTT timing histogram (0-RTT rejected, fallback)
    pub one_rtt_histogram: Arc<Histogram>,
    /// timestamp of first connection attempt (nanoseconds)
    pub first_seen_ns: AtomicU64,
    /// last successful connection timestamp (nanoseconds)
    pub last_success_ns: AtomicU64,
    /// connection attempt counter
    pub attempts: AtomicU64,
}

impl ConnectionTimings {
    /// create new connection timings entry
    fn new() -> Self {
        let now_ns = crate::monotonic_nanos();
        Self {
            handshake_histogram: Arc::new(Histogram::new()),
            zero_rtt_histogram: Arc::new(Histogram::new()),
            one_rtt_histogram: Arc::new(Histogram::new()),
            first_seen_ns: AtomicU64::new(now_ns),
            last_success_ns: AtomicU64::new(now_ns),
            attempts: AtomicU64::new(0),
        }
    }

    /// record timing for specific category
    #[inline]
    pub fn record(&self, time_ns: u64, category: TimingCategory) {
        // record to appropriate histogram
        match category {
            TimingCategory::Handshake => self.handshake_histogram.record(time_ns),
            TimingCategory::ZeroRtt => self.zero_rtt_histogram.record(time_ns),
            TimingCategory::OneRtt => self.one_rtt_histogram.record(time_ns),
        }

        // Update metadata
        self.attempts.fetch_add(1, Ordering::Relaxed);
        self.last_success_ns
            .store(crate::monotonic_nanos(), Ordering::Release);
    }
}

/// sharded timing registry for lock-free concurrent access
///
/// design:
/// - 16 shards to minimize lock contention
/// - each shard: RwLock<HashMap<SocketAddr, Arc<ConnectionTimings>>>
/// - ~600 validator IPs → ~38 IPs per shard
/// - read-heavy workload optimized with RwLock
pub struct TimingRegistry {
    shards: [RwLock<HashMap<SocketAddr, Arc<ConnectionTimings>>>; SHARDS],
}

impl TimingRegistry {
    /// create new timing registry
    pub fn new() -> Self {
        Self {
            shards: std::array::from_fn(|_| RwLock::new(HashMap::new())),
        }
    }

    /// get shard index for address using FNV-1a hash
    #[inline(always)]
    fn shard_index(&self, addr: &SocketAddr) -> usize {
        // FNV-1a hash for good distribution
        let mut hash = 2166136261u64;
        match addr {
            SocketAddr::V4(v4) => {
                let ip = u32::from_be_bytes(v4.ip().octets()) as u64;
                let port = v4.port() as u64;
                hash = (hash ^ ip).wrapping_mul(1099511628211);
                hash = (hash ^ port).wrapping_mul(1099511628211);
            }
            SocketAddr::V6(v6) => {
                for seg in v6.ip().segments() {
                    hash = (hash ^ seg as u64).wrapping_mul(1099511628211);
                }
                hash = (hash ^ v6.port() as u64).wrapping_mul(1099511628211);
            }
        }
        (hash as usize) % SHARDS
    }

    /// get or create timing entry for address
    ///
    /// fast path: read lock to check existence
    /// slow path: write lock to create new entry
    pub fn get_or_create(&self, addr: SocketAddr) -> Arc<ConnectionTimings> {
        let shard_idx = self.shard_index(&addr);
        let shard = &self.shards[shard_idx];

        // fast path: try read lock first
        {
            let read_guard = shard.read().unwrap();
            if let Some(timing) = read_guard.get(&addr) {
                return Arc::clone(timing);
            }
        }

        // slow path: create new entry with write lock
        let mut write_guard = shard.write().unwrap();
        write_guard
            .entry(addr)
            .or_insert_with(|| Arc::new(ConnectionTimings::new()))
            .clone()
    }

    /// get timing entry if exists (read-only, no allocation)
    #[inline]
    pub fn get(&self, addr: SocketAddr) -> Option<Arc<ConnectionTimings>> {
        let shard_idx = self.shard_index(&addr);
        let shard = &self.shards[shard_idx];
        let read_guard = shard.read().unwrap();
        read_guard.get(&addr).map(Arc::clone)
    }

    /// calculate global median handshake time across all known IPs
    ///
    /// used for bootstrapping timing for unknown validators.
    /// returns median p50 handshake time in milliseconds.
    pub fn global_median_handshake_ms(&self) -> Option<u64> {
        let mut all_p50s = Vec::new();

        // collect p50 from all validators
        for shard in &self.shards {
            let read_guard = shard.read().unwrap();
            for timing in read_guard.values() {
                let percentiles = timing.handshake_histogram.percentiles(&[50.0]);
                if percentiles[0] > 0 {
                    all_p50s.push(percentiles[0]);
                }
            }
        }

        if all_p50s.is_empty() {
            return None;
        }

        // calculate median of all p50s
        all_p50s.sort_unstable();
        let median_ns = all_p50s[all_p50s.len() / 2];
        Some(median_ns / 1_000_000) // convert to milliseconds
    }

    /// get number of tracked IPs
    pub fn ip_count(&self) -> usize {
        self.shards
            .iter()
            .map(|shard| shard.read().unwrap().len())
            .sum()
    }
}

impl Default for TimingRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timing_category() {
        let timings = ConnectionTimings::new();

        // record different categories
        timings.record(1_000_000, TimingCategory::Handshake); // 1ms
        timings.record(500_000, TimingCategory::ZeroRtt); // 0.5ms
        timings.record(2_000_000, TimingCategory::OneRtt); // 2ms

        assert_eq!(timings.handshake_histogram.count(), 1);
        assert_eq!(timings.zero_rtt_histogram.count(), 1);
        assert_eq!(timings.one_rtt_histogram.count(), 1);
        assert_eq!(timings.attempts.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn test_registry_get_or_create() {
        let registry = TimingRegistry::new();
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();

        // first access creates entry
        let timing1 = registry.get_or_create(addr);
        assert_eq!(registry.ip_count(), 1);

        // second access returns same entry
        let timing2 = registry.get_or_create(addr);
        assert!(Arc::ptr_eq(&timing1, &timing2));
        assert_eq!(registry.ip_count(), 1);
    }

    #[test]
    fn test_registry_get() {
        let registry = TimingRegistry::new();
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();

        // get on non-existent entry returns None
        assert!(registry.get(addr).is_none());

        // create entry
        let _timing = registry.get_or_create(addr);

        // get now returns Some
        assert!(registry.get(addr).is_some());
    }

    #[test]
    fn test_global_median() {
        let registry = TimingRegistry::new();

        // no data yet
        assert!(registry.global_median_handshake_ms().is_none());

        // add some data
        let addr1: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let timing1 = registry.get_or_create(addr1);
        timing1.record(1_000_000_000, TimingCategory::Handshake); // 1000ms

        let addr2: SocketAddr = "127.0.0.1:8081".parse().unwrap();
        let timing2 = registry.get_or_create(addr2);
        timing2.record(2_000_000_000, TimingCategory::Handshake); // 2000ms

        // median should be between 1000ms and 2000ms
        let median = registry.global_median_handshake_ms().unwrap();
        assert!(median >= 1000 && median <= 2000);
    }

    #[test]
    fn test_sharding() {
        let registry = TimingRegistry::new();

        // add many addresses to test sharding
        for i in 0..100 {
            let addr: SocketAddr = format!("127.0.0.1:{}", 8000 + i).parse().unwrap();
            registry.get_or_create(addr);
        }

        assert_eq!(registry.ip_count(), 100);

        // verify addresses are distributed across shards
        let mut occupied_shards = 0;
        for shard in &registry.shards {
            if !shard.read().unwrap().is_empty() {
                occupied_shards += 1;
            }
        }

        // with 100 addresses, should occupy multiple shards
        assert!(occupied_shards > 1);
    }
}
