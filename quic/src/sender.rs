//! transaction sender with connection strategies

use crate::config::Config;
use crate::endpoint::QuicEndpoint;
use crate::error::{Error, Result};
use crate::session_cache::SessionTokenCache;
use crate::strategy::{ConnectStrategy, InlinePrewarmConfig, TpuPort};
use land_traits::LeaderLookup;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

pub use land_traits::LeaderLookup as LeaderSource;

/// per-request send options
#[derive(Debug, Clone, Default)]
pub struct SendOptions {
    /// override default TPU port selection
    pub tpu_port: Option<TpuPort>,
    /// override fanout count (for JIT fanout mode)
    pub fanout: Option<usize>,
    // /// send to N neighbors per leader (0 = disabled, max 8)
    // /// neighbors are sent to asynchronously
    // pub neighbor_fanout: Option<usize>,
}

/// result of a fanout send operation
#[derive(Debug, Default, Clone, Copy)]
pub struct SendResult {
    /// successfully sent on established connections
    pub sent: usize,
    /// sent via 0-RTT early data
    pub early_data: usize,
    /// queued during handshake (will be sent when connection establishes)
    pub queued: usize,
    /// queue full (transaction dropped)
    pub queue_full: usize,
    /// dead connections (max retries exceeded)
    pub dead: usize,
    /// other errors
    pub errors: usize,
    // /// sent to neighbors (geographic redundancy)
    // pub neighbor_sent: usize,
}

/// transaction sender configuration
#[derive(Debug, Clone)]
pub struct SenderConfig {
    /// connection strategy (JIT, prewarm, or inlineprewarm)
    pub strategy: ConnectStrategy,
    /// default TPU port to use
    pub default_tpu_port: TpuPort,
    /// QUIC configuration
    pub quic_config: Config,
}

impl Default for SenderConfig {
    fn default() -> Self {
        Self {
            strategy: ConnectStrategy::default(),
            default_tpu_port: TpuPort::default(),
            quic_config: Config::default(),
        }
    }
}

/// transaction sender
pub struct TransactionSender<L: LeaderLookup> {
    config: SenderConfig,
    endpoint: QuicEndpoint,
    leaders: L,
    /// inline prewarm config (if using inline strategy)
    inline_prewarm_config: Option<InlinePrewarmConfig>,
    /// background prewarm thread handle (if using background strategy)
    prewarm_handle: Option<JoinHandle<()>>,
    prewarm_shutdown: Option<Arc<AtomicBool>>,
}

impl<L: LeaderLookup> TransactionSender<L> {
    /// send transaction to current leader (index 0)
    #[inline]
    pub fn send(&mut self, tx: &[u8], opts: SendOptions) -> Result<SendResult> {
        use crate::error::SendStatus;

        // poll before sending (same reason as send_fanout. fixme)
        self.endpoint.poll()?;

        let tpu_port = opts.tpu_port.unwrap_or(self.config.default_tpu_port);
        let addrs = self.get_leader_addrs(0, tpu_port)?;

        if let Some((start, end)) = self.leaders.get_leader_slots(0) {
            log::info!(
                "--------------------------sender: send to leader[0] slots {}-{}: {:?}",
                start,
                end,
                addrs
            );
        } else {
            log::info!(
                "--------------------------sender: send to leader[0]: {:?}",
                addrs
            );
        }

        let mut result = SendResult::default();

        for addr in addrs {
            match self.endpoint.send(addr, tx) {
                Ok(SendStatus::Sent) => result.sent += 1,
                Ok(SendStatus::SentEarlyData) => result.early_data += 1,
                Ok(SendStatus::Queued { .. }) => result.queued += 1,
                Ok(SendStatus::QueueFull) => result.queue_full += 1,
                Ok(SendStatus::Dead { .. }) => result.dead += 1,
                Err(e) => {
                    log::warn!("send: error sending to {}: {}", addr, e);
                    result.errors += 1;
                }
            }
        }

        Ok(result)
    }

