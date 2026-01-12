//! land - send transaction client.
//!
//!
//! # example
//!
//! ```no_run
//! use land_client::{LandClient, SendOptions};
//!
//! // connect to land rpc server
//! let mut land = LandClient::connect("127.0.0.1:8080").unwrap();
//!
//! // send with defaults (fanout=1, target_slot=0)
//! let result = land.send(&tx_bytes).unwrap();
//!
//! // send with custom options
//! let opts = SendOptions::default()
//!     .fanout(3)
//!     .target_slot(current_slot);
//! let result = land.send_with_opts(&tx_bytes, &opts).unwrap();
//! ```

mod client;
mod connection;
mod error;
mod request;
mod response;
mod types;

// public exports
pub use client::LandClient;
pub use error::{Error, Result};
pub use types::{SendOptions, SendResult};
