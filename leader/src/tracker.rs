//! leader tracker - busy-spin polls slot, updates buffer.

use crate::config::Config;
use crate::leader_buffer::{LeaderBuffer, LEADER_BUFFER_SIZE, SLOTS_PER_LEADER};
use crate::leader_entry::LeaderEntry;
use crate::schedule_fetcher::ScheduleFetcher;
use crate::slot_subscriber::{SlotState, SlotSubscriber};
use land_cpu::{set_cpu_affinity};
use solana_client::nonblocking::rpc_client::RpcClient;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use tokio::task::JoinHandle;

/// leader tracker - coordinates slot updates and buffer management.
pub struct LeaderTracker {
    config: Config,
    buffer: Arc<LeaderBuffer>,
    slot_state: Arc<SlotState>,
    schedule_fetcher: Arc<ScheduleFetcher>,
    running: Arc<AtomicBool>,
    subscriber: Option<SlotSubscriber>,
    refresh_task: Option<JoinHandle<()>>,
    tracker_thread: Option<thread::JoinHandle<()>>,
}

impl LeaderTracker {
    /// create a new leader tracker.
    pub fn new(config: Config) -> Self {
        let rpc_client = Arc::new(RpcClient::new(config.rpc_url.clone()));

        Self {
            config,
            buffer: Arc::new(LeaderBuffer::new()),
            slot_state: Arc::new(SlotState::new()),
            schedule_fetcher: Arc::new(ScheduleFetcher::new(rpc_client)),
            running: Arc::new(AtomicBool::new(false)),
            subscriber: None,
            refresh_task: None,
            tracker_thread: None,
        }
    }

    /// reference to the leader buffer for consumers.
    #[inline]
    pub fn buffer(&self) -> Arc<LeaderBuffer> {
        Arc::clone(&self.buffer)
    }

    /// schedule fetcher reference.
    #[inline]
    pub fn schedule_fetcher(&self) -> Arc<ScheduleFetcher> {
        Arc::clone(&self.schedule_fetcher)
    }

    /// start the tracker: fetch schedule, connect websocket, start busy-spin loop.
    pub async fn start(&mut self) -> Result<(), TrackerError> {
        if self.running.load(Ordering::Acquire) {
            return Err(TrackerError::AlreadyRunning);
        }

        // fetch initial leader schedule.
        tracing::debug!("Fetching initial leader schedule...");
        self.schedule_fetcher.refresh().await?;

        // start slot subscriber.
        tracing::debug!("Starting WebSocket subscriber...");
        let mut subscriber = SlotSubscriber::new(
            &self.config.ws_url,
            Arc::clone(&self.slot_state),
            self.config.subscriber_cpu_core,
        );
        subscriber.start();

        // wait for websocket connection.
        if !subscriber
            .wait_for_connection(std::time::Duration::from_secs(10))
            .await
        {
            subscriber.stop();
            return Err(TrackerError::ConnectionTimeout);
        }
        tracing::debug!("WebSocket connected");

        // wait for first slot update.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while self.slot_state.load() == 0 {
            if std::time::Instant::now() >= deadline {
                subscriber.stop();
                return Err(TrackerError::NoSlotReceived);
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        }

        let initial_slot = self.slot_state.load();
        tracing::debug!("First slot received: {}", initial_slot);

        // populate initial buffer.
        self.update_buffer(initial_slot);
        tracing::debug!("Initial buffer populated");

        // start background schedule refresh task.
        self.running.store(true, Ordering::Release);
        let refresh_task = self.spawn_refresh_task();
        self.refresh_task = Some(refresh_task);

        // start busy-spin tracker thread.
        let tracker_thread = self.spawn_tracker_thread();
        self.tracker_thread = Some(tracker_thread);

        self.subscriber = Some(subscriber);

        tracing::debug!("Leader tracker started");
        Ok(())
    }

    /// spawn background task for periodic schedule refresh.
    fn spawn_refresh_task(&self) -> JoinHandle<()> {
        let schedule_fetcher = Arc::clone(&self.schedule_fetcher);
        let slot_state = Arc::clone(&self.slot_state);
        let running = Arc::clone(&self.running);
        let interval = self.config.schedule_refresh_interval;

        tokio::spawn(async move {
            let mut last_epoch = schedule_fetcher.current_epoch();

            while running.load(Ordering::Acquire) {
                tokio::time::sleep(interval).await;

                if !running.load(Ordering::Acquire) {
                    break;
                }

                let current_slot = slot_state.load();

                // check if we need to refresh (approaching epoch boundary or slot not in cache).
                if !schedule_fetcher.is_slot_in_cache(current_slot) {
                    tracing::debug!("Slot {} not in cache, refreshing schedule...", current_slot);
                    match schedule_fetcher.refresh().await {
                        Ok(()) => {
                            let new_epoch = schedule_fetcher.current_epoch();
                            if new_epoch != last_epoch {
                                tracing::debug!("Schedule refreshed for epoch {:?}", new_epoch);
                                last_epoch = new_epoch;
                            }
                        }
                        Err(e) => {
                            tracing::error!("Failed to refresh schedule: {}", e);
                        }
                    }
                }
            }
        })
    }

