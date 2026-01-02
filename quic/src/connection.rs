//! QUIC connection state machine

use crate::error::{Error, Result};
use crate::monotonic_nanos;
use land_cpu::CachePadded;
use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// maximum packet size (MTU)
pub const MAX_PACKET_SIZE: usize = 1350;

/// transaction queue capacity
pub const DEFAULT_TX_QUEUE_CAPACITY: usize = 16;

/// pre-allocated lock-free SPSC ring buffer used during handshake.
///
/// # safety
///
/// single-producer / single-consumer only
/// - producer writes slot data, then publishes by advancing `head` (Release).
/// - consumer observes `head` (Acquire), copies out, then frees by advancing `tail` (Release).
///
/// violating SPSC (multiple producers/consumers) is UB.
#[repr(C, align(64))]
pub struct TransactionQueue<const N: usize = DEFAULT_TX_QUEUE_CAPACITY> {
    /// pre-allocated buffer
    buffers: Box<[UnsafeCell<[u8; MAX_PACKET_SIZE]>; N]>,
    /// length of valid data in each buffer
    lengths: Box<[UnsafeCell<u16>; N]>,
    /// producer index
    head: CachePadded<AtomicUsize>,
    /// consumer index
    tail: CachePadded<AtomicUsize>,
    /// count of items in queue
    count: CachePadded<AtomicUsize>,
}

// safety: SPSC contract ensures only one writer/reader per slot
unsafe impl<const N: usize> Sync for TransactionQueue<N> {}
unsafe impl<const N: usize> Send for TransactionQueue<N> {}

impl<const N: usize> TransactionQueue<N> {
    pub fn new() -> Self {
        Self {
            buffers: Box::new(std::array::from_fn(|_| {
                UnsafeCell::new([0u8; MAX_PACKET_SIZE])
            })),
            lengths: Box::new(std::array::from_fn(|_| UnsafeCell::new(0u16))),
            head: CachePadded::new(AtomicUsize::new(0)),
            tail: CachePadded::new(AtomicUsize::new(0)),
            count: CachePadded::new(AtomicUsize::new(0)),
        }
    }

    /// try to enqueue a transaction.
    /// lock-free
    #[inline(always)]
    pub fn try_enqueue(&self, data: &[u8]) -> crate::error::SendStatus {
        if data.len() > MAX_PACKET_SIZE {
            return crate::error::SendStatus::Dead { retries: 0 };
        }

        // check if full
        let count = self.count.load(Ordering::Acquire);
        if count >= N {
            return crate::error::SendStatus::QueueFull;
        }

        // reserve slot (atomic increment)
        let current_count = self.count.fetch_add(1, Ordering::AcqRel);
        if current_count >= N {
            // race: queue filled between check and increment
            self.count.fetch_sub(1, Ordering::Release);
            return crate::error::SendStatus::QueueFull;
        }

        // get head position
        let head = self.head.load(Ordering::Acquire);
        let slot = head % N;

        // safety: SPSC - producer has exclusive access to head slot
        unsafe {
            let buf_ptr = self.buffers[slot].get();
            (*buf_ptr)[..data.len()].copy_from_slice(data);

            let len_ptr = self.lengths[slot].get();
            *len_ptr = data.len() as u16;
        }

        // commit head (Release ensures visibility to consumer)
        self.head.fetch_add(1, Ordering::Release);

        crate::error::SendStatus::Queued {
            position: current_count as u8,
        }
    }

    /// dequeue next transaction. returning an owned copy (buffer, len).
    ///
    /// copying avoids returning a slice that could alias a slot after wrap-around.
    ///
    #[inline(always)]
    pub fn try_dequeue(&self) -> Option<([u8; MAX_PACKET_SIZE], usize)> {
        let count = self.count.load(Ordering::Acquire);
        if count == 0 {
            return None;
        }

        // reserve
        let prev_count = self.count.fetch_sub(1, Ordering::AcqRel);
        if prev_count == 0 {
            // race: emptied between check and decrement
            self.count.fetch_add(1, Ordering::Release);
            return None;
        }

        let tail = self.tail.load(Ordering::Acquire);
        let slot = tail % N;

        // safety: SPSC - consumer has exclusive access to tail slot after count decrement
        // memory ordering from head.fetch_add(Release) synchronizes with tail read
        let (buf, len) = unsafe {
            let len_ptr = self.lengths[slot].get();
            let len = *len_ptr as usize;

            let buf_ptr = self.buffers[slot].get();
            let buf = *buf_ptr; // copy entire buffer (stack-allocated)
            (buf, len)
        };

        // commit tail
        self.tail.fetch_add(1, Ordering::Release);

        Some((buf, len))
    }

