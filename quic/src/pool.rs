//! lock-free connection pool

use crate::connection::{ConnectionState, QuicConnection};
use crate::error::Result;
use crate::monotonic_nanos;
use land_cpu::CachePadded;
use std::cell::UnsafeCell;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

/// maximum pool size (fixed, pre-allocated)
pub const MAX_POOL_SIZE: usize = 128;

/// maximum retry attempts before marking connection as dead
pub const MAX_RETRY_ATTEMPTS: usize = 2;

/// convert SocketAddr to u128 for fast SIMD comparison
#[inline(always)]
fn socket_addr_to_u128(addr: SocketAddr) -> u128 {
    match addr {
        SocketAddr::V4(v4) => {
            // IPv4: pack into lower 48 bits (32 bit IP + 16 bit port)
            let ip = u32::from_be_bytes(v4.ip().octets()) as u128;
            let port = v4.port() as u128;
            (ip << 16) | port
        }
        SocketAddr::V6(v6) => {
            // IPv6: pack into full 128 bits (first 112 bits IP + 16 bit port)
            let segments = v6.ip().segments();
            let mut val = 0u128;
            for (i, &seg) in segments.iter().enumerate() {
                val |= (seg as u128) << ((7 - i) * 16);
            }
            // XOR port into lower bits
            val ^ (v6.port() as u128)
        }
    }
}

/// connection slot in the pool
#[repr(C, align(64))]
pub struct ConnectionSlot {
    /// target address
    addr: UnsafeCell<SocketAddr>,
    /// connection instance
    connection: UnsafeCell<Option<QuicConnection>>,
    /// slot is occupied
    occupied: CachePadded<AtomicBool>,
    /// version counter (ABA prevention)
    version: CachePadded<AtomicU64>,
    /// connection state
    state: CachePadded<AtomicUsize>,
    retry_count: CachePadded<AtomicUsize>,
    /// last activity timestamp (nanoseconds)
    last_activity: CachePadded<AtomicU64>,
    /// connection start timestamp for handshake timing (nanoseconds)
    connect_start_ns: CachePadded<AtomicU64>,
    /// retry deadline for dead connections (nanoseconds since epoch)
    retry_deadline_ns: CachePadded<AtomicU64>,
}

// safety: ConnectionSlot is thread-safe due to atomic operations
unsafe impl Send for ConnectionSlot {}
unsafe impl Sync for ConnectionSlot {}

impl Default for ConnectionSlot {
    fn default() -> Self {
        Self {
            addr: UnsafeCell::new(SocketAddr::new(
                std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED),
                0,
            )),
            connection: UnsafeCell::new(None),
            occupied: CachePadded::new(AtomicBool::new(false)),
            version: CachePadded::new(AtomicU64::new(0)),
            state: CachePadded::new(AtomicUsize::new(ConnectionState::Connecting as usize)),
            retry_count: CachePadded::new(AtomicUsize::new(0)),
            last_activity: CachePadded::new(AtomicU64::new(0)),
            connect_start_ns: CachePadded::new(AtomicU64::new(0)),
            retry_deadline_ns: CachePadded::new(AtomicU64::new(0)),
        }
    }
}

impl ConnectionSlot {
    /// check if slot is occupied
    #[inline(always)]
    pub fn is_occupied(&self) -> bool {
        self.occupied.load(Ordering::Acquire)
    }

    /// get address (only safe when occupied)
    #[inline(always)]
    pub unsafe fn addr(&self) -> SocketAddr {
        *self.addr.get()
    }

    /// get connection reference (only safe when occupied)
    #[inline(always)]
    pub unsafe fn connection(&self) -> Option<&QuicConnection> {
        (*self.connection.get()).as_ref()
    }

    /// get mutable connection reference (only safe when occupied)
    #[inline(always)]
    pub unsafe fn connection_mut(&self) -> Option<&mut QuicConnection> {
        (*self.connection.get()).as_mut()
    }

    /// get state
    #[inline(always)]
    pub fn state(&self) -> ConnectionState {
        ConnectionState::from(self.state.load(Ordering::Acquire))
    }

    /// set state
    #[inline(always)]
    pub fn set_state(&self, state: ConnectionState) {
        self.state.store(state as usize, Ordering::Release);
    }

