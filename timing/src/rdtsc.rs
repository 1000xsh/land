//! rdtsc (read time-stamp counter) wrapper for x86_64
//!
//! provides access to cpu cycle counter for ultra-precise timing.
//! on non-x86_64 platforms, falls back to standard time functions.

use std::time::{Duration, Instant};

/// reads the time-stamp counter
///
/// returns the current value of the processor's time-stamp counter.
/// this is the number of clock cycles since the last reset.
#[inline(always)]
#[cfg(target_arch = "x86_64")]
pub fn rdtsc() -> u64 {
    unsafe {
        let lo: u32;
        let hi: u32;
        // use inline assembly to read tsc
        std::arch::asm!(
            "rdtsc",
            out("eax") lo,
            out("edx") hi,
            options(nostack, nomem),
        );
        ((hi as u64) << 32) | (lo as u64)
    }
}

/// reads the time-stamp counter with serialization
///
/// rdtscp is a serializing variant that waits for all previous instructions
/// to complete before reading the counter. slightly slower but more accurate.
#[inline(always)]
#[cfg(target_arch = "x86_64")]
pub fn rdtscp() -> u64 {
    unsafe {
        let lo: u32;
        let hi: u32;
        let _aux: u32;
        // use inline assembly to read tsc with serialization
        std::arch::asm!(
            "rdtscp",
            out("eax") lo,
            out("edx") hi,
            out("ecx") _aux,
            options(nostack, nomem),
        );
        ((hi as u64) << 32) | (lo as u64)
    }
}

/// calibrates the cpu frequency by measuring rdtsc over a known time period
///
/// returns the frequency in cycles per second.
#[cfg(target_arch = "x86_64")]
pub fn calibrate_frequency() -> u64 {
    // warm up
    for _ in 0..10 {
        rdtsc();
    }

    // measure over 100ms for accuracy
    let duration = Duration::from_millis(100);

    let start_instant = Instant::now();
    let start_cycles = rdtsc();

    // busy wait for duration
    while start_instant.elapsed() < duration {
        std::hint::spin_loop();
    }

    let end_cycles = rdtsc();
    let elapsed = start_instant.elapsed();

    // calculate frequency
    let cycles = end_cycles - start_cycles;
    let nanos = elapsed.as_nanos();

    // cycles per nanosecond * 1e9 = cycles per second
    ((cycles as u128 * 1_000_000_000) / nanos) as u64
}

/// converts cpu cycles to nanoseconds using calibrated frequency
#[inline(always)]
pub fn cycles_to_nanos(cycles: u64) -> u64 {
    let freq = super::cpu_frequency();
    if freq == 0 {
        // not calibrated, assume 3ghz
        cycles / 3
    } else {
        // multiply by 1e9 and divide by frequency
        ((cycles as u128 * 1_000_000_000) / freq as u128) as u64
    }
}

/// converts nanoseconds to cpu cycles using calibrated frequency
#[inline(always)]
pub fn nanos_to_cycles(nanos: u64) -> u64 {
    let freq = super::cpu_frequency();
    if freq == 0 {
        // not calibrated, assume 3ghz
        nanos * 3
    } else {
        // multiply by frequency and divide by 1e9
        ((nanos as u128 * freq as u128) / 1_000_000_000) as u64
    }
}

/// memory fence to prevent instruction reordering
#[inline(always)]
pub fn fence() {
    std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
}

/// serializing fence using cpuid instruction
#[inline(always)]
#[cfg(target_arch = "x86_64")]
pub fn serialize() {
    unsafe {
        // cpuid is a serializing instruction
        // note: rbx is reserved by llvm, so we need to save/restore it?
        std::arch::asm!(
            "push rbx",
            "cpuid",
            "pop rbx",
            inout("eax") 0 => _,
            out("ecx") _,
            out("edx") _,
            options(nomem, preserves_flags),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rdtsc() {
        let start = rdtsc();
        // do some work
        let mut sum = 0u64;
        for i in 0..1000 {
            sum = sum.wrapping_add(i);
        }
        let end = rdtsc();

        // should advance
        assert!(end >= start);

        // prevent optimization
        std::hint::black_box(sum);
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn test_calibration() {
        let freq = calibrate_frequency();

        // typical cpu frequencies are 1-5 ghz
        assert!(freq > 500_000_000); // > 0.5 ghz
        assert!(freq < 10_000_000_000); // < 10 ghz

        eprintln!("calibrated frequency: {} ghz", freq as f64 / 1e9);
    }

    #[test]
    fn test_conversion() {
        super::super::init();

        let cycles = 1_000_000_000; // 1 billion cycles
        let nanos = cycles_to_nanos(cycles);
        let cycles2 = nanos_to_cycles(nanos);

        // should be approximately equal (some rounding error ok)
        let diff = if cycles2 > cycles {
            cycles2 - cycles
        } else {
            cycles - cycles2
        };

        assert!(diff < cycles / 100); // less than 1% error
    }
}
