````md
# land channel

**land channel** is a low-latency, lock-free inter-thread communication library for rust. it uses pre-allocated ring buffers, sequence-based coordination, and configurable wait strategies to achieve predictable, minimal-latency message passing.

## overview

- **lock-free** (atomics + fences, no mutexes)
- **pre-allocated ring buffer** (no hot-path allocation)
- **zero-copy claim/publish** for in-place mutation
- **batch operations** to amortize synchronization
- **spsc + mpsc** channel types
- **wait strategies** to tune latency vs cpu

## quick start

### spsc (single producer, single consumer)

```rust
use land_channel::spsc;

let (mut tx, mut rx) = spsc::channel::<u64>(1024);

tx.send(42).unwrap();
assert_eq!(rx.recv().unwrap(), 42);
```

### mpsc (multi producer, single consumer)

```rust
use land_channel::mpsc;

let (tx, mut rx) = mpsc::channel::<u64>(1024);
let tx2 = tx.clone();

tx.send(1).unwrap();
tx2.send(2).unwrap();

let _ = rx.recv().unwrap();
```

## zero-copy (claim / publish)

```rust
use land_channel::{spsc, SpinLoopHintWait};

let (mut tx, mut rx) = spsc::channel_with_factory(
    1024,
    || Vec::with_capacity(4096),
    SpinLoopHintWait,
);

{
    let slot = tx.claim().unwrap();
    slot.clear();
    slot.extend_from_slice(b"hello");
}
tx.publish();

let msg = rx.recv().unwrap();
assert_eq!(&msg[..], b"hello");
```

## notes

* `capacity` **must be a power of 2** (e.g. 256, 1024, 4096).
* prefer **spsc** for absolute lowest latency; use **mpsc** when you need multiple producers.
* use **batching** (`claim_batch` / `recv_batch` / `poll`) for throughput-critical paths.

```
```
