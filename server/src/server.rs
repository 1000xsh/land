///! https://graphics.stanford.edu/~seander/bithacks.html#ZeroInWord
use crate::config::ServerConfig;
use crate::error::{Result, ServerError};
use crate::http::{HttpRequest, HttpResponse};
use crate::queue::{
    create_request_queue, create_response_queue, generate_request_id, QueuedRequest,
    QueuedResponse, ResponseSender,
};
use crate::request::{JsonRpcRequest, JsonRpcResponse, ParsedRequest};
use crate::worker::Worker;
use land_quic::TransactionSender;
use land_traits::LeaderLookup;
use log::{error, info, warn};
use mio::net::{TcpListener, TcpStream};
use mio::{Events, Interest, Poll, Token};
use std::io::{self, Read, Write};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const SERVER_TOKEN: Token = Token(0);

/// buffer size for connection buffers (32KB).
const BUF_SIZE: usize = 32768;

/// maximum request size before rejection (1MB).
const MAX_REQUEST_SIZE: usize = 1024 * 1024;

/// connection state for HTTP keep-alive support.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ConnectionState {
    /// HTTP request.
    Reading,
    /// processing request, waiting for worker response.
    Processing,
    /// writing HTTP response.
    Writing,
}

/// TCP connection state with pre-allocated buffers.
struct Connection {
    /// TCP stream.
    stream: TcpStream,

    /// receive buffer (pre-allocated with capacity, reused across requests).
    recv_buf: Vec<u8>,

    /// cached position where we last searched for header end.
    last_scanned_pos: usize,

    /// cached headers_end position once found.
    headers_end_cache: Option<usize>,

    /// send buffer.
    send_buf: Vec<u8>,

    /// JSON serialization buffer (reused across keep-alive requests).
    json_buf: Vec<u8>,

    /// bytes written from send buffer.
    send_pos: usize,

    /// request ID if awaiting response.
    pending_request_id: Option<u64>,

    /// remote address.
    _addr: SocketAddr,

    /// connection state for keep-alive.
    state: ConnectionState,

    /// whether client requested connection close.
    should_close: bool,
}

impl Connection {
    fn new(stream: TcpStream, addr: SocketAddr) -> Self {
        Self {
            stream,
            recv_buf: Vec::with_capacity(BUF_SIZE), // 32KB
            last_scanned_pos: 0,
            headers_end_cache: None,
            send_buf: Vec::with_capacity(BUF_SIZE), // 32KB
            json_buf: Vec::with_capacity(4096),     // 4KB for JSON
            send_pos: 0,
            pending_request_id: None,
            _addr: addr,
            state: ConnectionState::Reading,
            should_close: false,
        }
    }

    /// get the receive buffer as a slice.
    #[inline]
    fn recv_buf(&self) -> &[u8] {
        &self.recv_buf
    }

    /// try to read from socket using stack buffer.
    fn try_read(&mut self) -> io::Result<bool> {
        // use 8KB stack buffer for read syscalls
        let mut buf = [0u8; 8192];

        loop {
            match self.stream.read(&mut buf) {
                Ok(0) => {
                    // connection closed
                    return Ok(false);
                }
                Ok(n) => {
                    // append to buffer (Vec handles growth efficiently)
                    self.recv_buf.extend_from_slice(&buf[..n]);

                    if self.recv_buf.len() > MAX_REQUEST_SIZE {
                        return Err(io::Error::new(io::ErrorKind::Other, "Request too large"));
                    }
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    // no more data available
                    break;
                }
                Err(e) => return Err(e),
            }
        }

        Ok(true)
    }

    /// try to write to socket.
    fn try_write(&mut self) -> io::Result<bool> {
        while self.send_pos < self.send_buf.len() {
            match self.stream.write(&self.send_buf[self.send_pos..]) {
                Ok(n) => {
                    self.send_pos += n;
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    // socket not ready for writing
                    return Ok(false);
                }
                Err(e) => return Err(e),
            }
        }

        // all data written
        Ok(true)
    }

