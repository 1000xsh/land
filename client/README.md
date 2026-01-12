# land client

send transaction client for the land rpc server.

## usage

```rust
use land_client::{LandClient, SendOptions};

// connect
let mut land = LandClient::connect("127.0.0.1:8080")?;

// send with defaults (fanout=1, target_slot=0)
let result = land.send(&tx_bytes)?;

// send with options
let opts = SendOptions::default()
    .fanout(3)
    .target_slot(current_slot);

let result = land.send_with_opts(&tx_bytes, &opts)?;
```

## examples

```bash
# tx example
cargo run -p land --example send_tx -- --keypair ./key.json --fanout 3
```

### send_tx options

```
--server        land server
--rpc           solana rpc
--keypair       keypair json file
--fanout        leader fanout (default: 1)
--recipient     recipient pubkey
--amount        lamports (default: 1000)
--priority-fee  micro-lamports/CU (default: 1000)
```