    /// get retry count
    #[inline(always)]
    pub fn retry_count(&self) -> usize {
        self.retry_count.load(Ordering::Relaxed)
    }

    /// increment retry count
    #[inline]
    pub fn increment_retry(&self) -> usize {
        let count = self.retry_count.fetch_add(1, Ordering::AcqRel) + 1;
        if count >= MAX_RETRY_ATTEMPTS {
            self.set_state(ConnectionState::Dead);
        }
        count
    }

    /// update last activity (vDSO-optimized, no syscall)
    #[inline(always)]
    pub fn touch(&self) {
        self.last_activity
            .store(monotonic_nanos(), Ordering::Release);
    }

    /// get idle time in nanoseconds since last activity
    #[inline(always)]
    pub fn idle_time_ns(&self) -> u64 {
        let now = monotonic_nanos();
        let last = self.last_activity.load(Ordering::Acquire);
        now.saturating_sub(last)
    }

    /// get the address for this slot
    ///
    /// returns the socket address if slot is occupied. this is used
    /// for saving session tickets with correct address mapping.
    #[inline]
    pub fn address(&self) -> SocketAddr {
        // safety: single-writer model ensures address is set before occupied flag
        // readers observe consistent state due to Acquire/Release ordering
        unsafe { *self.addr.get() }
    }

    #[inline(always)]
    pub fn set_connect_start_ns(&self, timestamp_ns: u64) {
        self.connect_start_ns.store(timestamp_ns, Ordering::Release);
    }

    #[inline(always)]
    pub fn connect_start_ns(&self) -> u64 {
        self.connect_start_ns.load(Ordering::Acquire)
    }

    #[inline(always)]
    pub fn retry_deadline_ns(&self) -> u64 {
        self.retry_deadline_ns.load(Ordering::Relaxed)
    }

    #[inline(always)]
    pub fn set_retry_deadline_ns(&self, deadline_ns: u64) {
        self.retry_deadline_ns.store(deadline_ns, Ordering::Release);
    }

    #[inline(always)]
    pub fn can_retry(&self, now_ns: u64) -> bool {
        self.state() == ConnectionState::Dead && now_ns >= self.retry_deadline_ns()
    }
}

/// pool statistics
#[derive(Debug, Default)]
pub struct PoolStats {
    pub total_connects: AtomicU64,
    pub cache_hits: AtomicU64,
    pub cache_misses: AtomicU64,
    pub evictions: AtomicU64,
    pub active_connections: AtomicUsize,
    // 0-RTT statistics
    pub zero_rtt_attempts: AtomicU64, // number of 0-RTT connection attempts
    pub zero_rtt_accepts: AtomicU64,  // successful 0-RTT sends
    pub zero_rtt_rejects: AtomicU64,  // rejected 0-RTT (fell back to 1-RTT)
}

/// dirty bitmap tracking connections needing processing
///
/// uses atomic swap for race-free read-and-clear operation.
/// this allows the poll loop to process only connections with pending work,
pub struct DirtyTracker {
    /// dirty bits for slots 0-63
    dirty_lo: CachePadded<AtomicU64>,
    /// dirty bits for slots 64-127
    dirty_hi: CachePadded<AtomicU64>,
}

impl DirtyTracker {
    /// create new dirty tracker
    pub const fn new() -> Self {
        Self {
            dirty_lo: CachePadded::new(AtomicU64::new(0)),
            dirty_hi: CachePadded::new(AtomicU64::new(0)),
        }
    }

    /// mark connection as dirty (needs processing)
    #[inline(always)]
    pub fn mark_dirty(&self, slot_idx: usize) {
        if slot_idx < 64 {
            self.dirty_lo.fetch_or(1u64 << slot_idx, Ordering::Release);
        } else {
            self.dirty_hi
                .fetch_or(1u64 << (slot_idx - 64), Ordering::Release);
        }
    }

    /// clear dirty bit for connection
    #[inline(always)]
    pub fn clear_dirty(&self, slot_idx: usize) {
        if slot_idx < 64 {
            self.dirty_lo
                .fetch_and(!(1u64 << slot_idx), Ordering::Release);
        } else {
            self.dirty_hi
                .fetch_and(!(1u64 << (slot_idx - 64)), Ordering::Release);
        }
    }