    /// spawn the busy-spin tracker thread.
    fn spawn_tracker_thread(&self) -> thread::JoinHandle<()> {
        let buffer = Arc::clone(&self.buffer);
        let slot_state = Arc::clone(&self.slot_state);
        let schedule_fetcher = Arc::clone(&self.schedule_fetcher);
        let running = Arc::clone(&self.running);
        let cpu_core = self.config.tracker_cpu_core;

        thread::Builder::new()
            .name("leader-tracker".into())
            .spawn(move || {
                if let Some(core) = cpu_core {
                    if let Err(e) = set_cpu_affinity([core]) {
                        tracing::warn!("Failed to pin to core {}: {}", core, e);
                    } else {
                        tracing::debug!("Tracker thread pinned to CPU core {}", core);
                    }
                }

                Self::busy_spin_loop(buffer, slot_state, schedule_fetcher, running);
            })
            .expect("Failed to spawn tracker thread")
    }

    /// the hot path: busy-spin loop checking for slot changes.
    /// hybrid: spin N times without pause, then pause.
    fn busy_spin_loop(
        buffer: Arc<LeaderBuffer>,
        slot_state: Arc<SlotState>,
        schedule_fetcher: Arc<ScheduleFetcher>,
        running: Arc<AtomicBool>,
    ) {
        let mut last_slot = 0u64;
        let mut last_leader_idx = 0u64;

        // wait for valid slot.
        while running.load(Ordering::Acquire) {
            let slot = slot_state.load();
            if slot > 0 {
                last_slot = slot;
                last_leader_idx = slot / SLOTS_PER_LEADER;
                break;
            }
            // cpu_pause();
        }

        // main loop: detect slot and leader changes.
        while running.load(Ordering::Acquire) {
            let current_slot = slot_state.load();

            // only act when slot actually changed.
            if current_slot != last_slot {
                let current_leader_idx = current_slot / SLOTS_PER_LEADER;

                if current_leader_idx != last_leader_idx {
                    // leader changed - fifo shift buffer.
                    let leaders_passed =
                        current_leader_idx.saturating_sub(last_leader_idx) as usize;

                    if leaders_passed >= LEADER_BUFFER_SIZE {
                        // too many missed - reinitialize full buffer.
                        Self::update_buffer_from_schedule(&buffer, &schedule_fetcher, current_slot);
                    } else {
                        // fifo: shift and append new leaders.
                        Self::fifo_shift_buffer(
                            &buffer,
                            &schedule_fetcher,
                            current_slot,
                            leaders_passed,
                        );
                    }
                    last_leader_idx = current_leader_idx;
                } else {
                    // same leader, just update slot.
                    buffer.set_current_slot(current_slot);
                }

                last_slot = current_slot;
            }

            // pause: signals spin-wait to CPU, reduces speculation flush penalty.
            // cpu_pause();
        }
    }

    /// shift buffer left and append new leaders.
    fn fifo_shift_buffer(
        buffer: &LeaderBuffer,
        schedule_fetcher: &ScheduleFetcher,
        current_slot: u64,
        leaders_passed: usize,
    ) {
        // fixed-size array on stack - no heap allocation.
        let mut new_entries = [LeaderEntry::EMPTY; LEADER_BUFFER_SIZE];

        // get the new leaders to append.
        let lookahead_slot = (current_slot / SLOTS_PER_LEADER
            + (LEADER_BUFFER_SIZE - leaders_passed) as u64)
            * SLOTS_PER_LEADER;

        // write directly to stack buffer.
        let count = leaders_passed.min(LEADER_BUFFER_SIZE);
        schedule_fetcher.get_next_leaders_into(lookahead_slot, &mut new_entries[..count]);

        buffer.shift_multiple(leaders_passed, &new_entries[..count], current_slot);
    }

    /// update buffer with current + next 9 leaders.
    fn update_buffer(&self, current_slot: u64) {
        Self::update_buffer_from_schedule(&self.buffer, &self.schedule_fetcher, current_slot);
    }

    /// update buffer with current + next 9 leaders.
    fn update_buffer_from_schedule(
        buffer: &LeaderBuffer,
        schedule_fetcher: &ScheduleFetcher,
        current_slot: u64,
    ) {
        // fixed-size array on stack - no heap allocation.
        let mut entries = [LeaderEntry::EMPTY; LEADER_BUFFER_SIZE];

        // write directly to stack buffer.
        schedule_fetcher.get_next_leaders_into(current_slot, &mut entries);

        buffer.update(current_slot, &entries);
    }

    /// stop the tracker.
    pub async fn stop(&mut self) {
        self.running.store(false, Ordering::Release);

        if let Some(mut subscriber) = self.subscriber.take() {
            subscriber.stop();
        }

        if let Some(task) = self.refresh_task.take() {
            task.abort();
        }

        if let Some(thread) = self.tracker_thread.take() {
            let _ = thread.join();
        }

        tracing::debug!("Leader tracker stopped");
    }

    /// check if running.
    #[inline]
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TrackerError {
    #[error("Schedule fetch error: {0}")]
    Fetch(#[from] crate::schedule_fetcher::FetcherError),
    #[error("WebSocket connection timeout")]
    ConnectionTimeout,
    #[error("No slot received within timeout")]
    NoSlotReceived,
    #[error("Tracker already running")]
    AlreadyRunning,
    #[error("Internal error: {0}")]
    Internal(&'static str),
}
