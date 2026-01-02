//! connection strategies for transaction sending

/// wich TPU port to use when sending transactions
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TpuPort {
    /// main TPU QUIC port (unstaked)
    Main,
    /// TPU QUIC Forward port (staked, higher priority)
    Forward,
    /// send to both ports for redundancy
    Both,
}

impl TpuPort {
    /// get default TPU port based on whether using staked identity
    #[inline]
    pub fn default_for_staked(is_staked: bool) -> Self {
        if is_staked {
            TpuPort::Forward
        } else {
            TpuPort::Main
        }
    }
}

impl Default for TpuPort {
    fn default() -> Self {
        TpuPort::Main
    }
}

/// just-in-time connection mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JitMode {
    /// connect only to current leader (index 0)
    Single,
    /// connect to multiple leaders for fanout (0..n)
    Fanout(usize),
}

/// just-in-time configuration with optional adaptive timing
#[derive(Debug, Clone)]
pub struct JitConfig {
    /// JIT mode (single or fanout)
    pub mode: JitMode,
    /// optional adaptive timing configuration
    /// when enabled, checks if connection will complete in time based on measured latency
    pub adaptive: Option<AdaptivePrewarmConfig>,
}

/// prewarm configuration for 0-RTT connection pool (background thread)
#[derive(Debug, Clone)]
pub struct PrewarmConfig {
    /// how many leaders ahead to prewarm (e.g., 3-5)
    pub lookahead: usize,
    /// poll interval in microseconds for checking ring buffer
    pub poll_interval_us: u64,
    /// optional CPU core to pin prewarm thread
    pub cpu_core: Option<usize>,
}

impl Default for PrewarmConfig {
    fn default() -> Self {
        Self {
            lookahead: 10,
            poll_interval_us: 1000, // 1ms
            cpu_core: None,
        }
    }
}

/// adaptive inline prewarming configuration for ultra-low latency
///
/// dynamically determines connection timing based on measured per-IP handshake latencies.
/// connects N solana slots early, where N = ceil(handshake_ms / 400ms).
#[derive(Debug, Clone)]
pub struct AdaptivePrewarmConfig {
    /// minimum lookahead solana slots (even for very fast validators)
    pub min_solana_slots: usize,
    pub max_solana_slots: usize,
    pub solana_slot_duration_ms: u64,
    pub default_handshake_ms: u64,

    /// use median from known validators for bootstrap timing
    pub bootstrap_from_median: bool,

    /// percentile to use for timing (p50, p90, p99)
    pub timing_percentile: f64,
}

impl Default for AdaptivePrewarmConfig {
    fn default() -> Self {
        Self {
            min_solana_slots: 4,
            max_solana_slots: 20,
            solana_slot_duration_ms: 400,
            default_handshake_ms: 2000,
            bootstrap_from_median: true,
            timing_percentile: 90.0, // use p90 for conservative estimation
        }
    }
}

impl AdaptivePrewarmConfig {
    /// create with custom minimum solana slots
    pub fn with_min_slots(min_slots: usize) -> Self {
        Self {
            min_solana_slots: min_slots,
            ..Default::default()
        }
    }

    /// create with custom percentile
    pub fn with_percentile(percentile: f64) -> Self {
        Self {
            timing_percentile: percentile,
            ..Default::default()
        }
    }
}

/// inline prewarm configuration for low latency (single-thread)
///
/// this strategy eliminates the prewarm thread and does prewarming inline
/// in the hot loop. this provides the lowest latency because:
/// - no thread synchronization overhead
/// - all connections stay in a single pool
/// - 0-RTT sessions are immediately available
#[derive(Debug, Clone)]
pub struct InlinePrewarmConfig {
    pub lookahead: usize,
    /// optional CPU core to pin the hot loop thread
    pub cpu_core: Option<usize>,
    /// optional adaptive timing configuration
    pub adaptive: Option<AdaptivePrewarmConfig>,
}

impl Default for InlinePrewarmConfig {
    fn default() -> Self {
        Self {
            lookahead: 10,
            cpu_core: None,
            adaptive: Some(AdaptivePrewarmConfig::default()),
        }
    }
}

impl InlinePrewarmConfig {
    /// create with specific lookahead
    pub fn with_lookahead(lookahead: usize) -> Self {
        Self {
            lookahead,
            cpu_core: None,
            adaptive: Some(AdaptivePrewarmConfig::default()),
        }
    }

    /// create with CPU core pinning
    pub fn with_cpu_core(mut self, core: usize) -> Self {
        self.cpu_core = Some(core);
        self
    }

    /// create with custom adaptive configuration
    pub fn with_adaptive(mut self, adaptive: AdaptivePrewarmConfig) -> Self {
        self.adaptive = Some(adaptive);
        self
    }

    /// disable adaptive prewarming (use static lookahead)
    pub fn without_adaptive(mut self) -> Self {
        self.adaptive = None;
        self
    }
}

/// connection strategy for transaction sending
#[derive(Debug, Clone)]
pub enum ConnectStrategy {
    /// jit: connect when sending transaction
    /// legacy: accepts JitMode directly
    Jit(JitMode),
    /// jit with configuration (supports adaptive timing)
    JitConfig(JitConfig),
    /// prewarm: background thread maintains warm connections + 0-RTT tickets
    Prewarm(PrewarmConfig),
    /// inline prewarm: single-thread prewarming in hot loop
    InlinePrewarm(InlinePrewarmConfig),
}

impl Default for ConnectStrategy {
    fn default() -> Self {
        // default to inline prewarm for lowest latency
        ConnectStrategy::InlinePrewarm(InlinePrewarmConfig::default())
    }
}

impl ConnectStrategy {
    pub fn jit_single() -> Self {
        ConnectStrategy::Jit(JitMode::Single)
    }

    pub fn jit_fanout(count: usize) -> Self {
        ConnectStrategy::Jit(JitMode::Fanout(count))
    }

    /// create adaptive JIT strategy (checks if connection will complete in time)
    pub fn adaptive_jit(mode: JitMode, adaptive: AdaptivePrewarmConfig) -> Self {
        ConnectStrategy::JitConfig(JitConfig {
            mode,
            adaptive: Some(adaptive),
        })
    }

    /// create background prewarm strategy (has thread sync overhead)
    pub fn prewarm(lookahead: usize) -> Self {
        ConnectStrategy::Prewarm(PrewarmConfig {
            lookahead,
            ..Default::default()
        })
    }

    pub fn inline_prewarm(lookahead: usize) -> Self {
        ConnectStrategy::InlinePrewarm(InlinePrewarmConfig::with_lookahead(lookahead))
    }

    /// create adaptive inline prewarm strategy (lowest latency with dynamic timing)
    pub fn adaptive_inline_prewarm(lookahead: usize, adaptive: AdaptivePrewarmConfig) -> Self {
        ConnectStrategy::InlinePrewarm(
            InlinePrewarmConfig::with_lookahead(lookahead).with_adaptive(adaptive),
        )
    }
}