    /// send transaction to multiple leaders (fanout)
    #[inline]
    pub fn send_fanout(
        &mut self,
        tx: &[u8],
        count: usize,
        opts: SendOptions,
    ) -> Result<SendResult> {
        use crate::error::SendStatus;

        // poll endpoint to process incoming packets (ACKs, flow control updates) before sending.
        // this ensures connections have fresh state with updated congestion windows.
        // critical after connections have been idle - prevents WouldBlock on send.
        self.endpoint.poll()?;

        let tpu_port = opts.tpu_port.unwrap_or(self.config.default_tpu_port);

        // get current slot for logging
        let current_slot = self.leaders.get_leader_slots(0).map(|(start, _)| start);

        let mut result = SendResult::default();
        let mut dead_count = 0; // track dead (not retryable) connections

        // let now_ns = crate::monotonic_nanos(); // single timestamp for all checks

        // primary fanout: indices 0-9
        let primary_fanout = count.min(10);

        for i in 0..primary_fanout {
            // poll before *each* fanout iteration (except first) to catch errors from previous sends
            // this ensures we detect APPLICATION_CLOSE err=2 (Disallowed) immediately after each fanout
            // and can skip to next fanout instead of continuing to rate-limited validators.
            // todo:  "frm APPLICATION_CLOSE err=1 reason=[64, 72, 6f, 70, 70, 65, 64]" - dropped
            if i > 0 {
                self.endpoint.poll()?;
            }

            if !self.leaders.is_valid(i) {
                log::debug!("sender: fanout index {} not valid, stopping fanout", i);
                break;
            }

            let addrs = match self.get_leader_addrs(i, tpu_port) {
                Ok(addrs) => addrs,
                Err(e) => {
                    log::warn!("sender: failed to get leader addrs for index {}: {}", i, e);
                    continue; // skip this leader but continue with others
                }
            };

            // track if this fanout index should be skipped due to Disallowed error
            let mut skip_to_next_fanout = false;

            for addr in addrs {
                if let Some((start, end)) = self.leaders.get_leader_slots(i) {
                    if let Some(current) = current_slot {
                        log::info!("-------------sender: send to fanout[{}] current_slot={} leader_slots={}-{} address: {}", i, current, start, end, addr);
                    } else {
                        log::info!(
                            "-------------sender: send to fanout[{}] slots {}-{} address: {}",
                            i,
                            start,
                            end,
                            addr
                        );
                    }
                } else {
                    log::info!(
                        "-------------sender: send to fanout[{}] address: {}",
                        i,
                        addr
                    );
                }

                // event-based send with status tracking
                match self.endpoint.send(addr, tx) {
                    Ok(SendStatus::Sent) => {
                        // reset retry counter on successful send
                        if let Some(slot_idx) = self.endpoint.pool().find(addr) {
                            self.endpoint.pool().reset_retry(slot_idx);
                        }
                        result.sent += 1;
                    }
                    Ok(SendStatus::SentEarlyData) => {
                        if let Some(slot_idx) = self.endpoint.pool().find(addr) {
                            self.endpoint.pool().reset_retry(slot_idx);
                        }
                        result.early_data += 1;
                    }
                    Ok(SendStatus::Queued { .. }) => result.queued += 1,
                    Ok(SendStatus::QueueFull) => result.queue_full += 1,

                    // Dead status indicates connection received APPLICATION_CLOSE error code 2 (Disallowed)
                    // this typically means rate limiting by the validator.
                    // skip immediately to next fanout index to try another validator.
                    //
                    // alternative strategy (not implemented):
                    // - if using staked identity (forward TPU) and get Disallowed, could fallback to main TPU
                    // - if main TPU works, continue using main TPU for this validator
                    // - then try next fanout index at forward TPU (staked) again
                    // this would provide fallback when forward TPU is rate-limited but main TPU still works.
                    Ok(SendStatus::Dead { .. }) => {
                        result.dead += 1;
                        dead_count += 1;
                        log::warn!(
                            "sender: fanout[{}] address {} returned Dead (Disallowed/rate-limited), skipping to fanout[{}]",
                            i, addr, i + 1
                        );
                        skip_to_next_fanout = true;
                        //break;  // skip remaining addresses at this fanout index (typically only 1 address anyway)
                        // testing
                        continue;
                    }

                    Err(e) => {
                        log::warn!("sender: failed to send to {}: {}", addr, e);
                        result.errors += 1;
                    }
                }
            }

            // if we encountered a Dead/Disallowed connection, skip to next fanout index
            if skip_to_next_fanout {
                log::info!(
                    "sender: skipping fanout[{}] due to Disallowed error, continuing to fanout[{}]",
                    i,
                    i + 1
                );
                continue; // continue to next fanout index
            }
        }

        // fallback: all primary connections dead -> try backup leaders 10-13
        // fix me
        if dead_count >= primary_fanout && primary_fanout == 10 {
            log::warn!(
                "sender: all {} primary fanout connections dead, falling back to backup leaders 10-13",
                primary_fanout
            );

            for i in 10..14 {
                if !self.leaders.is_valid(i) {
                    log::debug!("sender: backup leader index {} not valid", i);
                    break;
                }

                let addrs = match self.get_leader_addrs(i, tpu_port) {
                    Ok(addrs) => addrs,
                    Err(e) => {
                        log::warn!(
                            "sender: failed to get backup leader addrs for index {}: {}",
                            i,
                            e
                        );
                        continue;
                    }
                };

                for addr in addrs {
                    if let Some((start, end)) = self.leaders.get_leader_slots(i) {
                        log::info!(
                            "sender: FALLBACK send to backup leader[{}] slots {}-{} address: {}",
                            i,
                            start,
                            end,
                            addr
                        );
                    }

                    match self.endpoint.send(addr, tx) {
                        Ok(SendStatus::Sent) => {
                            if let Some(slot_idx) = self.endpoint.pool().find(addr) {
                                self.endpoint.pool().reset_retry(slot_idx);
                            }
                            result.sent += 1;
                            log::info!("sender: successfully sent to backup leader[{}]", i);
                        }
                        Ok(SendStatus::SentEarlyData) => {
                            if let Some(slot_idx) = self.endpoint.pool().find(addr) {
                                self.endpoint.pool().reset_retry(slot_idx);
                            }
                            result.early_data += 1;
                        }
                        Ok(SendStatus::Queued { .. }) => result.queued += 1,
                        Ok(SendStatus::QueueFull) => result.queue_full += 1,
                        Ok(SendStatus::Dead { .. }) => {
                            result.dead += 1;
                            log::warn!("sender: backup leader[{}] is also dead", i);
                        }
                        Err(e) => {
                            log::warn!("sender: failed to send to backup leader[{}]: {}", i, e);
                            result.errors += 1;
                        }
                    }
                }
            }
        }

        Ok(result)
    }

