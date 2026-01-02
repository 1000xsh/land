//! land transaction sender core.
//!
//! thin orchestration layer that wires together:
//! - **land_leader**: busy-spin leader tracking with SeqLock buffer
//! - **land_quic**: zero-copy QUIC sender with connection pooling
//! - **land_server**: lock-free JSON-RPC server
//!
//! # architecture
//!
//! ```text
//! ┌─────────────────┐     ┌─────────────────┐     ┌─────────────────┐
//! │ leader-tracker  │     │   quic-sender   │     │   rpc-server    │
//! │ (isolated core) │     │ (worker thread) │     │  (mio/epoll)    │
//! └────────┬────────┘     └────────┬────────┘     └────────┬────────┘
//!          │                       │                       │
//!          ▼                       ▼                       ▼
//! ┌─────────────────────────────────────────────────────────────────┐
//! │              Arc<LeaderBuffer> (implements LeaderLookup)        │
//! └─────────────────────────────────────────────────────────────────┘
//! ```

pub mod config;
pub mod warmup;

pub use config::{Config, CoreAllocation};
