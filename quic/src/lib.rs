//! low latency QUIC connection library for solana
//!
//! this library provides QUIC connections to solana validators
//!
//! # example
//!
//! ```no_run
//! use quic::{Config, QuicClient};
//! use std::net::SocketAddr;
//!
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let config = Config::default();
//!     let mut client = QuicClient::new(config)?;
//!
//!     let validator: SocketAddr = "127.0.0.1:8009".parse()?;
//!     client.send(validator, b"transaction data")?;
//!
//!     Ok(())
//! }
//! ```

use std::sync::OnceLock;
use std::time::Instant;

/// baseline instant for monotonic timestamps (initialized on first use)
static BASELINE: OnceLock<Instant> = OnceLock::new();

/// get monotonic nanoseconds since baseline (vDSO-optimized on linux, no syscall)
/// https://man7.org/linux/man-pages/man7/vdso.7.html
#[inline(always)]
pub fn monotonic_nanos() -> u64 {
    let baseline = BASELINE.get_or_init(Instant::now);
    baseline.elapsed().as_nanos() as u64
}

pub mod config;
pub mod connection;
pub mod endpoint;
pub mod error;
pub mod pool;
pub mod prewarm;
pub mod sender;
pub mod session_cache;
pub mod strategy;
pub mod timing_registry;
pub mod tls;

// io_uring backend for zero-copy UDP send
#[cfg(all(target_os = "linux", feature = "io_uring"))]
pub mod io_uring_backend;

pub use config::{Config, StakedIdentity};
pub use connection::{ConnectionState, QuicConnection, MAX_PACKET_SIZE};
pub use endpoint::QuicEndpoint;
pub use error::{Error, Result};
pub use pool::{ConnectionPool, ConnectionSlot, PoolStats, MAX_POOL_SIZE, MAX_RETRY_ATTEMPTS};
pub use prewarm::{spawn_prewarm_thread, SessionTicket};
pub use sender::{LeaderSource, SendOptions, SenderConfig, TransactionSender};
pub use session_cache::{CacheStats, SessionTokenCache};
pub use timing_registry::{ConnectionTimings, TimingCategory, TimingRegistry};
// re-export trait for convenience
pub use land_traits::LeaderLookup;
pub use strategy::{
    AdaptivePrewarmConfig, ConnectStrategy, InlinePrewarmConfig, JitConfig, JitMode, PrewarmConfig,
    TpuPort,
};
pub use tls::Keypair;

use std::net::SocketAddr;
use std::sync::Arc;

/// QUIC client
pub struct QuicClient {
    endpoint: QuicEndpoint,
    config: Config,
    session_cache: Arc<SessionTokenCache>,
}

impl QuicClient {
    /// create new QUIC client
    pub fn new(config: Config) -> Result<Self> {
        let bind_addr: SocketAddr = "0.0.0.0:0".parse().unwrap();
        let session_cache = Arc::new(SessionTokenCache::new());
        let endpoint = QuicEndpoint::new(bind_addr, &config, Some(session_cache.clone()))?;

        Ok(Self {
            endpoint,
            config,
            session_cache,
        })
    }

    /// create new QUIC client with specific bind address
    pub fn with_bind_addr(bind_addr: SocketAddr, config: Config) -> Result<Self> {
        let session_cache = Arc::new(SessionTokenCache::new());
        let endpoint = QuicEndpoint::new(bind_addr, &config, Some(session_cache.clone()))?;
        Ok(Self {
            endpoint,
            config,
            session_cache,
        })
    }

    /// connect to validator (blocking mode)
    ///
    /// use `endpoint.connect(addr, false)` directly for non-blocking 0-RTT mode
    #[inline]
    pub fn connect(&mut self, addr: SocketAddr) -> Result<()> {
        self.endpoint.connect(addr, true)?;
        Ok(())
    }

    /// send data to a validator via unidirectional stream
    #[inline]
    pub fn send(&mut self, addr: SocketAddr, data: &[u8]) -> Result<()> {
        use error::SendStatus;
        match self.endpoint.send(addr, data)? {
            SendStatus::Sent | SendStatus::SentEarlyData | SendStatus::Queued { .. } => Ok(()),
            SendStatus::QueueFull => Err(Error::WouldBlock),
            SendStatus::Dead { retries } => Err(Error::Dead(retries)),
        }
    }

    /// poll for incoming packets (non-blocking)
    #[inline]
    pub fn poll(&mut self) -> Result<()> {
        self.endpoint.poll()
    }

    /// run busy-spin event loop with callback
    ///
    /// this runs an infinite loop that:
    /// 1. polls for incoming packets
    /// 2. calls the user callback
    /// 3. issues a spin_loop hint
    ///
    /// if cpu_core is configured, the thread will be pinned to that core.
    pub fn run_loop<F>(&mut self, callback: F)
    where
        F: FnMut(&mut QuicEndpoint),
    {
        // pin to CPU core if configured
        if let Some(core) = self.config.cpu_core {
            if let Err(e) = land_cpu::set_cpu_affinity([core]) {
                log::warn!("Failed to pin thread to core {}: {}", core, e);
            }
        }

        self.endpoint.run_loop(callback);
    }

    /// get endpoint reference
    #[inline]
    pub fn endpoint(&self) -> &QuicEndpoint {
        &self.endpoint
    }

    /// get mutable endpoint reference
    #[inline]
    pub fn endpoint_mut(&mut self) -> &mut QuicEndpoint {
        &mut self.endpoint
    }

    /// get connection pool reference
    #[inline]
    pub fn pool(&self) -> &ConnectionPool {
        self.endpoint.pool()
    }

    /// get local address
    #[inline]
    pub fn local_addr(&self) -> SocketAddr {
        self.endpoint.local_addr()
    }

    /// get configuration reference
    #[inline]
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// get session cache reference
    #[inline]
    pub fn session_cache(&self) -> &Arc<SessionTokenCache> {
        &self.session_cache
    }

    /// get session cache statistics
    #[inline]
    pub fn cache_stats(&self) -> CacheStats {
        self.session_cache.stats()
    }
}

/// prelude for convenient imports
pub mod prelude {
    pub use crate::config::{Config, StakedIdentity};
    pub use crate::connection::{ConnectionState, MAX_PACKET_SIZE};
    pub use crate::error::{Error, Result};
    pub use crate::pool::{MAX_POOL_SIZE, MAX_RETRY_ATTEMPTS};
    pub use crate::QuicClient;
}
