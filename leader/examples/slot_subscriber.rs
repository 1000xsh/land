//! direct slot subscriber example using land-pubsub.
//!
//! demonstrates slot subscription with:
//! - zero-copy SPSC channel from land-pubsub
//! - busy-spin consumer thread (pinned to CPU core)
//! - lock-free atomic reads from shared state
//!
//! architecture:
//!   land-pubsub WebSocket thread (unpinned, I/O bound)
//!      SPSC channel (zero-copy, lock-free)
//! ->
//!   consumer thread (CPU-pinned, busy-spin)
//!     atomic writes
//! ->
//!   SlotState (lock-free atomic reads)

use land_cpu::set_cpu_affinity;
use land_leader::{SlotState, SlotSubscriber};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use tokio::signal;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // init tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("land_leader=debug".parse().unwrap())
                .add_directive("land_pubsub=debug".parse().unwrap()),
        )
        .init();

    // WebSocket URLd
    let ws_url = std::env::var("WS_URL").unwrap_or_else(|_| "ws://45.152.160.253:8900".to_string());

    tracing::info!("Starting slot subscriber");
    tracing::info!("WebSocket: {}", ws_url);

    // shared slot state (lock-free atomic reads)
    let slot_state = Arc::new(SlotState::new());

    // subscriber with CPU pinning
    let mut subscriber = SlotSubscriber::new(&ws_url, Arc::clone(&slot_state), Some(6));

    // start subscriber (spawns threads: WebSocket + consumer)
    subscriber.start();

    tracing::info!("Waiting for connection...");
    if !subscriber.wait_for_connection(std::time::Duration::from_secs(10)).await {
        tracing::error!("Failed to connect within timeout");
        return Ok(());
    }

    tracing::info!("Connected! Starting reader thread...");

    // spawn reader thread on core (demonstrates multi-core lock-free reads)
    let reader_running = Arc::new(AtomicBool::new(true));
    let reader_running_clone = Arc::clone(&reader_running);
    let slot_state_clone = Arc::clone(&slot_state);

    let reader_thread = thread::Builder::new()
        .name("slot-reader".into())
        .spawn(move || {
            // pin to different core than consumer (demonstrates lock-free reads)
            if let Err(e) = set_cpu_affinity([7]) {
                tracing::warn!("Failed to pin reader to core 7: {}", e);
            } else {
                tracing::info!("Reader thread pinned to core 7");
            }

            let mut last_slot = 0u64;
            let mut update_count = 0u64;

            while reader_running_clone.load(Ordering::Acquire) {
                // atomic read
                let slot = slot_state_clone.load();
                let connected = slot_state_clone.is_connected();

                // print on slot change
                if slot != last_slot && slot > 0 {
                    update_count += 1;

                    // measure read latency
                    let start = std::time::Instant::now();
                    let current_slot = slot_state_clone.load();
                    let read_ns = start.elapsed().as_nanos();

                    tracing::info!(
                        "[{}] slot: {} | connected: {} | read_latency: {}ns",
                        update_count,
                        current_slot,
                        connected,
                        read_ns
                    );

                    last_slot = slot;
                }

                // busy-spin
                std::hint::spin_loop();
            }

            tracing::info!("Reader thread stopped (received {} updates)", update_count);
        })
        .expect("Failed to spawn reader thread");

    // wait for shutdown signal
    tracing::info!("Press Ctrl+C to stop");
    signal::ctrl_c().await?;

    tracing::info!("Shutting down...");
    reader_running.store(false, Ordering::Release);
    let _ = reader_thread.join();
    subscriber.stop();

    Ok(())
}