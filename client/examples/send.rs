//! example: send transaction using land client.
//!
//! usage:
//!   cargo run -p land_client --example send

use land_client::{LandClient, SendOptions};

fn main() {
    // connect to land rpc server
    let mut land = match LandClient::connect("127.0.0.1:8080") {
        Ok(c) => c,
        Err(e) => {
            eprintln!("connect failed: {}", e);
            return;
        }
    };

    println!("connected to land server");

    // example transaction bytes (replace with your signed tx)
    let tx_bytes: Vec<u8> = vec![
        1, 0, 1, 3, // header
          // ... your serialized transaction
    ];

    // simple send with defaults (fanout=1, target_slot=0)
    match land.send(&tx_bytes) {
        Ok(result) => {
            println!("queued: request_id={}", result.request_id);
        }
        Err(e) => {
            eprintln!("send failed: {}", e);
        }
    }

    // send with custom options
    let opts = SendOptions::default().fanout(3).target_slot(12345678);

    match land.send_with_opts(&tx_bytes, &opts) {
        Ok(result) => {
            println!("queued with fanout: request_id={}", result.request_id);
        }
        Err(e) => {
            eprintln!("send failed: {}", e);
        }
    }

    // send with all options
    let opts = SendOptions::default().fanout(5).target_slot(12345678);

    match land.send_with_opts(&tx_bytes, &opts) {
        Ok(result) => {
            println!("queued with neighbors: request_id={}", result.request_id);
        }
        Err(e) => {
            eprintln!("send failed: {}", e);
        }
    }
}
