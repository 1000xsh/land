//! lock-free leader buffer using SeqLock pattern.
//!
//! provides low-latency reads with single-writer semantics.
//! position 0 = current leader, position 1 = next leader
//! tracker.rs FIFO shift: shift buffer left and append new leaders (zero-allocation)
//!
//! safety and concurrency
//!
//! LeaderBuffer assumes a _single writer_ at all times.
//!
//! - multiple readers: safe (lock-free via SeqLock)
//! - single writer: safe (assumed by design)
//! - multiple writers: x unsafe (will corrupt version counter)
//!
//! the LeaderTracker ensures single-writer by design:
//! - only the busy-spin thread calls update/shift methods
//! - no public API exposes buffer writer access
//!
//! if you create LeaderBuffer directly, ensure only _one_ thread calls
//! update(), shift_and_append() or shift_multiple().
//! todo: force single writer by design
//!

use crate::leader_entry::LeaderEntry;
use land_cpu::{fence_acquire, CachePadded};
use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicU64, Ordering};

/// number of leaders in the buffer.
pub const LEADER_BUFFER_SIZE: usize = 10;

/// number of solana slots per leader.
pub const SLOTS_PER_LEADER: u64 = 4;

/// memory layout:
/// - version: 64 bytes (cache-line padded via CachePadded)
/// - entries: 10 * 64 = 640 bytes (each entry includes start_slot)
/// - current_slot: 64 bytes (cache-line padded via CachePadded)
///
/// total: ~768 bytes
#[repr(C)]
pub struct LeaderBuffer {
    /// SeqLock version counter.
    /// odd value = write in progress, even value = consistent state.
    version: CachePadded<AtomicU64>,
    /// fixed-size array of leader entries (each entry includes start_slot).
    entries: UnsafeCell<[LeaderEntry; LEADER_BUFFER_SIZE]>,
    /// current slot (cache-line padded to prevent false sharing).
    current_slot: CachePadded<AtomicU64>,
}

// safety: LeaderBuffer uses SeqLock for sync.
// single writer assumed, multiple readers supported.
unsafe impl Sync for LeaderBuffer {}
unsafe impl Send for LeaderBuffer {}

impl LeaderBuffer {
    /// create a new empty leader buffer.
    pub fn new() -> Self {
        Self {
            version: CachePadded::new(AtomicU64::new(0)),
            entries: UnsafeCell::new([LeaderEntry::EMPTY; LEADER_BUFFER_SIZE]),
            current_slot: CachePadded::new(AtomicU64::new(0)),
        }
    }

    /// begin a write operation (increment version to odd).
    #[inline]
    fn begin_write(&self) {
        let v = self.version.load(Ordering::Relaxed);
        self.version.store(v.wrapping_add(1), Ordering::Release);
    }

    /// end a write operation (increment version to even).
    #[inline]
    fn end_write(&self) {
        let v = self.version.load(Ordering::Relaxed);
        self.version.store(v.wrapping_add(1), Ordering::Release);
    }

    /// try to read a single leader at position (non-blocking).
    ///
    /// returns `None` if a write is in progress (caller should retry).
    #[inline]
    pub fn try_read(&self, position: usize) -> Option<LeaderEntry> {
        debug_assert!(position < LEADER_BUFFER_SIZE);

        // read version before.
        let v1 = self.version.load(Ordering::Acquire);
        if v1 & 1 == 1 {
            return None; // write in progress.
        }

        // read the entry.
        // safety: we check version consistency after read.
        let entry = unsafe { (*self.entries.get())[position] };

        // ensure data read completes before checking v2.
        // x86: compiler fence only (TSO provides load-load ordering).
        fence_acquire();

        // read version after.
        let v2 = self.version.load(Ordering::Acquire);
        if v1 != v2 {
            return None; // write happened during read.
        }

        Some(entry)
    }

    /// read a single leader at position (spin-wait until consistent).
    ///
    #[inline]
    pub fn read(&self, position: usize) -> LeaderEntry {
        loop {
            if let Some(entry) = self.try_read(position) {
                return entry;
            }
            // PAUSE: signals spin-wait to CPU, reduces speculation flush penalty.
            // cpu_pause();
        }
    }

