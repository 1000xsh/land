//! session token cache for 0-RTT connection resumption
//!
//! lock-free session token cache using SeqLock.
//! cache is designed for a single-writer (prewarm
//! thread) and multi-reader (main send thread) scenario.

use land_sync::SeqLock;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// maximum number of cached session tokens
const MAX_CACHE_SIZE: usize = 128;

/// number of shards for lock-free parallel access
const SHARDS: usize = 64;

/// slots per shard (128 / 64 = 2 slots per shard)
const SLOTS_PER_SHARD: usize = MAX_CACHE_SIZE / SHARDS;

/// session ticket TTL in nanoseconds (1 hour)
const SESSION_TTL_NS: u64 = 3_600_000_000_000;

/// pack SocketAddr into u64 for atomic storage and comparison
#[inline(always)]
fn pack_addr(addr: SocketAddr) -> u64 {
    match addr {
        SocketAddr::V4(v4) => {
            // IPv4: 32 bits IP + 16 bits port = 48 bits total
            let ip = u32::from_be_bytes(v4.ip().octets()) as u64;
            let port = v4.port() as u64;
            (ip << 16) | port
        }
        SocketAddr::V6(v6) => {
            // IPv6: hash to 48 bits (loses information but acceptable for cache lookup)
            let segments = v6.ip().segments();
            let port = v6.port() as u64;
            let mut hash = 2166136261u64;
            for seg in segments {
                hash = (hash ^ seg as u64).wrapping_mul(1099511628211);
            }
            hash = (hash ^ port).wrapping_mul(1099511628211);
            hash & 0xFFFF_FFFF_FFFF // keep 48 bits
        }
    }
}

/// unpack u64 back to SocketAddr (only works for IPv4, IPv6 needs original)
#[inline(always)]
#[allow(dead_code)]
fn unpack_addr_v4(packed: u64) -> SocketAddr {
    let port = (packed & 0xFFFF) as u16;
    let ip = (packed >> 16) as u32;
    SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::from(ip)), port)
}

/// fixed-size session token (copy-compatible for SeqLock)
///
/// quiche session tickets can be 200-600+ bytes depending on server.
/// we use a 1024-byte buffer to accommodate larger tickets.
/// todo
#[derive(Copy, Clone)]
pub struct SessionToken {
    /// session data from quiche::Connection::session()
    pub data: [u8; 1024],
    /// actual length of valid data in buffer
    pub len: usize,
    /// zimestamp when token was created (for expiry)
    pub timestamp_ns: u64,
}

impl SessionToken {
    /// create empty/invalid session token
    const fn empty() -> Self {
        Self {
            data: [0u8; 1024],
            len: 0,
            timestamp_ns: 0,
        }
    }

    /// check if token is valid (non-empty and not expired)
    #[inline]
    pub fn is_valid(&self, now_ns: u64) -> bool {
        self.len > 0 && (now_ns.saturating_sub(self.timestamp_ns) < SESSION_TTL_NS)
    }

    /// get token data as slice
    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        &self.data[..self.len]
    }
}

/// session cache entry with address mapping
#[derive(Copy, Clone)]
pub struct SessionCacheEntry {
    /// peer address (for validation)
    pub addr: SocketAddr,
    /// session token data
    pub token: SessionToken,
    /// whether this entry is occupied
    pub valid: bool,
}

impl SessionCacheEntry {
    /// create empty cache entry
    #[allow(dead_code)]
    const fn empty() -> Self {
        Self {
            addr: SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::new(0, 0, 0, 0)), 0),
            token: SessionToken::empty(),
            valid: false,
        }
    }
}

/// lock-free atomic slot for session cache (cache-line aligned)
#[repr(C, align(64))]
struct AtomicSlot {
    /// packed SocketAddr (0 = empty)
    addr: AtomicU64,
    /// session token (lock-free read via SeqLock)
    token: SeqLock<SessionToken>,
    /// version for ABA prevention
    version: AtomicU64,
}

impl AtomicSlot {
    const fn new() -> Self {
        Self {
            addr: AtomicU64::new(0),
            token: SeqLock::new(SessionToken::empty()),
            version: AtomicU64::new(0),
        }
    }
}

/// cache-line aligned shard containing multiple slots
#[repr(align(64))]
struct Shard {
    slots: [AtomicSlot; SLOTS_PER_SHARD],
}

impl Shard {
    const fn new() -> Self {
        Self {
            slots: [AtomicSlot::new(), AtomicSlot::new()],
        }
    }
}

