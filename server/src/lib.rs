//! RPC server for land.
//!
//! lock-free, low-latency RPC server designed for handling
//! transaction submissions with fanout and target slot parameters.
//!
//! # architecture
//!
//! the server uses a two-thread model:
//! - **HTTP accept thread**: handles TCP connections with mio (non-blocking I/O)
//! - **worker thread**: processes requests from a lock-free SPSC queue
//!
//! # example
//!
//! ```no_run
//! use land_server::{RpcServer, ServerConfig};
//!
//! let config = ServerConfig::new()
//!     .with_bind_addr("127.0.0.1:8080".parse().unwrap())
//!     .with_queue_size(4096)
//!     .with_worker_cpu_core(1);
//!
//! let server = RpcServer::new(config).unwrap();
//! server.run().unwrap();
//! ```
//!
//! # JSON-RPC 2.0 API
//!
//! ## request format
//!
//! ```json
//! {
//!   "jsonrpc": "2.0",
//!   "id": 1,
//!   "method": "sendTransaction",
//!   "params": {
//!     "transaction": "base64_encoded_data",
//!     "fanout": 3,
//!     "target_slot": 12345678
//!   }
//! }
//! ```
//!
//! ## response format (success)
//! response with tx signature
//!
//! ```json
//! {
//!   "jsonrpc": "2.0",
//!   "id": 1,
//!   "result": {
//!     "status": "queued",
//!     "request_id": "000000000000001a"
//!   }
//! }
//! ```
//!
//! ## response format (error)
//!
//! ```json
//! {
//!   "jsonrpc": "2.0",
//!   "id": 1,
//!   "error": {
//!     "code": -32602,
//!     "message": "Invalid params",
//!     "data": "fanout must be greater than 0"
//!   }
//! }
//! ```

mod config;
mod date;
mod error;
mod http;
mod queue;
mod request;
mod server;
mod worker;

// public exports
pub use config::ServerConfig;
pub use error::{Result, ServerError};
pub use server::RpcServer;
pub use worker::Worker;

// re-export request/response types
pub use request::{
    JsonRpcError, JsonRpcRequest, JsonRpcResponse, ParsedRequest, SendTransactionParams,
    SendTransactionResult,
};

// re-export traits for convenience
pub use land_traits::LeaderLookup;
