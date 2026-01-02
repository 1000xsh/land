//! configuration for the core application.

use land_cpu::{cpu_count, physical_core_count};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

/// core application configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// leader tracker config.
    pub leader: LeaderConfig,
    /// quic sender config.
    pub quic: QuicConfig,
    /// http server config.
    pub http: HttpConfig,
    /// core allocation.
    pub allocation: CoreAllocation,
}

/// leader tracker configuration.
#[derive(Debug, Clone)]
pub struct LeaderConfig {
    /// RPC URL for fetching leader schedule.
    pub rpc_url: String,
    /// websocket URL for slot updates.
    pub ws_url: String,
    /// leader schedule refresh interval.
    pub schedule_refresh_interval: Duration,
}

/// quic sender configuration.
#[derive(Debug, Clone)]
pub struct QuicConfig {
    /// local bind address for quic endpoint.
    pub bind_addr: SocketAddr,
    /// number of leaders to prewarm connections to.
    pub prewarm_lookahead: usize,
    /// staked keypair path for priority access (optional).
    pub staked_keypair: Option<PathBuf>,
    /// connection idle timeout.
    pub idle_timeout: Duration,
    /// connect timeout.
    pub connect_timeout: Duration,
}

/// http server configuration.
#[derive(Debug, Clone)]
pub struct HttpConfig {
    /// http server bind address.
    pub bind_addr: SocketAddr,
    /// request queue size (must be power of 2).
    pub queue_size: usize,
    /// max concurrent connections.
    pub max_connections: usize,
    /// max request body size.
    pub max_request_size: usize,
}

/// cpu core allocation for threads.
#[derive(Debug, Clone)]
pub struct CoreAllocation {
    pub leader_tracker: Option<usize>,
    pub quic_sender: Option<usize>,
    pub quic_prewarm: Option<usize>,
    pub http_cores: Vec<usize>,
}

impl Config {
    /// create config for mainnet with auto-detected core allocation.
    pub fn mainnet() -> Self {
        Self {
            leader: LeaderConfig {
                rpc_url: "http://45.152.160.253:8899".into(),
                ws_url: "ws://45.152.160.253:8900".into(),
                schedule_refresh_interval: Duration::from_secs(30),
            },
            quic: QuicConfig {
                bind_addr: "0.0.0.0:0".parse().unwrap(),
                prewarm_lookahead: 5,
                staked_keypair: None,
                idle_timeout: Duration::from_secs(60),
                connect_timeout: Duration::from_millis(300),
            },
            http: HttpConfig {
                bind_addr: "127.0.0.1:10080".parse().unwrap(),
                queue_size: 4096,
                max_connections: 10240,
                max_request_size: 1024 * 1024,
            },
            allocation: CoreAllocation::auto_detect(),
        }
    }

    /// create config for devnet.
    pub fn devnet() -> Self {
        Self {
            leader: LeaderConfig {
                rpc_url: "https://api.devnet.solana.com".into(),
                ws_url: "wss://api.devnet.solana.com".into(),
                schedule_refresh_interval: Duration::from_secs(30),
            },
            quic: QuicConfig {
                bind_addr: "0.0.0.0:0".parse().unwrap(),
                prewarm_lookahead: 5,
                staked_keypair: None,
                idle_timeout: Duration::from_secs(60),
                connect_timeout: Duration::from_millis(300),
            },
            http: HttpConfig {
                bind_addr: "127.0.0.1:8080".parse().unwrap(),
                queue_size: 4096,
                max_connections: 10240,
                max_request_size: 1024 * 1024,
            },
            allocation: CoreAllocation::auto_detect(),
        }
    }