    /// get leader addresses based on TPU port selection
    #[inline]
    fn get_leader_addrs(&self, index: usize, tpu_port: TpuPort) -> Result<Vec<SocketAddr>> {
        let (main, fwd) = self.leaders.get_leader(index).ok_or(Error::NotFound)?;

        Ok(match tpu_port {
            TpuPort::Main => vec![main],
            TpuPort::Forward => vec![fwd],
            TpuPort::Both => vec![main, fwd],
        })
    }

    /// poll endpoint (drive connections)
    ///
    /// for InlinePrewarm strategy, this also automatically prewarms upcoming leaders
    /// and keeps them alive to prevent idle timeouts.
    #[inline]
    pub fn poll(&mut self) -> Result<()> {
        // first poll the endpoint to process incoming packets
        self.endpoint.poll()?;

        // auto-prewarm if using inline strategy
        if let Some(config) = &self.inline_prewarm_config {
            if config.adaptive.is_some() {
                // use adaptive prewarming with solana slot-aware timing
                self.prewarm_adaptive();
            } else {
                // use legacy static prewarming
                self.prewarm();
            }

            // keep lookahead connections alive (prevent 2s idle timeout)
            // drive connections if idle > 500ms to maintain fresh flow control state
            self.keep_alive_lookahead()?;
        }

        Ok(())
    }