    /// check if request is complete using cached header position.
    fn is_request_complete(&mut self) -> bool {
        let buf_len = self.recv_buf.len();

        // if we already found headers end, check body completion
        if let Some(headers_end) = self.headers_end_cache {
            return HttpRequest::is_body_complete(&self.recv_buf, headers_end);
        }

        if buf_len < 4 {
            return false;
        }

        // scan only new bytes for \r\n\r\n (incremental scanning)
        let start = self.last_scanned_pos.saturating_sub(3);
        for i in start..buf_len.saturating_sub(3) {
            if &self.recv_buf[i..i + 4] == b"\r\n\r\n" {
                self.headers_end_cache = Some(i);
                self.last_scanned_pos = buf_len;
                return HttpRequest::is_body_complete(&self.recv_buf, i);
            }
        }

        self.last_scanned_pos = buf_len;
        false
    }

    /// clear receive buffer after processing (reset for keep-alive).
    /// keeps capacity to avoid reallocation.
    fn clear_recv_buf(&mut self) {
        self.recv_buf.clear(); // keeps capacity
        self.last_scanned_pos = 0;
        self.headers_end_cache = None;
    }

    /// queue response for sending.
    fn queue_response(&mut self, response: Vec<u8>) {
        self.send_buf = response;
        self.send_pos = 0;
        self.state = ConnectionState::Writing;
    }

    /// check if response is fully sent.
    #[inline]
    fn is_response_sent(&self) -> bool {
        self.send_pos >= self.send_buf.len()
    }

    /// reset for next request (keep-alive).
    fn reset_for_next_request(&mut self) {
        self.clear_recv_buf();
        self.send_buf.clear();
        self.json_buf.clear(); // Keep capacity
        self.send_pos = 0;
        self.pending_request_id = None;
        self.state = ConnectionState::Reading;
    }
}

/// recycled buffers for connection reuse (avoids deallocation).
struct RecycledBuffers {
    recv_buf: Vec<u8>,
    send_buf: Vec<u8>,
    json_buf: Vec<u8>,
}

/// vec-based connection pool for O(1) indexed access with buffer recycling.
struct ConnectionPool {
    /// connections indexed by token value.
    connections: Vec<Option<Connection>>,
    /// free slots for reuse.
    free_slots: Vec<usize>,
    /// recycled buffers from closed connections (avoids malloc/free in hot path).
    recycled_buffers: Vec<RecycledBuffers>,
}

impl ConnectionPool {
    fn new(capacity: usize) -> Self {
        Self {
            connections: Vec::with_capacity(capacity),
            free_slots: Vec::new(),
            recycled_buffers: Vec::with_capacity(64), // keep up to 64 recycled buffer pairs
        }
    }

    /// create a new connection, reusing recycled buffers if available.
    fn create_connection(&mut self, stream: TcpStream, addr: SocketAddr) -> Connection {
        if let Some(recycled) = self.recycled_buffers.pop() {
            // reuse existing buffers (already have capacity allocated)
            Connection {
                stream,
                recv_buf: recycled.recv_buf, // capacity preserved
                last_scanned_pos: 0,
                headers_end_cache: None,
                send_buf: recycled.send_buf, // capacity preserved
                json_buf: recycled.json_buf, // capacity preserved
                send_pos: 0,
                pending_request_id: None,
                _addr: addr,
                state: ConnectionState::Reading,
                should_close: false,
            }
        } else {
            // no recycled buffers, create new
            Connection::new(stream, addr)
        }
    }

    /// allocate an empty slot and return its index (for use as token).
    /// call set() to fill the slot after registration.
    fn alloc_empty(&mut self) -> usize {
        if let Some(idx) = self.free_slots.pop() {
            idx
        } else {
            let idx = self.connections.len();
            self.connections.push(None);
            idx
        }
    }

    /// set connection at index (after registration with poll).
    fn set(&mut self, idx: usize, conn: Connection) {
        if idx < self.connections.len() {
            self.connections[idx] = Some(conn);
        }
    }

