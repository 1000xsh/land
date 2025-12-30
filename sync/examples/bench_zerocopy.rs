//! benchmark: copy vs zero-copy SeqLock reads
//!
//! run with: cargo run --release --example bench_zerocopy

use land_sync::SeqLock;
use std::hint::black_box;
use std::time::Instant;

/// large struct to demonstrate zero-copy benefits
#[derive(Copy, Clone)]
#[repr(C, align(64))]
struct LargeEntry {
    id: u64,
    timestamp: u64,
    value: u64,
    flags: u32,
    _padding: [u8; 1024],
}

impl LargeEntry {
    const fn new(id: u64) -> Self {
        Self {
            id,
            timestamp: 0,
            value: 0,
            flags: 0,
            _padding: [0; 1024],
        }
    }
}

const ITERATIONS: u64 = 10_000_000;
const WARMUP: u64 = 100_000;

fn bench_copy_read(lock: &SeqLock<LargeEntry>) -> (u64, u128) {
    // warmup
    for _ in 0..WARMUP {
        let entry = black_box(lock).read();
        black_box(entry.id);
    }

    let start = Instant::now();
    let mut sum = 0u64;

    for _ in 0..ITERATIONS {
        let entry = black_box(lock).read();
        sum = sum.wrapping_add(black_box(entry).id);
    }

    let elapsed_ns = start.elapsed().as_nanos();
    (black_box(sum), elapsed_ns)
}

fn bench_zerocopy_read(lock: &SeqLock<LargeEntry>) -> (u64, u128) {
    // warmup
    for _ in 0..WARMUP {
        let id = black_box(lock).read_with(|entry| entry.id);
        black_box(id);
    }

    let start = Instant::now();
    let mut sum = 0u64;

    for _ in 0..ITERATIONS {
        let id = black_box(lock).read_with(|entry| entry.id);
        sum = sum.wrapping_add(black_box(id));
    }

    let elapsed_ns = start.elapsed().as_nanos();
    (black_box(sum), elapsed_ns)
}

fn bench_copy_read_array(lock: &SeqLock<[LargeEntry; 10]>) -> (u64, u128) {
    // warmup
    for _ in 0..WARMUP {
        let entries = black_box(lock).read();
        black_box(entries[0].id);
    }

    let start = Instant::now();
    let mut sum = 0u64;

    for _ in 0..ITERATIONS {
        let entries = black_box(lock).read();
        sum = sum.wrapping_add(black_box(entries)[0].id);
    }

    let elapsed_ns = start.elapsed().as_nanos();
    (black_box(sum), elapsed_ns)
}

fn bench_zerocopy_read_array(lock: &SeqLock<[LargeEntry; 10]>) -> (u64, u128) {
    // warmup
    for _ in 0..WARMUP {
        let id = black_box(lock).read_with(|entries| entries[0].id);
        black_box(id);
    }

    let start = Instant::now();
    let mut sum = 0u64;

    for _ in 0..ITERATIONS {
        let id = black_box(lock).read_with(|entries| entries[0].id);
        sum = sum.wrapping_add(black_box(id));
    }

    let elapsed_ns = start.elapsed().as_nanos();
    (black_box(sum), elapsed_ns)
}

fn main() {
    println!("SeqLock Copy vs Zero-Copy Benchmark");
    println!("====================================");
    println!("iterations: {}", ITERATIONS);
    println!();

    // single entry benchmark (~1KB struct)
    println!("--- Single LargeEntry (~1KB) ---");
    let single = SeqLock::new(LargeEntry::new(42));

    let (sum1, ns_copy) = bench_copy_read(&single);
    let ns_per_copy = ns_copy as f64 / ITERATIONS as f64;
    println!("copy read:      {:.2} ns/op  (total: {} ms)", ns_per_copy, ns_copy / 1_000_000);

    let (sum2, ns_zerocopy) = bench_zerocopy_read(&single);
    let ns_per_zc = ns_zerocopy as f64 / ITERATIONS as f64;
    println!("zero-copy read: {:.2} ns/op  (total: {} ms)", ns_per_zc, ns_zerocopy / 1_000_000);

    if ns_per_zc > 0.0 {
        println!("speedup:        {:.2}x", ns_per_copy / ns_per_zc);
    }
    println!("checksum:       {} == {} ({})", sum1, sum2, if sum1 == sum2 { "OK" } else { "MISMATCH" });
    println!();

    // array benchmark (~10KB)
    println!("--- Array [LargeEntry; 10] (~10KB) ---");
    let array = SeqLock::new([LargeEntry::new(42); 10]);

    let (sum3, ns_copy_arr) = bench_copy_read_array(&array);
    let ns_per_copy_arr = ns_copy_arr as f64 / ITERATIONS as f64;
    println!("copy read:      {:.2} ns/op  (total: {} ms)", ns_per_copy_arr, ns_copy_arr / 1_000_000);

    let (sum4, ns_zc_arr) = bench_zerocopy_read_array(&array);
    let ns_per_zc_arr = ns_zc_arr as f64 / ITERATIONS as f64;
    println!("zero-copy read: {:.2} ns/op  (total: {} ms)", ns_per_zc_arr, ns_zc_arr / 1_000_000);

    if ns_per_zc_arr > 0.0 {
        println!("speedup:        {:.2}x", ns_per_copy_arr / ns_per_zc_arr);
    }
    println!("checksum:       {} == {} ({})", sum3, sum4, if sum3 == sum4 { "OK" } else { "MISMATCH" });
    println!();

    // size info
    println!("--- Size Info ---");
    println!("LargeEntry size:        {} bytes", std::mem::size_of::<LargeEntry>());
    println!("[LargeEntry; 10] size:  {} bytes", std::mem::size_of::<[LargeEntry; 10]>());
    println!("u64 size:               {} bytes", std::mem::size_of::<u64>());
}