/// lock-free session token cache with sharding
///
/// - sharded design (64 shards × 2 slots = 128 total)
/// - lock-free reads: single atomic load + SeqLock read (~5-20ns)
/// - lock-free writes: CAS to claim slot + SeqLock write
/// - SIMD-friendly: sequential scan of 2-slot shard
pub struct SessionTokenCache {
    /// sharded atomic slots
    shards: Box<[Shard; SHARDS]>,
    /// global eviction counter (for LRU-ish behavior)
    evict_counter: AtomicUsize,
}

impl SessionTokenCache {
    /// create new lock-free session token cache
    pub fn new() -> Self {
        const EMPTY_SHARD: Shard = Shard::new();
        let shards = Box::new([EMPTY_SHARD; SHARDS]);

        Self {
            shards,
            evict_counter: AtomicUsize::new(0),
        }
    }

    /// get shard index for address (hash-based sharding)
    #[inline(always)]
    fn shard_for(&self, addr: SocketAddr) -> usize {
        let packed = pack_addr(addr);
        (packed as usize) % SHARDS
    }

    /// get session token for address
    ///
    /// returns None if no valid token exists or token is expired.
    #[inline]
    pub fn get(&self, addr: SocketAddr) -> Option<SessionToken> {
        let shard_idx = self.shard_for(addr);
        let shard = &self.shards[shard_idx];
        let target_addr = pack_addr(addr);

        // SIMD-friendly linear scan of shard (only 2 slots per shard)
        for slot in &shard.slots {
            // single atomic load
            let slot_addr = slot.addr.load(Ordering::Acquire);

            if slot_addr == target_addr {
                // found matching address - read token via SeqLock
                let token = slot.token.read();

                // validate token
                let now_ns = crate::monotonic_nanos();
                if token.is_valid(now_ns) {
                    let age_s = (now_ns - token.timestamp_ns) as f64 / 1e9;
                    log::debug!(
                        "session_cache::get: found for {} (age: {:.1}s, {} bytes)",
                        addr,
                        age_s,
                        token.len
                    );
                    return Some(token);
                } else {
                    let age_s = (now_ns - token.timestamp_ns) as f64 / 1e9;
                    log::debug!(
                        "session_cache::get: expired for {} (age: {:.1}s)",
                        addr,
                        age_s
                    );
                    return None;
                }
            }
        }

        log::debug!("session_cache::get: no entry for {}", addr);
        None
    }

    /// insert or update session token (lock-free with CAS)
    pub fn insert(&self, addr: SocketAddr, token_data: &[u8]) -> Result<(), &'static str> {
        if token_data.len() > 1024 {
            log::warn!(
                "Session token too large: {} bytes (max 1024)",
                token_data.len()
            );
            return Err("Session token exceeds maximum size");
        }

        let now_ns = crate::monotonic_nanos();
        let shard_idx = self.shard_for(addr);
        let shard = &self.shards[shard_idx];
        let target_addr = pack_addr(addr);

        // create session token
        let mut token = SessionToken::empty();
        token.data[..token_data.len()].copy_from_slice(token_data);
        token.len = token_data.len();
        token.timestamp_ns = now_ns;

        // try to find existing slot or claim empty slot
        for slot in &shard.slots {
            let current_addr = slot.addr.load(Ordering::Acquire);

            // update existing entry
            if current_addr == target_addr {
                slot.token.write(token);
                slot.version.fetch_add(1, Ordering::Release);
                log::debug!(
                    "SessionTokenCache: updated token for {} ({} bytes)",
                    addr,
                    token_data.len()
                );
                return Ok(());
            }

            // claim empty slot
            if current_addr == 0 {
                match slot.addr.compare_exchange(
                    0,
                    target_addr,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => {
                        slot.token.write(token);
                        slot.version.fetch_add(1, Ordering::Release);
                        log::debug!(
                            "SessionTokenCache: inserted token for {} ({} bytes)",
                            addr,
                            token_data.len()
                        );
                        return Ok(());
                    }
                    Err(_) => continue, // lost CAS race, try next slot
                }
            }
        }

        // no empty slots - evict oldest based on global counter
        let evict_slot_idx = self.evict_counter.fetch_add(1, Ordering::Relaxed) % SLOTS_PER_SHARD;
        let slot = &shard.slots[evict_slot_idx];

        // overwrite slot (no CAS needed for eviction)
        slot.addr.store(target_addr, Ordering::Release);
        slot.token.write(token);
        slot.version.fetch_add(1, Ordering::Release);

