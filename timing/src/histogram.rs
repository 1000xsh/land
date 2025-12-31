//! high-performance histogram for latency tracking
//!
//! uses logarithmic buckets for efficient storage and fast percentile calculation.
//! designed for nanosecond-precision measurements with minimal overhead.

use std::sync::atomic::{AtomicU64, Ordering};

/// number of buckets in the histogram
/// covers range from 1ns to ~4.6 hours with logarithmic distribution
const NUM_BUCKETS: usize = 64;

/// histogram for tracking latency distributions
///
/// thread-safe using atomics for lock-free updates.
pub struct Histogram {
    /// bucket counts
    buckets: [AtomicU64; NUM_BUCKETS],
    /// total count of samples
    count: AtomicU64,
    /// sum of all samples (for mean calculation)
    sum: AtomicU64,
    /// minimum value seen
    min: AtomicU64,
    /// maximum value seen
    max: AtomicU64,
}

impl Histogram {
    /// creates a new empty histogram
    pub fn new() -> Self {
        Self {
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            count: AtomicU64::new(0),
            sum: AtomicU64::new(0),
            min: AtomicU64::new(u64::MAX),
            max: AtomicU64::new(0),
        }
    }

    /// records a value in nanoseconds
    #[inline(always)]
    pub fn record(&self, value_ns: u64) {
        if value_ns == 0 {
            return;
        }

        // determine bucket index using logarithmic scaling
        let bucket = Self::value_to_bucket(value_ns);

        // increment bucket count
        self.buckets[bucket].fetch_add(1, Ordering::Relaxed);

        // update statistics
        self.count.fetch_add(1, Ordering::Relaxed);
        self.sum.fetch_add(value_ns, Ordering::Relaxed);

        // update min/max (may have races but that's ok for statistics)
        self.update_min(value_ns);
        self.update_max(value_ns);
    }

    /// updates minimum value
    fn update_min(&self, value: u64) {
        let mut current = self.min.load(Ordering::Relaxed);
        while value < current {
            match self.min.compare_exchange_weak(
                current,
                value,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(x) => current = x,
            }
        }
    }

    /// updates maximum value
    fn update_max(&self, value: u64) {
        let mut current = self.max.load(Ordering::Relaxed);
        while value > current {
            match self.max.compare_exchange_weak(
                current,
                value,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(x) => current = x,
            }
        }
    }

    /// converts a value to bucket index
    ///
    /// uses logarithmic scaling: bucket = floor(log2(value))
    #[inline(always)]
    fn value_to_bucket(value: u64) -> usize {
        if value == 0 {
            0
        } else {
            // count leading zeros and invert to get log2
            let bucket = 63 - value.leading_zeros() as usize;
            bucket.min(NUM_BUCKETS - 1)
        }
    }

    /// converts a bucket index to the minimum value in that bucket
    #[inline(always)]
    fn bucket_to_value(bucket: usize) -> u64 {
        if bucket == 0 {
            0
        } else {
            1u64 << bucket
        }
    }

    /// clears the histogram
    pub fn clear(&self) {
        for bucket in &self.buckets {
            bucket.store(0, Ordering::Relaxed);
        }
        self.count.store(0, Ordering::Relaxed);
        self.sum.store(0, Ordering::Relaxed);
        self.min.store(u64::MAX, Ordering::Relaxed);
        self.max.store(0, Ordering::Relaxed);
    }

    /// gets the total count of samples
    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    /// gets the minimum value
    pub fn min(&self) -> u64 {
        let min = self.min.load(Ordering::Relaxed);
        if min == u64::MAX {
            0
        } else {
            min
        }
    }

    /// gets the maximum value
    pub fn max(&self) -> u64 {
        self.max.load(Ordering::Relaxed)
    }

    /// calculates the mean value
    pub fn mean(&self) -> u64 {
        let count = self.count.load(Ordering::Relaxed);
        if count == 0 {
            0
        } else {
            self.sum.load(Ordering::Relaxed) / count
        }
    }

