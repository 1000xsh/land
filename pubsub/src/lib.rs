//! low-latency websocket slot subscriber for solana
//!
//! this library provides a minimal websocket client
//! for subscribing to solana slot updates using land-channel spsc for event-driven updates.
//!
//! # example
//!
//! ```no_run
//! use land_pubsub::SlotSubscriber;
//!
//! // create subscriber with 64 slot capacity
//! let (mut subscriber, mut rx) = SlotSubscriber::new("ws://localhost:8900", 64);
//!
//! // start background thread
//! subscriber.start().expect("failed to start");
//!
//! // event-driven consumer loop
//! loop {
//!     let count = rx.poll(|info, _seq, _eob| {
//!         println!("slot {} parent {} root {}", info.slot, info.parent, info.root);
//!     });
//!
//!     if count == 0 {
//!         std::hint::spin_loop();
//!     }
//! }
//! ```

mod error;
mod parser;
mod subscriber;
mod types;
mod websocket;

// public exports
pub use error::{Error, Result};
pub use subscriber::SlotSubscriber;
pub use types::{Slot, SlotInfo, SubscriptionId};

// re-export land-channel types for convenience
pub use land_channel::spsc::{Consumer, Producer};
pub use land_cpu::SpinLoopHintWait;