    /// get connection by index.
    #[inline]
    #[allow(dead_code)]
    fn _get(&self, idx: usize) -> Option<&Connection> {
        self.connections.get(idx).and_then(|c| c.as_ref())
    }

    /// get mutable connection by index.
    #[inline]
    fn get_mut(&mut self, idx: usize) -> Option<&mut Connection> {
        self.connections.get_mut(idx).and_then(|c| c.as_mut())
    }

    /// remove connection, recycling its buffers for reuse.
    fn remove(&mut self, idx: usize) {
        if idx < self.connections.len() {
            if let Some(mut conn) = self.connections[idx].take() {
                // recycle buffers if we have room (avoids deallocation)
                if self.recycled_buffers.len() < 64 {
                    conn.recv_buf.clear();
                    conn.send_buf.clear();
                    conn.json_buf.clear();
                    self.recycled_buffers.push(RecycledBuffers {
                        recv_buf: conn.recv_buf,
                        send_buf: conn.send_buf,
                        json_buf: conn.json_buf,
                    });
                }
                // otherwise buffers get dropped (normal deallocation)
            }
            self.free_slots.push(idx);
        }
    }
}

/// JSON-RPC server.
#[derive(Clone)]
pub struct RpcServer {
    config: Arc<ServerConfig>,
    shutdown: Arc<AtomicBool>,
}

impl RpcServer {
    /// create a new RPC server.
    pub fn new(config: ServerConfig) -> Result<Self> {
        config.validate().map_err(|e| ServerError::Internal(e))?;

        Ok(Self {
            config: Arc::new(config),
            shutdown: Arc::new(AtomicBool::new(false)),
        })
    }