    /// calculates percentiles
    pub fn percentiles(&self, percentiles: &[f64]) -> Vec<u64> {
        let count = self.count.load(Ordering::Relaxed);
        if count == 0 {
            return vec![0; percentiles.len()];
        }

        // collect bucket counts
        let mut cumulative = 0u64;
        let mut buckets = Vec::with_capacity(NUM_BUCKETS);
        for i in 0..NUM_BUCKETS {
            let bucket_count = self.buckets[i].load(Ordering::Relaxed);
            cumulative += bucket_count;
            buckets.push((Self::bucket_to_value(i), cumulative));
        }

        // calculate percentiles
        let mut results = Vec::with_capacity(percentiles.len());
        for &p in percentiles {
            let target = ((count as f64 * p / 100.0) as u64).max(1);

            // find bucket containing target
            let mut value = 0u64;
            for &(bucket_value, cum_count) in &buckets {
                if cum_count >= target {
                    value = bucket_value;
                    break;
                }
            }
            results.push(value);
        }

        results
    }

    /// gets common percentiles (p50, p90, p99, p999, p9999)
    pub fn common_percentiles(&self) -> Percentiles {
        let values = self.percentiles(&[50.0, 90.0, 99.0, 99.9, 99.99]);
        Percentiles {
            p50: values[0],
            p90: values[1],
            p99: values[2],
            p999: values[3],
            p9999: values[4],
        }
    }

    /// gets latency statistics
    pub fn stats(&self) -> super::LatencyStats {
        let percentiles = self.common_percentiles();
        super::LatencyStats {
            min: self.min(),
            max: self.max(),
            mean: self.mean(),
            p50: percentiles.p50,
            p90: percentiles.p90,
            p99: percentiles.p99,
            p999: percentiles.p999,
            p9999: percentiles.p9999,
            count: self.count(),
        }
    }
}

impl Default for Histogram {
    fn default() -> Self {
        Self::new()
    }
}

/// common percentile values
#[derive(Debug, Clone, Copy, Default)]
pub struct Percentiles {
    pub p50: u64,
    pub p90: u64,
    pub p99: u64,
    pub p999: u64,
    pub p9999: u64,
}

impl Percentiles {
    /// formats percentiles as a string
    pub fn format(&self) -> String {
        format!(
            "p50={}ns p90={}ns p99={}ns p999={}ns p9999={}ns",
            self.p50, self.p90, self.p99, self.p999, self.p9999
        )
    }

    /// formats with microsecond units
    pub fn format_micros(&self) -> String {
        format!(
            "p50={}μs p90={}μs p99={}μs p999={}μs p9999={}μs",
            self.p50 / 1000,
            self.p90 / 1000,
            self.p99 / 1000,
            self.p999 / 1000,
            self.p9999 / 1000
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_histogram_basic() {
        let hist = Histogram::new();

        // record some values
        hist.record(100);
        hist.record(200);
        hist.record(300);
        hist.record(400);
        hist.record(500);

        assert_eq!(hist.count(), 5);
        assert_eq!(hist.min(), 100);
        assert_eq!(hist.max(), 500);
        assert_eq!(hist.mean(), 300);
    }

    #[test]
    fn test_percentiles() {
        let hist = Histogram::new();

        // record values 1-100
        for i in 1..=100 {
            hist.record(i * 1000); // in nanoseconds
        }

        let percentiles = hist.common_percentiles();

        // p50 should be around 50,000
        assert!(percentiles.p50 >= 32_768 && percentiles.p50 <= 65_536);

        // p99 should be near 99,000
        assert!(percentiles.p99 >= 65_536);
    }

    #[test]
    fn test_bucket_conversion() {
        assert_eq!(Histogram::value_to_bucket(0), 0);
        assert_eq!(Histogram::value_to_bucket(1), 0);
        assert_eq!(Histogram::value_to_bucket(2), 1);
        assert_eq!(Histogram::value_to_bucket(4), 2);
        assert_eq!(Histogram::value_to_bucket(8), 3);
        assert_eq!(Histogram::value_to_bucket(1024), 10);
        assert_eq!(Histogram::value_to_bucket(1_000_000), 19); // ~1ms
        assert_eq!(Histogram::value_to_bucket(1_000_000_000), 29); // ~1s
    }

    #[test]
    fn test_clear() {
        let hist = Histogram::new();

        hist.record(100);
        hist.record(200);
        assert_eq!(hist.count(), 2);

        hist.clear();
        assert_eq!(hist.count(), 0);
        assert_eq!(hist.min(), 0);
        assert_eq!(hist.max(), 0);
    }
}