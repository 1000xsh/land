//! apples-to-apples loop unroll benchmark
//!
//! proves/disproves whether manual unrolling beats LLVM auto-unrolling.
//!
//! key insight: LLVM aggressively unrolls simple loops. to create fair
//! comparisons we test scenarios where LLVM won't/can't unroll effectively:
//! - dynamic bounds (black_box prevents compile-time unrolling)
//! - complex loop bodies
//! - large iteration counts (beyond LLVM's threshold)
//!
//! run: cargo run --release --example latency
//! verify asm: objdump -d -M amd target/release/examples/latency | grep -A 50 '<bench_'

use land_timing::rdtsc;
use land_unroll::unroll;
use std::hint::black_box;
use std::ptr::write_volatile;

/// number of outer repetitions to amortize measurement overhead
const OUTER_REPS: usize = 1000;

// ============================================================================
// measurement infrastructure
// ============================================================================

/// measure cycles for a function that does OUTER_REPS internal iterations
/// returns (total_cycles, p10_cycles, checksum) - NOT divided, to avoid rounding
#[inline(never)]
fn measure_cycles<F: FnMut() -> u64>(
    mut f: F,
    warmup: usize,
    iterations: usize,
) -> (u64, u64, u64) {
    let mut checksum = 0u64;

    // warmup
    for _ in 0..warmup {
        checksum = checksum.wrapping_add(black_box(f()));
    }

    // measure
    let mut samples = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
        let start = rdtsc();
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
        let result = f();
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
        let end = rdtsc();
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
        checksum = checksum.wrapping_add(black_box(result));
        // report total cycles for OUTER_REPS iterations (don't divide to avoid rounding)
        samples.push(end.wrapping_sub(start));
    }

    samples.sort_unstable();
    let median = samples[iterations / 2];
    let p10 = samples[iterations / 10];
    (median, p10, checksum)
}

fn print_comparison(name: &str, results: &[(&str, (u64, u64, u64))]) {
    println!("\n{name} ({} reps):", OUTER_REPS);

    let best = results.iter().map(|(_, (m, _, _))| *m).min().unwrap_or(1);

    for (variant, (median, _, _)) in results {
        let ratio = *median as f64 / best.max(1) as f64;
        let per_iter = *median as f64 / OUTER_REPS as f64;

        println!(
            "  {:<12} {:>6} total ({:>5.2}/iter) {:.2}x",
            variant, median, per_iter, ratio
        );
    }
}

// ============================================================================
// TEST 1: small fixed array (8 elements)
// baseline: LLVM will likely unroll both, expect ~tie
// ============================================================================

#[inline(never)]
fn bench_unroll_8(data: &[u64; 8], sink: &mut u64) -> u64 {
    let mut total = 0u64;
    for _ in 0..OUTER_REPS {
        let mut sum = 0u64;
        unroll!(8, |i| {
            sum = sum.wrapping_add(data[i]);
        });
        total = total.wrapping_add(sum);
        // prevent loop from being optimized away
        unsafe {
            write_volatile(sink, sum);
        }
    }
    total
}

#[inline(never)]
fn bench_loop_8(data: &[u64; 8], sink: &mut u64) -> u64 {
    let mut total = 0u64;
    for _ in 0..OUTER_REPS {
        let mut sum = 0u64;
        for i in 0..8 {
            sum = sum.wrapping_add(data[i]);
        }
        total = total.wrapping_add(sum);
        unsafe {
            write_volatile(sink, sum);
        }
    }
    total
}

#[inline(never)]
fn bench_iter_8(data: &[u64; 8], sink: &mut u64) -> u64 {
    let mut total = 0u64;
    for _ in 0..OUTER_REPS {
        let sum = data.iter().fold(0u64, |acc, &x| acc.wrapping_add(x));
        total = total.wrapping_add(sum);
        unsafe {
            write_volatile(sink, sum);
        }
    }
    total
}

// ============================================================================
// TEST 2: dynamic bounds - KEY TEST
// LLVM CANNOT unroll because bounds unknown at compile time
// unroll! should win decisively here
// ============================================================================

#[inline(never)]
fn bench_unroll_dynamic_8(data: &[u64; 8], _n: usize, sink: &mut u64) -> u64 {
    // we know size is 8, use unroll! with const 8
    let mut total = 0u64;
    for _ in 0..OUTER_REPS {
        let mut sum = 0u64;
        unroll!(8, |i| {
            sum = sum.wrapping_add(data[i]);
        });
        total = total.wrapping_add(sum);
        unsafe {
            write_volatile(sink, sum);
        }
    }
    total
}