    /// create config with custom endpoints.
    pub fn custom(rpc_url: impl Into<String>, ws_url: impl Into<String>) -> Self {
        Self {
            leader: LeaderConfig {
                rpc_url: rpc_url.into(),
                ws_url: ws_url.into(),
                schedule_refresh_interval: Duration::from_secs(30),
            },
            quic: QuicConfig {
                bind_addr: "0.0.0.0:0".parse().unwrap(),
                prewarm_lookahead: 10,
                staked_keypair: None,
                idle_timeout: Duration::from_secs(60),
                connect_timeout: Duration::from_millis(300),
            },
            http: HttpConfig {
                bind_addr: "127.0.0.1:8080".parse().unwrap(),
                queue_size: 4096,
                max_connections: 10240,
                max_request_size: 1024 * 1024,
            },
            allocation: CoreAllocation::auto_detect(),
        }
    }

    /// set http bind address.
    pub fn with_http_addr(mut self, addr: SocketAddr) -> Self {
        self.http.bind_addr = addr;
        self
    }

    /// set quic bind address.
    pub fn with_quic_addr(mut self, addr: SocketAddr) -> Self {
        self.quic.bind_addr = addr;
        self
    }

    /// set core allocation.
    pub fn with_allocation(mut self, allocation: CoreAllocation) -> Self {
        self.allocation = allocation;
        self
    }

    /// set prewarm lookahead.
    pub fn with_prewarm_lookahead(mut self, lookahead: usize) -> Self {
        self.quic.prewarm_lookahead = lookahead;
        self
    }

    /// set queue size.
    pub fn with_queue_size(mut self, size: usize) -> Self {
        self.http.queue_size = size;
        self
    }

    /// set staked keypair path.
    pub fn with_staked_keypair(mut self, path: impl Into<PathBuf>) -> Self {
        self.quic.staked_keypair = Some(path.into());
        self
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::mainnet()
    }
}

impl CoreAllocation {
    /// create allocation with explicit core assignments.
    pub fn new(
        leader_tracker: Option<usize>,
        quic_sender: Option<usize>,
        quic_prewarm: Option<usize>,
        http_cores: Vec<usize>,
    ) -> Self {
        Self {
            leader_tracker,
            quic_sender,
            quic_prewarm,
            http_cores,
        }
    }

    /// auto-detect optimal core allocation based on system topology.
    ///
    /// layout:
    /// - core 0: avoid
    /// - core 1: leader tracker (isolated)
    /// - core 2: quic sender (isolated)
    /// - core 3: quic prewarm (isolated)
    /// - core 4+: http/background
    pub fn auto_detect() -> Self {
        let cores = cpu_count().unwrap_or(4);
        let physical = physical_core_count().unwrap_or(cores);

        if physical >= 8 {
            // 8+ cores: full isolation
            Self {
                leader_tracker: Some(1),
                quic_sender: Some(2),
                quic_prewarm: Some(3),
                http_cores: (4..physical.min(8)).collect(),
            }
        } else if physical >= 4 {
            // 4-7 cores: partial isolation
            Self {
                leader_tracker: Some(1),
                quic_sender: Some(2),
                quic_prewarm: Some(3),
                http_cores: vec![],
            }
        } else {
            // <4 cores: no isolation
            Self {
                leader_tracker: None,
                quic_sender: None,
                quic_prewarm: None,
                http_cores: vec![],
            }
        }
    }

    /// create allocation for 8-core system.
    pub fn for_8_cores() -> Self {
        Self {
            leader_tracker: Some(1),
            quic_sender: Some(2),
            quic_prewarm: Some(3),
            http_cores: vec![4, 5, 6, 7],
        }
    }

    /// create allocation for 16-core system.
    pub fn for_16_cores() -> Self {
        Self {
            leader_tracker: Some(1),
            quic_sender: Some(2),
            quic_prewarm: Some(4),
            http_cores: vec![5, 6, 7, 8, 9, 10],
        }
    }

    /// create allocation with no pinning.
    pub fn none() -> Self {
        Self {
            leader_tracker: None,
            quic_sender: None,
            quic_prewarm: None,
            http_cores: vec![],
        }
    }
}

impl Default for CoreAllocation {
    fn default() -> Self {
        Self::auto_detect()
    }
}
