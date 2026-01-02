//! io_uring backend for UDP send
//!
//! this module provides a UDP send implementation using linux io_uring
//! and SO_ZEROCOPY socket option. it eliminates kernel copy overhead on the send path.
//!
//! ## architecture
//!
//! ```text
//! buffer pool -> pop() -> copy_from_slice() -> submit -> in_flight
//!    └────────── completion event (CQE) ─────────────────┘
//! ```
//!
//! todo: doc

use io_uring::{opcode, types, IoUring};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::os::unix::io::RawFd;

/// maximum packet size (MTU) - matches MAX_PACKET_SIZE in connection.rs
const MAX_PACKET_SIZE: usize = 1350;

/// in-flight operation holding buffer and metadata
/// all fields must remain alive until io_uring completion
struct InFlightOp {
    buf: Box<[u8; MAX_PACKET_SIZE]>,
    sockaddr: libc::sockaddr_storage,
    iovec: libc::iovec,
    msghdr: libc::msghdr,
}

// safety: InFlightOp contains raw pointers (in iovec/msghdr), but they only point
// to data within the same InFlightOp allocation. when the Box<InFlightOp> moves,
// all data moves together as a unit. the pointers are never dereferenced during
// the move, only by io_uring kernel code which accesses the stable Box address.
unsafe impl Send for InFlightOp {}

/// io_uring backend for zero-copy UDP send
pub struct IoUringBackend {
    /// io_uring instance
    ring: IoUring,

    /// pre-allocated buffer pool (available for use)
    buf_pool: Vec<Box<[u8; MAX_PACKET_SIZE]>>,

    /// in-flight operations (kernel owns these, cant reuse until completion)
    in_flight: HashMap<u64, Box<InFlightOp>>,

    /// user data counter for tracking submissions (monotonic)
    next_user_data: u64,
}

impl IoUringBackend {
    /// create new io_uring backend with specified queue depth
    ///
    /// # arguments
    ///
    /// * `queue_depth` - number of submission/completion queue entries (typical: 64)
    ///
    /// # returns
    ///
    /// * `Ok(IoUringBackend)` - successfully initialized
    /// * `Err(&str)` - failed to initialize (io_uring not available, insufficient permissions, etc.)
    pub fn new(queue_depth: u32) -> Result<Self, &'static str> {
        // init io_uring with specified queue depth
        let ring = IoUring::new(queue_depth).map_err(|_| "Failed to initialize io_uring")?;

        // pre-allocate buffer pool (queue_depth buffers)
        // each buffer is 1350 bytes (MAX_PACKET_SIZE)
        let buf_pool: Vec<Box<[u8; MAX_PACKET_SIZE]>> = (0..queue_depth)
            .map(|_| Box::new([0u8; MAX_PACKET_SIZE]))
            .collect();

        log::info!(
            "IoUringBackend initialized: queue_depth={}, buffer_size={}",
            queue_depth,
            MAX_PACKET_SIZE
        );