#[inline(never)]
fn bench_loop_dynamic(data: &[u64], n: usize, sink: &mut u64) -> u64 {
    let mut total = 0u64;
    for _ in 0..OUTER_REPS {
        let mut sum = 0u64;
        // n is runtime value - LLVM cannot unroll this
        for i in 0..n {
            sum = sum.wrapping_add(data[i]);
        }
        total = total.wrapping_add(sum);
        unsafe {
            write_volatile(sink, sum);
        }
    }
    total
}

#[inline(never)]
fn bench_iter_dynamic(data: &[u64], n: usize, sink: &mut u64) -> u64 {
    let mut total = 0u64;
    for _ in 0..OUTER_REPS {
        let sum = data[..n].iter().fold(0u64, |acc, &x| acc.wrapping_add(x));
        total = total.wrapping_add(sum);
        unsafe {
            write_volatile(sink, sum);
        }
    }
    total
}

// ============================================================================
// TEST 3: dependency chain (16 elements) - mul/add chain
// tests instruction scheduling benefits
// ============================================================================

#[inline(never)]
fn bench_unroll_chain_16(data: &[u64; 16], sink: &mut u64) -> u64 {
    let mut total = 0u64;
    for _ in 0..OUTER_REPS {
        let mut state = 1u64;
        unroll!(16, |i| {
            state = state.wrapping_mul(data[i]).wrapping_add(data[i]);
        });
        total = total.wrapping_add(state);
        unsafe {
            write_volatile(sink, state);
        }
    }
    total
}

#[inline(never)]
fn bench_loop_chain_16(data: &[u64; 16], sink: &mut u64) -> u64 {
    let mut total = 0u64;
    for _ in 0..OUTER_REPS {
        let mut state = 1u64;
        for i in 0..16 {
            state = state.wrapping_mul(data[i]).wrapping_add(data[i]);
        }
        total = total.wrapping_add(state);
        unsafe {
            write_volatile(sink, state);
        }
    }
    total
}

#[inline(never)]
fn bench_iter_chain_16(data: &[u64; 16], sink: &mut u64) -> u64 {
    let mut total = 0u64;
    for _ in 0..OUTER_REPS {
        let state = data
            .iter()
            .fold(1u64, |s, &x| s.wrapping_mul(x).wrapping_add(x));
        total = total.wrapping_add(state);
        unsafe {
            write_volatile(sink, state);
        }
    }
    total
}

// ============================================================================
// TEST 4: large iteration (64 elements) - near LLVM threshold
// ============================================================================

#[inline(never)]
fn bench_unroll_64(data: &[u64; 64], sink: &mut u64) -> u64 {
    let mut total = 0u64;
    for _ in 0..OUTER_REPS {
        let mut sum = 0u64;
        unroll!(64, |i| {
            sum = sum.wrapping_add(data[i]);
        });
        total = total.wrapping_add(sum);
        unsafe {
            write_volatile(sink, sum);
        }
    }
    total
}

#[inline(never)]
fn bench_loop_64(data: &[u64; 64], sink: &mut u64) -> u64 {
    let mut total = 0u64;
    for _ in 0..OUTER_REPS {
        let mut sum = 0u64;
        for i in 0..64 {
            sum = sum.wrapping_add(data[i]);
        }
        total = total.wrapping_add(sum);
        unsafe {
            write_volatile(sink, sum);
        }
    }
    total
}

#[inline(never)]
fn bench_iter_64(data: &[u64; 64], sink: &mut u64) -> u64 {
    let mut total = 0u64;
    for _ in 0..OUTER_REPS {
        let sum = data.iter().fold(0u64, |acc, &x| acc.wrapping_add(x));
        total = total.wrapping_add(sum);
        unsafe {
            write_volatile(sink, sum);
        }
    }
    total
}

// ============================================================================
// TEST 5: very large (128 elements) - beyond LLVM threshold
// ============================================================================

#[inline(never)]
fn bench_unroll_128(data: &[u64; 128], sink: &mut u64) -> u64 {
    let mut total = 0u64;
    for _ in 0..OUTER_REPS {
        let mut sum = 0u64;
        unroll!(128, |i| {
            sum = sum.wrapping_add(data[i]);
        });
        total = total.wrapping_add(sum);
        unsafe {
            write_volatile(sink, sum);
        }
    }
    total
}

#[inline(never)]
fn bench_loop_128(data: &[u64; 128], sink: &mut u64) -> u64 {
    let mut total = 0u64;
    for _ in 0..OUTER_REPS {
        let mut sum = 0u64;
        for i in 0..128 {
            sum = sum.wrapping_add(data[i]);
        }
        total = total.wrapping_add(sum);
        unsafe {
            write_volatile(sink, sum);
        }
    }
    total
}

