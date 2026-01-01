//! leader schedule fetcher.
//!
//! hot path is lock-free: atomic Arc load + array index.

use crate::leader_buffer::SLOTS_PER_LEADER;
use crate::leader_entry::LeaderEntry;
use arc_swap::ArcSwap;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

/// TPU addresses for a validator.
#[derive(Debug, Clone, Copy)]
pub struct TpuAddresses {
    pub tpu_quic: Option<SocketAddr>,
    pub tpu_quic_fwd: Option<SocketAddr>,
}

/// cached leader schedule for an epoch.
/// uses flat Vec indexed by (slot - epoch_start_slot) for O(1) lookup.
struct EpochSchedule {
    epoch: u64,
    epoch_start_slot: u64,
    slots_in_epoch: u64,
    /// flat array: index = slot - epoch_start_slot.
    /// pre-joined leader + TPU addresses for single lookup.
    leaders: Vec<LeaderEntry>,
}

/// leader schedule fetcher with lock-free reads via ArcSwap.
pub struct ScheduleFetcher {
    rpc_client: Arc<RpcClient>,
    /// ArcSwap for lock-free atomic reads.
    /// writer: refresh() swaps in new Arc.
    /// reader: load() returns Arc without blocking.
    cache: ArcSwap<Option<EpochSchedule>>,
}

impl ScheduleFetcher {
    pub fn new(rpc_client: Arc<RpcClient>) -> Self {
        Self {
            rpc_client,
            cache: ArcSwap::new(Arc::new(None)),
        }
    }

    /// fetch leader schedule for current epoch.
    /// slow path - called from background thread.
    pub async fn refresh(&self) -> Result<(), FetcherError> {
        // get epoch info.
        let epoch_info = self.rpc_client.get_epoch_info().await?;
        let current_epoch = epoch_info.epoch;
        let epoch_start_slot = epoch_info.absolute_slot - epoch_info.slot_index;
        let slots_in_epoch = epoch_info.slots_in_epoch;

        // check if we already have this epoch cached.
        {
            let cache = self.cache.load();
            if let Some(ref c) = **cache {
                if c.epoch == current_epoch {
                    return Ok(());
                }
            }
        }

        tracing::debug!("Fetching leader schedule for epoch {}", current_epoch);

        // fetch leader schedule.
        let schedule = self
            .rpc_client
            .get_leader_schedule(Some(epoch_start_slot))
            .await?
            .ok_or(FetcherError::NoSchedule)?;

        // build slot -> leader mapping (temp HashMap for construction).
        let mut slot_leaders: HashMap<u64, Pubkey> = HashMap::new();
        for (leader_str, slots) in &schedule {
            let leader: Pubkey = leader_str.parse().map_err(|_| FetcherError::InvalidPubkey)?;
            for &relative_slot in slots {
                let absolute_slot = epoch_start_slot + relative_slot as u64;
                slot_leaders.insert(absolute_slot, leader);
            }
        }

        // fetch cluster nodes for TPU addresses.
        let nodes = self.rpc_client.get_cluster_nodes().await?;

        let mut tpu_addresses: HashMap<Pubkey, TpuAddresses> = HashMap::new();
        for node in nodes {
            let pubkey: Pubkey = node.pubkey.parse().map_err(|_| FetcherError::InvalidPubkey)?;
            tpu_addresses.insert(
                pubkey,
                TpuAddresses {
                    tpu_quic: node.tpu_quic,
                    tpu_quic_fwd: node.tpu_forwards_quic,
                },
            );
        }

        // build flat Vec<LeaderEntry> indexed by slot offset.
        // pre-join leader pubkey + TPU addresses for single lookup.
        let mut leaders = Vec::with_capacity(slots_in_epoch as usize);
        for slot_offset in 0..slots_in_epoch {
            let absolute_slot = epoch_start_slot + slot_offset;
            let entry = if let Some(&leader) = slot_leaders.get(&absolute_slot) {
                let tpu = tpu_addresses.get(&leader).copied().unwrap_or(TpuAddresses {
                    tpu_quic: None,
                    tpu_quic_fwd: None,
                });
                if let (Some(tpu_quic), Some(tpu_quic_fwd)) = (tpu.tpu_quic, tpu.tpu_quic_fwd) {
                    LeaderEntry::new(leader, tpu_quic, tpu_quic_fwd)
                } else {
                    LeaderEntry::EMPTY
                }
            } else {
                LeaderEntry::EMPTY
            };
            leaders.push(entry);
        }

        let slot_count = slot_leaders.len();
        let tpu_count = tpu_addresses.len();

        // atomic swap.
        let new_schedule = EpochSchedule {
            epoch: current_epoch,
            epoch_start_slot,
            slots_in_epoch,
            leaders,
        };

        self.cache.store(Arc::new(Some(new_schedule)));

        tracing::debug!(
            "Loaded {} slot mappings, {} TPU addresses",
            slot_count,
            tpu_count
        );

        Ok(())
    }

    /// get current epoch from cache (lock-free).
    #[inline]
    pub fn current_epoch(&self) -> Option<u64> {
        let cache = self.cache.load();
        cache.as_ref().as_ref().map(|c| c.epoch)
    }

    /// check if slot is in cached epoch (lock-free).
    #[inline]
    pub fn is_slot_in_cache(&self, slot: u64) -> bool {
        let cache = self.cache.load();
        if let Some(ref c) = **cache {
            slot >= c.epoch_start_slot && slot < c.epoch_start_slot + c.slots_in_epoch
        } else {
            false
        }
    }

    /// get leader entry for a slot (lock-free).
    /// returns LeaderEntry directly via array index.
    #[inline]
    pub fn get_leader_at_slot(&self, slot: u64) -> Option<LeaderEntry> {
        let cache = self.cache.load();
        let c = cache.as_ref().as_ref()?;

        // bounds check.
        if slot < c.epoch_start_slot || slot >= c.epoch_start_slot + c.slots_in_epoch {
            return None;
        }

        // O(1) array access - no hashing, no pointer chasing.
        let index = (slot - c.epoch_start_slot) as usize;
        Some(c.leaders[index])
    }

    /// get next N leaders starting from slot.
    /// writes directly to callers buffer. returns number of entries written.
    #[inline]
    pub fn get_next_leaders_into(
        &self,
        start_slot: u64,
        buffer: &mut [LeaderEntry],
    ) -> usize {
        let cache = self.cache.load();
        let c = match cache.as_ref().as_ref() {
            Some(c) => c,
            None => return 0,
        };

        // align to leader boundary.
        let mut slot = (start_slot / SLOTS_PER_LEADER) * SLOTS_PER_LEADER;
        let max_slot = c.epoch_start_slot + c.slots_in_epoch;
        let mut written = 0;

        for entry in buffer.iter_mut() {
            if slot >= max_slot {
                break;
            }

            // bounds check for slot in epoch.
            if slot >= c.epoch_start_slot {
                // O(1) array access.
                let index = (slot - c.epoch_start_slot) as usize;
                *entry = c.leaders[index];
                written += 1;
            }

            slot += SLOTS_PER_LEADER;
        }

        written
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FetcherError {
    #[error("RPC error: {0}")]
    Rpc(#[from] solana_client::client_error::ClientError),
    #[error("No leader schedule returned")]
    NoSchedule,
    #[error("Invalid pubkey")]
    InvalidPubkey,
}
