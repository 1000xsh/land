//! error types for channel operations.
//!
//! this module provides error types for send and receive operations
//! on channels. the errors are designed to be zero-cost when not used
//! and to return ownership of failed sends back to the caller.
//!
//! # error types
//!
//! - [`SendError`]: returned when sending fails because the channel is closed
//! - [`TrySendError`]: returned by `try_send` when the channel is full or closed
//! - [`RecvError`]: returned when receiving fails because the channel is closed
//! - [`TryRecvError`]: returned by `try_recv` when the channel is empty or closed

use core::fmt;

/// error returned when a send operation fails.
///
/// this occurs when the receiving end of the channel has been dropped,
/// meaning no more messages can ever be received.
///
/// the error contains the message that failed to send, allowing the
/// caller to recover it.
///
/// # example
///
/// ```
/// use land_channel::error::SendError;
///
/// let err: SendError<String> = SendError(String::from("hello"));
/// let recovered: String = err.0;
/// assert_eq!(recovered, "hello");
/// ```
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SendError<T>(pub T);

impl<T> SendError<T> {
    /// consume the error and return the message that failed to send.
    ///
    /// # example
    ///
    /// ```
    /// use land_channel::error::SendError;
    ///
    /// let err = SendError(42);
    /// assert_eq!(err.into_inner(), 42);
    /// ```
    #[inline]
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T: fmt::Debug> fmt::Debug for SendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SendError").field("value", &self.0).finish()
    }
}

impl<T> fmt::Display for SendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sending on a closed channel")
    }
}

impl<T: fmt::Debug> std::error::Error for SendError<T> {}

/// error returned when a non-blocking send operation fails.
///
/// this can occur for two reasons:
/// - [`Full`](TrySendError::Full): the channel's buffer is full
/// - [`Disconnected`](TrySendError::Disconnected): The receiving end was dropped
///
/// in both cases, the message that failed to send is returned.
///
/// # example
///
/// ```
/// use land_channel::error::TrySendError;
///
/// let err: TrySendError<i32> = TrySendError::Full(42);
/// assert!(err.is_full());
/// assert!(!err.is_disconnected());
///
/// let value = err.into_inner();
/// assert_eq!(value, 42);
/// ```
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TrySendError<T> {
    /// the channel's buffer is full.
    ///
    /// the send operation could not complete because there's no room
    /// in the buffer. the message can be retried later.
    Full(T),

    /// the receiving end of the channel has been dropped.
    ///
    /// the message can never be received, so the send fails permanently.
    Disconnected(T),
}

impl<T> TrySendError<T> {
    /// returns `true` if this error is the `Full` variant.
    #[inline]
    pub fn is_full(&self) -> bool {
        matches!(self, TrySendError::Full(_))
    }

    /// returns `true` if this error is the `Disconnected` variant.
    #[inline]
    pub fn is_disconnected(&self) -> bool {
        matches!(self, TrySendError::Disconnected(_))
    }

    /// consume the error and return the message that failed to send.
    #[inline]
    pub fn into_inner(self) -> T {
        match self {
            TrySendError::Full(v) | TrySendError::Disconnected(v) => v,
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for TrySendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TrySendError::Full(v) => f.debug_tuple("Full").field(v).finish(),
            TrySendError::Disconnected(v) => f.debug_tuple("Disconnected").field(v).finish(),
        }
    }
}

impl<T> fmt::Display for TrySendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TrySendError::Full(_) => write!(f, "sending on a full channel"),
            TrySendError::Disconnected(_) => write!(f, "sending on a closed channel"),
        }
    }
}

impl<T: fmt::Debug> std::error::Error for TrySendError<T> {}

impl<T> From<SendError<T>> for TrySendError<T> {
    fn from(err: SendError<T>) -> Self {
        TrySendError::Disconnected(err.0)
    }
}

/// error returned when a receive operation fails.
///
/// this occurs when the sending end of the channel has been dropped
/// and all buffered messages have been received. no more messages
/// will ever be available.
///
/// # example
///
/// ```
/// use land_channel::error::RecvError;
///
/// let err = RecvError;
/// assert_eq!(format!("{}", err), "receiving on a closed channel");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecvError;