#[inline(never)]
fn bench_iter_128(data: &[u64; 128], sink: &mut u64) -> u64 {
    let mut total = 0u64;
    for _ in 0..OUTER_REPS {
        let sum = data.iter().fold(0u64, |acc, &x| acc.wrapping_add(x));
        total = total.wrapping_add(sum);
        unsafe {
            write_volatile(sink, sum);
        }
    }
    total
}

// ============================================================================
// TEST 6: nested loops (4x4 matrix) - LLVM rarely unrolls outer loop
// ============================================================================

#[inline(never)]
fn bench_unroll_nested_4x4(matrix: &[[u64; 4]; 4], sink: &mut u64) -> u64 {
    let mut total = 0u64;
    for _ in 0..OUTER_REPS {
        let mut sum = 0u64;
        unroll!(4, |r| {
            unroll!(4, |c| {
                sum = sum.wrapping_add(matrix[r][c]);
            });
        });
        total = total.wrapping_add(sum);
        unsafe {
            write_volatile(sink, sum);
        }
    }
    total
}

#[inline(never)]
fn bench_loop_nested_4x4(matrix: &[[u64; 4]; 4], sink: &mut u64) -> u64 {
    let mut total = 0u64;
    for _ in 0..OUTER_REPS {
        let mut sum = 0u64;
        for r in 0..4 {
            for c in 0..4 {
                sum = sum.wrapping_add(matrix[r][c]);
            }
        }
        total = total.wrapping_add(sum);
        unsafe {
            write_volatile(sink, sum);
        }
    }
    total
}

#[inline(never)]
fn bench_iter_nested_4x4(matrix: &[[u64; 4]; 4], sink: &mut u64) -> u64 {
    let mut total = 0u64;
    for _ in 0..OUTER_REPS {
        let sum = matrix
            .iter()
            .flat_map(|row| row.iter())
            .fold(0u64, |acc, &x| acc.wrapping_add(x));
        total = total.wrapping_add(sum);
        unsafe {
            write_volatile(sink, sum);
        }
    }
    total
}

// ============================================================================
// TEST 7: complex body (mul + conditional per iteration)
// LLVM is conservative about unrolling complex bodies
// ============================================================================

#[inline(never)]
fn bench_unroll_complex_16(data: &[u64; 16], coeffs: &[u64; 16], sink: &mut u64) -> u64 {
    let mut total = 0u64;
    for _ in 0..OUTER_REPS {
        let mut acc = 0u64;
        let mut prev = 0u64;
        unroll!(16, |i| {
            let val = data[i].wrapping_mul(coeffs[i]);
            let diff = if val > prev { val - prev } else { prev - val };
            acc = acc.wrapping_add(diff);
            prev = val;
        });
        total = total.wrapping_add(acc);
        unsafe {
            write_volatile(sink, acc);
        }
    }
    total
}

#[inline(never)]
fn bench_loop_complex_16(data: &[u64; 16], coeffs: &[u64; 16], sink: &mut u64) -> u64 {
    let mut total = 0u64;
    for _ in 0..OUTER_REPS {
        let mut acc = 0u64;
        let mut prev = 0u64;
        for i in 0..16 {
            let val = data[i].wrapping_mul(coeffs[i]);
            let diff = if val > prev { val - prev } else { prev - val };
            acc = acc.wrapping_add(diff);
            prev = val;
        }
        total = total.wrapping_add(acc);
        unsafe {
            write_volatile(sink, acc);
        }
    }
    total
}

#[inline(never)]
fn bench_iter_complex_16(data: &[u64; 16], coeffs: &[u64; 16], sink: &mut u64) -> u64 {
    let mut total = 0u64;
    for _ in 0..OUTER_REPS {
        let mut acc = 0u64;
        let mut prev = 0u64;
        for (&d, &c) in data.iter().zip(coeffs.iter()) {
            let val = d.wrapping_mul(c);
            let diff = if val > prev { val - prev } else { prev - val };
            acc = acc.wrapping_add(diff);
            prev = val;
        }
        total = total.wrapping_add(acc);
        unsafe {
            write_volatile(sink, acc);
        }
    }
    total
}

// ============================================================================
// TEST 8: order book (10 levels) - trading scenario
// ============================================================================