    /// try to read all entries atomically.
    ///
    /// returns `None` if interrupted by a write.
    #[inline]
    pub fn try_read_all(&self) -> Option<[LeaderEntry; LEADER_BUFFER_SIZE]> {
        let v1 = self.version.load(Ordering::Acquire);
        if v1 & 1 == 1 {
            return None;
        }

        // safety: we check version consistency after read.
        let entries = unsafe { *self.entries.get() };

        fence_acquire();

        let v2 = self.version.load(Ordering::Acquire);
        if v1 != v2 {
            return None;
        }

        Some(entries)
    }

    /// read all entries (spin-wait until consistent).
    /// see `read()` for PAUSE rationale.
    #[inline]
    pub fn read_all(&self) -> [LeaderEntry; LEADER_BUFFER_SIZE] {
        loop {
            if let Some(entries) = self.try_read_all() {
                return entries;
            }
            // cpu_pause();
        }
    }

    /// get current slot (lock-free, always succeeds).
    #[inline]
    pub fn current_slot(&self) -> u64 {
        self.current_slot.load(Ordering::Acquire)
    }

    /// update current slot without modifying entries (no SeqLock needed).
    /// use this for slot updates that dont change the leader.
    #[inline]
    pub fn set_current_slot(&self, slot: u64) {
        self.current_slot.store(slot, Ordering::Release);
    }

    /// update all entries (single writer only).
    ///
    /// # safety
    /// must only be called from a single writer thread.
    pub fn update(&self, new_slot: u64, new_entries: &[LeaderEntry]) {
        // compute base slot _before_ entering critical section.
        let base_slot = (new_slot / SLOTS_PER_LEADER) * SLOTS_PER_LEADER;
        let copy_len = new_entries.len().min(LEADER_BUFFER_SIZE);

        self.begin_write();

        // safety: single writer assumed.
        let entries = unsafe { &mut *self.entries.get() };

        entries[..copy_len].copy_from_slice(&new_entries[..copy_len]);

        // clear remaining entries if new_entries is shorter.
        for entry in entries.iter_mut().skip(copy_len) {
            *entry = LeaderEntry::EMPTY;
        }

        // set start_slot on each entry (required for new entries from schedule).
        for (i, entry) in entries.iter_mut().enumerate() {
            entry.set_start_slot(base_slot + (i as u64) * SLOTS_PER_LEADER);
        }

        self.current_slot.store(new_slot, Ordering::Release);

        self.end_write();
    }

    /// fifo shift: remove first entry, shift all left, add new at end.
    ///
    /// # safety
    /// must only be called from a single writer thread.
    pub fn shift_and_append(&self, mut new_leader: LeaderEntry, new_slot: u64) {
        // compute slot for new entry _before_ entering critical section.
        let base_slot = (new_slot / SLOTS_PER_LEADER) * SLOTS_PER_LEADER;
        new_leader.set_start_slot(base_slot + ((LEADER_BUFFER_SIZE - 1) as u64) * SLOTS_PER_LEADER);

        self.begin_write();

        // safety: single writer assumed.
        let entries = unsafe { &mut *self.entries.get() };

        // shift entries left (position 0 removed, 1->0, 2->1, etc.).
        // shifted entries retain their start_slot values which remain correct.
        entries.copy_within(1..LEADER_BUFFER_SIZE, 0);

        // add new leader at end (already has correct start_slot).
        entries[LEADER_BUFFER_SIZE - 1] = new_leader;

        self.current_slot.store(new_slot, Ordering::Release);

        self.end_write();
    }

