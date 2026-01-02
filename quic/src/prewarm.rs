//! background connection prewarming for 0-RTT

use crate::config::Config;
use crate::endpoint::QuicEndpoint;
use crate::session_cache::SessionTokenCache;
use crate::strategy::{PrewarmConfig, TpuPort};
use land_cpu::set_cpu_affinity;
use land_traits::LeaderLookup;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// session ticket for 0-RTT connection resumption
#[derive(Debug, Clone)]
pub struct SessionTicket {
    /// serialized session data from quiche
    pub data: Vec<u8>,
    /// timestamp when ticket was created
    pub created_ns: u64,
}

/// prewarm thread for maintaining warm connections
pub struct PrewarmThread<L: LeaderLookup> {
    endpoint: QuicEndpoint,
    leaders: Arc<L>,
    config: PrewarmConfig,
    tpu_port: TpuPort,
    shutdown: Arc<AtomicBool>,
    /// track which leader indices are connected
    connected: [bool; 10],
    /// track addresses we're connected to (for detecting changes)
    connected_addrs: [Option<SocketAddr>; 10],
}

impl<L: LeaderLookup + 'static> PrewarmThread<L> {
    /// create new prewarm thread
    pub fn new(
        bind_addr: SocketAddr,
        quic_config: &Config,
        leaders: Arc<L>,
        config: PrewarmConfig,
        tpu_port: TpuPort,
        shutdown: Arc<AtomicBool>,
        session_cache: Arc<SessionTokenCache>,
    ) -> crate::Result<Self> {
        // create endpoint with shared session cache for 0-RTT
        let endpoint = QuicEndpoint::new(bind_addr, quic_config, Some(session_cache))?;

        Ok(Self {
            endpoint,
            leaders,
            config,
            tpu_port,
            shutdown,
            connected: [false; 10],
            connected_addrs: [None; 10],
        })
    }

    /// run prewarm loop (blocking, runs in separate thread)
    pub fn run(mut self) {
        log::info!("PrewarmThread: starting");

        // pin to CPU core if configured
        if let Some(core) = self.config.cpu_core {
            if let Err(e) = set_cpu_affinity([core]) {
                log::warn!("PrewarmThread: failed to pin to core {}: {}", core, e);
            } else {
                log::info!("PrewarmThread: pinned to core {}", core);
            }
        }

        loop {
            // check for shutdown
            if self.shutdown.load(Ordering::Acquire) {
                log::info!("PrewarmThread: shutdown requested");
                break;
            }

            // poll endpoint to drive existing connections
            let _ = self.endpoint.poll();

            // refresh connections to leaders
            self.refresh_connections();

            // sleep or busy-spin based on poll interval
            if self.config.poll_interval_us > 0 {
                std::thread::sleep(std::time::Duration::from_micros(
                    self.config.poll_interval_us,
                ));
            } else {
                std::hint::spin_loop();
            }
        }

        log::info!("PrewarmThread: stopped");
    }

    /// check for leader changes and connect to new leaders
    fn refresh_connections(&mut self) {
        for i in 0..self.config.lookahead.min(10) {
            if !self.leaders.is_valid(i) {
                continue;
            }

            // get leader addresses
            let (main, fwd) = match self.leaders.get_leader(i) {
                Some(addrs) => addrs,
                None => continue,
            };

            // select address based on TPU port config
            let addr = match self.tpu_port {
                TpuPort::Main => main,
                TpuPort::Forward => fwd,
                TpuPort::Both => main, // connect to main for prewarm
            };

            // check if address changed (leader rotation)
            if let Some(prev_addr) = self.connected_addrs[i] {
                if prev_addr != addr {
                    log::debug!(
                        "PrewarmThread: leader[{}] rotated {} -> {}",
                        i,
                        prev_addr,
                        addr
                    );
                    self.connected[i] = false;
                    self.connected_addrs[i] = None;
                }
            }

            // connect if not already connected (use blocking mode for prewarm)
            if !self.connected[i] {
                match self.endpoint.connect(addr, true) {
                    Ok(_) => {
                        log::info!("PrewarmThread: connected to leader[{}] at {}", i, addr);
                        self.connected[i] = true;
                        self.connected_addrs[i] = Some(addr);
                    }
                    Err(e) => {
                        log::warn!(
                            "PrewarmThread: failed to connect to leader[{}] at {}: {}",
                            i,
                            addr,
                            e
                        );
                    }
                }
            }
        }
    }
}

/// spawn prewarm thread
pub fn spawn_prewarm_thread<L: LeaderLookup + 'static>(
    bind_addr: SocketAddr,
    quic_config: &Config,
    leaders: Arc<L>,
    config: PrewarmConfig,
    tpu_port: TpuPort,
    shutdown: Arc<AtomicBool>,
    session_cache: Arc<SessionTokenCache>,
) -> crate::Result<std::thread::JoinHandle<()>> {
    let thread = PrewarmThread::new(
        bind_addr,
        quic_config,
        leaders,
        config,
        tpu_port,
        shutdown,
        session_cache,
    )?;

    let handle = std::thread::Builder::new()
        .name("quic-prewarm".to_string())
        .spawn(move || thread.run())
        .map_err(|e| crate::Error::Io(e))?;

    Ok(handle)
}
