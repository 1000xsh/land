//! timer utilities for measuring code execution time
//!
//! provides raii-style scoped timers and manual timers with histogram integration.

use crate::{Histogram, TimePoint};
use std::sync::Arc;

/// manual timer for measuring elapsed time
pub struct Timer {
    start: TimePoint,
    histogram: Option<Arc<Histogram>>,
}

impl Timer {
    /// creates a new timer starting now
    #[inline(always)]
    pub fn start() -> Self {
        Self {
            start: TimePoint::now(),
            histogram: None,
        }
    }

    /// creates a timer that will record to a histogram
    #[inline(always)]
    pub fn start_with_histogram(histogram: Arc<Histogram>) -> Self {
        Self {
            start: TimePoint::now(),
            histogram: Some(histogram),
        }
    }

    /// gets elapsed time in nanoseconds
    #[inline(always)]
    pub fn elapsed_ns(&self) -> u64 {
        self.start.elapsed_ns()
    }

    /// gets elapsed time in microseconds
    #[inline(always)]
    pub fn elapsed_micros(&self) -> u64 {
        self.elapsed_ns() / 1000
    }

    /// gets elapsed time in milliseconds
    #[inline(always)]
    pub fn elapsed_millis(&self) -> u64 {
        self.elapsed_ns() / 1_000_000
    }

    /// stops the timer and records to histogram if configured
    #[inline(always)]
    pub fn stop(self) -> u64 {
        let elapsed = self.elapsed_ns();
        if let Some(histogram) = &self.histogram {
            histogram.record(elapsed);
        }
        elapsed
    }

    /// restarts the timer from current time
    #[inline(always)]
    pub fn restart(&mut self) {
        self.start = TimePoint::now();
    }
}

/// raii guard that records elapsed time when dropped
pub struct TimerGuard {
    start: TimePoint,
    histogram: Arc<Histogram>,
    #[cfg_attr(not(debug_assertions), allow(dead_code))]
    name: Option<&'static str>,
}

impl TimerGuard {
    /// creates a new timer guard
    #[inline(always)]
    pub fn new(histogram: Arc<Histogram>) -> Self {
        Self {
            start: TimePoint::now(),
            histogram,
            name: None,
        }
    }

    /// creates a named timer guard (for debugging)
    #[inline(always)]
    pub fn named(histogram: Arc<Histogram>, name: &'static str) -> Self {
        Self {
            start: TimePoint::now(),
            histogram,
            name: Some(name),
        }
    }

    /// gets elapsed time so far
    #[inline(always)]
    pub fn elapsed_ns(&self) -> u64 {
        self.start.elapsed_ns()
    }
}

impl Drop for TimerGuard {
    #[inline(always)]
    fn drop(&mut self) {
        let elapsed = self.start.elapsed_ns();
        self.histogram.record(elapsed);

        // optionally log slow operations
        #[cfg(debug_assertions)]
        if elapsed > 1_000_000 && self.name.is_some() {
            // > 1ms
            eprintln!(
                "slow operation '{}': {}μs",
                self.name.unwrap(),
                elapsed / 1000
            );
        }
    }
}

/// scoped timer for specific code sections
pub struct ScopedTimer {
    histogram: Arc<Histogram>,
    threshold_ns: u64,
}

impl ScopedTimer {
    /// creates a new scoped timer
    pub fn new(histogram: Arc<Histogram>) -> Self {
        Self {
            histogram,
            threshold_ns: 1_000_000, // default 1ms threshold
        }
    }

    /// sets the warning threshold in nanoseconds
    pub fn with_threshold(mut self, threshold_ns: u64) -> Self {
        self.threshold_ns = threshold_ns;
        self
    }

    /// times a closure and records the result
    #[inline(always)]
    pub fn time<F, R>(&self, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        let start = TimePoint::now();
        let result = f();
        let elapsed = start.elapsed_ns();

        self.histogram.record(elapsed);

        if elapsed > self.threshold_ns {
            eprintln!("operation exceeded threshold: {}μs", elapsed / 1000);
        }

        result
    }

    /// times a closure with a name for debugging
    #[inline(always)]
    pub fn time_named<F, R>(&self, name: &str, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        let start = TimePoint::now();
        let result = f();
        let elapsed = start.elapsed_ns();

        self.histogram.record(elapsed);

        if elapsed > self.threshold_ns {
            eprintln!(
                "operation '{}' exceeded threshold: {}μs",
                name,
                elapsed / 1000
            );
        }

        result
    }

    /// creates a timer guard for raii-style timing
    #[inline(always)]
    pub fn guard(&self) -> TimerGuard {
        TimerGuard::new(Arc::clone(&self.histogram))
    }

    /// creates a named timer guard
    #[inline(always)]
    pub fn guard_named(&self, name: &'static str) -> TimerGuard {
        TimerGuard::named(Arc::clone(&self.histogram), name)
    }
}

/// global histogram registry for named histograms
///
/// note: this module is optional and requires additional setup. fixme.
pub mod global {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{OnceLock, RwLock};

    static HISTOGRAMS: OnceLock<RwLock<HashMap<String, Arc<Histogram>>>> = OnceLock::new();

    fn get_registry() -> &'static RwLock<HashMap<String, Arc<Histogram>>> {
        HISTOGRAMS.get_or_init(|| RwLock::new(HashMap::new()))
    }

    /// gets or creates a named histogram
    pub fn histogram(name: &str) -> Arc<Histogram> {
        // check if exists
        {
            let histograms = get_registry().read().unwrap();
            if let Some(hist) = histograms.get(name) {
                return Arc::clone(hist);
            }
        }

        // create new
        let mut histograms = get_registry().write().unwrap();
        histograms
            .entry(name.to_string())
            .or_insert_with(|| Arc::new(Histogram::new()))
            .clone()
    }

    /// reports all histogram statistics
    pub fn report_all() {
        let histograms = get_registry().read().unwrap();
        for (name, hist) in histograms.iter() {
            let stats = hist.stats();
            if stats.count > 0 {
                eprintln!("{}: {}", name, stats.format_micros());
            }
        }
    }

    /// clears all histograms
    pub fn clear_all() {
        let histograms = get_registry().read().unwrap();
        for hist in histograms.values() {
            hist.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timer() {
        let mut timer = Timer::start();

        // do some work
        std::thread::sleep(std::time::Duration::from_millis(1));

        let elapsed = timer.elapsed_ns();
        assert!(elapsed > 1_000_000); // > 1ms

        timer.restart();
        let elapsed2 = timer.elapsed_ns();
        assert!(elapsed2 < elapsed); // should be less after restart
    }

    #[test]
    fn test_timer_guard() {
        let histogram = Arc::new(Histogram::new());

        {
            let _guard = TimerGuard::new(Arc::clone(&histogram));
            // do some work
            std::thread::sleep(std::time::Duration::from_millis(1));
        } // guard drops here and records time

        assert_eq!(histogram.count(), 1);
        assert!(histogram.max() > 1_000_000); // > 1ms
    }

    #[test]
    fn test_scoped_timer() {
        let histogram = Arc::new(Histogram::new());
        let timer = ScopedTimer::new(Arc::clone(&histogram));

        let result = timer.time(|| {
            // do some work
            std::thread::sleep(std::time::Duration::from_millis(1));
            42
        });

        assert_eq!(result, 42);
        assert_eq!(histogram.count(), 1);
    }
}
