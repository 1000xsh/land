//! shared traits for land_* crates.
//!
use std::net::SocketAddr;

/// leader lookup - zero copy read from ring buffer.
///
/// returns (tpu_quic_addr, tpu_quic_fwd_addr) tuple.
/// designed for lock-free reads from SeqLock-based buffer.
pub trait LeaderLookup: Send + Sync {
    /// get leader at index. 0 = current, 1 = next, etc.
    ///
    /// returns `None` if slot invalid or index out of range.
    fn get_leader(&self, index: usize) -> Option<(SocketAddr, SocketAddr)>;

    /// check if leader at index is valid (has TPU address).
    fn is_valid(&self, index: usize) -> bool;

    /// get slot range for leader at index.
    ///
    /// returns (start_slot, end_slot) tuple. for solana, typically a range of 4 slots.
    /// returns `None` if index out of range.
    fn get_leader_slots(&self, index: usize) -> Option<(u64, u64)>;

    /// get neighbor TPU addresses for leader at index.
    ///
    /// returns a vector of (tpu_quic, tpu_quic_fwd) tuples for valid neighbors,
    /// sorted by geographic distance (closest first).
    /// returns empty vector if no neighbors or index out of range.
    ///
    /// default implementation returns empty vector for backward compatibility.
    fn get_leader_neighbors(&self, index: usize) -> Vec<(SocketAddr, SocketAddr)> {
        let _ = index;
        Vec::new()
    }
}

/// blanket impl for Arc<T> - zero cost, just forwards.
impl<T: LeaderLookup> LeaderLookup for std::sync::Arc<T> {
    #[inline]
    fn get_leader(&self, index: usize) -> Option<(SocketAddr, SocketAddr)> {
        (**self).get_leader(index)
    }

    #[inline]
    fn is_valid(&self, index: usize) -> bool {
        (**self).is_valid(index)
    }

    #[inline]
    fn get_leader_slots(&self, index: usize) -> Option<(u64, u64)> {
        (**self).get_leader_slots(index)
    }

    #[inline]
    fn get_leader_neighbors(&self, index: usize) -> Vec<(SocketAddr, SocketAddr)> {
        (**self).get_leader_neighbors(index)
    }
}
