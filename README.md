# land.fast - solana tpu transaction service

sends transactions directly to solana leader validators via QUIC, minimizing network hops and latency.

### WIP - do not use in production
this repo is under active development and may include permanent breaking changes.

## features

- **lock-free data structures** - seqlock-based leader buffer for wait-free reads
- **disruptor-inspired channels** - zero-copy spsc/mpsc channels with pre-allocated ring buffers
- **zero-copy transport** - efficient quic with minimal allocations
- **session resumption** - tls session cache for 0-rtt connections
- **rdtsc timing** - nanosecond-precision measurement for connection handshake tracking
- **json-rpc http api** - lock-free request queue with mio-based accept thread

### core components

- **land-core** - thin orchestration layer
- **land-leader** - solana leader tracker with seqlock buffer and busy-spin update loop
- **land-quic** - quic connection library with adaptive timing, connection pooling, session cache
- **land-server** - lock-free json-rpc http server using mio + spsc queue

### supporting crates

- **land-channel** - lock-free spsc/mpsc channels inspired by lmax disruptor with pre-allocated ring buffers
- **land-sync** - seqlock and lock-free synchronization primitives for single-writer multi-reader scenarios
- **land-timing** - rdtsc-based timing infrastructure with histogram tracking for nanosecond-level measurements
- **land-pubsub** - low-latency websocket slot subscriber using land-channel for event-driven updates
- **land-cpu** - cpu topology detection, core pinning utilities, cache padding, wait strategies, memory fences
- **land-traits** - shared traits (LeaderLookup for zero-copy leader buffer reads)

## setup

### prerequisites

install openssl with quic support:

```bash
chmod +x setup.sh
./setup.sh
./set.sh
```

### build

```bash
cd core && cargo b -r
```

## usage

### basic

```bash
./target/release/land-core \
  --rpc http://your-rpc-endpoint:8899 \
  --ws ws://your-ws-endpoint:8900 \
  --http 127.0.0.1:8080
```

### with staked identity (priority access)

```bash
./target/release/land-core \
  --rpc http://your-rpc-endpoint:8899 \
  --ws ws://your-ws-endpoint:8900 \
  --http 127.0.0.1:8080 \
  --keypair /path/to/staked-keypair.json
```

### environment variables

alternatively, use environment variables:

```bash
export RPC_URL=http://your-rpc-endpoint:8899
export WS_URL=ws://your-ws-endpoint:8900
export HTTP_ADDR=127.0.0.1:8080
export KEYPAIR_PATH=/path/to/staked-keypair.json

./target/release/land-core
```

### presets

```rust
Config::mainnet()        // default mainnet configuration
Config::devnet()         // devnet configuration
Config::custom(rpc, ws)  // custom endpoints
```

## api

send transactions via json-rpc:

```bash
curl -X POST http://127.0.0.1:8080 \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "id": 1,
    "method": "sendTransaction",
    "params": {
      "transaction": "<base64-encoded-transaction>",
      "fanout": 3,
    }
  }'
```

### response

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "status": "queued",
    "request_id": "000000000000001a"
  }
}
```

## design principles

- **zero-allocation** - pre-allocated buffers
- **lock-free** - seqlock for reads, disruptor-style channels
- **cache-aware** - 64-byte alignment, padding to prevent false sharing, hot fields grouped
- **wait strategies** - configurable spin/yield/backoff for different latency/cpu trade-offs
- mixed strategies need a fix to bring more consistency

## todo
- improve stability — the focus has mostly been on latency, not safety
- batching server and quic sender
- remove websocket support (agave will no longer maintain rpc/ws). switch to either grpc or gossip shreds
- add blacklist for slot bandits
- add adaptive timing based on ip perf
- add target_slot
- replace some crates

## license

MIT