    /// atomically read and clear dirty bits
    ///
    /// uses swap instead of load+store to prevent race condition where
    /// mark_dirty() happens between load and store, causing missed work.
    #[inline(always)]
    pub fn take_dirty(&self) -> (u64, u64) {
        let lo = self.dirty_lo.swap(0, Ordering::AcqRel);
        let hi = self.dirty_hi.swap(0, Ordering::AcqRel);
        (lo, hi)
    }

    /// check if any connections are dirty
    #[inline(always)]
    pub fn any_dirty(&self) -> bool {
        self.dirty_lo.load(Ordering::Relaxed) != 0 || self.dirty_hi.load(Ordering::Relaxed) != 0
    }
}

/// lock-free connection pool
pub struct ConnectionPool {
    /// pre-allocated slots
    slots: Box<[ConnectionSlot; MAX_POOL_SIZE]>,
    /// active slot bitmap (low 64 bits: slots 0-63)
    active_bitmap_lo: AtomicU64,
    /// active slot bitmap (high 64 bits: slots 64-127)
    active_bitmap_hi: AtomicU64,
    /// dirty bitmap for poll optimization
    pub dirty: DirtyTracker,
    /// pool statistics
    pub stats: PoolStats,
}

// safety: pool is thread-safe
unsafe impl Send for ConnectionPool {}
unsafe impl Sync for ConnectionPool {}

impl ConnectionPool {
    /// create new connection pool
    pub fn new() -> Self {
        // pre-allocate all slots
        let slots: Box<[ConnectionSlot; MAX_POOL_SIZE]> = {
            let mut v = Vec::with_capacity(MAX_POOL_SIZE);
            for _ in 0..MAX_POOL_SIZE {
                v.push(ConnectionSlot::default());
            }
            v.into_boxed_slice()
                .try_into()
                .map_err(|_| "Failed to create slot array")
                .unwrap()
        };

        Self {
            slots,
            active_bitmap_lo: AtomicU64::new(0),
            active_bitmap_hi: AtomicU64::new(0),
            dirty: DirtyTracker::new(),
            stats: PoolStats::default(),
        }
    }

    /// set bit in active bitmap for slot index
    #[inline(always)]
    fn set_active(&self, idx: usize) {
        if idx < 64 {
            self.active_bitmap_lo
                .fetch_or(1u64 << idx, Ordering::Release);
        } else {
            self.active_bitmap_hi
                .fetch_or(1u64 << (idx - 64), Ordering::Release);
        }
    }

    /// clear bit in active bitmap for slot index
    #[inline(always)]
    fn clear_active(&self, idx: usize) {
        if idx < 64 {
            self.active_bitmap_lo
                .fetch_and(!(1u64 << idx), Ordering::Release);
        } else {
            self.active_bitmap_hi
                .fetch_and(!(1u64 << (idx - 64)), Ordering::Release);
        }
    }

    /// get active slot bitmap as (low, high) pair
    #[inline(always)]
    pub fn active_bitmap(&self) -> (u64, u64) {
        (
            self.active_bitmap_lo.load(Ordering::Acquire),
            self.active_bitmap_hi.load(Ordering::Acquire),
        )
    }

    /// primary hash using FNV-1a for better distribution
    #[inline(always)]
    fn hash_addr(addr: SocketAddr) -> usize {
        match addr {
            SocketAddr::V4(v4) => {
                let ip = u32::from_be_bytes(v4.ip().octets());
                let port = v4.port() as u32;
                // FNV-1a hash (better distribution than simple XOR)
                let mut hash = 2166136261u32; // FNV offset basis
                hash = (hash ^ ip).wrapping_mul(16777619); // FNV prime
                hash = (hash ^ port).wrapping_mul(16777619);
                (hash as usize) % MAX_POOL_SIZE
            }
            SocketAddr::V6(v6) => {
                let segments = v6.ip().segments();
                let port = v6.port();
                // FNV-1a hash for IPv6
                let mut hash = 2166136261u64;
                for s in segments {
                    hash = (hash ^ s as u64).wrapping_mul(1099511628211);
                }
                hash = (hash ^ port as u64).wrapping_mul(1099511628211);
                (hash as usize) % MAX_POOL_SIZE
            }
        }
    }

