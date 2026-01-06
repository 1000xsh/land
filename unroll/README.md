# land_unroll

compile-time loop unrolling for rust

## when to use

**only two scenarios provide measurable speedup:**

| scenario            | speedup  | why      
|---------------------|----------|----------
| **dynamic bounds**  | **7.5x** | slice with known size - LLVM cant unroll
| **128+ iterations** | **~2x**  | beyond LLVMs unroll threshold

everything else ties with regular loops - LLVM already optimizes them.

## use case

```rust
use land_unroll::unroll;

// slice with runtime bounds - LLVM cannot unroll
fn process_slice(data: &[u64]) {
    // slow: LLVM sees runtime bounds
    for i in 0..data.len() { /* ... */ }

    // fast: you know its always 8 elements
    unroll!(8, |i| { let _ = data[i]; });  // 7.5x faster
}
```

common in trading (order book levels), network protocols (fixed headers), SIMD operations.

## features

- **zero dependencies** - pure declarative macros
- **no proc-macro** - fast compilation
- **const loop vars** - enables LLVM optimization
- **no_std** - embedded compatible

## supported sizes

- **0-64**: full support
- **128, 256, 512, 1024**: via recursive bisection

## usage

```rust
use land_unroll::unroll;

let mut sum = 0;
unroll!(8, |i| sum += i);
assert_eq!(sum, 28);
```

nested:
```rust
unroll!(4, |row| {
    unroll!(4, |col| {
        matrix[row][col] = row * 4 + col;
    })
});
```

## benchmarks

```
dynamic bounds (1000 reps):
  unroll!         494 total (0.49/iter) 1.00x
  for loop       3690 total (3.69/iter) 7.47x  <- SLOW

128 elements (1000 reps):
  unroll!         495 total (0.49/iter) 1.00x
  for loop        945 total (0.94/iter) 1.91x  <- SLOW

fixed 8 elements (1000 reps):
  unroll!         494 total (0.49/iter) 1.00x
  for loop        494 total (0.49/iter) 1.00x  <- TIE (no benefit)
```

run: `cargo run --release --example latency`

## when NOT to use

fixed-size arrays with compile-time bounds - LLVM already unrolls these:

```rust
let data = [0u64; 8];
// no benefit - same performance
unroll!(8, |i| sum += data[i]);
for i in 0..8 { sum += data[i]; }
```

## implementation

**inline expansion (n <= 16):**
```rust
unroll!(3, |i| f(i))
// expands to:
{ const i: usize = 0; f(i) }
{ const i: usize = 1; f(i) }
{ const i: usize = 2; f(i) }
```

**recursive bisection (n > 16):**
```rust
unroll!(64, |i| f(i))
// expands to:
unroll!(32, |i| f(i))
unroll!(32, |i| f(i + 32))
```

## recursion limit

for n > 128:
```rust
#![recursion_limit = "256"]
```

## no_std

```rust
#![no_std]
use land_unroll::unroll;
```

## license

MIT