    /// run the server with a transaction sender (blocking).
    pub fn run_with_sender<L: LeaderLookup + Send + 'static>(
        &self,
        sender: TransactionSender<L>,
    ) -> Result<()> {
        info!("Starting RPC server on {}", self.config.bind_addr);

        // create queues
        let (mut req_tx, req_rx) = create_request_queue(self.config.queue_size);
        let (resp_tx, mut resp_rx) = create_response_queue(self.config.queue_size);

        // start worker thread with QUIC sender
        let worker = Worker::new(self.config.clone(), req_rx, sender, self.shutdown.clone());
        let worker_handle = worker.start();

        // create mio poll
        let mut poll = Poll::new().map_err(ServerError::Io)?;
        let mut events = Events::with_capacity(1024);

        // create TCP listener
        let addr = self.config.bind_addr;
        let mut listener = TcpListener::bind(addr).map_err(ServerError::Io)?;

        // register listener with poll
        poll.registry()
            .register(&mut listener, SERVER_TOKEN, Interest::READABLE)
            .map_err(ServerError::Io)?;

        info!("RPC server listening on {}", addr);

        // vec-based connection pool for O(1) access
        let mut pool = ConnectionPool::new(self.config.max_connections);

        // response batch buffer (reused across loop iterations)
        let mut response_batch = Vec::with_capacity(64);

        // main event loop
        while !self.shutdown.load(Ordering::Acquire) {
            // poll for events
            // fix me
            poll.poll(&mut events, Some(Duration::from_millis(10)))
                .map_err(ServerError::Io)?;

            // process events
            for event in events.iter() {
                match event.token() {
                    SERVER_TOKEN => {
                        // accept new connections
                        loop {
                            match listener.accept() {
                                Ok((stream, addr)) => {
                                    // set TCP_NODELAY (bypassing the nagle-delayed ACK delay)
                                    if let Err(e) = stream.set_nodelay(true) {
                                        warn!("Failed to set TCP_NODELAY: {}", e);
                                    }

                                    let idx = pool.alloc_empty();
                                    let mut conn = pool.create_connection(stream, addr);
                                    let token = Token(idx + 1); // +1 to avoid SERVER_TOKEN(0)

                                    // register stream with poll
                                    poll.registry()
                                        .register(
                                            &mut conn.stream,
                                            token,
                                            Interest::READABLE | Interest::WRITABLE,
                                        )
                                        .map_err(ServerError::Io)?;

                                    // store connection after registration
                                    pool.set(idx, conn);
                                }
                                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                                    break;
                                }
                                Err(e) => {
                                    error!("Accept error: {}", e);
                                    break;
                                }
                            }
                        }
                    }
                    token => {
                        let idx = token.0 - 1; // convert token back to pool index

                        // handle client connection
                        let should_remove = if let Some(conn) = pool.get_mut(idx) {
                            let mut remove = false;

                            if event.is_readable() && conn.state == ConnectionState::Reading {
                                match conn.try_read() {
                                    Ok(false) => {
                                        // connection closed by client
                                        remove = true;
                                    }
                                    Err(e) => {
                                        error!("Read error on {:?}: {}", token, e);
                                        remove = true;
                                    }
                                    Ok(true) => {
                                        // check if request is complete
                                        if conn.is_request_complete()
                                            && conn.pending_request_id.is_none()
                                        {
                                            // process request
                                            let buf = conn.recv_buf();
                                            match self.handle_request(
                                                buf,
                                                token.0,
                                                &mut req_tx,
                                                &resp_tx,
                                            ) {
                                                Ok((req_id, connection_close)) => {
                                                    conn.pending_request_id = Some(req_id);
                                                    conn.should_close = connection_close;
                                                    conn.state = ConnectionState::Processing;
                                                    conn.clear_recv_buf();
                                                }
                                                Err(e) => {
                                                    error!(
                                                        "Failed to handle request from {:?}: {}",
                                                        token, e
                                                    );
                                                    let response = self.error_response(
                                                        serde_json::json!(null),
                                                        e,
                                                        false, // keep-alive for errors too
                                                    );
                                                    conn.queue_response(response);
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            if !remove && event.is_writable() && !conn.send_buf.is_empty() {
                                match conn.try_write() {
                                    Ok(_) => {
                                        if conn.is_response_sent() {
                                            if conn.should_close {
                                                remove = true;
                                            } else {
                                                // keep-alive: reset for next request
                                                conn.reset_for_next_request();
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        error!("Write error on {:?}: {}", token, e);
                                        remove = true;
                                    }
                                }
                            }

                            remove
                        } else {
                            false
                        };

                        if should_remove {
                            // deregister from poll before removing (prevents FD leak)
                            if let Some(conn) = pool.get_mut(idx) {
                                let _ = poll.registry().deregister(&mut conn.stream);
                            }
                            pool.remove(idx);
                        }
                    }
                }
            }

            // batch worker responses (drain all available before processing)
            // benchmark it. remove or increase?
            // I/O events first, then response. so this is unlikely
            response_batch.clear();
            while let Ok(response) = resp_rx.try_recv() {
                response_batch.push(response);
                if response_batch.len() >= 64 {
                    break; // limit batch size to avoid hoarding
                }
            }

            // process response batch
            for response in &response_batch {
                let idx = response.token - 1; // convert token back to pool index

                let should_remove = if let Some(conn) = pool.get_mut(idx) {
                    // verify this is the right request
                    if conn.pending_request_id == Some(response.request_id) {
                        let http_response =
                            self.create_response(response, conn.should_close, &mut conn.json_buf);
                        conn.queue_response(http_response);
                        conn.pending_request_id = None;

                        // try to write immediately
                        match conn.try_write() {
                            Ok(_) => {
                                if conn.is_response_sent() {
                                    if conn.should_close {
                                        true
                                    } else {
                                        // keep-alive: reset for next request
                                        conn.reset_for_next_request();
                                        false
                                    }
                                } else {
                                    false
                                }
                            }
                            Err(e) => {
                                error!("Write error on token {}: {}", response.token, e);
                                true
                            }
                        }
                    } else {
                        // request ID mismatch - stale response, ignore
                        warn!("Stale response for request_id {}", response.request_id);
                        false
                    }
                } else {
                    false
                };

                if should_remove {
                    // deregister from poll before removing (prevents file discroptor leak)
                    if let Some(conn) = pool.get_mut(idx) {
                        let _ = poll.registry().deregister(&mut conn.stream);
                    }
                    pool.remove(idx);
                }
            }
        }

        info!("Shutting down server...");
        let _ = worker_handle.join();
        info!("Server shut down");

        Ok(())
    }

    /// handle incoming HTTP request.
    /// returns (request_id, connection_close) on success.
    fn handle_request(
        &self,
        buf: &[u8],
        token: usize,
        req_tx: &mut land_channel::spsc::Producer<QueuedRequest>,
        resp_tx: &land_channel::mpsc::Producer<QueuedResponse>,
    ) -> Result<(u64, bool)> {
        // parse HTTP
        let http_req = HttpRequest::parse(buf)?;
        let connection_close = http_req.connection_close;

        // validate method and path
        if http_req.method != "POST" {
            return Err(ServerError::InvalidHttp(format!(
                "Method not allowed: {}",
                http_req.method
            )));
        }

        // parse JSON-RPC
        let rpc_req: JsonRpcRequest = serde_json::from_slice(&http_req.body)?;

        // parse and validate request
        let parsed = ParsedRequest::from_rpc_request(rpc_req)?;

        // generate request ID
        let request_id = generate_request_id();

        // create queued request with embedded token for direct response routing
        let queued = QueuedRequest {
            request_id,
            token,
            request: parsed,
            response_tx: Arc::new(ResponseSender::new(resp_tx.clone(), request_id, token)),
        };

        // send to worker
        req_tx.send(queued).map_err(|_| ServerError::QueueFull)?;

        Ok((request_id, connection_close))
    }

    /// write JSON response directly to buffer (no serde_json::Value allocations).
    /// avoiding intermediate Value creation/destruction.
    #[inline]
    fn write_json_response(json_buf: &mut Vec<u8>, response: &QueuedResponse) {
        json_buf.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"id\":");

        // write request_id as integer (no quotes, no Value allocation)
        let mut id_buf = itoa::Buffer::new();
        json_buf.extend_from_slice(id_buf.format(response.request_id).as_bytes());

        match &response.result {
            Ok(request_id_hex) => {
                json_buf.extend_from_slice(b",\"result\":{\"status\":\"queued\",\"request_id\":\"");
                json_buf.extend_from_slice(request_id_hex.as_bytes());
                json_buf.extend_from_slice(b"\"}}");
            }
            Err(error_msg) => {
                json_buf.extend_from_slice(b",\"error\":{\"code\":-32603,\"message\":\"");
                // todo: proper JSON string escaping if error messages can contain quotes
                json_buf.extend_from_slice(error_msg.as_bytes());
                json_buf.extend_from_slice(b"\"}}");
            }
        }
    }

    /// create HTTP response from worker response (reuses connections json_buf).
    fn create_response(
        &self,
        response: &QueuedResponse,
        connection_close: bool,
        json_buf: &mut Vec<u8>,
    ) -> Vec<u8> {
        // clear and reuse connections JSON buffer
        json_buf.clear();

        // write JSON directly
        Self::write_json_response(json_buf, response);

        // build HTTP response
        let mut resp = HttpResponse::ok().with_json_bytes(json_buf.clone());
        if connection_close {
            resp = resp.with_connection_close();
        }
        resp.build()
    }

    /// create error HTTP response.
    fn error_response(
        &self,
        id: serde_json::Value,
        error: ServerError,
        connection_close: bool,
    ) -> Vec<u8> {
        let json_response = JsonRpcResponse::from_error(id, error);

        // use serde_json::to_writer to avoid intermediate String
        let mut json_buf = Vec::with_capacity(256);
        serde_json::to_writer(&mut json_buf, &json_response).unwrap();

        let mut resp = HttpResponse::bad_request().with_json_bytes(json_buf);
        if connection_close {
            resp = resp.with_connection_close();
        }
        resp.build()
    }

    /// signal shutdown.
    pub fn shutdown(&self) {
        info!("Shutdown signal received");
        self.shutdown.store(true, Ordering::Release);
    }
}