    /// keep lookahead connections alive by driving idle connections
    ///
    /// solana validators have a 2-second idle timeout. we drive connections
    /// that have been idle > 500ms to keep them alive and maintain fresh
    /// flow control state. this prevents WouldBlock errors on send.
    #[inline]
    fn keep_alive_lookahead(&mut self) -> Result<()> {
        let lookahead = self
            .inline_prewarm_config
            .as_ref()
            .map(|c| c.lookahead)
            .unwrap_or(10);

        let tpu_port = self.config.default_tpu_port;

        // keep alive threshold: 500ms (well under 2s server timeout)
        const KEEP_ALIVE_THRESHOLD_NS: u64 = 1_000_000_000; // 500ms in nanoseconds = 500_000_000

        for i in 0..lookahead {
            if !self.leaders.is_valid(i) {
                continue;
            }

            if let Some((main, fwd)) = self.leaders.get_leader(i) {
                let addrs = match tpu_port {
                    crate::strategy::TpuPort::Main => vec![main],
                    crate::strategy::TpuPort::Forward => vec![fwd],
                    crate::strategy::TpuPort::Both => vec![main, fwd],
                };

                for addr in addrs {
                    // keep alive if idle > threshold
                    let _ = self
                        .endpoint
                        .keep_alive_if_idle(addr, KEEP_ALIVE_THRESHOLD_NS);
                }
            }
        }

        Ok(())
    }

    #[inline]
    pub fn local_addr(&self) -> SocketAddr {
        self.endpoint.local_addr()
    }

    /// endpoint reference
    #[inline]
    pub fn endpoint(&self) -> &QuicEndpoint {
        &self.endpoint
    }

    /// mutable endpoint reference
    #[inline]
    pub fn endpoint_mut(&mut self) -> &mut QuicEndpoint {
        &mut self.endpoint
    }

    /// inline prewarm lookahead (if using inline strategy)
    #[inline]
    pub fn inline_lookahead(&self) -> Option<usize> {
        self.inline_prewarm_config.as_ref().map(|c| c.lookahead)
    }

    // inline prewarm api

    /// prewarm connections to upcoming leaders (non-blocking, inline)
    ///
    /// call this in your hot loop to maintain warm connections.
    /// uses the configured lookahead from InlinePrewarmConfig.
    #[inline]
    pub fn prewarm(&mut self) {
        let lookahead = self
            .inline_prewarm_config
            .as_ref()
            .map(|c| c.lookahead)
            .unwrap_or(5);

        let tpu_port = self.config.default_tpu_port;
        let addrs = self.get_prewarm_addrs(lookahead, tpu_port);

        // if !addrs.is_empty() {
        //     log::trace!("prewarm: {} addresses (lookahead={})", addrs.len(), lookahead);
        // }

        self.endpoint.prewarm(addrs);
    }

