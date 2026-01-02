//! QUIC endpoint wrapper using quiche

use crate::config::Config;
use crate::connection::{ConnectionState, QuicConnection, MAX_PACKET_SIZE};
use crate::error::{Error, Result};
use crate::pool::ConnectionPool;
use crate::session_cache::SessionTokenCache;
use crate::strategy::AdaptivePrewarmConfig;
use crate::timing_registry::{TimingCategory, TimingRegistry};
use crate::tls::{Keypair, SOLANA_TPU_ALPN};
use mio::net::UdpSocket;
use ring::signature::KeyPair;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

/// maximum packets to batch in single recvmmsg/sendmmsg call
#[cfg(target_os = "linux")]
const BATCH_SIZE: usize = 16;

/// send buffer size (single packet)
const SEND_BUF_SIZE: usize = MAX_PACKET_SIZE;

/// receive buffer size (single packet)
const RECV_BUF_SIZE: usize = MAX_PACKET_SIZE;

/// convert libc sockaddr_storage to SocketAddr
#[cfg(target_os = "linux")]
unsafe fn sockaddr_to_socket_addr(addr: &libc::sockaddr_storage) -> Option<SocketAddr> {
    match addr.ss_family as libc::c_int {
        libc::AF_INET => {
            let addr4 = &*(addr as *const _ as *const libc::sockaddr_in);
            let ip = std::net::Ipv4Addr::from(u32::from_be(addr4.sin_addr.s_addr));
            let port = u16::from_be(addr4.sin_port);
            Some(SocketAddr::new(ip.into(), port))
        }
        libc::AF_INET6 => {
            let addr6 = &*(addr as *const _ as *const libc::sockaddr_in6);
            let ip = std::net::Ipv6Addr::from(addr6.sin6_addr.s6_addr);
            let port = u16::from_be(addr6.sin6_port);
            Some(SocketAddr::new(ip.into(), port))
        }
        _ => None,
    }
}

/// QUIC endpoint for connecting to validators
pub struct QuicEndpoint {
    /// UDP socket
    socket: UdpSocket,
    /// local address
    local_addr: SocketAddr,
    /// QUIC config
    quic_config: quiche::Config,
    /// connection pool
    pool: ConnectionPool,
    /// connection ID to pool index mapping (pre-allocated capacity)
    conn_id_map: HashMap<quiche::ConnectionId<'static>, usize>,
    /// shared session token cache for 0-RTT (optional)
    session_cache: Option<Arc<SessionTokenCache>>,
    /// connection timing registry for adaptive prewarming
    timing_registry: Arc<TimingRegistry>,
    /// adaptive prewarming configuration (optional)
    adaptive_config: Option<AdaptivePrewarmConfig>,
    /// pre-allocated send buffer
    send_buf: Box<[u8; SEND_BUF_SIZE]>,
    /// pre-allocated receive buffer
    #[allow(dead_code)]
    recv_buf: Box<[u8; RECV_BUF_SIZE]>,
    /// SCID counter for generating unique connection IDs
    scid_counter: u64,
    /// pre-allocated batch receive buffers
    #[cfg(target_os = "linux")]
    batch_recv_bufs: Box<[[u8; MAX_PACKET_SIZE]; BATCH_SIZE]>,
    /// io_uring backend for zero-copy UDP send. requires io_uring feature
    #[cfg(all(target_os = "linux", feature = "io_uring"))]
    io_uring: Option<crate::io_uring_backend::IoUringBackend>,
}

impl QuicEndpoint {
    /// create new QUIC endpoint
    pub fn new(
        bind_addr: SocketAddr,
        config: &Config,
        session_cache: Option<Arc<SessionTokenCache>>,
    ) -> Result<Self> {
        log::debug!("QuicEndpoint::new: binding to {}", bind_addr);

        // UDP socket
        let socket = UdpSocket::bind(bind_addr)?;
        let local_addr = socket.local_addr()?;
        log::debug!("QuicEndpoint::new: bound to {}", local_addr);

        // set socket options
        Self::configure_socket(&socket)?;
        log::debug!("QuicEndpoint::new: socket configured");

        // create quiche config
        let quic_config = Self::create_quic_config(config)?;
        log::debug!("QuicEndpoint::new: quiche config created");

        // initialize io_uring backend
        #[cfg(all(target_os = "linux", feature = "io_uring"))]
        let io_uring = match crate::io_uring_backend::IoUringBackend::new(64) {
            Ok(backend) => {
                log::info!("QuicEndpoint::new: io_uring backend initialized (queue depth: 64)");
                Some(backend)
            }
            Err(e) => {
                log::warn!("QuicEndpoint::new: failed to initialize io_uring: {}, falling back to send_to()", e);
                None
            }
        };

        if session_cache.is_some() {
            log::info!(
                "QuicEndpoint::new: endpoint ready on {} (0-RTT enabled)",
                local_addr
            );
        } else {
            log::info!("QuicEndpoint::new: endpoint ready on {}", local_addr);
        }

        Ok(Self {
            socket,
            local_addr,
            quic_config,
            pool: ConnectionPool::new(),
            conn_id_map: HashMap::with_capacity(128),
            session_cache,
            timing_registry: Arc::new(TimingRegistry::new()),
            adaptive_config: None, // default: no adaptive timing (will be set by sender)
            send_buf: Box::new([0u8; SEND_BUF_SIZE]),
            recv_buf: Box::new([0u8; RECV_BUF_SIZE]),
            scid_counter: 0,
            #[cfg(target_os = "linux")]
            batch_recv_bufs: Box::new([[0u8; MAX_PACKET_SIZE]; BATCH_SIZE]),
            #[cfg(all(target_os = "linux", feature = "io_uring"))]
            io_uring,
        })
    }

    /// configure UDP socket
    #[cfg(target_os = "linux")]
    fn configure_socket(socket: &UdpSocket) -> Result<()> {
        use std::os::unix::io::AsRawFd;

        let fd = socket.as_raw_fd();

        unsafe {
            let val_1: libc::c_int = 1;

            // SO_REUSEADDR & SO_REUSEPORT
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_REUSEADDR,
                &val_1 as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_REUSEPORT,
                &val_1 as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );

            // SO_BUSY_POLL: enable kernel busy polling (microseconds)
            // reduces latency by polling in kernel before blocking
            let busy_poll_us: libc::c_int = 50; // 50μs busy poll
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_BUSY_POLL,
                &busy_poll_us as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );

