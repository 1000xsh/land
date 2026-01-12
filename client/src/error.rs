//! error types for land client.

use std::fmt;
use std::io;

/// error type for land client operations.
#[derive(Debug)]
pub enum Error {
    /// connection failed
    Connect(io::Error),
    /// send failed
    Send(io::Error),
    /// recv failed
    Recv(io::Error),
    /// invalid http response
    InvalidHttp,
    /// invalid json-rpc response
    InvalidResponse,
    /// rpc error from server
    Rpc { code: i32 },
    /// operation would block (non-blocking mode)
    WouldBlock,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Connect(e) => write!(f, "connect: {}", e),
            Error::Send(e) => write!(f, "send: {}", e),
            Error::Recv(e) => write!(f, "recv: {}", e),
            Error::InvalidHttp => write!(f, "invalid http response"),
            Error::InvalidResponse => write!(f, "invalid json-rpc response"),
            Error::Rpc { code } => write!(f, "rpc error: code={}", code),
            Error::WouldBlock => write!(f, "would block"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Connect(e) | Error::Send(e) | Error::Recv(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        if e.kind() == io::ErrorKind::WouldBlock {
            Error::WouldBlock
        } else {
            Error::Send(e)
        }
    }
}

/// result type alias
pub type Result<T> = std::result::Result<T, Error>;