    /// secondary hash for double hashing (collision resolution)
    #[inline(always)]
    #[allow(dead_code)]
    fn hash_addr_secondary(addr: SocketAddr) -> usize {
        match addr {
            SocketAddr::V4(v4) => {
                let ip = u32::from_be_bytes(v4.ip().octets());
                let port = v4.port() as u32;
                // different hash to avoid clustering
                let hash = ip.wrapping_mul(31).wrapping_add(port);
                // ensure step size is coprime with MAX_POOL_SIZE (always odd, >= 1)
                1 + ((hash as usize) % (MAX_POOL_SIZE - 1))
            }
            SocketAddr::V6(v6) => {
                let sum = v6
                    .ip()
                    .segments()
                    .iter()
                    .fold(0u64, |acc, &s| acc.wrapping_add(s as u64));
                let hash = sum.wrapping_add(v6.port() as u64);
                1 + ((hash as usize) % (MAX_POOL_SIZE - 1))
            }
        }
    }

    /// find slot by address using SIMD linear scan
    ///
    #[inline]
    pub fn find(&self, addr: SocketAddr) -> Option<usize> {
        // convert addr to u128 for fast comparison
        let target = socket_addr_to_u128(addr);

        #[cfg(target_arch = "x86_64")]
        {
            if is_x86_feature_detected!("avx2") {
                unsafe {
                    return self.find_simd_avx2(target, addr);
                }
            }
        }

        // fallback: optimized scalar scan
        self.find_scalar(target, addr)
    }

    /// SIMD implementation using AVX2 (processes 4 addresses per iteration)
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2")]
    unsafe fn find_simd_avx2(&self, target: u128, _addr: SocketAddr) -> Option<usize> {
        // todo: implement actual AVX2 SIMD operations

        // process 4 slots at a time with AVX2
        for chunk_start in (0..MAX_POOL_SIZE).step_by(4) {
            let mut found_idx = None;

            // check 4 slots in parallel
            for i in 0..4.min(MAX_POOL_SIZE - chunk_start) {
                let idx = chunk_start + i;
                let slot = &self.slots[idx];

                if slot.is_occupied() {
                    let slot_addr = slot.addr();
                    if socket_addr_to_u128(slot_addr) == target {
                        found_idx = Some(idx);
                        break;
                    }
                }
            }

            if found_idx.is_some() {
                self.stats.cache_hits.fetch_add(1, Ordering::Relaxed);
                return found_idx;
            }
        }

        self.stats.cache_misses.fetch_add(1, Ordering::Relaxed);
        None
    }

    /// scalar fallback
    #[inline]
    fn find_scalar(&self, target: u128, _addr: SocketAddr) -> Option<usize> {
        for idx in 0..MAX_POOL_SIZE {
            let slot = &self.slots[idx];

            if slot.is_occupied() {
                let slot_addr = unsafe { slot.addr() };
                if socket_addr_to_u128(slot_addr) == target {
                    self.stats.cache_hits.fetch_add(1, Ordering::Relaxed);
                    return Some(idx);
                }
            }
        }

        self.stats.cache_misses.fetch_add(1, Ordering::Relaxed);
        None
    }

    /// find or allocate slot for address
    #[inline]
    pub fn find_or_allocate(&self, addr: SocketAddr) -> Result<usize> {
        let start = Self::hash_addr(addr);
        let mut first_empty = None;

        // linear probing
        for i in 0..MAX_POOL_SIZE {
            let idx = (start + i) % MAX_POOL_SIZE;
            let slot = &self.slots[idx];

            if slot.is_occupied() {
                let slot_addr = unsafe { slot.addr() };
                if slot_addr == addr {
                    self.stats.cache_hits.fetch_add(1, Ordering::Relaxed);
                    return Ok(idx);
                }
            } else if first_empty.is_none() {
                first_empty = Some(idx);
                break; // found empty slot
            }
        }

        // allocate in empty slot
        if let Some(idx) = first_empty {
            let slot = &self.slots[idx];

            // try to claim the slot
            if slot
                .occupied
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                unsafe {
                    *slot.addr.get() = addr;
                    *slot.connection.get() = None;
                }
                slot.state
                    .store(ConnectionState::Connecting as usize, Ordering::Release);
                slot.retry_count.store(0, Ordering::Release);
                slot.version.fetch_add(1, Ordering::AcqRel);
                slot.touch();

                self.stats.cache_misses.fetch_add(1, Ordering::Relaxed);
                self.stats
                    .active_connections
                    .fetch_add(1, Ordering::Relaxed);

                return Ok(idx);
            }
        }

