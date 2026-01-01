//! cache-line aligned leader entry.

use solana_sdk::pubkey::Pubkey;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

/// cache line size
pub const CACHE_LINE_SIZE: usize = 64;

/// leader entry storing pubkey, TPU addresses and slot range.
///
/// memory layout (64 bytes, cache-line aligned):
/// - pubkey:           32 bytes (offset 0)
/// - tpu_quic_ip:       4 bytes (offset 32)
/// - tpu_quic_port:     2 bytes (offset 36)
/// - tpu_quic_fwd_ip:   4 bytes (offset 38)
/// - tpu_quic_fwd_port: 2 bytes (offset 42)
/// - (implicit pad):    4 bytes (offset 44, aligns u64)
/// - start_slot:        8 bytes (offset 48)
/// - _padding:          8 bytes (offset 56)
/// total: 64 bytes
#[repr(C, align(64))]
#[derive(Clone, Copy)]
pub struct LeaderEntry {
    /// leaders public key.
    pub pubkey: Pubkey,
    /// TPU QUIC IPv4 address bytes.
    tpu_quic_ip: [u8; 4],
    /// TPU QUIC port.
    tpu_quic_port: u16,
    /// TPU QUIC forward IPv4 address bytes.
    tpu_quic_fwd_ip: [u8; 4],
    /// TPU QUIC forward port.
    tpu_quic_fwd_port: u16,
    /// start slot for this leader (slot range = [start_slot, start_slot + 3]).
    start_slot: u64,
    /// padding to reach exactly 64 bytes.
    _padding: [u8; 8],
}

// compile-time size and alignment verification.
const _: () = {
    assert!(std::mem::size_of::<LeaderEntry>() == CACHE_LINE_SIZE);
    assert!(std::mem::align_of::<LeaderEntry>() == CACHE_LINE_SIZE);
};

impl LeaderEntry {
    /// empty/invalid entry constant.
    pub const EMPTY: Self = Self {
        pubkey: Pubkey::new_from_array([0u8; 32]),
        tpu_quic_ip: [0; 4],
        tpu_quic_port: 0,
        tpu_quic_fwd_ip: [0; 4],
        tpu_quic_fwd_port: 0,
        start_slot: 0,
        _padding: [0; 8],
    };

    /// create a new leader entry (start_slot defaults to 0, set via set_start_slot).
    #[inline]
    pub fn new(pubkey: Pubkey, tpu_quic: SocketAddr, tpu_quic_fwd: SocketAddr) -> Self {
        let (quic_ip, quic_port) = Self::extract_ipv4(tpu_quic);
        let (fwd_ip, fwd_port) = Self::extract_ipv4(tpu_quic_fwd);

        Self {
            pubkey,
            tpu_quic_ip: quic_ip,
            tpu_quic_port: quic_port,
            tpu_quic_fwd_ip: fwd_ip,
            tpu_quic_fwd_port: fwd_port,
            start_slot: 0,
            _padding: [0; 8],
        }
    }

    /// extract IPv4 address and port from SocketAddr.
    #[inline]
    fn extract_ipv4(addr: SocketAddr) -> ([u8; 4], u16) {
        match addr {
            SocketAddr::V4(v4) => (v4.ip().octets(), v4.port()),
            SocketAddr::V6(v6) => {
                // handle IPv4-mapped IPv6 addresses.
                if let Some(ipv4) = v6.ip().to_ipv4_mapped() {
                    (ipv4.octets(), v6.port())
                } else {
                    // fallback for pure IPv6.
                    // fixme
                    ([0; 4], v6.port())
                }
            }
        }
    }

    /// TPU QUIC socket address.
    #[inline]
    pub fn tpu_quic(&self) -> SocketAddr {
        SocketAddr::V4(SocketAddrV4::new(
            Ipv4Addr::from(self.tpu_quic_ip),
            self.tpu_quic_port,
        ))
    }

    /// TPU QUIC forward socket address.
    #[inline]
    pub fn tpu_quic_fwd(&self) -> SocketAddr {
        SocketAddr::V4(SocketAddrV4::new(
            Ipv4Addr::from(self.tpu_quic_fwd_ip),
            self.tpu_quic_fwd_port,
        ))
    }

    /// check if this entry is valid (has non-zero TPU port).
    #[inline]
    pub fn is_valid(&self) -> bool {
        self.tpu_quic_port != 0
    }

    /// get the start slot for this leader.
    #[inline]
    pub fn start_slot(&self) -> u64 {
        self.start_slot
    }

    /// get the end slot for this leader (start_slot + 3).
    #[inline]
    pub fn end_slot(&self) -> u64 {
        self.start_slot + 3
    }

    /// set the start slot for this leader.
    #[inline]
    pub fn set_start_slot(&mut self, slot: u64) {
        self.start_slot = slot;
    }
}

impl Default for LeaderEntry {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl std::fmt::Debug for LeaderEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeaderEntry")
            .field("pubkey", &self.pubkey)
            .field("tpu_quic", &self.tpu_quic())
            .field("tpu_quic_fwd", &self.tpu_quic_fwd())
            .field("start_slot", &self.start_slot)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_size_and_alignment() {
        assert_eq!(std::mem::size_of::<LeaderEntry>(), 64);
        assert_eq!(std::mem::align_of::<LeaderEntry>(), 64);
    }

    #[test]
    fn test_entry_creation() {
        let pubkey = Pubkey::new_unique();
        let tpu_quic: SocketAddr = "127.0.0.1:8000".parse().unwrap();
        let tpu_quic_fwd: SocketAddr = "127.0.0.1:8001".parse().unwrap();

        let entry = LeaderEntry::new(pubkey, tpu_quic, tpu_quic_fwd);

        assert_eq!(entry.pubkey, pubkey);
        assert_eq!(entry.tpu_quic(), tpu_quic);
        assert_eq!(entry.tpu_quic_fwd(), tpu_quic_fwd);
        assert!(entry.is_valid());
    }

    #[test]
    fn test_empty_entry() {
        let entry = LeaderEntry::EMPTY;
        assert!(!entry.is_valid());
    }
}