    /// shift multiple leaders at once (when skipping slots).
    ///
    /// # safety
    /// must only be called from a single writer thread.
    pub fn shift_multiple(&self, count: usize, new_leaders: &[LeaderEntry], new_slot: u64) {
        if count == 0 {
            return;
        }

        // compute base slot _before_ entering critical section.
        let base_slot = (new_slot / SLOTS_PER_LEADER) * SLOTS_PER_LEADER;

        self.begin_write();

        // safety: single writer assumed.
        let entries = unsafe { &mut *self.entries.get() };

        if count >= LEADER_BUFFER_SIZE {
            // full replacement - must set start_slot on all entries.
            let copy_len = new_leaders.len().min(LEADER_BUFFER_SIZE);
            entries[..copy_len].copy_from_slice(&new_leaders[..copy_len]);
            for entry in entries.iter_mut().skip(copy_len) {
                *entry = LeaderEntry::EMPTY;
            }
            // set start_slot on all entries (unavoidable for full replacement).
            for (i, entry) in entries.iter_mut().enumerate() {
                entry.set_start_slot(base_slot + (i as u64) * SLOTS_PER_LEADER);
            }
        } else {
            // shift left by `count` positions.
            // shifted entries retain their start_slot values which remain correct.
            entries.copy_within(count..LEADER_BUFFER_SIZE, 0);

            // fill the end with new leaders.
            let start_pos = LEADER_BUFFER_SIZE - count;
            let new_count = new_leaders.len().min(count);
            entries[start_pos..start_pos + new_count].copy_from_slice(&new_leaders[..new_count]);

            // clear any remaining entries.
            for entry in entries.iter_mut().skip(start_pos + new_count) {
                *entry = LeaderEntry::EMPTY;
            }

            // only set start_slot on new entries at the end (not shifted ones).
            for i in start_pos..LEADER_BUFFER_SIZE {
                entries[i].set_start_slot(base_slot + (i as u64) * SLOTS_PER_LEADER);
            }
        }

        self.current_slot.store(new_slot, Ordering::Release);

        self.end_write();
    }
}

impl Default for LeaderBuffer {
    fn default() -> Self {
        Self::new()
    }
}

// LeaderLookup trait implementation.
impl land_traits::LeaderLookup for LeaderBuffer {
    #[inline]
    fn get_leader(&self, index: usize) -> Option<(std::net::SocketAddr, std::net::SocketAddr)> {
        if index >= LEADER_BUFFER_SIZE {
            return None;
        }
        let entry = self.read(index); // SeqLock read
        if entry.is_valid() {
            Some((entry.tpu_quic(), entry.tpu_quic_fwd()))
        } else {
            None
        }
    }

    #[inline]
    fn is_valid(&self, index: usize) -> bool {
        index < LEADER_BUFFER_SIZE && self.read(index).is_valid()
    }

    #[inline]
    fn get_leader_slots(&self, index: usize) -> Option<(u64, u64)> {
        if index >= LEADER_BUFFER_SIZE {
            return None;
        }
        let entry = self.read(index); // SeqLock read, same cache line as addresses
        Some((entry.start_slot(), entry.end_slot()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_sdk::pubkey::Pubkey;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    fn make_entry(id: u8) -> LeaderEntry {
        let mut pubkey_bytes = [0u8; 32];
        pubkey_bytes[0] = id;
        let pubkey = Pubkey::new_from_array(pubkey_bytes);
        // avoid string allocation in tests.
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, id)), 8000);
        LeaderEntry::new(pubkey, addr, addr)
    }

    #[test]
    fn test_buffer_creation() {
        let buffer = LeaderBuffer::new();
        assert_eq!(buffer.current_slot(), 0);

        let entry = buffer.read(0);
        assert!(!entry.is_valid());
    }

    #[test]
    fn test_update() {
        let buffer = LeaderBuffer::new();

        let entries: Vec<LeaderEntry> = (1..=10).map(make_entry).collect();
        buffer.update(100, &entries);

        assert_eq!(buffer.current_slot(), 100);

        for i in 0..10 {
            let entry = buffer.read(i);
            assert!(entry.is_valid());
            assert_eq!(entry.pubkey.to_bytes()[0], (i + 1) as u8);
        }
    }

    #[test]
    fn test_shift_and_append() {
        let buffer = LeaderBuffer::new();

        // initial entries: 1, 2, 3, ..., 10
        let entries: Vec<LeaderEntry> = (1..=10).map(make_entry).collect();
        buffer.update(100, &entries);

        // shift and append entry 11.
        let new_entry = make_entry(11);
        buffer.shift_and_append(new_entry, 104);

        assert_eq!(buffer.current_slot(), 104);

        // now should be: 2, 3, 4, ..., 10, 11
        for i in 0..9 {
            let entry = buffer.read(i);
            assert_eq!(entry.pubkey.to_bytes()[0], (i + 2) as u8);
        }
        let last = buffer.read(9);
        assert_eq!(last.pubkey.to_bytes()[0], 11);
    }