    /// check if queue has pending transactions
    #[inline(always)]
    pub fn has_pending(&self) -> bool {
        self.count.load(Ordering::Relaxed) > 0
    }

    /// drain all pending transactions
    pub fn drain<F>(&self, mut f: F)
    where
        F: FnMut(&[u8]),
    {
        while let Some((buf, len)) = self.try_dequeue() {
            f(&buf[..len]);
        }
    }
}

/// connection states
#[repr(usize)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// connection handshake in progress
    Connecting = 0,
    /// connection established and ready
    Ready = 1,
    /// connection failed (can retry)
    Failed = 2,
    /// connection dead (max retries exceeded)
    Dead = 3,
}

impl From<usize> for ConnectionState {
    #[inline(always)]
    fn from(v: usize) -> Self {
        match v {
            0 => ConnectionState::Connecting,
            1 => ConnectionState::Ready,
            2 => ConnectionState::Failed,
            3 => ConnectionState::Dead,
            _ => ConnectionState::Dead,
        }
    }
}

/// QUIC connection wrapper with atomic state
pub struct QuicConnection {
    /// inner quiche connection
    inner: quiche::Connection,
    /// connection state (atomic for lock-free access)
    state: AtomicUsize,
    /// retry count
    retry_count: AtomicUsize,
    /// last activity timestamp
    last_activity_ns: AtomicU64,
    /// local unidirectional stream ID generator
    local_stream_uni_next: u64,
    /// transaction queue for handshake-time buffering
    tx_queue: TransactionQueue,
}

impl QuicConnection {
    /// create new connection wrapper
    #[inline]
    pub fn new(conn: quiche::Connection) -> Self {
        let now_ns = monotonic_nanos();
        let is_established = conn.is_established();

        Self {
            inner: conn,
            state: AtomicUsize::new(if is_established {
                ConnectionState::Ready as usize
            } else {
                ConnectionState::Connecting as usize
            }),
            retry_count: AtomicUsize::new(0),
            last_activity_ns: AtomicU64::new(now_ns),
            // client-initiated unidirectional streams start at 0x02
            local_stream_uni_next: 0x02,
            // initialize transaction queue
            tx_queue: TransactionQueue::new(),
        }
    }

    /// current state
    #[inline(always)]
    pub fn state(&self) -> ConnectionState {
        ConnectionState::from(self.state.load(Ordering::Acquire))
    }

    /// set state
    #[inline(always)]
    pub fn set_state(&self, state: ConnectionState) {
        self.state.store(state as usize, Ordering::Release);
    }

    /// check if connection is established
    #[inline(always)]
    pub fn is_established(&self) -> bool {
        self.inner.is_established()
    }

    /// check if handshake is confirmed (handshake_done)
    #[inline(always)]
    pub fn is_handshake_confirmed(&self) -> bool {
        self.inner.is_handshake_confirmed()
    }