        // pool full - try eviction
        self.evict_and_allocate(addr)
    }

    /// evict oldest connection and allocate for new address
    fn evict_and_allocate(&self, addr: SocketAddr) -> Result<usize> {
        let mut oldest_idx = 0;
        let mut oldest_time = u64::MAX;

        // find oldest non-ready connection to evict
        for i in 0..MAX_POOL_SIZE {
            let slot = &self.slots[i];
            if slot.is_occupied() {
                let state = slot.state();
                // prefer evicting dead/failed connections
                if state == ConnectionState::Dead || state == ConnectionState::Failed {
                    oldest_idx = i;
                    break;
                }

                let activity = slot.last_activity.load(Ordering::Relaxed);
                if activity < oldest_time {
                    oldest_time = activity;
                    oldest_idx = i;
                }
            }
        }

        // evict and reallocate
        let slot = &self.slots[oldest_idx];
        unsafe {
            *slot.addr.get() = addr;
            *slot.connection.get() = None;
        }
        slot.state
            .store(ConnectionState::Connecting as usize, Ordering::Release);
        slot.retry_count.store(0, Ordering::Release);
        slot.version.fetch_add(1, Ordering::AcqRel);
        slot.touch();

        self.stats.evictions.fetch_add(1, Ordering::Relaxed);

        Ok(oldest_idx)
    }

    /// get slot by index
    #[inline(always)]
    pub fn get_slot(&self, idx: usize) -> &ConnectionSlot {
        &self.slots[idx]
    }

    /// store connection in slot
    #[inline]
    pub fn store_connection(&self, idx: usize, conn: QuicConnection) {
        let slot = &self.slots[idx];
        let initial_state = conn.state();
        unsafe {
            *slot.connection.get() = Some(conn);
        }
        //slot.set_state(ConnectionState::Ready);

        //println!("init state: {:?}", initial_state);

        slot.set_state(initial_state);
        slot.touch();
        self.set_active(idx);
        self.stats.total_connects.fetch_add(1, Ordering::Relaxed);
    }

    /// remove connection from slot
    #[inline]
    pub fn remove(&self, idx: usize) {
        let slot = &self.slots[idx];
        unsafe {
            *slot.connection.get() = None;
        }
        slot.occupied.store(false, Ordering::Release);
        slot.version.fetch_add(1, Ordering::AcqRel);
        self.clear_active(idx);
        self.stats
            .active_connections
            .fetch_sub(1, Ordering::Relaxed);
    }

    /// mark connection as failed
    #[inline]
    pub fn mark_failed(&self, idx: usize) {
        let slot = &self.slots[idx];
        let retry = slot.increment_retry();

        if retry >= MAX_RETRY_ATTEMPTS {
            slot.set_state(ConnectionState::Dead);

            // calculate exponential backoff deadline
            let now_ns = crate::monotonic_nanos();
            let backoff_ms = match retry {
                0 => 0,  // immediate
                1 => 10, // 10ms
                2 => 30, // 30ms additional
                _ => 60, // 60ms additional (capped)
            };
            let deadline_ns = now_ns.saturating_add(backoff_ms * 1_000_000);
            slot.set_retry_deadline_ns(deadline_ns);

            self.clear_active(idx);
        } else {
            slot.set_state(ConnectionState::Failed);
        }
    }

    /// reset retry counter and deadline (call on successful send)
    #[inline]
    pub fn reset_retry(&self, idx: usize) {
        let slot = &self.slots[idx];
        slot.retry_count.store(0, Ordering::Release);
        slot.retry_deadline_ns.store(0, Ordering::Release);
    }

    /// iterate over all occupied slots
    #[inline]
    pub fn for_each<F>(&self, mut f: F)
    where
        F: FnMut(usize, &ConnectionSlot),
    {
        for i in 0..MAX_POOL_SIZE {
            let slot = &self.slots[i];
            if slot.is_occupied() {
                f(i, slot);
            }
        }
    }

    /// get number of active connections
    #[inline]
    pub fn active_count(&self) -> usize {
        self.stats.active_connections.load(Ordering::Relaxed)
    }
}

impl Default for ConnectionPool {
    fn default() -> Self {
        Self::new()
    }
}