    #[test]
    fn test_shift_multiple() {
        let buffer = LeaderBuffer::new();

        // initial entries: 1, 2, 3, ..., 10
        let entries: Vec<LeaderEntry> = (1..=10).map(make_entry).collect();
        buffer.update(100, &entries);

        // shift by 3, add entries 11, 12, 13.
        let new_entries: Vec<LeaderEntry> = (11..=13).map(make_entry).collect();
        buffer.shift_multiple(3, &new_entries, 112);

        assert_eq!(buffer.current_slot(), 112);

        // now should be: 4, 5, 6, 7, 8, 9, 10, 11, 12, 13
        for i in 0..7 {
            let entry = buffer.read(i);
            assert_eq!(entry.pubkey.to_bytes()[0], (i + 4) as u8);
        }
        for i in 7..10 {
            let entry = buffer.read(i);
            assert_eq!(entry.pubkey.to_bytes()[0], (i + 4) as u8);
        }
    }

    #[test]
    fn test_entry_start_slot() {
        let buffer = LeaderBuffer::new();

        let entries: Vec<LeaderEntry> = (1..=10).map(make_entry).collect();
        // current_slot = 100, so base_slot = 100 (aligned to SLOTS_PER_LEADER=4)
        buffer.update(100, &entries);

        // position 0 should have slot 100
        let entry = buffer.read(0);
        assert!(entry.is_valid());
        assert_eq!(entry.start_slot(), 100);
        assert_eq!(entry.end_slot(), 103);

        // position 1 should have slot 104
        let entry = buffer.read(1);
        assert!(entry.is_valid());
        assert_eq!(entry.start_slot(), 104);

        // position 9 should have slot 136
        let entry = buffer.read(9);
        assert!(entry.is_valid());
        assert_eq!(entry.start_slot(), 136);
    }

    #[test]
    fn test_read_all_with_embedded_slots() {
        let buffer = LeaderBuffer::new();

        let entries: Vec<LeaderEntry> = (1..=10).map(make_entry).collect();
        buffer.update(100, &entries);

        let read_entries = buffer.read_all();

        for i in 0..LEADER_BUFFER_SIZE {
            assert!(read_entries[i].is_valid());
            assert_eq!(read_entries[i].pubkey.to_bytes()[0], (i + 1) as u8);
            assert_eq!(
                read_entries[i].start_slot(),
                100 + (i as u64) * SLOTS_PER_LEADER
            );
        }
    }

    #[test]
    fn test_shift_and_append_updates_slots() {
        let buffer = LeaderBuffer::new();

        let entries: Vec<LeaderEntry> = (1..=10).map(make_entry).collect();
        buffer.update(100, &entries);

        // after shift_and_append with new_slot=104, position 0 should now have slot 104
        let new_entry = make_entry(11);
        buffer.shift_and_append(new_entry, 104);

        let entry = buffer.read(0);
        assert_eq!(entry.start_slot(), 104);

        // position 9 should have slot 140
        let entry = buffer.read(9);
        assert_eq!(entry.start_slot(), 140);
    }

    #[test]
    fn test_shift_multiple_updates_slots() {
        let buffer = LeaderBuffer::new();

        let entries: Vec<LeaderEntry> = (1..=10).map(make_entry).collect();
        buffer.update(100, &entries);

        // shift by 3 with new_slot=112, position 0 should have slot 112
        let new_entries: Vec<LeaderEntry> = (11..=13).map(make_entry).collect();
        buffer.shift_multiple(3, &new_entries, 112);

        let entry = buffer.read(0);
        assert_eq!(entry.start_slot(), 112);

        // position 9 should have slot 148
        let entry = buffer.read(9);
        assert_eq!(entry.start_slot(), 148);
    }

    #[test]
    fn test_slots_with_non_aligned_current_slot() {
        let buffer = LeaderBuffer::new();

        let entries: Vec<LeaderEntry> = (1..=10).map(make_entry).collect();
        // current_slot = 102, but base_slot should still be 100 (aligned)
        buffer.update(102, &entries);

        let entry = buffer.read(0);
        assert_eq!(entry.start_slot(), 100); // aligned down to leader boundary

        let entry = buffer.read(1);
        assert_eq!(entry.start_slot(), 104);
    }
}