impl fmt::Display for RecvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "receiving on a closed channel")
    }
}

impl std::error::Error for RecvError {}

/// error returned when a non-blocking receive operation fails.
///
/// this can occur for two reasons:
/// - [`Empty`](TryRecvError::Empty): no messages are currently available
/// - [`Disconnected`](TryRecvError::Disconnected): the channel is closed
///
/// # example
///
/// ```
/// use land_channel::error::TryRecvError;
///
/// let err = TryRecvError::Empty;
/// assert!(err.is_empty());
/// assert!(!err.is_disconnected());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TryRecvError {
    /// no messages are currently available in the channel.
    ///
    /// the receive operation could not complete because the buffer is empty.
    /// more messages may arrive later if the sender is still active.
    Empty,

    /// the sending end of the channel has been dropped.
    ///
    /// no more messages will ever be available. the channel is closed.
    Disconnected,
}

impl TryRecvError {
    /// returns `true` if this error is the `Empty` variant.
    #[inline]
    pub fn is_empty(&self) -> bool {
        matches!(self, TryRecvError::Empty)
    }

    /// returns `true` if this error is the `Disconnected` variant.
    #[inline]
    pub fn is_disconnected(&self) -> bool {
        matches!(self, TryRecvError::Disconnected)
    }
}

impl fmt::Display for TryRecvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TryRecvError::Empty => write!(f, "receiving on an empty channel"),
            TryRecvError::Disconnected => write!(f, "receiving on a closed channel"),
        }
    }
}

impl std::error::Error for TryRecvError {}

impl From<RecvError> for TryRecvError {
    fn from(_: RecvError) -> Self {
        TryRecvError::Disconnected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_send_error() {
        let err = SendError(42);
        assert_eq!(err.into_inner(), 42);
    }

    #[test]
    fn test_send_error_display() {
        let err: SendError<i32> = SendError(0);
        assert_eq!(format!("{}", err), "sending on a closed channel");
    }

    #[test]
    fn test_try_send_error_full() {
        let err: TrySendError<i32> = TrySendError::Full(42);
        assert!(err.is_full());
        assert!(!err.is_disconnected());
        assert_eq!(err.into_inner(), 42);
    }

    #[test]
    fn test_try_send_error_disconnected() {
        let err: TrySendError<i32> = TrySendError::Disconnected(42);
        assert!(!err.is_full());
        assert!(err.is_disconnected());
        assert_eq!(err.into_inner(), 42);
    }

    #[test]
    fn test_try_send_error_display() {
        let full: TrySendError<i32> = TrySendError::Full(0);
        assert_eq!(format!("{}", full), "sending on a full channel");

        let disc: TrySendError<i32> = TrySendError::Disconnected(0);
        assert_eq!(format!("{}", disc), "sending on a closed channel");
    }

    #[test]
    fn test_recv_error() {
        let err = RecvError;
        assert_eq!(format!("{}", err), "receiving on a closed channel");
    }

    #[test]
    fn test_try_recv_error_empty() {
        let err = TryRecvError::Empty;
        assert!(err.is_empty());
        assert!(!err.is_disconnected());
    }

    #[test]
    fn test_try_recv_error_disconnected() {
        let err = TryRecvError::Disconnected;
        assert!(!err.is_empty());
        assert!(err.is_disconnected());
    }

    #[test]
    fn test_try_recv_error_display() {
        assert_eq!(
            format!("{}", TryRecvError::Empty),
            "receiving on an empty channel"
        );
        assert_eq!(
            format!("{}", TryRecvError::Disconnected),
            "receiving on a closed channel"
        );
    }

    #[test]
    fn test_send_error_into_try_send() {
        let send_err = SendError(42);
        let try_err: TrySendError<i32> = send_err.into();
        assert!(try_err.is_disconnected());
        assert_eq!(try_err.into_inner(), 42);
    }

    #[test]
    fn test_recv_error_into_try_recv() {
        let recv_err = RecvError;
        let try_err: TryRecvError = recv_err.into();
        assert!(try_err.is_disconnected());
    }
}