#[inline(never)]
fn bench_unroll_orderbook(prices: &[u64; 10], sizes: &[u64; 10], sink: &mut u64) -> u64 {
    let mut total = 0u64;
    for _ in 0..OUTER_REPS {
        let mut best = 0u64;
        unroll!(10, |i| {
            let notional = prices[i].wrapping_mul(sizes[i]);
            if notional > best {
                best = notional;
            }
        });
        total = total.wrapping_add(best);
        unsafe {
            write_volatile(sink, best);
        }
    }
    total
}

#[inline(never)]
fn bench_loop_orderbook(prices: &[u64; 10], sizes: &[u64; 10], sink: &mut u64) -> u64 {
    let mut total = 0u64;
    for _ in 0..OUTER_REPS {
        let mut best = 0u64;
        for i in 0..10 {
            let notional = prices[i].wrapping_mul(sizes[i]);
            if notional > best {
                best = notional;
            }
        }
        total = total.wrapping_add(best);
        unsafe {
            write_volatile(sink, best);
        }
    }
    total
}

#[inline(never)]
fn bench_iter_orderbook(prices: &[u64; 10], sizes: &[u64; 10], sink: &mut u64) -> u64 {
    let mut total = 0u64;
    for _ in 0..OUTER_REPS {
        let best = prices
            .iter()
            .zip(sizes.iter())
            .map(|(&p, &s)| p.wrapping_mul(s))
            .max()
            .unwrap_or(0);
        total = total.wrapping_add(best);
        unsafe {
            write_volatile(sink, best);
        }
    }
    total
}

// ============================================================================
// main
// ============================================================================

