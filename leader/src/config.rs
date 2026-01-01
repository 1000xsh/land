//! configuration for the leader tracker.

use std::time::Duration;

/// leader tracker configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// RPC URL for fetching leader schedule.
    pub rpc_url: String,
    /// websocket URL for slot updates.
    pub ws_url: String,
    /// leader schedule refresh interval.
    pub schedule_refresh_interval: Duration,
    /// CPU core to pin the tracker thread.
    pub tracker_cpu_core: Option<usize>,
    /// CPU core to pin the slot subscriber thread.
    pub subscriber_cpu_core: Option<usize>,
}

impl Config {
    /// create config for mainnet.
    pub fn mainnet() -> Self {
        Self {
            rpc_url: "http://45.152.160.253:8899".to_string(),
            ws_url: "ws://45.152.160.253:8900".to_string(),
            schedule_refresh_interval: Duration::from_secs(600),
            tracker_cpu_core: Some(2),
            subscriber_cpu_core: Some(6),
        }
    }

    /// create config for devnet.
    pub fn devnet() -> Self {
        Self {
            rpc_url: "https://api.devnet.solana.com".to_string(),
            ws_url: "wss://api.devnet.solana.com".to_string(),
            schedule_refresh_interval: Duration::from_secs(30),
            tracker_cpu_core: None,
            subscriber_cpu_core: None,
        }
    }

    /// create config with custom endpoints.
    pub fn custom(rpc_url: impl Into<String>, ws_url: impl Into<String>) -> Self {
        Self {
            rpc_url: rpc_url.into(),
            ws_url: ws_url.into(),
            schedule_refresh_interval: Duration::from_secs(30),
            tracker_cpu_core: None,
            subscriber_cpu_core: None,
        }
    }

    /// set the CPU core for thread pinning.
    pub fn with_cpu_core(mut self, core: usize) -> Self {
        self.tracker_cpu_core = Some(core);
        self
    }

    /// set the schedule refresh interval.
    pub fn with_refresh_interval(mut self, interval: Duration) -> Self {
        self.schedule_refresh_interval = interval;
        self
    }

    /// set the CPU core for the slot subscriber thread.
    pub fn with_subscriber_cpu_core(mut self, core: usize) -> Self {
        self.subscriber_cpu_core = Some(core);
        self
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::mainnet()
    }
}