    /// adaptive prewarm with solana slot-aware timing
    ///
    /// dynamically determines connection initiation timing based on:
    /// - current solana slot
    /// - measured per-IP handshake latency
    /// - calculated adaptive lookahead slots
    /// - validator slot history
    ///
    /// connects N solana slots early, where N = ceil(handshake_ms / 400ms).
    #[inline]
    pub fn prewarm_adaptive(&mut self) {
        let tpu_port = self.config.default_tpu_port;

        // get current solana slot from leader[0]
        let current_solana_slot = self
            .leaders
            .get_leader_slots(0)
            .map(|(start, _)| start)
            .unwrap_or(0);

        if current_solana_slot == 0 {
            // no solana slot information available yet
            return;
        }

        // check upcoming leaders
        let max_lookahead = self
            .inline_prewarm_config
            .as_ref()
            .map(|c| c.lookahead)
            .unwrap_or(10);

        for i in 0..max_lookahead {
            if !self.leaders.is_valid(i) {
                continue;
            }

            // get leader addresses and solana slot range
            let (main, fwd) = match self.leaders.get_leader(i) {
                Some(addrs) => addrs,
                None => continue,
            };

            let (leader_start_solana_slot, _leader_end_solana_slot) =
                match self.leaders.get_leader_slots(i) {
                    Some(slots) => slots,
                    None => continue,
                };

            // select address based on TPU port
            let addr = match tpu_port {
                crate::strategy::TpuPort::Main => main,
                crate::strategy::TpuPort::Forward => fwd,
                crate::strategy::TpuPort::Both => main, // prewarm main first
            };

            // calculate adaptive lookahead solana slots for this specific IP
            let lookahead_solana_slots = self.endpoint.calculate_lookahead_solana_slots(addr);

            // determine connection initiation solana slot
            let connect_at_solana_slot =
                leader_start_solana_slot.saturating_sub(lookahead_solana_slots as u64);

            // should we init connection now?
            if current_solana_slot >= connect_at_solana_slot
                && current_solana_slot < leader_start_solana_slot
            {
                // check if already connected
                if self.endpoint.connection_state(addr).is_none() {
                    log::info!(
                        "Adaptive prewarm: initiating connection to {} (leader[{}] start_solana_slot={}, current_solana_slot={}, lookahead_solana_slots={})",
                        addr,
                        i,
                        leader_start_solana_slot,
                        current_solana_slot,
                        lookahead_solana_slots
                    );

                    // init non-blocking connection
                    let _ = self.endpoint.connect(addr, false);
                }
            }
        }
    }

    /// prewarm connections with custom lookahead
    #[inline]
    pub fn prewarm_with_lookahead(&mut self, lookahead: usize) {
        let tpu_port = self.config.default_tpu_port;
        let addrs = self.get_prewarm_addrs(lookahead, tpu_port);
        self.endpoint.prewarm(addrs);
    }

    /// get addresses for prewarming
    fn get_prewarm_addrs(&self, lookahead: usize, tpu_port: TpuPort) -> Vec<SocketAddr> {
        let mut addrs = Vec::with_capacity(lookahead * 2);

        for i in 0..lookahead {
            if !self.leaders.is_valid(i) {
                continue;
            }

            if let Some((main, fwd)) = self.leaders.get_leader(i) {
                match tpu_port {
                    TpuPort::Main => addrs.push(main),
                    TpuPort::Forward => addrs.push(fwd),
                    TpuPort::Both => {
                        addrs.push(main);
                        addrs.push(fwd);
                    }
                }
            }
        }

        addrs
    }

    /// send: never blocks, returns immediately if not ready
    ///
    /// this is the fastest send path for hot loops. use this with inline prewarm.
    #[inline(always)]
    pub fn send_nonblocking(&mut self, tx: &[u8], opts: SendOptions) -> Result<()> {
        let tpu_port = opts.tpu_port.unwrap_or(self.config.default_tpu_port);
        let addrs = self.get_leader_addrs(0, tpu_port)?;

        for addr in addrs {
            self.endpoint.send_nonblocking(addr, tx)?;
        }

        Ok(())
    }

