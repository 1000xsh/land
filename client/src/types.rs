//! types for land client.

/// options for sending a transaction.
#[derive(Debug, Clone)]
pub struct SendOptions {
    /// number of leaders to fan out to (default: 5)
    pub fanout: Option<usize>,
    /// target slot for timing (default: 0)
    pub target_slot: Option<u64>,
}

impl Default for SendOptions {
    #[inline]
    fn default() -> Self {
        Self {
            fanout: None,
            target_slot: None,
        }
    }
}

impl SendOptions {
    /// create new options with default values.
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// set fanout count.
    #[inline]
    pub fn fanout(mut self, n: usize) -> Self {
        self.fanout = Some(n);
        self
    }

    /// set target slot.
    #[inline]
    pub fn target_slot(mut self, slot: u64) -> Self {
        self.target_slot = Some(slot);
        self
    }

    /// get fanout with default.
    #[inline]
    pub fn get_fanout(&self) -> usize {
        self.fanout.unwrap_or(1)
    }

    /// get target slot with default.
    #[inline]
    pub fn get_target_slot(&self) -> u64 {
        self.target_slot.unwrap_or(0)
    }
}

/// result of a send operation.
#[derive(Debug, Clone)]
pub struct SendResult {
    /// request id assigned by the server
    pub request_id: u64,
}
