//! leader tracker example.

use land_cpu::set_cpu_affinity;
use land_leader::{Config, LeaderTracker};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tokio::signal;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // init.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("land_leader=info".parse().unwrap()),
        )
        .init();

    // configuration - customize as needed.
    let config = Config::mainnet().with_refresh_interval(Duration::from_secs(600));

    tracing::info!("Starting leader tracker");
    tracing::info!("RPC: {}", config.rpc_url);
    tracing::info!("WS: {}", config.ws_url);

    let mut tracker = LeaderTracker::new(config);

    // start the tracker.
    tracker.start().await?;

    // get buffer handle for reading.
    let buffer = tracker.buffer();

    // spawn consumer thread pinned to core 4.
    let consumer_running = Arc::new(AtomicBool::new(true));
    let consumer_running_clone = Arc::clone(&consumer_running);
    let buffer_clone = Arc::clone(&buffer);

    let consumer_thread = thread::Builder::new()
        .name("consumer".into())
        .spawn(move || {
            // pin to core 4.
            if let Err(e) = set_cpu_affinity([7]) {
                tracing::warn!("Failed to pin consumer to core 4: {}", e);
            } else {
                tracing::info!("Consumer thread pinned to core 4");
            }

            let mut last_slot = 0u64;

            while consumer_running_clone.load(Ordering::Acquire) {
                let slot = buffer_clone.current_slot();

                // only print when slot changes.
                if slot != last_slot && slot > 0 {
                    // measure read latency (single atomic read of all entries).
                    // slot info is embedded in each entry.
                    let start = std::time::Instant::now();
                    let entries = buffer_clone.read_all();
                    let read_ns = start.elapsed().as_nanos();

                    tracing::info!("# slot {} | read_latency={}ns", slot, read_ns);

                    for (i, entry) in entries.iter().enumerate() {
                        if entry.is_valid() {
                            tracing::info!(
                                "[{}] slots {}-{} | {} | {} | {}",
                                i,
                                entry.start_slot(),
                                entry.end_slot(),
                                entry.pubkey,
                                entry.tpu_quic(),
                                entry.tpu_quic_fwd()
                            );
                        } else {
                            tracing::info!(
                                "[{}] slots {}-{} | <no TPU>",
                                i,
                                entry.start_slot(),
                                entry.end_slot()
                            );
                        }
                    }

                    last_slot = slot;
                }

                // busy-spin with pause hint.
                //cpu_pause();
            }
        })
        .expect("Failed to spawn consumer thread");

    // wait for shutdown signal.
    tracing::info!("Press Ctrl+C to stop");
    signal::ctrl_c().await?;

    tracing::info!("Shutting down...");
    consumer_running.store(false, Ordering::Release);
    let _ = consumer_thread.join();
    tracker.stop().await;

    Ok(())
}