    /// fanout send: never blocks
    #[inline]
    pub fn send_nonblocking_fanout(
        &mut self,
        tx: &[u8],
        count: usize,
        opts: SendOptions,
    ) -> Result<()> {
        let tpu_port = opts.tpu_port.unwrap_or(self.config.default_tpu_port);

        for i in 0..count {
            if !self.leaders.is_valid(i) {
                break;
            }

            if let Ok(addrs) = self.get_leader_addrs(i, tpu_port) {
                for addr in addrs {
                    // best effort - log errors but continue
                    if let Err(e) = self.endpoint.send_nonblocking(addr, tx) {
                        if !matches!(e, Error::NotEstablished) {
                            log::trace!("send_nonblocking_fanout: {} not ready", addr);
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// tight hot loop with inline prewarming
    ///
    /// this is the recommended API for low latency transaction sending.
    /// it combines polling, prewarming and sending in a single tight loop.
    ///
    /// # arguments
    /// * `get_tx` - returns transaction data to send, or None to skip this iteration
    ///
    /// # example
    /// ```ignore
    /// sender.run_hot_loop(|| {
    ///     tx_queue.try_recv().ok()
    /// });
    /// ```
    #[inline]
    pub fn run_hot_loop<F>(&mut self, mut get_tx: F) -> !
    where
        F: FnMut() -> Option<Vec<u8>>,
    {
        // pin to CPU if configured
        if let Some(cfg) = &self.inline_prewarm_config {
            if let Some(core) = cfg.cpu_core {
                if let Err(e) = land_cpu::set_cpu_affinity([core]) {
                    log::warn!("Failed to pin to core {}: {}", core, e);
                } else {
                    log::info!("Hot loop pinned to CPU core {}", core);
                }
            }
        }

        let lookahead = self
            .inline_prewarm_config
            .as_ref()
            .map(|c| c.lookahead)
            .unwrap_or(5);

        let tpu_port = self.config.default_tpu_port;

        loop {
            // poll for incoming packets (drive connections)
            let _ = self.endpoint.poll();

            // inline prewarm upcoming leaders
            let addrs = self.get_prewarm_addrs(lookahead, tpu_port);
            self.endpoint.prewarm(addrs);

            // get and send transaction (non-blocking)
            if let Some(tx) = get_tx() {
                // send to current leader (index 0)
                if let Ok(leader_addrs) = self.get_leader_addrs(0, tpu_port) {
                    for addr in leader_addrs {
                        match self.endpoint.send_nonblocking(addr, &tx) {
                            Ok(()) => {
                                log::trace!("hot_loop: sent {} bytes to {}", tx.len(), addr);
                            }
                            Err(Error::NotEstablished) => {
                                log::trace!("hot_loop: {} not ready", addr);
                            }
                            Err(e) => {
                                log::warn!("hot_loop: send failed: {}", e);
                            }
                        }
                    }
                }
            }

            // minimal yield (CPU hint, no syscall)
            std::hint::spin_loop();
        }
    }

    /// run hot loop with fanout to multiple leaders
    #[inline]
    pub fn run_hot_loop_fanout<F>(&mut self, fanout: usize, mut get_tx: F) -> !
    where
        F: FnMut() -> Option<Vec<u8>>,
    {
        // pin to CPU if configured
        if let Some(cfg) = &self.inline_prewarm_config {
            if let Some(core) = cfg.cpu_core {
                if let Err(e) = land_cpu::set_cpu_affinity([core]) {
                    log::warn!("Failed to pin to core {}: {}", core, e);
                }
            }
        }

        let lookahead = self
            .inline_prewarm_config
            .as_ref()
            .map(|c| c.lookahead)
            .unwrap_or(fanout.max(5));

        let tpu_port = self.config.default_tpu_port;

        loop {
            // poll
            let _ = self.endpoint.poll();

            // prewarm
            let addrs = self.get_prewarm_addrs(lookahead, tpu_port);
            self.endpoint.prewarm(addrs);

            // send with fanout
            if let Some(tx) = get_tx() {
                let _ = self.send_nonblocking_fanout(&tx, fanout, SendOptions::default());
            }

            std::hint::spin_loop();
        }
    }

    /// shutdown prewarm thread if running
    pub fn shutdown(&mut self) -> Result<()> {
        if let Some(shutdown) = self.prewarm_shutdown.take() {
            shutdown.store(true, Ordering::Release);
        }

        if let Some(handle) = self.prewarm_handle.take() {
            let _ = handle.join();
        }

        Ok(())
    }
}

impl<L: LeaderLookup + Clone + 'static> TransactionSender<L> {
    /// create new transaction sender
    pub fn new(mut config: SenderConfig, leaders: L, bind_addr: SocketAddr) -> Result<Self> {
        // create shared session cache for 0-RTT
        let session_cache = Arc::new(SessionTokenCache::new());

        // create endpoint with shared session cache (mutable for configuration)
        let mut endpoint =
            QuicEndpoint::new(bind_addr, &config.quic_config, Some(session_cache.clone()))?;

        // auto-select TPU port based on staked identity:
        // - staked: use fwd port (higher priority)
        // - unstaked: use main port
        let is_staked = config.quic_config.staked_identity.is_some();
        config.default_tpu_port = TpuPort::default_for_staked(is_staked);

        let (inline_prewarm_config, prewarm_handle, prewarm_shutdown) = match &config.strategy {
            ConnectStrategy::InlinePrewarm(cfg) => {
                // configure adaptive timing on endpoint if enabled
                if let Some(adaptive_cfg) = &cfg.adaptive {
                    endpoint.set_adaptive_config(adaptive_cfg.clone());
                    log::info!(
                        "Using adaptive inline prewarm strategy (lookahead={}, min_solana_slots={}, LOWEST LATENCY + ADAPTIVE TIMING)",
                        cfg.lookahead,
                        adaptive_cfg.min_solana_slots
                    );
                } else {
                    log::info!(
                        "Using inline prewarm strategy (lookahead={}, LOWEST LATENCY)",
                        cfg.lookahead
                    );
                }
                log::info!("config: {:?}", cfg);
                (Some(cfg.clone()), None, None)
            }
            ConnectStrategy::Prewarm(cfg) => {
                let shutdown = Arc::new(AtomicBool::new(false));

                // spawn prewarm thread with shared session cache
                let handle = crate::prewarm::spawn_prewarm_thread(
                    bind_addr,
                    &config.quic_config,
                    Arc::new(leaders.clone()),
                    cfg.clone(),
                    config.default_tpu_port,
                    shutdown.clone(),
                    session_cache.clone(),
                )?;

                log::info!(
                    "Spawned background prewarm thread with lookahead={}, poll_interval={}us",
                    cfg.lookahead,
                    cfg.poll_interval_us
                );
                (None, Some(handle), Some(shutdown))
            }
            ConnectStrategy::Jit(_) => {
                log::info!("Using JIT connection strategy (no prewarm)");
                (None, None, None)
            }
            ConnectStrategy::JitConfig(cfg) => {
                // configure adaptive timing on endpoint if enabled
                if let Some(adaptive_cfg) = &cfg.adaptive {
                    endpoint.set_adaptive_config(adaptive_cfg.clone());
                    log::info!(
                        "Using ADAPTIVE JIT strategy (mode={:?}, min_solana_slots={})",
                        cfg.mode,
                        adaptive_cfg.min_solana_slots
                    );
                    log::info!("  → Connects on-demand, checks if handshake will complete in time");
                } else {
                    log::info!(
                        "Using JIT connection strategy (mode={:?}, no prewarm)",
                        cfg.mode
                    );
                }
                (None, None, None)
            }
        };

        Ok(Self {
            config,
            endpoint,
            leaders,
            inline_prewarm_config,
            prewarm_handle,
            prewarm_shutdown,
        })
    }
}

impl<L: LeaderLookup> Drop for TransactionSender<L> {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}