fn main() {
    land_timing::init();

    println!("=== loop unroll benchmark: apples-to-apples comparison ===");
    println!("comparing: unroll! macro vs for loop vs iterator");
    println!();
    println!("methodology:");
    println!(
        "  - {} outer reps to amortize measurement overhead",
        OUTER_REPS
    );
    println!("  - #[inline(never)] for assembly inspection");
    println!("  - volatile sink prevents dead code elimination");
    println!("  - cycles shown are per single iteration of inner loop");
    println!();

    const WARMUP: usize = 100;
    const ITERATIONS: usize = 1000;

    let mut sink = 0u64;

    // test 1: fixed 8
    {
        let data: [u64; 8] = std::array::from_fn(|i| i as u64 + 1);
        let data = black_box(&data);

        let r_unroll = measure_cycles(|| bench_unroll_8(data, &mut sink), WARMUP, ITERATIONS);
        let r_loop = measure_cycles(|| bench_loop_8(data, &mut sink), WARMUP, ITERATIONS);
        let r_iter = measure_cycles(|| bench_iter_8(data, &mut sink), WARMUP, ITERATIONS);

        print_comparison(
            "fixed 8 elements (LLVM likely unrolls both)",
            &[
                ("unroll!", r_unroll),
                ("for loop", r_loop),
                ("iterator", r_iter),
            ],
        );
    }

    // test 2: dynamic bounds - KEY TEST
    {
        let data: [u64; 8] = std::array::from_fn(|i| i as u64 + 1);
        let data = black_box(&data);
        let n = black_box(8usize);

        let r_unroll = measure_cycles(
            || bench_unroll_dynamic_8(data, n, &mut sink),
            WARMUP,
            ITERATIONS,
        );
        let r_loop = measure_cycles(
            || bench_loop_dynamic(data.as_slice(), n, &mut sink),
            WARMUP,
            ITERATIONS,
        );
        let r_iter = measure_cycles(
            || bench_iter_dynamic(data.as_slice(), n, &mut sink),
            WARMUP,
            ITERATIONS,
        );

        print_comparison(
            "dynamic bounds (LLVM cannot unroll - unroll! should win)",
            &[
                ("unroll!", r_unroll),
                ("for loop", r_loop),
                ("iterator", r_iter),
            ],
        );
    }

    // test 3: dependency chain
    {
        let data: [u64; 16] = std::array::from_fn(|i| i as u64 + 1);
        let data = black_box(&data);

        let r_unroll = measure_cycles(
            || bench_unroll_chain_16(data, &mut sink),
            WARMUP,
            ITERATIONS,
        );
        let r_loop = measure_cycles(|| bench_loop_chain_16(data, &mut sink), WARMUP, ITERATIONS);
        let r_iter = measure_cycles(|| bench_iter_chain_16(data, &mut sink), WARMUP, ITERATIONS);

        print_comparison(
            "dependency chain 16 (mul/add chain)",
            &[
                ("unroll!", r_unroll),
                ("for loop", r_loop),
                ("iterator", r_iter),
            ],
        );
    }

    // test 4: 64 elements
    {
        let data: [u64; 64] = std::array::from_fn(|i| i as u64 + 1);
        let data = black_box(&data);

        let r_unroll = measure_cycles(|| bench_unroll_64(data, &mut sink), WARMUP, ITERATIONS);
        let r_loop = measure_cycles(|| bench_loop_64(data, &mut sink), WARMUP, ITERATIONS);
        let r_iter = measure_cycles(|| bench_iter_64(data, &mut sink), WARMUP, ITERATIONS);

        print_comparison(
            "64 elements (near LLVM unroll threshold)",
            &[
                ("unroll!", r_unroll),
                ("for loop", r_loop),
                ("iterator", r_iter),
            ],
        );
    }

    // test 5: 128 elements
    {
        let data: [u64; 128] = std::array::from_fn(|i| i as u64 + 1);
        let data = black_box(&data);

        let r_unroll = measure_cycles(|| bench_unroll_128(data, &mut sink), WARMUP, ITERATIONS);
        let r_loop = measure_cycles(|| bench_loop_128(data, &mut sink), WARMUP, ITERATIONS);
        let r_iter = measure_cycles(|| bench_iter_128(data, &mut sink), WARMUP, ITERATIONS);

        print_comparison(
            "128 elements (beyond LLVM unroll threshold)",
            &[
                ("unroll!", r_unroll),
                ("for loop", r_loop),
                ("iterator", r_iter),
            ],
        );
    }

    // test 6: nested 4x4
    {
        let matrix: [[u64; 4]; 4] =
            std::array::from_fn(|r| std::array::from_fn(|c| (r * 4 + c + 1) as u64));
        let matrix = black_box(&matrix);

        let r_unroll = measure_cycles(
            || bench_unroll_nested_4x4(matrix, &mut sink),
            WARMUP,
            ITERATIONS,
        );
        let r_loop = measure_cycles(
            || bench_loop_nested_4x4(matrix, &mut sink),
            WARMUP,
            ITERATIONS,
        );
        let r_iter = measure_cycles(
            || bench_iter_nested_4x4(matrix, &mut sink),
            WARMUP,
            ITERATIONS,
        );

        print_comparison(
            "nested 4x4 matrix (LLVM rarely unrolls outer)",
            &[
                ("unroll!", r_unroll),
                ("for loop", r_loop),
                ("iterator", r_iter),
            ],
        );
    }

    // test 7: complex body
    {
        let data: [u64; 16] = std::array::from_fn(|i| i as u64 + 1);
        let coeffs: [u64; 16] = std::array::from_fn(|i| (16 - i) as u64);
        let data = black_box(&data);
        let coeffs = black_box(&coeffs);

        let r_unroll = measure_cycles(
            || bench_unroll_complex_16(data, coeffs, &mut sink),
            WARMUP,
            ITERATIONS,
        );
        let r_loop = measure_cycles(
            || bench_loop_complex_16(data, coeffs, &mut sink),
            WARMUP,
            ITERATIONS,
        );
        let r_iter = measure_cycles(
            || bench_iter_complex_16(data, coeffs, &mut sink),
            WARMUP,
            ITERATIONS,
        );

        print_comparison(
            "complex body 16 (mul + branch per iter)",
            &[
                ("unroll!", r_unroll),
                ("for loop", r_loop),
                ("iterator", r_iter),
            ],
        );
    }

    // test 8: order book
    {
        let prices: [u64; 10] = [100, 99, 98, 97, 96, 95, 94, 93, 92, 91];
        let sizes: [u64; 10] = [10, 20, 30, 40, 50, 60, 70, 80, 90, 100];
        let prices = black_box(&prices);
        let sizes = black_box(&sizes);

        let r_unroll = measure_cycles(
            || bench_unroll_orderbook(prices, sizes, &mut sink),
            WARMUP,
            ITERATIONS,
        );
        let r_loop = measure_cycles(
            || bench_loop_orderbook(prices, sizes, &mut sink),
            WARMUP,
            ITERATIONS,
        );
        let r_iter = measure_cycles(
            || bench_iter_orderbook(prices, sizes, &mut sink),
            WARMUP,
            ITERATIONS,
        );

        print_comparison(
            "order book 10 levels (trading scenario)",
            &[
                ("unroll!", r_unroll),
                ("for loop", r_loop),
                ("iterator", r_iter),
            ],
        );
    }

    // prevent sink from being optimized away
    black_box(sink);

    println!();
    println!("to verify assembly:");
    println!("  cargo build --release --example latency");
    println!(
        "  objdump -d target/release/examples/latency | grep bench_loop"
    );
        println!(
        "  objdump -d target/release/examples/latency | grep bench_unroll"
    );
}