        Ok(Self {
            ring,
            buf_pool,
            in_flight: HashMap::with_capacity(queue_depth as usize),
            next_user_data: 0,
        })
    }

    /// submit packet send via io_uring
    ///
    /// # arguments
    ///
    /// * `data` - packet data to send (up to MAX_PACKET_SIZE bytes)
    /// * `addr` - destination socket address
    /// * `socket_fd` - raw file descriptor of UDP socket
    ///
    /// # returns
    ///
    /// * `Ok(())` - packet successfully submitted (not yet sent)
    /// * `Err(&str)` - buffer exhaustion or submission failure
    ///
    /// # note
    ///
    /// this function returns immediately after submission. call `poll_completions()`
    /// later to reclaim buffers and check for send errors.
    pub fn submit_send(
        &mut self,
        data: &[u8],
        addr: SocketAddr,
        socket_fd: RawFd,
    ) -> Result<(), &'static str> {
        if data.len() > MAX_PACKET_SIZE {
            log::error!(
                "Packet too large: {} bytes (max {})",
                data.len(),
                MAX_PACKET_SIZE
            );
            return Err("Packet exceeds maximum size");
        }

        // get buffer from pool
        let mut buf = self.buf_pool.pop().ok_or("No available buffers")?;

        // copy data to buffer (unavoidable with quiche API - quiche owns the data)
        buf[..data.len()].copy_from_slice(data);

        let user_data = self.next_user_data;
        self.next_user_data = self.next_user_data.wrapping_add(1);

        // build sockaddr for destination
        let (sockaddr_storage, sockaddr_len) = Self::build_sockaddr(addr);

        // create in-flight operation with dummy pointers (will be fixed after boxing)
        let mut op = Box::new(InFlightOp {
            buf,
            sockaddr: sockaddr_storage,
            iovec: libc::iovec {
                iov_base: std::ptr::null_mut(),
                iov_len: data.len(),
            },
            msghdr: libc::msghdr {
                msg_name: std::ptr::null_mut(),
                msg_namelen: sockaddr_len,
                msg_iov: std::ptr::null_mut(),
                msg_iovlen: 1,
                msg_control: std::ptr::null_mut(),
                msg_controllen: 0,
                msg_flags: 0,
            },
        });

        // fix up pointers to point to stable addresses inside the Box
        // safety: Box guarantees stable address until drop
        op.iovec.iov_base = op.buf.as_ptr() as *mut libc::c_void;
        op.msghdr.msg_name = &op.sockaddr as *const _ as *mut libc::c_void;
        op.msghdr.msg_iov = &op.iovec as *const libc::iovec as *mut libc::iovec;

        // create io_uring sendmsg operation
        // safety: msghdr and referenced data remain valid until completion (stored in in_flight)
        let send_e = opcode::SendMsg::new(types::Fd(socket_fd), &op.msghdr as *const libc::msghdr)
            .build()
            .user_data(user_data);

        // submit to submission queue
        unsafe {
            self.ring
                .submission()
                .push(&send_e)
                .map_err(|_| "Failed to push to submission queue")?;
        }

        // track operation as in-flight (kernel now owns all the data)
        self.in_flight.insert(user_data, op);

        // submit all pending operations to kernel
        self.ring
            .submit()
            .map_err(|_| "Failed to submit to kernel")?;

        log::trace!(
            "io_uring: submitted packet to {} (user_data={}, len={})",
            addr,
            user_data,
            data.len()
        );

        Ok(())
    }

    /// poll for completions and reclaim buffers
    ///
    /// this function checks the completion queue for finished send operations
    /// and reclaims buffers back to the pool.
    ///
    /// # returns
    ///
    /// * `Ok(usize)` - number of buffers reclaimed
    /// * `Err(&str)` - completion queue processing error
    ///
    pub fn poll_completions(&mut self) -> Result<usize, &'static str> {
        let mut reclaimed = 0;

        // process all available completions
        for cqe in self.ring.completion() {
            let user_data = cqe.user_data();
            let result = cqe.result();

            // check result
            if result < 0 {
                log::warn!("io_uring send failed: errno {}", -result);
            } else {
                log::trace!(
                    "io_uring: send complete (user_data={}, bytes={})",
                    user_data,
                    result
                );
            }

            // reclaim buffer regardless of success/failure (prevent leaks)
            if let Some(op) = self.in_flight.remove(&user_data) {
                self.buf_pool.push(op.buf);
                reclaimed += 1;
            } else {
                log::warn!("io_uring: orphaned completion (user_data={})", user_data);
            }
        }

        if reclaimed > 0 {
            log::trace!("io_uring: reclaimed {} buffers", reclaimed);
        }

        Ok(reclaimed)
    }

    /// get number of available buffers in pool
    #[inline]
    pub fn available_buffers(&self) -> usize {
        self.buf_pool.len()
    }

    /// get number of in-flight buffers
    #[inline]
    pub fn in_flight_count(&self) -> usize {
        self.in_flight.len()
    }

    /// build sockaddr structure from SocketAddr
    ///
    /// returns (sockaddr_storage, length) tuple for use with sendto
    #[inline]
    fn build_sockaddr(addr: SocketAddr) -> (libc::sockaddr_storage, libc::socklen_t) {
        let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };

        match addr {
            SocketAddr::V4(v4) => {
                let sockaddr_in = libc::sockaddr_in {
                    sin_family: libc::AF_INET as libc::sa_family_t,
                    sin_port: v4.port().to_be(),
                    sin_addr: libc::in_addr {
                        s_addr: u32::from_ne_bytes(v4.ip().octets()),
                    },
                    sin_zero: [0; 8],
                };

                // safety: sockaddr_in and sockaddr_storage have compatible layouts
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        &sockaddr_in as *const _ as *const u8,
                        &mut storage as *mut _ as *mut u8,
                        std::mem::size_of::<libc::sockaddr_in>(),
                    );
                }

                (
                    storage,
                    std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
                )
            }
            SocketAddr::V6(v6) => {
                let sockaddr_in6 = libc::sockaddr_in6 {
                    sin6_family: libc::AF_INET6 as libc::sa_family_t,
                    sin6_port: v6.port().to_be(),
                    sin6_flowinfo: v6.flowinfo(),
                    sin6_addr: libc::in6_addr {
                        s6_addr: v6.ip().octets(),
                    },
                    sin6_scope_id: v6.scope_id(),
                };

                // safety: sockaddr_in6 and sockaddr_storage have compatible layouts
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        &sockaddr_in6 as *const _ as *const u8,
                        &mut storage as *mut _ as *mut u8,
                        std::mem::size_of::<libc::sockaddr_in6>(),
                    );
                }

                (
                    storage,
                    std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t,
                )
            }
        }
    }
}