            // SO_BUSY_POLL_BUDGET: max packets to process per busy poll
            const SO_BUSY_POLL_BUDGET: libc::c_int = 70;
            let budget: libc::c_int = 8;
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                SO_BUSY_POLL_BUDGET,
                &budget as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );

            // express lane for packets
            // IP_TOS: set DSCP for low-latency traffic (EF = expedited forwarding)
            // DSCP 46 is the highest priority class for latency-sensitive traffic.
            // strict priority queuing in routers/switches
            let tos: libc::c_int = 0xB8; // DSCP EF (46) << 2 = 184 = 0xB8
            libc::setsockopt(
                fd,                                                    // socket fd
                libc::IPPROTO_IP,                                      // IP protocol level
                libc::IP_TOS,                                          // type of service
                &tos as *const _ as *const libc::c_void,               // pointer
                std::mem::size_of::<libc::c_int>() as libc::socklen_t, //size
            );

            // SO_PRIORITY: socket priority for QoS
            let priority: libc::c_int = 6; // high priority (0-7)
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PRIORITY,
                &priority as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );

            // SO_RCVLOWAT: wake up immediately on any data
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_RCVLOWAT,
                &val_1 as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );

            // SO_INCOMING_CPU: prefer receiving on current CPU (reduces cache misses)
            let cpu = libc::sched_getcpu();
            if cpu >= 0 {
                libc::setsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_INCOMING_CPU,
                    &cpu as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                );
            }

            // IP_MTU_DISCOVER: set to PMTUDISC_DO to avoid fragmentation
            let pmtu: libc::c_int = libc::IP_PMTUDISC_DO;
            libc::setsockopt(
                fd,
                libc::IPPROTO_IP,
                libc::IP_MTU_DISCOVER,
                &pmtu as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );

            // buffer sizes: moderate size to avoid bufferbloat
            // too large = latency, too small = drops
            let buf_size: libc::c_int = 512 * 1024; // 512KB
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_RCVBUF,
                &buf_size as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                &buf_size as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );

            // SO_ZEROCOPY: avoid kernel copy on send (requires sendmsg with MSG_ZEROCOPY)
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_ZEROCOPY,
                &val_1 as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );

            // SO_TIMESTAMPING: enable hardware timestamps if available
            const SOF_TIMESTAMPING_RX_HARDWARE: libc::c_uint = 1 << 2;
            const SOF_TIMESTAMPING_TX_HARDWARE: libc::c_uint = 1 << 3;
            const SOF_TIMESTAMPING_RAW_HARDWARE: libc::c_uint = 1 << 6;
            let ts_flags: libc::c_uint = SOF_TIMESTAMPING_RX_HARDWARE
                | SOF_TIMESTAMPING_TX_HARDWARE
                | SOF_TIMESTAMPING_RAW_HARDWARE;
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_TIMESTAMPING,
                &ts_flags as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_uint>() as libc::socklen_t,
            );
        }

        log::debug!("configure_socket: applied low-latency socket options");
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    fn configure_socket(_socket: &UdpSocket) -> Result<()> {
        Ok(())
    }

    /// quiche config
    fn create_quic_config(config: &Config) -> Result<quiche::Config> {
        let mut quic_config = quiche::Config::new(quiche::PROTOCOL_VERSION)?;

        // set ALPN for solana TPU
        quic_config.set_application_protos(&[SOLANA_TPU_ALPN])?;

        // idle timeout dead connection detection
        quic_config.set_max_idle_timeout(config.idle_timeout_ms);

        // faster ACK delay = faster handshake completion
        quic_config.set_ack_delay_exponent(0); // 2^0 = 1μs granularity
        quic_config.set_max_ack_delay(1); // 1ms max ACK delay (default is 25ms)

        // packset size
        quic_config.set_max_recv_udp_payload_size(MAX_PACKET_SIZE);
        quic_config.set_max_send_udp_payload_size(MAX_PACKET_SIZE);

        // stream limits
        quic_config.set_initial_max_streams_uni(config.max_streams_uni);
        quic_config.set_initial_max_data(10_000_000);
        quic_config.set_initial_max_stream_data_uni(MAX_PACKET_SIZE as u64);

        // disable migration (not needed, saves processing)
        quic_config.set_disable_active_migration(true);

        // enable 0-RTT for faster reconnection (saves 1 RTT on reconnect)
        quic_config.enable_early_data();

        // skip peer certificate verification (solana uses self-signed)
        quic_config.verify_peer(false);

        // disable GREASE (saves ~10 bytes per packet, reduces processing)
        quic_config.grease(false);

        // use smaller initial window for faster ramp-up feedback
        quic_config.set_initial_congestion_window_packets(10);

        // disable hystart (can add latency during slow start)
        quic_config.enable_hystart(false);

        // disable pacing (send immediately, let network handle it)
        quic_config.enable_pacing(false);

        // load client certificate for staked identity, or dummy cert for unstaked
        if let Some(ref staked) = config.staked_identity {
            Self::load_client_cert(&mut quic_config, &staked.keypair_path)?;
        } else {
            // unstaked: load dummy certificate
            Self::load_dummy_cert(&mut quic_config)?;
        }

        log::debug!("create_quic_config: applied low-latency QUIC settings");
        Ok(quic_config)
    }

    /// load dummy Ed25519 certificate for signature scheme negotiation
    ///
    /// solana validators require all QUIC clients to present certificates,
    /// even for unstaked connections. this generates a throwaway self-signed cert.
    fn load_dummy_cert(quic_config: &mut quiche::Config) -> Result<()> {
        // generate a proper Ed25519 keypair using ring's cryptographic RNG
        let rng = ring::rand::SystemRandom::new();
        let pkcs8_bytes = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng)
            .map_err(|_| Error::Tls("Failed to generate keypair".into()))?;

        // parse the keypair to get public key
        let ring_keypair = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8_bytes.as_ref())
            .map_err(|_| Error::Tls("Failed to parse keypair".into()))?;

        let public_key_bytes = ring_keypair.public_key().as_ref();

        // ring's PKCS#8 format (83 bytes for Ed25519) includes:
        // - ASN.1 SEQUENCE header
        // - version
        // - algorithm identifier
        // - private key wrapped in OCTET STRING (which itself contains the 32-byte seed + public key)
        // we need to parse it properly to extract just the 32-byte seed

        let pkcs8_doc = pkcs8_bytes.as_ref();

        // parse ASN.1 to find the private key seed
        // the structure is: SEQUENCE { version, AlgorithmIdentifier, PrivateKey }
        // PrivateKey is an OCTET STRING containing another OCTET STRING with the 32-byte seed

        // find the actual 32-byte Ed25519 seed by scanning for the pattern
        // look for: 0x04 0x20 (OCTET STRING, length 32) followed by 32 bytes of key material
        let mut seed_offset = None;
        for i in 0..pkcs8_doc.len().saturating_sub(34) {
            if pkcs8_doc[i] == 0x04 && pkcs8_doc[i + 1] == 0x20 {
                // found potential seed location
                seed_offset = Some(i + 2);
                break;
            }
        }

        let secret_key_bytes = match seed_offset {
            Some(offset) if offset + 32 <= pkcs8_doc.len() => &pkcs8_doc[offset..offset + 32],
            _ => {
                log::error!(
                    "load_dummy_cert: could not parse PKCS#8 document (len={})",
                    pkcs8_doc.len()
                );
                return Err(Error::Tls(
                    "Could not extract Ed25519 seed from PKCS#8".into(),
                ));
            }
        };

        // create our Keypair struct for certificate generation
        let mut keypair = Keypair {
            secret: [0u8; 32],
            public: [0u8; 32],
        };
        keypair.secret.copy_from_slice(secret_key_bytes);
        keypair.public.copy_from_slice(public_key_bytes);

        // generate properly formatted PEM files using existing infrastructure
        let cert_pem = keypair.certificate_pem();
        let key_pem = keypair.private_key_pem();

        // write temporary PEM files (quiche requires file paths)
        let cert_path = "/tmp/quic_dummy_cert.pem";
        let key_path = "/tmp/quic_dummy_key.pem";

        std::fs::write(cert_path, &cert_pem).map_err(Error::Io)?;
        std::fs::write(key_path, &key_pem).map_err(Error::Io)?;

        // load into quiche config
        quic_config
            .load_cert_chain_from_pem_file(cert_path)
            .map_err(|e| {
                // clean up on error
                let _ = std::fs::remove_file(cert_path);
                let _ = std::fs::remove_file(key_path);
                Error::Tls(format!("Failed to load dummy cert: {}", e))
            })?;

        quic_config
            .load_priv_key_from_pem_file(key_path)
            .map_err(|e| {
                // clean up on error
                let _ = std::fs::remove_file(cert_path);
                let _ = std::fs::remove_file(key_path);
                Error::Tls(format!("Failed to load dummy key: {}", e))
            })?;

        // clean up temp files
        let _ = std::fs::remove_file(cert_path);
        let _ = std::fs::remove_file(key_path);

        log::debug!("Loaded dummy Ed25519 certificate for unstaked TLS handshake");
        Ok(())
    }

    /// load client certificate for staked connections
    fn load_client_cert(quic_config: &mut quiche::Config, keypair_path: &str) -> Result<()> {
        let keypair = Keypair::load(keypair_path)?;
        let cert_pem = keypair.certificate_pem();
        let key_pem = keypair.private_key_pem();

        // write temporary PEM files (quiche requires file paths)
        let cert_path = "/tmp/quic_client_cert.pem";
        let key_path = "/tmp/quic_client_key.pem";

        std::fs::write(cert_path, &cert_pem).map_err(Error::Io)?;
        std::fs::write(key_path, &key_pem).map_err(Error::Io)?;

        quic_config
            .load_cert_chain_from_pem_file(cert_path)
            .map_err(|e| Error::Tls(format!("Failed to load cert: {}", e)))?;

        quic_config
            .load_priv_key_from_pem_file(key_path)
            .map_err(|e| Error::Tls(format!("Failed to load key: {}", e)))?;

        // clean up temp files
        let _ = std::fs::remove_file(cert_path);
        let _ = std::fs::remove_file(key_path);

        Ok(())
    }

    /// generate unique 8-byte connection ID (smaller = less overhead)
    #[inline(always)]
    fn generate_scid(&mut self) -> quiche::ConnectionId<'static> {
        self.scid_counter = self.scid_counter.wrapping_add(1);
        quiche::ConnectionId::from_vec(self.scid_counter.to_le_bytes().to_vec())
    }

    /// batch receive packets using recvmmsg
    /// returns (packet_count, [(len, from), ...])
    #[cfg(target_os = "linux")]
    fn recv_batch(&mut self) -> std::io::Result<Vec<(usize, SocketAddr)>> {
        use std::os::unix::io::AsRawFd;

        let fd = self.socket.as_raw_fd();
        let mut results = Vec::with_capacity(BATCH_SIZE);

        // prepare mmsghdr structures on stack
        let mut msgs: [libc::mmsghdr; BATCH_SIZE] = unsafe { std::mem::zeroed() };
        let mut iovecs: [libc::iovec; BATCH_SIZE] = unsafe { std::mem::zeroed() };
        let mut addrs: [libc::sockaddr_storage; BATCH_SIZE] = unsafe { std::mem::zeroed() };

        for i in 0..BATCH_SIZE {
            iovecs[i].iov_base = self.batch_recv_bufs[i].as_mut_ptr() as *mut libc::c_void;
            iovecs[i].iov_len = MAX_PACKET_SIZE;
            msgs[i].msg_hdr.msg_iov = &mut iovecs[i];
            msgs[i].msg_hdr.msg_iovlen = 1;
            msgs[i].msg_hdr.msg_name = &mut addrs[i] as *mut _ as *mut libc::c_void;
            msgs[i].msg_hdr.msg_namelen = std::mem::size_of::<libc::sockaddr_storage>() as u32;
        }

        // call recvmmsg with no timeout (non-blocking via socket)
        let n = unsafe {
            libc::recvmmsg(
                fd,
                msgs.as_mut_ptr(),
                BATCH_SIZE as libc::c_uint,
                libc::MSG_DONTWAIT,
                std::ptr::null_mut(),
            )
        };

        if n < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::WouldBlock {
                return Ok(results);
            }
            return Err(err);
        }

        // convert results
        for i in 0..(n as usize) {
            let len = msgs[i].msg_len as usize;
            let addr = unsafe { sockaddr_to_socket_addr(&addrs[i]) };
            if let Some(addr) = addr {
                results.push((len, addr));
            }
        }

        Ok(results)
    }

    /// connect to a remote address
    ///
    /// - `blocking = true`: waits for handshake to complete (max 3s timeout)
    /// - `blocking = false`: returns immediately after sending init packet (0-RTT)
    #[inline]
    pub fn connect(&mut self, addr: SocketAddr, blocking: bool) -> Result<usize> {
        log::debug!(
            "connect: initiating connection to {} (blocking={})",
            addr,
            blocking
        );

        // find or allocate slot
        let slot_idx = self.pool.find_or_allocate(addr)?;
        let slot = self.pool.get_slot(slot_idx);

        // check if already connected
        if slot.state() == ConnectionState::Ready {
            log::debug!("connect: already connected to {} (slot {})", addr, slot_idx);
            return Ok(slot_idx);
        }

        // generate connection ID
        let scid = self.generate_scid();
        log::debug!("connect: generated scid {:?} for {}", scid, addr);

        // create server name for SNI (solana format: "{ip}.{port}.sol")
        let server_name = format!("{}.{}.sol", addr.ip(), addr.port());
        log::debug!("connect: using server name '{}' for {}", server_name, addr);

        // create quiche connection
        let mut conn = quiche::connect(
            Some(&server_name),
            &scid,
            self.local_addr,
            addr,
            &mut self.quic_config,
        )?;

        // restore session if available (for 0-RTT) - zero-copy path
        if let Some(cache) = &self.session_cache {
            if let Some(token) = cache.get(addr) {
                // validate age (expire after 1 hour)
                let age_ns = crate::monotonic_nanos() - token.timestamp_ns;
                if age_ns < 3_600_000_000_000 {
                    // 1 hour in nanoseconds
                    log::debug!(
                        "connect: using cached session for {} (age: {:.1}s, 0-RTT enabled)",
                        addr,
                        age_ns as f64 / 1_000_000_000.0
                    );
                    // restore session - no copy needed, use slice directly
                    if let Err(e) = conn.set_session(token.as_slice()) {
                        log::warn!("connect: failed to restore session for {}: {} (falling back to full handshake)", addr, e);
                    } else {
                        // track 0-RTT attempt
                        self.pool
                            .stats
                            .zero_rtt_attempts
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        log::debug!("connect: session restored for {} (0-RTT active)", addr);
                    }
                } else {
                    log::debug!(
                        "connect: cached session expired for {} (age: {:.1}s)",
                        addr,
                        age_ns as f64 / 1_000_000_000.0
                    );
                    cache.invalidate(addr);
                }
            } else {
                log::debug!("connect: no cached session for {}, full handshake", addr);
            }
        } else {
            log::debug!("connect: no session cache available");
        }

        log::debug!("connect: quiche connection created for {}", addr);

        // record connection start time for handshake timing measurement
        let connect_start_ns = crate::monotonic_nanos();
        let slot = self.pool.get_slot(slot_idx);
        slot.set_connect_start_ns(connect_start_ns);

        // wrap and store
        let quic_conn = QuicConnection::new(conn);
        self.pool.store_connection(slot_idx, quic_conn);
        log::debug!("connect: stored connection in slot {}", slot_idx);

        // map connection ID to slot
        self.conn_id_map.insert(scid, slot_idx);

        // send initial handshake packet (may contain 0-RTT early data if session restored)
        self.drive_slot(slot_idx)?;

        // for non-blocking mode, return immediately
        // connection can be used via is_in_early_data() or handshake completes in background poll()
        if !blocking {
            let slot = self.pool.get_slot(slot_idx);
            if let Some(conn) = unsafe { slot.connection_mut() } {
                let is_early_data = conn.inner().is_in_early_data();
                let established = conn.inner().is_established();
                log::debug!(
                    "connect: non-blocking mode, returning slot {} (0-RTT={}) established={}",
                    slot_idx,
                    is_early_data,
                    established
                );
            }
            return Ok(slot_idx);
        }

        // blocking mode: wait for handshake to complete with timeout
        let timeout = std::time::Duration::from_millis(3000);
        let start = std::time::Instant::now();

        log::debug!(
            "connect: blocking mode, waiting for handshake to {} (timeout {:?})",
            addr,
            timeout
        );

        // track whether we've received the session ticket
        let mut session_ticket_received = false;

        // stack buffer - MaybeUninit avoids initialization cost
        let mut pkt_buf: std::mem::MaybeUninit<[u8; MAX_PACKET_SIZE]> =
            std::mem::MaybeUninit::uninit();

        while start.elapsed() < timeout {
            // receive and process packets
            let mut recv_count = 0;
            loop {
                // safety: recv_from will initialize the bytes it writes
                let buf = unsafe { &mut *pkt_buf.as_mut_ptr() };
                match self.socket.recv_from(buf) {
                    Ok((len, from)) => {
                        recv_count += 1;
                        self.handle_recv(&mut buf[..len], from)?;
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => return Err(Error::Io(e)),
                }
            }

            // drive the connection (send any pending packets)
            self.drive_slot(slot_idx)?;

            // check if established
            let slot = self.pool.get_slot(slot_idx);
            if let Some(conn) = unsafe { slot.connection_mut() } {
                let inner = conn.inner();

                if recv_count > 0 {
                    log::debug!(
                        "connect: handshake state - established={} closed={} is_in_early_data={} peer_streams_left_uni={} timeout={:?}",
                        inner.is_established(),
                        inner.is_closed(),
                        inner.is_in_early_data(),
                        inner.peer_streams_left_uni(),
                        inner.timeout()
                    );

                    if let Some(err) = inner.peer_error() {
                        log::error!(
                            "connect: peer error - is_app={} code={} reason={:?}",
                            err.is_app,
                            err.error_code,
                            String::from_utf8_lossy(&err.reason)
                        );
                    }
                    if let Some(err) = inner.local_error() {
                        log::error!(
                            "connect: local error - is_app={} code={} reason={:?}",
                            err.is_app,
                            err.error_code,
                            String::from_utf8_lossy(&err.reason)
                        );
                    }
                }

                // only transition to Ready when handshake is confirmed
                if conn.is_handshake_confirmed() {
                    slot.set_state(ConnectionState::Ready);
                    // // save session ticket for 0-RTT (may not be available yet)
                    // if let Some(cache) = &self.session_cache {
                    //     if let Some(session_data) = inner.session() {
                    //         if let Err(e) = cache.insert(addr, session_data) {
                    //             log::warn!("connect: failed to save session ticket: {}", e);
                    //         } else {
                    //             log::debug!(
                    //                 "connect: saved session ticket for {} ({} bytes)",
                    //                 addr,
                    //                 session_data.len()
                    //             );
                    //         }
                    //     } else {
                    //         log::debug!("connect: no session ticket yet (server sends later)");
                    //     }
                    // }
                    //
                    // check for session ticket
                    if !session_ticket_received {
                        if let Some(cache) = &self.session_cache {
                            if let Some(session_data) = inner.session() {
                                if let Err(e) = cache.insert(addr, session_data) {
                                    log::warn!("connect: failed to save session ticket: {}", e);
                                } else {
                                    log::debug!(
                                        "connect: saved session ticket for {} ({} bytes)",
                                        addr,
                                        session_data.len()
                                    );
                                    session_ticket_received = true;
                                }
                            }
                        }
                    }

                    // keep processing incoming packets after handshake completion to receive
                    // TLS 1.3 NewSessionTicket (session resumption tickets), which are *post-handshake* messages.
                    // https://datatracker.ietf.org/doc/html/rfc8446
                    // https://datatracker.ietf.org/doc/html/rfc9001
                    if !session_ticket_received
                        && start.elapsed() < std::time::Duration::from_millis(300)
                    {
                        // give server up to 100ms total to send ticket
                        std::thread::sleep(std::time::Duration::from_micros(300));
                        continue;
                    }

                    log::info!(
                        "connect: handshake complete to {} in {:?} (slot {}, 0-RTT={})",
                        addr,
                        start.elapsed(),
                        slot_idx,
                        inner.is_in_early_data()
                    );
                    return Ok(slot_idx);
                }

                if conn.is_closed() {
                    log::error!("connect: connection closed during handshake to {}", addr);
                    self.pool.mark_failed(slot_idx);
                    return Err(Error::Closed);
                }
            }

            // small sleep to avoid busy-spin during handshake
            std::hint::spin_loop();
        }

        // timeout
        log::error!("connect: handshake timeout to {} after {:?}", addr, timeout);
        self.pool.mark_failed(slot_idx);
        Err(Error::Timeout)
    }

    /// event-based send: never blocks, auto-queues during handshake
    ///
    ///
    /// this method never blocks. if connection is not ready, the transaction
    /// is automatically queued and will be sent when handshake completes.
    #[inline]
    pub fn send(&mut self, addr: SocketAddr, data: &[u8]) -> Result<crate::error::SendStatus> {
        use crate::error::SendStatus;

        log::trace!("send: {} bytes to {}", data.len(), addr);

        // find or create connection - propagate errors
        let slot_idx = match self.pool.find(addr) {
            Some(idx) => idx,
            None => self.connect(addr, false)?, // non-blocking connect
        };

        let slot = self.pool.get_slot(slot_idx);

        match slot.state() {
            ConnectionState::Ready => {
                // hot path: send immediately
                let conn = unsafe { slot.connection_mut() }.ok_or(Error::NotFound)?;
                match conn.send_uni(data) {
                    Ok(()) => {
                        self.flush_slot(slot_idx)?;
                        self.pool.reset_retry(slot_idx); // reset retry state on successful send
                        Ok(SendStatus::Sent)
                    }
                    Err(Error::WouldBlock) => {
                        // flow control blocked - queue for later
                        // note: tx_queue().try_enqueue(&self) works due to interior mutability
                        Ok(conn.tx_queue().try_enqueue(data))
                    }
                    Err(e) => Err(e),
                }
            }

            ConnectionState::Connecting => {
                // get connection - can use &self methods on tx_queue due to UnsafeCell
                let conn = unsafe { slot.connection() }.ok_or(Error::NotFound)?;

                // try 0-RTT early data first (needs &mut for send_uni)
                if conn.inner().is_in_early_data() {
                    let conn_mut = unsafe { slot.connection_mut() }.ok_or(Error::NotFound)?;
                    if let Ok(()) = conn_mut.send_uni(data) {
                        self.pool
                            .stats
                            .zero_rtt_accepts
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        self.flush_slot(slot_idx)?;
                        self.pool.reset_retry(slot_idx); // reset retry state on successful 0-RTT send
                        return Ok(SendStatus::SentEarlyData);
                    }
                }
                // queue for handshake completion (uses &self via interior mutability)
                let status = conn.tx_queue().try_enqueue(data);
                self.pool.dirty.mark_dirty(slot_idx);
                Ok(status)
            }

            ConnectionState::Failed => {
                // retry connection (releases slot borrow)
                self.connect(addr, false)?;
                // re-get slot after connect
                let slot = self.pool.get_slot(slot_idx);
                let conn = unsafe { slot.connection() }.ok_or(Error::NotFound)?;
                let status = conn.tx_queue().try_enqueue(data);
                self.pool.dirty.mark_dirty(slot_idx);
                Ok(status)
            }

            ConnectionState::Dead => Ok(SendStatus::Dead {
                retries: slot.retry_count(),
            }),
        }
    }

    /// send all pending QUIC packets for a connection slot
    /// returns the number of packets sent
    ///
    /// this helper extracts the packet send loop to allow calling it multiple times
    /// within drive_slot (e.g., before and after flushing transaction queue)
    #[inline(always)]
    fn send_pending_packets(&mut self, slot_idx: usize) -> Result<usize> {
        let slot = self.pool.get_slot(slot_idx);
        let conn = unsafe { slot.connection_mut() }.ok_or(Error::NotFound)?;

        let mut packets_sent = 0;
        loop {
            match conn.send(&mut self.send_buf[..]) {
                Ok((len, info)) => {
                    // try io_uring if available (requires io_uring feature)
                    #[cfg(all(target_os = "linux", feature = "io_uring"))]
                    if let Some(ref mut uring) = self.io_uring {
                        use std::os::unix::io::AsRawFd;
                        match uring.submit_send(
                            &self.send_buf[..len],
                            info.to,
                            self.socket.as_raw_fd(),
                        ) {
                            Ok(_) => {
                                packets_sent += 1;
                                continue;
                            }
                            Err(e) => {
                                log::warn!(
                                    "io_uring send failed: {}, falling back to send_to()",
                                    e
                                );
                                // fall through to traditional send
                            }
                        }
                    }

                    // fallback: traditional send_to()
                    log::trace!(
                        "send_pending_packets[{}]: sending {} bytes to {}",
                        slot_idx,
                        len,
                        info.to
                    );
                    self.socket.send_to(&self.send_buf[..len], info.to)?;
                    packets_sent += 1;
                }
                Err(Error::WouldBlock) => break,
                Err(e) => {
                    log::error!("send_pending_packets[{}]: send error: {}", slot_idx, e);
                    return Err(e);
                }
            }
        }

        Ok(packets_sent)
    }

    /// drive connection handshake and I/O for a slot
    /// generates and sends all pending QUIC packets, updates state, records timing
    #[inline(always)]
    fn drive_slot(&mut self, slot_idx: usize) -> Result<()> {
        // track if state was already Ready before this drive (get and release borrow)
        let was_ready = {
            let slot = self.pool.get_slot(slot_idx);
            slot.state() == ConnectionState::Ready
        };

        // first: send all pending packets (handshake, ACKs, etc.)
        let packets_sent = self.send_pending_packets(slot_idx)?;
        if packets_sent > 0 {
            log::debug!("drive_slot[{}]: sent {} packets", slot_idx, packets_sent);
        }

        // update state only when handshake is confirmed (HANDSHAKE_DONE received)
        // not just when established (1-RTT keys ready)
        let is_confirmed = {
            let slot = self.pool.get_slot(slot_idx);
            let conn = unsafe { slot.connection() }.ok_or(Error::NotFound)?;
            conn.is_handshake_confirmed()
        };

        if is_confirmed && !was_ready {
            // test: check connection health before sending
            let slot = self.pool.get_slot(slot_idx);
            let conn = unsafe { slot.connection() }.ok_or(Error::NotFound)?;
            let inner = conn.inner();

            // check for connection close/errors *before* flushing queue
            if let Some(err) = inner.peer_error() {
                let reason = String::from_utf8_lossy(&err.reason);
                log::error!(
                    "drive_slot[{}]: connection closed by peer - code={} reason={:?}",
                    slot_idx,
                    err.error_code,
                    reason
                );

                // rate limiting (code=2 "Disallowed") should mark as dead? *fix me*
                // to prevent infinite reconnection loop
                if err.error_code == 2 {
                    log::error!(
                        "drive_slot[{}]: rate limited by server, marking as dead to prevent retry",
                        slot_idx
                    );
                    let slot = self.pool.get_slot(slot_idx);
                    slot.set_state(ConnectionState::Dead);
                    // clear dirty bit to prevent poll loop from retrying
                    self.pool.dirty.clear_dirty(slot_idx);
                } else {
                    self.pool.mark_failed(slot_idx);
                    // clear dirty bit for failed connections too
                    self.pool.dirty.clear_dirty(slot_idx);
                }
                return Ok(());
            }

            if inner.is_closed() {
                log::error!(
                    "drive_slot[{}]: connection is closed, aborting send",
                    slot_idx
                );
                self.pool.mark_failed(slot_idx);
                // clear dirty bit to prevent poll loop from retrying
                self.pool.dirty.clear_dirty(slot_idx);
                return Ok(());
            }

            // connection is good - set state to Ready and drain transaction queue
            let flushed_count = {
                let slot = self.pool.get_slot(slot_idx);
                slot.set_state(ConnectionState::Ready);
                let conn = unsafe { slot.connection_mut() }.ok_or(Error::NotFound)?;

                // drain transaction queue with x retry logic
                let mut count = 0;
                const MAX_RETRIES: usize = 5;

                loop {
                    // get reference to queue, dequeue one item, release reference
                    let item = {
                        let tx_queue = conn.tx_queue();
                        tx_queue.try_dequeue()
                    };

                    match item {
                        Some((buf, len)) => {
                            let mut sent = false;

                            // try sending with retries (non-blocking)
                            for attempt in 0..MAX_RETRIES {
                                match conn.send_uni(&buf[..len]) {
                                    Ok(()) => {
                                        count += 1;
                                        sent = true;
                                        if attempt > 0 {
                                            log::debug!(
                                                "drive_slot[{}]: sent transaction {} (attempt {})",
                                                slot_idx,
                                                count,
                                                attempt + 1
                                            );
                                        }
                                        break;
                                    }
                                    Err(Error::WouldBlock) => {
                                        log::debug!(
                                            "drive_slot[{}]: WouldBlock on attempt {}, retrying",
                                            slot_idx,
                                            attempt + 1
                                        );
                                        // small yield to let connection process
                                        std::hint::spin_loop();
                                        continue;
                                    }
                                    Err(e) => {
                                        log::warn!(
                                            "drive_slot[{}]: send error on attempt {}: {}",
                                            slot_idx,
                                            attempt + 1,
                                            e
                                        );
                                        break;
                                    }
                                }
                            }

                            if !sent {
                                log::error!(
                                    "drive_slot[{}]: failed to send transaction after {} retries",
                                    slot_idx,
                                    MAX_RETRIES
                                );
                                break;
                            }
                        }
                        None => break, // queue empty
                    }
                }
                count // return flush count
            }; // release all borrows

            if flushed_count > 0 {
                log::info!(
                    "drive_slot[{}]: flushed {} queued transactions with retry logic",
                    slot_idx,
                    flushed_count
                );

                // instant send stream frames containing transaction data
                let stream_packets_sent = self.send_pending_packets(slot_idx)?;
                if stream_packets_sent > 0 {
                    log::debug!(
                        "drive_slot[{}]: sent {} packets with transaction STREAM frames",
                        slot_idx,
                        stream_packets_sent
                    );
                }
            }

            // clear dirty bit if queue is empty (new scope for fresh borrows)
            {
                let slot = self.pool.get_slot(slot_idx);
                if let Some(conn) = unsafe { slot.connection() } {
                    if !conn.tx_queue().has_pending() {
                        self.pool.dirty.clear_dirty(slot_idx);
                    }
                }
            }

            // measure and record handshake timing (new scope for fresh borrows)
            {
                let slot = self.pool.get_slot(slot_idx);
                let conn = unsafe { slot.connection() }.ok_or(Error::NotFound)?;
                let connect_start_ns = slot.connect_start_ns();
                let connect_end_ns = crate::monotonic_nanos();

                if connect_start_ns > 0 {
                    let handshake_time_ns = connect_end_ns - connect_start_ns;
                    let peer_addr = slot.address();

                    // determine timing category
                    let timing_category = if conn.inner().is_in_early_data() {
                        TimingCategory::ZeroRtt
                    } else if self
                        .session_cache
                        .as_ref()
                        .and_then(|c| c.get(peer_addr))
                        .is_some()
                    {
                        // had session but wasn't used -> 1-RTT fallback
                        TimingCategory::OneRtt
                    } else {
                        // no cached session -> full handshake
                        TimingCategory::Handshake
                    };

                    // record timing to registry
                    let timing = self.timing_registry.get_or_create(peer_addr);
                    timing.record(handshake_time_ns, timing_category);

                    // log diagnostics with quiche stats
                    let stats = conn.inner().stats();
                    log::debug!(
                    "drive_slot[{}]: handshake complete to {} in {:.1}ms (category={:?}, sent={}, recv={})",
                    slot_idx,
                    peer_addr,
                    handshake_time_ns as f64 / 1_000_000.0,
                    timing_category,
                    stats.sent,
                    stats.recv
                );
                }

                // save session ticket for 0-RTT
                if let Some(cache) = &self.session_cache {
                    if let Some(session_data) = conn.inner().session() {
                        let peer_addr = slot.address();
                        if let Err(e) = cache.insert(peer_addr, session_data) {
                            log::warn!(
                                "drive_slot[{}]: failed to save session ticket: {}",
                                slot_idx,
                                e
                            );
                        } else {
                            log::debug!(
                                "drive_slot[{}]: saved session ticket for {} ({} bytes)",
                                slot_idx,
                                peer_addr,
                                session_data.len()
                            );
                        }
                    } else {
                        log::debug!("drive_slot[{}]: no session ticket available yet", slot_idx);
                    }
                }
            } // end of timing and session scope

            log::info!("drive_slot[{}]: handshake confirmed!", slot_idx);
        }

        Ok(())
    }

    /// flush pending packets for a slot (alias to drive_slot)
    /// kept for API compatibility - drive_slot handles everything
    /// fix me
    #[inline(always)]
    fn flush_slot(&mut self, slot_idx: usize) -> Result<()> {
        // optional diagnostic logging (only in debug builds)
        #[cfg(debug_assertions)]
        {
            let slot = self.pool.get_slot(slot_idx);
            if let Some(conn) = unsafe { slot.connection() } {
                let inner = conn.inner();
                let stats = inner.stats();
                let path_stats = inner.path_stats().next();
                let (cwnd, rtt) = if let Some(ps) = path_stats {
                    (ps.cwnd, ps.rtt.as_micros() as u64)
                } else {
                    (0, 0)
                };
                log::trace!(
                    "flush_slot[{}]: established={} early_data={} streams_left={} cwnd={} recv={} sent={} lost={} rtt={}us",
                    slot_idx,
                    inner.is_established(),
                    inner.is_in_early_data(),
                    inner.peer_streams_left_uni(),
                    cwnd,
                    stats.recv,
                    stats.sent,
                    stats.lost,
                    rtt
                );
            }
        }

        // drive the slot (io_uring support)
        self.drive_slot(slot_idx)?;

        // poll io_uring completions to reclaim buffers
        #[cfg(all(target_os = "linux", feature = "io_uring"))]
        if let Some(ref mut uring) = self.io_uring {
            if let Err(e) = uring.poll_completions() {
                log::warn!("io_uring poll_completions failed: {}", e);
            }
        }

        Ok(())
    }

    /// poll for incoming packets (non-blocking, zero-allocation hot path)
    #[cfg(target_os = "linux")]
    #[inline]
    pub fn poll(&mut self) -> Result<()> {
        // batch receive packets using recvmmsg
        loop {
            match self.recv_batch() {
                Ok(packets) if packets.is_empty() => break,
                Ok(packets) => {
                    for (buf_idx, (len, from)) in packets.into_iter().enumerate() {
                        // inline packet processing (avoids borrow conflict + saves call overhead)
                        log::trace!("poll: {} bytes from {}", len, from);

                        if let Some(slot_idx) = self.pool.find(from) {
                            let slot = self.pool.get_slot(slot_idx);

                            // skip processing packets from Dead connections unless they're retryable
                            // dead connections with expired retry deadlines may recover via packet processing
                            if slot.state() == ConnectionState::Dead {
                                let now_ns = crate::monotonic_nanos();
                                if !slot.can_retry(now_ns) {
                                    log::trace!(
                                        "poll: skipping packet from Dead connection {} (slot {}, retryable in {:.1}ms)",
                                        from,
                                        slot_idx,
                                        (slot.retry_deadline_ns().saturating_sub(now_ns)) as f64 / 1_000_000.0
                                    );
                                    continue;
                                }
                                // if can_retry(), allow packet processing (may recover connection)
                            }

                            let was_established = slot.state() == ConnectionState::Ready;

                            if let Some(conn) = unsafe { slot.connection_mut() } {
                                let recv_info = quiche::RecvInfo {
                                    from,
                                    to: self.local_addr,
                                };

                                let data = &mut self.batch_recv_bufs[buf_idx][..len];
                                let inner = conn.inner_mut();
                                match inner.recv(data, recv_info) {
                                    Ok(recv_len) => {
                                        log::debug!(
                                            "poll[{}]: processed {} bytes from {}",
                                            slot_idx,
                                            recv_len,
                                            from
                                        );
                                        slot.touch();
                                        // let drive_slot() handle the state
                                        // so queue flush happens atomically
                                        if inner.is_handshake_confirmed() && !was_established {
                                            log::info!("poll[{}]: handshake confirmed!", slot_idx);
                                        }
                                        // check for session ticket (may arrive after handshake)
                                        if inner.is_established() {
                                            if let Some(cache) = &self.session_cache {
                                                if let Some(session_data) = inner.session() {
                                                    let peer_addr = slot.address();
                                                    if let Ok(()) =
                                                        cache.insert(peer_addr, session_data)
                                                    {
                                                        log::debug!("poll[{}]: saved session ticket for {} ({} bytes)", slot_idx, peer_addr, session_data.len());
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    Err(quiche::Error::Done) => {
                                        log::trace!("poll[{}]: done", slot_idx);
                                    }
                                    Err(e) => {
                                        log::warn!("poll[{}]: error: {}", slot_idx, e);
                                        self.pool.mark_failed(slot_idx);
                                    }
                                }
                            }
                        } else {
                            log::trace!("poll: no connection for {}", from);
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(Error::Io(e)),
            }
        }

        // process *only* dirty connections
        // uses atomic swap for race-free read-and-clear
        let (dirty_lo, dirty_hi) = self.pool.dirty.take_dirty();

        // process low 64 bits (slots 0-63)
        let mut lo = dirty_lo;
        while lo != 0 {
            let idx = lo.trailing_zeros() as usize;
            lo &= lo - 1; // clear lowest bit

            let _ = self.drive_slot(idx);

            // re-mark dirty if still has work (skip Dead/Failed to prevent loops)
            let slot = self.pool.get_slot(idx);
            let state = slot.state();
            if state == ConnectionState::Dead || state == ConnectionState::Failed {
                // dont re-mark Dead/Failed connections as dirty
                continue;
            }

            if state == ConnectionState::Connecting {
                self.pool.dirty.mark_dirty(idx);
            } else if let Some(conn) = unsafe { slot.connection() } {
                if conn.tx_queue().has_pending() {
                    self.pool.dirty.mark_dirty(idx);
                }
            }
        }

        // process high 64 bits (slots 64-127)
        let mut hi = dirty_hi;
        while hi != 0 {
            let idx = 64 + hi.trailing_zeros() as usize;
            hi &= hi - 1; // clear lowest bit

            let _ = self.drive_slot(idx);

            // re-mark dirty if still has work (skip Dead/Failed to prevent loops)
            let slot = self.pool.get_slot(idx);
            let state = slot.state();
            if state == ConnectionState::Dead || state == ConnectionState::Failed {
                // don't re-mark Dead/Failed connections as dirty
                continue;
            }

            if state == ConnectionState::Connecting {
                self.pool.dirty.mark_dirty(idx);
            } else if let Some(conn) = unsafe { slot.connection() } {
                if conn.tx_queue().has_pending() {
                    self.pool.dirty.mark_dirty(idx);
                }
            }
        }

        // poll io_uring completions
        #[cfg(all(target_os = "linux", feature = "io_uring"))]
        if let Some(ref mut uring) = self.io_uring {
            if let Err(e) = uring.poll_completions() {
                log::warn!("poll: io_uring poll_completions failed: {}", e);
            }
        }

        Ok(())
    }

    /// poll for incoming packets (non-blocking)
    #[cfg(not(target_os = "linux"))]
    #[inline]
    pub fn poll(&mut self) -> Result<()> {
        // stack buffer for recv - MaybeUninit avoids initialization cost
        let mut pkt_buf: std::mem::MaybeUninit<[u8; MAX_PACKET_SIZE]> =
            std::mem::MaybeUninit::uninit();

        // receive packets
        loop {
            // safety: recv_from will initialize the bytes it writes
            let buf = unsafe { &mut *pkt_buf.as_mut_ptr() };
            match self.socket.recv_from(buf) {
                Ok((len, from)) => {
                    self.handle_recv(&mut buf[..len], from)?;
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(Error::Io(e)),
            }
        }

        // process *only* dirty connections
        // Uses atomic swap for race-free read-and-clear
        let (dirty_lo, dirty_hi) = self.pool.dirty.take_dirty();

        // process low 64 bits (slots 0-63)
        let mut lo = dirty_lo;
        while lo != 0 {
            let idx = lo.trailing_zeros() as usize;
            lo &= lo - 1; // clear lowest bit

            let _ = self.drive_slot(idx);

            // re-mark dirty if still has work (skip Dead/Failed to prevent loops)
            let slot = self.pool.get_slot(idx);
            let state = slot.state();
            if state == ConnectionState::Dead || state == ConnectionState::Failed {
                // don't re-mark Dead/Failed connections as dirty
                continue;
            }

            if state == ConnectionState::Connecting {
                self.pool.dirty.mark_dirty(idx);
            } else if let Some(conn) = unsafe { slot.connection() } {
                if conn.tx_queue().has_pending() {
                    self.pool.dirty.mark_dirty(idx);
                }
            }
        }

        // process high 64 bits (slots 64-127)
        let mut hi = dirty_hi;
        while hi != 0 {
            let idx = 64 + hi.trailing_zeros() as usize;
            hi &= hi - 1; // clear lowest bit

            let _ = self.drive_slot(idx);

            // re-mark dirty if still has work (skip Dead/Failed to prevent loops)
            let slot = self.pool.get_slot(idx);
            let state = slot.state();
            if state == ConnectionState::Dead || state == ConnectionState::Failed {
                // don't re-mark Dead/Failed connections as dirty
                continue;
            }

            if state == ConnectionState::Connecting {
                self.pool.dirty.mark_dirty(idx);
            } else if let Some(conn) = unsafe { slot.connection() } {
                if conn.tx_queue().has_pending() {
                    self.pool.dirty.mark_dirty(idx);
                }
            }
        }

        Ok(())
    }

    /// handle received packet
    #[inline(always)]
    fn handle_recv(&mut self, data: &mut [u8], from: SocketAddr) -> Result<()> {
        log::trace!("handle_recv: {} bytes from {}", data.len(), from);

        // find connection by source address
        if let Some(slot_idx) = self.pool.find(from) {
            let slot = self.pool.get_slot(slot_idx);

            // skip processing packets from Dead connections unless they're retryable
            // dead connections with expired retry deadlines may recover via packet processing
            if slot.state() == ConnectionState::Dead {
                let now_ns = crate::monotonic_nanos();
                if !slot.can_retry(now_ns) {
                    log::trace!(
                        "handle_recv: skipping packet from Dead connection {} (slot {})",
                        from,
                        slot_idx
                    );
                    return Ok(());
                }
                // if can_retry(), allow packet processing (may recover connection)
            }

            let was_established = slot.state() == ConnectionState::Ready;

            if let Some(conn) = unsafe { slot.connection_mut() } {
                let recv_info = quiche::RecvInfo {
                    from,
                    to: self.local_addr,
                };

                let inner = conn.inner_mut();
                match inner.recv(data, recv_info) {
                    Ok(len) => {
                        log::debug!(
                            "handle_recv[{}]: processed {} bytes from {}",
                            slot_idx,
                            len,
                            from
                        );
                        slot.touch();
                        // dont set state here - let drive_slot() handle it
                        // so queue flush happens atomically
                        if inner.is_handshake_confirmed() && !was_established {
                            log::info!("handle_recv[{}]: handshake confirmed!", slot_idx);
                        }
                        // check for session ticket (may arrive after handshake)
                        if inner.is_established() {
                            if let Some(cache) = &self.session_cache {
                                if let Some(session_data) = inner.session() {
                                    if let Ok(()) = cache.insert(from, session_data) {
                                        log::debug!("handle_recv[{}]: saved session ticket for {} ({} bytes)", slot_idx, from, session_data.len());
                                    }
                                }
                            }
                        }
                    }
                    Err(quiche::Error::Done) => {
                        log::trace!("handle_recv[{}]: done", slot_idx);
                    }
                    Err(e) => {
                        log::warn!("handle_recv[{}]: error: {}", slot_idx, e);
                        self.pool.mark_failed(slot_idx);
                    }
                }
            }
        } else {
            log::trace!("handle_recv: no connection for {}", from);
        }

        Ok(())
    }

    /// run busy-spin loop
    pub fn run_loop<F>(&mut self, mut callback: F)
    where
        F: FnMut(&mut Self),
    {
        loop {
            // poll for packets
            let _ = self.poll();

            // user callback
            callback(self);

            // spin hint
            std::hint::spin_loop();
        }
    }

    /// get pool reference
    #[inline]
    pub fn pool(&self) -> &ConnectionPool {
        &self.pool
    }

    /// get local address
    #[inline]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// get session cache reference
    #[inline]
    pub fn session_cache(&self) -> Option<&Arc<SessionTokenCache>> {
        self.session_cache.as_ref()
    }

    /// inline prewarm api
    ///
    /// prewarm connections to upcoming leaders (non-blocking, inline)
    ///
    /// call this in your hot loop to maintain warm connections without a separate thread.
    ///

    /// keep connection alive if idle beyond threshold
    ///
    /// drives the connection to process any pending state and prevent idle timeout.
    /// call this periodically for connections you want to keep warm.
    ///
    /// solana validators have a 2-second idle timeout. this method should be called
    /// with a threshold well under that (e.g., 500ms) to maintain connections and
    /// keep flow control state fresh.
    ///
    /// # arguments
    /// * `addr` - address of the connection to keep alive
    /// * `idle_threshold_ns` - idle time threshold in nanoseconds
    ///
    /// # example
    /// ```ignore
    /// // Keep alive if idle > 500ms
    /// endpoint.keep_alive_if_idle(addr, 500_000_000)?;
    /// ```
    #[inline]
    pub fn keep_alive_if_idle(&mut self, addr: SocketAddr, idle_threshold_ns: u64) -> Result<()> {
        // find connection
        if let Some(slot_idx) = self.pool.find(addr) {
            let slot = self.pool.get_slot(slot_idx);

            // if idle beyond threshold
            let idle_time = slot.idle_time_ns();
            if idle_time > idle_threshold_ns {
                // drive connection to keep it alive
                // log::trace!(
                //     "keep_alive: driving slot {} for {} (idle {:.1}ms)",
                //     slot_idx,
                //     addr,
                //     idle_time as f64 / 1_000_000.0
                // );
                self.drive_slot(slot_idx)?;
            }
        }

        Ok(())
    }

    #[inline]
    pub fn prewarm<I>(&mut self, addrs: I)
    where
        I: IntoIterator<Item = SocketAddr>,
    {
        let mut initiated = 0;
        let mut skipped = 0;

        for addr in addrs {
            // skip if already have connection
            if self.pool.find(addr).is_some() {
                skipped += 1;
                continue;
            }

            // start non-blocking connect (sends Initial packet, returns immediately)
            // if we have a cached session, 0-RTT will be available
            match self.connect(addr, false) {
                Ok(slot_idx) => {
                    log::info!(
                        "prewarm: initiated connection to {} (slot {})",
                        addr,
                        slot_idx
                    );
                    initiated += 1;
                }
                Err(e) => {
                    log::debug!("prewarm: failed to initiate connection to {}: {}", addr, e);
                }
            }
        }

        if initiated > 0 {
            log::info!(
                "prewarm: initiated {} new connections (skipped {} existing)",
                initiated,
                skipped
            );
        }
    }

    /// send: never blocks, returns immediately if not ready
    ///
    /// this is the fastest send path for hot loops. It will:
    /// - use existing ready connection if available
    /// - use 0-RTT early data if connection is in handshake but has session
    /// - return `NotEstablished` if connection not ready (caller should retry next loop)
    ///
    /// # returns
    /// - `Ok(())` - data sent successfully
    /// - `Err(NotEstablished)` - connection not ready, retry next iteration
    /// - `Err(Dead)` - connection failed permanently
    #[inline(always)]
    pub fn send_nonblocking(&mut self, addr: SocketAddr, data: &[u8]) -> Result<()> {
        if let Some(slot_idx) = self.pool.find(addr) {
            let slot = self.pool.get_slot(slot_idx);

            match slot.state() {
                ConnectionState::Ready => {
                    // hot path: connection ready, send immediately
                    let conn = unsafe { slot.connection_mut() }.ok_or(Error::NotFound)?;
                    conn.send_uni(data)?;
                    self.flush_slot(slot_idx)?;
                    return Ok(());
                }
                ConnectionState::Connecting => {
                    // check 0-RTT availability
                    if let Some(conn) = unsafe { slot.connection_mut() } {
                        if conn.inner().is_in_early_data() {
                            // 0-RTT available: send immediately
                            self.pool
                                .stats
                                .zero_rtt_accepts
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            conn.send_uni(data)?;
                            self.flush_slot(slot_idx)?;
                            return Ok(());
                        }
                    }
                    // not ready yet, drive and return
                    let _ = self.drive_slot(slot_idx);
                    return Err(Error::NotEstablished);
                }
                ConnectionState::Failed => {
                    // try to reconnect non-blocking
                    let _ = self.connect(addr, false);
                    return Err(Error::NotEstablished);
                }
                ConnectionState::Dead => {
                    let retry_count = slot.retry_count();
                    return Err(Error::Dead(retry_count));
                }
            }
        }

        // no connection: start non-blocking connect if we have session
        let has_session = self
            .session_cache
            .as_ref()
            .and_then(|cache| cache.get(addr))
            .is_some();

        if has_session {
            // start 0-RTT connection (non-blocking)
            let slot_idx = self.connect(addr, false)?;
            let slot = self.pool.get_slot(slot_idx);

            // try 0-RTT immediately
            if let Some(conn) = unsafe { slot.connection_mut() } {
                if conn.inner().is_in_early_data() {
                    self.pool
                        .stats
                        .zero_rtt_accepts
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    conn.send_uni(data)?;
                    self.flush_slot(slot_idx)?;
                    return Ok(());
                }
            }
        } else {
            // no session: initiate connection for next time
            let _ = self.connect(addr, false);
        }

        Err(Error::NotEstablished)
    }

    /// run tight non-blocking event loop with inline prewarming
    ///
    /// this is the recommended API for low latency transaction sending.
    /// it combines polling, prewarming, and sending in a single tight loop.
    ///
    /// # arguments
    /// * `get_prewarm_addrs` - returns addresses to prewarm (called each iteration)
    /// * `get_tx` - returns (addr, data) to send, or None to skip
    ///
    #[inline]
    pub fn run_hot_loop<P, T, I>(&mut self, mut get_prewarm_addrs: P, mut get_tx: T) -> !
    where
        P: FnMut() -> I,
        I: IntoIterator<Item = SocketAddr>,
        T: FnMut() -> Option<(SocketAddr, Vec<u8>)>,
    {
        loop {
            // poll for incoming packets (drive connections)
            let _ = self.poll();

            // prewarm upcoming leaders (inliney)
            self.prewarm(get_prewarm_addrs());

            // get and send transaction (non-blocking)
            if let Some((addr, data)) = get_tx() {
                match self.send_nonblocking(addr, &data) {
                    Ok(()) => {
                        log::trace!("hot_loop: sent {} bytes to {}", data.len(), addr);
                    }
                    Err(Error::NotEstablished) => {
                        // connection not ready - will be ready next iteration
                        log::trace!("hot_loop: {} not ready, will retry", addr);
                    }
                    Err(e) => {
                        log::warn!("hot_loop: send to {} failed: {}", addr, e);
                    }
                }
            }

            // minimal yield (CPU hint)
            std::hint::spin_loop();
        }
    }

    /// run tight loop with callback
    ///
    /// # arguments
    /// * `callback` - called each iteration with mutable endpoint reference
    ///
    /// the callback should:
    /// 1. call `prewarm()` with upcoming leader addresses
    /// 2. call `send_nonblocking()` for any pending transactions
    #[inline]
    pub fn run_tight_loop<F>(&mut self, mut callback: F) -> !
    where
        F: FnMut(&mut Self),
    {
        loop {
            // poll for packets
            let _ = self.poll();

            // user callback (prewarm + send)
            callback(self);

            // spin hint
            std::hint::spin_loop();
        }
    }

    /// check if connection to address is ready for sending
    #[inline]
    pub fn is_ready(&self, addr: SocketAddr) -> bool {
        self.pool
            .find(addr)
            .map(|idx| self.pool.get_slot(idx).state() == ConnectionState::Ready)
            .unwrap_or(false)
    }

    /// check if connection has 0-RTT early data available
    #[inline]
    pub fn has_early_data(&self, addr: SocketAddr) -> bool {
        if let Some(idx) = self.pool.find(addr) {
            let slot = self.pool.get_slot(idx);
            if let Some(conn) = unsafe { slot.connection() } {
                return conn.inner().is_in_early_data();
            }
        }
        false
    }

    /// get connection state for address
    #[inline]
    pub fn connection_state(&self, addr: SocketAddr) -> Option<ConnectionState> {
        self.pool
            .find(addr)
            .map(|idx| self.pool.get_slot(idx).state())
    }

    // adaptive timing api

    /// set adaptive prewarming configuration
    #[inline]
    pub fn set_adaptive_config(&mut self, config: AdaptivePrewarmConfig) {
        self.adaptive_config = Some(config);
    }

    /// get timing registry reference
    #[inline]
    pub fn timing_registry(&self) -> &Arc<TimingRegistry> {
        &self.timing_registry
    }

    /// calculate adaptive lookahead solana slots for address
    ///
    /// returns number of solana slots to connect early based on historical timing.
    /// uses configured percentile (default p90) from timing histogram.
    #[inline]
    pub fn calculate_lookahead_solana_slots(&self, addr: SocketAddr) -> usize {
        let config = match &self.adaptive_config {
            Some(cfg) => cfg,
            None => {
                // no adaptive config, return default minimum
                return 4;
            }
        };

        // get timing metrics for this ip
        let timing = self.timing_registry.get(addr);

        // determine expected handshake time in milliseconds
        let handshake_ms = if let Some(timing) = timing {
            // use configured percentile from historical data
            let percentiles = timing
                .handshake_histogram
                .percentiles(&[config.timing_percentile]);
            let handshake_ns = percentiles[0];

            // if no handshake data yet, try 0-RTT timing as fallback
            if handshake_ns == 0 {
                let zero_rtt_percentiles = timing
                    .zero_rtt_histogram
                    .percentiles(&[config.timing_percentile]);
                let zero_rtt_ns = zero_rtt_percentiles[0];

                if zero_rtt_ns > 0 {
                    zero_rtt_ns / 1_000_000 // convert to ms
                } else {
                    // no historical data, use bootstrap timing
                    self.get_bootstrap_timing_ms()
                }
            } else {
                handshake_ns / 1_000_000 // convert to ms
            }
        } else {
            // unknown IP, use bootstrap timing
            self.get_bootstrap_timing_ms()
        };

        // calculate solana slots needed: ceil(handshake_ms / solana_slot_duration_ms)
        let solana_slots_needed = ((handshake_ms + config.solana_slot_duration_ms - 1)
            / config.solana_slot_duration_ms) as usize;

        // clamp to [min_solana_slots, max_solana_slots]
        solana_slots_needed.clamp(config.min_solana_slots, config.max_solana_slots)
    }

    /// get bootstrap timing for unknown validators
    ///
    /// returns default handshake time in milliseconds, either from:
    /// 1. global median of known validators (if bootstrap_from_median = true)
    /// 2. configured default (fallback)
    #[inline]
    fn get_bootstrap_timing_ms(&self) -> u64 {
        let config = match &self.adaptive_config {
            Some(cfg) => cfg,
            None => return 2000, // fallback default
        };

        if config.bootstrap_from_median {
            // try to calculate median from all known validators
            if let Some(median_ms) = self.timing_registry.global_median_handshake_ms() {
                log::debug!("Bootstrap timing: using global median {:.1}ms", median_ms);
                return median_ms;
            }
        }

        // // fallback to configured default
        // log::debug!(
        //     "Bootstrap timing: using default {:.1}ms",
        //     config.default_handshake_ms
        // );
        config.default_handshake_ms
    }
}
