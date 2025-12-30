use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

// slot number newtype for type safety
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Slot(pub u64);

impl Slot {
    #[inline]
    pub const fn new(slot: u64) -> Self {
        Self(slot)
    }

    #[inline]
    pub const fn get(self) -> u64 {
        self.0
    }
}

// subscription id newtype
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubscriptionId(pub u64);

impl SubscriptionId {
    #[inline]
    pub const fn new(id: u64) -> Self {
        Self(id)
    }

    #[inline]
    pub const fn get(self) -> u64 {
        self.0
    }
}

// basic slot information (24 bytes, copy-able)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotInfo {
    pub slot: u64,
    pub parent: u64,
    pub root: u64,
}

impl Default for SlotInfo {
    fn default() -> Self {
        Self {
            slot: 0,
            parent: 0,
            root: 0,
        }
    }
}

// cache-aligned connection state
#[repr(align(64))]
pub struct ConnectionState {
    connected: AtomicBool,
    last_slot: AtomicU64,
}

impl ConnectionState {
    pub fn new() -> Self {
        Self {
            connected: AtomicBool::new(false),
            last_slot: AtomicU64::new(0),
        }
    }

    #[inline]
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    #[inline]
    pub fn set_connected(&self, connected: bool) {
        self.connected.store(connected, Ordering::Release);
    }

    #[inline]
    pub fn last_slot(&self) -> u64 {
        self.last_slot.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn set_last_slot(&self, slot: u64) {
        self.last_slot.store(slot, Ordering::Relaxed);
    }
}

impl Default for ConnectionState {
    fn default() -> Self {
        Self::new()
    }
}

// compile-time size checks
const _: () = {
    assert!(std::mem::size_of::<SlotInfo>() == 24);
    assert!(std::mem::size_of::<Slot>() == 8);
    assert!(std::mem::size_of::<SubscriptionId>() == 8);
};