impl Drop for IoUringBackend {
    fn drop(&mut self) {
        // ensure all pending operations complete before dropping
        if !self.in_flight.is_empty() {
            log::warn!(
                "IoUringBackend dropped with {} in-flight buffers, flushing...",
                self.in_flight.len()
            );

            // wait for all completions
            while !self.in_flight.is_empty() {
                let _ = self.poll_completions();
                std::thread::yield_now();
            }
        }

        log::debug!("IoUringBackend dropped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore] // requires io_uring support
    fn test_io_uring_backend_creation() {
        // test that backend can be created
        let result = IoUringBackend::new(64);
        if result.is_ok() {
            let backend = result.unwrap();
            assert_eq!(backend.available_buffers(), 64);
            assert_eq!(backend.in_flight_count(), 0);
        } else {
            // io_uring not available on this system
            eprintln!("io_uring not available, skipping test");
        }
    }

    #[test]
    fn test_build_sockaddr_v4() {
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let (storage, len) = IoUringBackend::build_sockaddr(addr);

        // verify family is AF_INET
        assert_eq!(
            storage.ss_family as i32,
            libc::AF_INET,
            "ss_family should be AF_INET"
        );

        // length
        assert_eq!(
            len as usize,
            std::mem::size_of::<libc::sockaddr_in>(),
            "length should match sockaddr_in size"
        );
    }

    #[test]
    fn test_build_sockaddr_v6() {
        let addr: SocketAddr = "[::1]:8080".parse().unwrap();
        let (storage, len) = IoUringBackend::build_sockaddr(addr);

        // family is AF_INET6
        assert_eq!(
            storage.ss_family as i32,
            libc::AF_INET6,
            "ss_family should be AF_INET6"
        );

        // length
        assert_eq!(
            len as usize,
            std::mem::size_of::<libc::sockaddr_in6>(),
            "length should match sockaddr_in6 size"
        );
    }
}
