//! solana leader tracker.
//!
//! provides a lock-free ring buffer of current and upcoming leaders
//! with their TPU addresses for transaction routing.
//!
//! # architecture
//!
//! - **LeaderBuffer**: SeqLock-based buffer with 10 leader entries
//! - **LeaderEntry**: 64-byte cache-aligned struct with pubkey + TPU addresses
//! - **SlotSubscriber**: websocket slot updates writing to shared atomic
//! - **ScheduleFetcher**: RPC-based leader schedule with epoch caching
//! - **LeaderTracker**: coordinator with busy-spin loop on dedicated thread
//!
//! # usage
//!
//! ```no_run
//! use land_leader::{Config, LeaderTracker};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let config = Config::mainnet();
//!     let mut tracker = LeaderTracker::new(config);
//!
//!     tracker.start().await?;
//!
//!     // get buffer handle for lock-free reads.
//!     let buffer = tracker.buffer();
//!
//!     // read current leader (position 0).
//!     let current = buffer.read(0);
//!     if current.is_valid() {
//!         println!("Current leader: {}", current.pubkey);
//!         println!("TPU QUIC: {}", current.tpu_quic());
//!     }
//!
//!     // read next leader (position 1).
//!     let next = buffer.read(1);
//!
//!     tracker.stop().await;
//!     Ok(())
//! }
//! ```

pub mod config;
pub mod leader_buffer;
pub mod leader_entry;
pub mod schedule_fetcher;
pub mod slot_subscriber;
pub mod tracker;

pub use config::Config;
pub use leader_buffer::{LeaderBuffer, LEADER_BUFFER_SIZE, SLOTS_PER_LEADER};
pub use leader_entry::LeaderEntry;
pub use schedule_fetcher::{FetcherError, ScheduleFetcher, TpuAddresses};
pub use slot_subscriber::{SlotState, SlotSubscriber};
pub use tracker::{LeaderTracker, TrackerError};

// re-export trait for convenience.
pub use land_traits::LeaderLookup;
