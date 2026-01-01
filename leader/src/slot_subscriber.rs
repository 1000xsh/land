//! websocket slot subscription - writes to shared atomic.
//! commitment: processed

use land_cpu::set_cpu_affinity;
use land_pubsub::{ConnectionState, SlotInfo, SlotSubscriber as PubsubSlotSubscriber};
use land_channel::spsc::Consumer;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;

/// shared slot state
pub struct SlotState {
    pub slot: AtomicU64,
    pub connected: AtomicBool,
}

impl SlotState {
    pub fn new() -> Self {
        Self {
            slot: AtomicU64::new(0),
            connected: AtomicBool::new(false),
        }
    }

    #[inline]
    pub fn load(&self) -> u64 {
        self.slot.load(Ordering::Acquire)
    }

    #[inline]
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }
}

impl Default for SlotState {
    fn default() -> Self {
        Self::new()
    }
}

/// websocket slot subscriber - writes directly to shared atomic.
pub struct SlotSubscriber {
    state: Arc<SlotState>,
    running: Arc<AtomicBool>,
    cpu_core: Option<usize>,
    pubsub_subscriber: Option<PubsubSlotSubscriber>,
    consumer: Option<Consumer<SlotInfo>>,
    thread_handle: Option<thread::JoinHandle<()>>,
}

impl SlotSubscriber {
    pub fn new(ws_url: impl Into<String>, state: Arc<SlotState>, cpu_core: Option<usize>) -> Self {
        let (pubsub_subscriber, consumer) = PubsubSlotSubscriber::new(ws_url, 64);

        Self {
            state,
            running: Arc::new(AtomicBool::new(false)),
            cpu_core,
            pubsub_subscriber: Some(pubsub_subscriber),
            consumer: Some(consumer),
            thread_handle: None,
        }
    }

    /// start the websocket subscription on a dedicated thread.
    pub fn start(&mut self) {
        if self.running.load(Ordering::Acquire) {
            return;
        }

        let pubsub = self.pubsub_subscriber.as_mut().expect("Pubsub subscriber missing");

        // start the pubsub websocket thread
        if let Err(e) = pubsub.start() {
            tracing::error!("Failed to start pubsub subscriber: {}", e);
            return;
        }

        // get connection state for monitoring
        let pubsub_state = pubsub.connection_state();

        let state = Arc::clone(&self.state);
        let running = Arc::clone(&self.running);
        let cpu_core = self.cpu_core;
        let consumer = self.consumer.take().expect("Consumer already consumed");

        running.store(true, Ordering::Release);

        let handle = thread::Builder::new()
            .name("slot-subscriber".into())
            .spawn(move || {
                // pin to CPU core if configured.
                if let Some(core) = cpu_core {
                    if let Err(e) = set_cpu_affinity([core]) {
                        tracing::warn!("Failed to pin subscriber to core {}: {}", core, e);
                    } else {
                        tracing::debug!("Subscriber thread pinned to CPU core {}", core);
                    }
                }

                Self::run_loop(state, running, consumer, pubsub_state);
            })
            .expect("Failed to spawn subscriber thread");

        self.thread_handle = Some(handle);
    }

    fn run_loop(
        state: Arc<SlotState>,
        running: Arc<AtomicBool>,
        mut consumer: Consumer<SlotInfo>,
        pubsub_state: Arc<ConnectionState>,
    ) {
        tracing::debug!("Slot subscriber loop started");

        loop {
            if !running.load(Ordering::Acquire) {
                break;
            }

            // poll consumer for slot updates
            let count = consumer.poll(|info, _seq, _eob| {
                // write slot to shared atomic
                state.slot.store(info.slot, Ordering::Release);
            });

            // update connection status (atomic read)
            let connected = pubsub_state.is_connected();
            state.connected.store(connected, Ordering::Release);

            // busy-spin when idle
            if count == 0 {
                std::hint::spin_loop();
            }
        }

        tracing::debug!("Slot subscriber loop stopped");
    }

    pub fn stop(&mut self) {
        self.running.store(false, Ordering::Release);

        // stop the pubsub subscriber
        if let Some(pubsub) = self.pubsub_subscriber.as_mut() {
            pubsub.stop();
        }

        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }

    /// wait for websocket connection with timeout.
    pub async fn wait_for_connection(&self, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if self.state.is_connected() {
                return true;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        }
        false
    }
}

impl Drop for SlotSubscriber {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
    }
}
