//! configuration for low-latency QUIC connections

/// QUIC connection configuration
#[derive(Debug, Clone)]
pub struct Config {
    /// maximum connections in pool
    pub max_connections: usize,
    pub idle_timeout_ms: u64,
    pub connect_timeout_ms: u64,
    pub keepalive_interval_ms: u64,
    /// maximum concurrent unidirectional streams
    pub max_streams_uni: u64,
    /// maximum retry attempts before marking dead
    pub max_retry_attempts: usize,
    pub first_attempt_timeout_ms: u64,
    pub retry_timeout_ms: u64,
    /// staked identity for swqos
    pub staked_identity: Option<StakedIdentity>,
    /// CPU core to pin the main loop to
    pub cpu_core: Option<usize>,
}

/// staked identity configuration
#[derive(Debug, Clone)]
pub struct StakedIdentity {
    /// path to keypair file (JSON array, raw bytes, or base58)
    pub keypair_path: String,
}

impl Default for Config {
    #[inline]
    fn default() -> Self {
        Self {
            max_connections: 128,
            idle_timeout_ms: 60_000,
            connect_timeout_ms: 300,
            keepalive_interval_ms: 500,
            max_streams_uni: 2048,
            max_retry_attempts: 3,
            first_attempt_timeout_ms: 1000,
            retry_timeout_ms: 500,
            staked_identity: None,
            cpu_core: None,
        }
    }
}

impl Config {
    /// create config with staked identity
    #[inline]
    pub fn with_staked_identity(mut self, keypair_path: &str) -> Self {
        self.staked_identity = Some(StakedIdentity {
            keypair_path: keypair_path.to_string(),
        });
        self
    }

    /// set CPU core for thread pinning
    #[inline]
    pub fn with_cpu_core(mut self, core: usize) -> Self {
        self.cpu_core = Some(core);
        self
    }
}
