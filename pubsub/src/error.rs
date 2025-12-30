use std::fmt;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    // io errors
    Io(std::io::Error),

    // websocket protocol errors
    InvalidHandshake(String),
    InvalidFrame(String),
    ConnectionClosed,

    // json parsing errors
    InvalidJson(String),

    // subscriber errors
    AlreadyStarted,
    NotStarted,

    // channel errors
    ChannelFull,
    ChannelDisconnected,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "io error: {}", e),
            Error::InvalidHandshake(msg) => write!(f, "invalid websocket handshake: {}", msg),
            Error::InvalidFrame(msg) => write!(f, "invalid websocket frame: {}", msg),
            Error::ConnectionClosed => write!(f, "connection closed"),
            Error::InvalidJson(msg) => write!(f, "invalid json: {}", msg),
            Error::AlreadyStarted => write!(f, "subscriber already started"),
            Error::NotStarted => write!(f, "subscriber not started"),
            Error::ChannelFull => write!(f, "channel full"),
            Error::ChannelDisconnected => write!(f, "channel disconnected"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Error::Io(err)
    }
}