        log::debug!(
            "SessionTokenCache: evicted and inserted token for {} ({} bytes)",
            addr,
            token_data.len()
        );

        Ok(())
    }

    /// invalidate session token for address (lock-free)
    pub fn invalidate(&self, addr: SocketAddr) {
        let shard_idx = self.shard_for(addr);
        let shard = &self.shards[shard_idx];
        let target_addr = pack_addr(addr);

        // find and clear matching slot
        for slot in &shard.slots {
            let current_addr = slot.addr.load(Ordering::Acquire);
            if current_addr == target_addr {
                slot.addr.store(0, Ordering::Release); // mark as empty
                slot.version.fetch_add(1, Ordering::Release);
                log::debug!("SessionTokenCache: invalidated token for {}", addr);
                return;
            }
        }

        log::debug!("SessionTokenCache: no token to invalidate for {}", addr);
    }

    /// get cache statistics (for debugging)
    pub fn stats(&self) -> CacheStats {
        let mut entries_used = 0;
        let mut expired = 0;
        let now_ns = crate::monotonic_nanos();

        // count occupied and expired slots across all shards
        for shard in self.shards.iter() {
            for slot in &shard.slots {
                let addr_val = slot.addr.load(Ordering::Acquire);
                if addr_val != 0 {
                    entries_used += 1;

                    // check if expired
                    if let Some(token) = slot.token.try_read() {
                        if !token.is_valid(now_ns) {
                            expired += 1;
                        }
                    }
                }
            }
        }

        CacheStats {
            total_capacity: MAX_CACHE_SIZE,
            entries_used,
            entries_expired: expired,
        }
    }
}

impl Default for SessionTokenCache {
    fn default() -> Self {
        Self::new()
    }
}

/// cache statistics
#[derive(Debug, Clone, Copy)]
pub struct CacheStats {
    /// total cache capacity
    pub total_capacity: usize,
    /// number of entries currently in use
    pub entries_used: usize,
    /// number of expired entries
    pub entries_expired: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_token_empty() {
        let token = SessionToken::empty();
        assert_eq!(token.len, 0);
        assert!(!token.is_valid(crate::monotonic_nanos()));
    }

    #[test]
    fn test_session_token_validity() {
        let mut token = SessionToken::empty();
        token.len = 10;
        token.timestamp_ns = crate::monotonic_nanos();

        assert!(token.is_valid(crate::monotonic_nanos()));

        // test expiry
        let far_future = crate::monotonic_nanos() + SESSION_TTL_NS + 1;
        assert!(!token.is_valid(far_future));
    }

    #[test]
    fn test_cache_insert_and_get() {
        let cache = SessionTokenCache::new();
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let token_data = b"test_session_token_data";

        // insert
        cache.insert(addr, token_data).unwrap();

        // get
        let retrieved = cache.get(addr).unwrap();
        assert_eq!(retrieved.len, token_data.len());
        assert_eq!(&retrieved.data[..retrieved.len], token_data);
    }

    #[test]
    fn test_cache_invalidate() {
        let cache = SessionTokenCache::new();
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let token_data = b"test_token";

        cache.insert(addr, token_data).unwrap();
        assert!(cache.get(addr).is_some());

        cache.invalidate(addr);
        assert!(cache.get(addr).is_none());
    }

    #[test]
    fn test_cache_update_existing() {
        let cache = SessionTokenCache::new();
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();

        // insert first token
        cache.insert(addr, b"token1").unwrap();
        let token1 = cache.get(addr).unwrap();
        assert_eq!(token1.len, 6);

        // update with new token
        cache.insert(addr, b"token2_longer").unwrap();
        let token2 = cache.get(addr).unwrap();
        assert_eq!(token2.len, 13);
    }

    #[test]
    fn test_cache_stats() {
        let cache = SessionTokenCache::new();
        let stats = cache.stats();

        assert_eq!(stats.total_capacity, MAX_CACHE_SIZE);
        assert_eq!(stats.entries_used, 0);

        // add some entries
        for i in 0..5 {
            let addr: SocketAddr = format!("127.0.0.1:{}", 8080 + i).parse().unwrap();
            cache.insert(addr, b"token").unwrap();
        }

        let stats = cache.stats();
        assert_eq!(stats.entries_used, 5);
    }

    #[test]
    fn test_token_too_large() {
        let cache = SessionTokenCache::new();
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let large_token = vec![0u8; 1025]; // exceeds 1024 byte limit

        let result = cache.insert(addr, &large_token);
        assert!(result.is_err());
    }
}
