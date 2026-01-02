use crate::config::ServerConfig;
use crate::queue::QueuedRequest;
use land_channel::spsc::Consumer;
use land_cpu::{prefetch_read, set_cpu_affinity, SpinLoopHintWait};
use land_quic::{SendOptions, TransactionSender};
use land_traits::LeaderLookup;
use log::{error, info, warn};
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Instant;

/// maximum batch size for request processing.
const BATCH_SIZE: usize = 64;

/// prefetch distance - how many requests ahead to prefetch.
/// 8 iterations ahead hides ~200 cycle memory latency.
const PREFETCH_DISTANCE: usize = 8;

/// worker thread that processes queued requests and sends via QUIC.
pub struct Worker<L: LeaderLookup + 'static> {
    /// configuration.
    config: Arc<ServerConfig>,

    /// request queue receiver.
    request_rx: Consumer<QueuedRequest, SpinLoopHintWait>,

    /// transaction sender.
    sender: TransactionSender<L>,

    /// shutdown signal.
    shutdown: Arc<AtomicBool>,
}

impl<L: LeaderLookup + Send + 'static> Worker<L> {
    /// create a new worker with QUIC sender.
    pub fn new(
        config: Arc<ServerConfig>,
        request_rx: Consumer<QueuedRequest, SpinLoopHintWait>,
        sender: TransactionSender<L>,
        shutdown: Arc<AtomicBool>,
    ) -> Self {
        Self {
            config,
            request_rx,
            sender,
            shutdown,
        }
    }

    /// start the worker thread.
    pub fn start(self) -> thread::JoinHandle<()> {
        let config = self.config.clone();
        let request_rx = self.request_rx;
        let sender = self.sender;
        let shutdown = self.shutdown.clone();

        let handle = thread::spawn(move || {
            worker_loop(config, request_rx, sender, shutdown);
        });

        info!("Worker thread started");
        handle
    }
}

/// main worker loop with batching and prefetching.
fn worker_loop<L: LeaderLookup>(
    config: Arc<ServerConfig>,
    mut request_rx: Consumer<QueuedRequest, SpinLoopHintWait>,
    mut sender: TransactionSender<L>,
    shutdown: Arc<AtomicBool>,
) {
    // set CPU affinity if configured
    if let Some(core) = config.worker_cpu_core {
        match set_cpu_affinity([core]) {
            Ok(_) => info!("Worker pinned to CPU core {}", core),
            Err(e) => warn!("Failed to pin worker to CPU core {}: {:?}", core, e),
        }
    }

    let mut processed_count = 0u64;
    let start_time = Instant::now();

    // pre-allocated batch buffer (avoids allocation in hot path)
    let mut batch: [MaybeUninit<QueuedRequest>; BATCH_SIZE] =
        unsafe { MaybeUninit::uninit().assume_init() };

    info!(
        "Worker loop started (batch_size={}, prefetch_dist={})",
        BATCH_SIZE, PREFETCH_DISTANCE
    );

    while !shutdown.load(Ordering::Acquire) {
        // poll QUIC endpoint to drive connections
        let _ = sender.poll();

        // drain as many requests as available (up to BATCH_SIZE)
        let mut count = 0;
        while count < BATCH_SIZE {
            match request_rx.try_recv() {
                Ok(req) => {
                    batch[count].write(req);
                    count += 1;
                }
                Err(_) => break,
            }
        }

        if count == 0 {
            // no requests available, yield briefly
            std::hint::spin_loop();
            continue;
        }

        // process batch with prefetching
        // prefetch transaction data for requests ahead to hide memory latency
        for i in 0..count {
            // prefetch transaction data for request [i + PREFETCH_DISTANCE]
            if i + PREFETCH_DISTANCE < count {
                unsafe {
                    let future_req = batch[i + PREFETCH_DISTANCE].assume_init_ref();
                    prefetch_read(future_req as *const QueuedRequest);
                    if !future_req.request.transaction.is_empty() {
                        prefetch_read(future_req.request.transaction.as_ptr());
                    }
                }
            }

            // process current request - send via QUIC
            let req = unsafe { batch[i].assume_init_read() };
            process_request(req, &mut sender);
        }

        processed_count += count as u64;
    }

    info!(
        "Worker loop stopped. Processed {} requests in {:?}",
        processed_count,
        start_time.elapsed()
    );
}

/// process a single request - send via QUIC.
#[inline]
fn process_request<L: LeaderLookup>(queued: QueuedRequest, sender: &mut TransactionSender<L>) {
    // validate transaction data
    if queued.request.transaction.is_empty() {
        error!("Request {}: Empty transaction", queued.request_id);
        let _ = queued
            .response_tx
            .send(Err("Empty transaction".to_string()));
        return;
    }

    // send via QUIC with fanout and optional neighbor fanout
    let fanout = queued.request.fanout as usize;
    let send_opts = SendOptions {
        tpu_port: None,
        fanout: None,
        // neighbor_fanout: queued.request.neighbor_fanout,
    };

    match sender.send_fanout(&queued.request.transaction, fanout, send_opts) {
        Ok(result) => {
            let request_id_str = format!("{:016x}", queued.request_id);
            info!(
                "process_request: sent={} early_data={} queued={} queue_full={} dead={} errors={}",
                result.sent, result.early_data, result.queued, result.queue_full, result.dead, result.errors
            );
            if let Err(e) = queued.response_tx.send(Ok(request_id_str)) {
                error!(
                    "Request {}: Failed to send response: {}",
                    queued.request_id, e
                );
            }
        }
        Err(e) => {
            error!("Request {}: QUIC send failed: {}", queued.request_id, e);
            let _ = queued.response_tx.send(Err(e.to_string()));
        }
    }
}

// todo: add integration tests with mock LeaderLookup
#[cfg(test)]
mod tests {
    // tests require a mock TransactionSender, which needs a mock LeaderLookup.
    // for now, integration tests should be done at the core level.
}