    /// check if connection is closed
    #[inline(always)]
    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }

    /// get retry count
    #[inline(always)]
    pub fn retry_count(&self) -> usize {
        self.retry_count.load(Ordering::Relaxed)
    }

    /// increment retry count and return new value
    #[inline]
    pub fn increment_retry(&self) -> usize {
        self.retry_count.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// reset retry count
    #[inline]
    pub fn reset_retry(&self) {
        self.retry_count.store(0, Ordering::Release);
    }

    /// update last activity timestamp (vDSO-optimized, no syscall)
    #[inline(always)]
    pub fn touch(&self) {
        self.last_activity_ns
            .store(monotonic_nanos(), Ordering::Release);
    }

    /// get last activity timestamp
    #[inline(always)]
    pub fn last_activity_ns(&self) -> u64 {
        self.last_activity_ns.load(Ordering::Acquire)
    }

    /// get reference to transaction queue (interior mutability via UnsafeCell)
    #[inline(always)]
    pub fn tx_queue(&self) -> &TransactionQueue {
        &self.tx_queue
    }

    /// open a new unidirectional stream and send data
    #[inline]
    pub fn send_uni(&mut self, data: &[u8]) -> Result<()> {
        if self.inner.is_closed() {
            return Err(Error::Closed);
        }

        // allow sending if:
        // connection is fully established, OR
        // connection is in early data (0-RTT)
        //
        // for new connections without 0-RTT, we must wait for handshake to complete
        // before we can send stream data. the caller should retry when the connection
        // becomes established (signaled via poll loop).
        if !self.inner.is_established() && !self.inner.is_in_early_data() {
            return Err(Error::NotEstablished);
        }

        // // check if we can open a new stream
        // if self.inner.peer_streams_left_uni() == 0 {
        //     return Err(Error::WouldBlock);
        // }

        let stream_id = self.local_stream_uni_next;

        // check if stream is already in use
        let stream_writable = self.inner.stream_writable(stream_id, 1);
        let stream_finished = self.inner.stream_finished(stream_id);

        log::debug!(
            "send_uni: attempting stream {} (local_next={}, streams_left={}, writable={:?}, finished={})",
            stream_id,
            self.local_stream_uni_next,
            self.inner.peer_streams_left_uni(),
            stream_writable,
            stream_finished
        );

        let stream_capacity = self.inner.stream_capacity(stream_id).unwrap_or(0);

        // send data on the stream with FIN flag
        match self.inner.stream_send(stream_id, data, true) {
            Ok(written) => {
                log::debug!(
                    "send_uni: queued {} bytes on stream {} (requested {} bytes, streams_left={}, stream_capacity_before={}) established={}",
                    written,
                    stream_id,
                    data.len(),
                    self.inner.peer_streams_left_uni(),
                    stream_capacity,
                    self.inner.is_established()
                );
                // increment stream ID *after* successful send (fix stream ID waste on WouldBlock)
                let old_next = self.local_stream_uni_next;
                self.local_stream_uni_next += 4; // next client-initiated uni stream
                log::debug!(
                    "send_uni: incremented stream counter from {} to {} after successful send",
                    old_next,
                    self.local_stream_uni_next
                );
                self.touch();
                Ok(())
            }
            Err(quiche::Error::Done) => {
                log::warn!(
                    "send_uni: stream_send returned Done for stream {} ({} bytes, streams_left={})",
                    stream_id,
                    data.len(),
                    self.inner.peer_streams_left_uni()
                );
                Err(Error::WouldBlock)
            }
            Err(e) => {
                log::error!(
                    "send_uni: stream_send error: {:?} (stream {}, {} bytes)",
                    e,
                    stream_id,
                    data.len()
                );
                Err(Error::Quic(e))
            }
        }
    }

    /// process received data with recv info
    #[inline]
    pub fn recv(&mut self, buf: &mut [u8], info: quiche::RecvInfo) -> Result<usize> {
        match self.inner.recv(buf, info) {
            Ok(len) => {
                self.touch();
                Ok(len)
            }
            Err(quiche::Error::Done) => Err(Error::WouldBlock),
            Err(e) => Err(Error::Quic(e)),
        }
    }

    /// generate packets to send
    #[inline]
    pub fn send(&mut self, out: &mut [u8]) -> Result<(usize, quiche::SendInfo)> {
        match self.inner.send(out) {
            Ok((len, info)) => Ok((len, info)),
            Err(quiche::Error::Done) => Err(Error::WouldBlock),
            Err(e) => Err(Error::Quic(e)),
        }
    }

    /// get connection timeout
    #[inline(always)]
    pub fn timeout(&self) -> Option<std::time::Duration> {
        self.inner.timeout()
    }

    /// handle timeout
    #[inline]
    pub fn on_timeout(&mut self) {
        self.inner.on_timeout();
    }

    /// get inner connection
    #[inline(always)]
    pub fn inner(&self) -> &quiche::Connection {
        &self.inner
    }

    /// get mutable inner connection
    #[inline(always)]
    pub fn inner_mut(&mut self) -> &mut quiche::Connection {
        &mut self.inner
    }

    // /// read data from a readable stream
    // ///
    // /// returns Ok(Some((stream_id, data))) if data was read,
    // /// Ok(None) if stream finished, or Err on error.
    // #[inline]
    // pub fn read_stream(&mut self, stream_id: u64, buf: &mut [u8]) -> Result<Option<usize>> {
    //     match self.inner.stream_recv(stream_id, buf) {
    //         Ok((len, fin)) => {
    //             self.touch();
    //             if fin {
    //                 log::debug!("Stream {} finished", stream_id);
    //             }
    //             Ok(Some(len))
    //         }
    //         Err(quiche::Error::Done) => Ok(None),
    //         Err(e) => Err(Error::Quic(e)),
    //     }
    // }

    // /// get iterator over readable streams
    // #[inline]
    // pub fn readable_streams(&self) -> impl Iterator<Item = u64> + '_ {
    //     self.inner.readable()
    // }
}
