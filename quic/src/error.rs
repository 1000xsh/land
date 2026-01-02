//! error types for QUIC connection library

use thiserror::Error;

/// QUIC connection errors
#[derive(Debug, Error)]
pub enum Error {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("QUIC error: {0}")]
    Quic(#[from] quiche::Error),

    #[error("TLS error: {0}")]
    Tls(String),

    #[error("Connection not established")]
    NotEstablished,

    #[error("Connection closed")]
    Closed,

    #[error("Connection timeout")]
    Timeout,

    #[error("Connection pool full")]
    PoolFull,

    #[error("Connection not found")]
    NotFound,

    #[error("Connection dead after {0} retries")]
    Dead(usize),

    #[error("Invalid keypair format")]
    InvalidKeypair,

    #[error("Buffer exhausted")]
    BufferExhausted,

    #[error("Would block")]
    WouldBlock,
}

/// result type alias
pub type Result<T> = std::result::Result<T, Error>;

/// status returned from event-based send operations
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendStatus {
    /// data sent successfully on established connection
    Sent,
    /// data queued during handshake (will be sent when connection establishes)
    Queued { position: u8 },
    /// data sent via 0-RTT early data
    SentEarlyData,
    /// transaction queue is full (drop or retry)
    QueueFull,
    /// connection dead after max retries
    Dead { retries: usize },
}
