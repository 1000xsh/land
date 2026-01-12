//! land client.
//!
//! send transaction client for the land rpc server.

use crate::connection::{parse_addr, Connection};
use crate::error::{Error, Result};
use crate::request::build_request;
use crate::response::{is_complete, parse_response};
use crate::types::{SendOptions, SendResult};

const DEFAULT_SEND_BUF_SIZE: usize = 4096;
const DEFAULT_RECV_BUF_SIZE: usize = 4096;

/// land client for sending transactions.
///
/// # example
///
/// ```no_run
/// use land_client::LandClient;
///
/// let mut land = LandClient::connect("127.0.0.1:8080").unwrap();
/// let tx_bytes = vec![1, 2, 3, 4]; // your signed transaction
/// let result = land.send(&tx_bytes).unwrap();
/// println!("request_id: {}", result.request_id);
/// ```
pub struct LandClient {
    conn: Connection,
    send_buf: Vec<u8>,
    recv_buf: Vec<u8>,
    request_id: u64,
    host: String,
}

impl LandClient {
    /// connect to the land rpc server.
    ///
    /// # arguments
    ///
    /// * `addr` - server address (e.g., "127.0.0.1:8080", "https://example.com")
    ///
    /// # errors
    ///
    /// returns `Error::Connect` if connection fails.
    pub fn connect(addr: &str) -> Result<Self> {
        let url_info = parse_addr(addr)?;
        let conn = Connection::connect(addr)?;

        // build host header (hostname:port for non-standard ports)
        let host = if (url_info.use_tls && url_info.port == 443)
            || (!url_info.use_tls && url_info.port == 80)
        {
            url_info.host
        } else {
            format!("{}:{}", url_info.host, url_info.port)
        };

        Ok(Self {
            conn,
            send_buf: Vec::with_capacity(DEFAULT_SEND_BUF_SIZE),
            recv_buf: Vec::with_capacity(DEFAULT_RECV_BUF_SIZE),
            request_id: 0,
            host,
        })
    }

    /// send a transaction with default options.
    ///
    /// defaults: fanout=1, target_slot=0
    ///
    /// # arguments
    ///
    /// * `tx` - signed transaction bytes (bincode/wincode serialized)
    ///
    /// # errors
    ///
    /// returns error on connection failure, invalid response, or rpc error.
    #[inline]
    pub fn send(&mut self, tx: &[u8]) -> Result<SendResult> {
        self.send_with_opts(tx, &SendOptions::default())
    }

    /// send a transaction with custom options.
    ///
    /// # arguments
    ///
    /// * `tx` - signed transaction bytes (bincode/wincode serialized)
    /// * `opts` - send options (fanout, target_slot)
    ///
    /// # errors
    ///
    /// returns error on connection failure, invalid response, or rpc error.
    #[inline]
    pub fn send_with_opts(&mut self, tx: &[u8], opts: &SendOptions) -> Result<SendResult> {
        // increment request id
        self.request_id = self.request_id.wrapping_add(1);

        // build request
        build_request(
            &mut self.send_buf,
            &self.host,
            self.request_id,
            tx,
            opts.get_fanout(),
            opts.get_target_slot(),
        );

        // send request
        self.conn.send(&self.send_buf)?;

        // receive response
        self.recv_buf.clear();
        loop {
            // wait for data
            if self.recv_buf.is_empty() {
                self.conn.wait_readable()?;
            }

            // read available data
            match self.conn.recv(&mut self.recv_buf) {
                Ok(_) => {}
                Err(Error::WouldBlock) => {
                    self.conn.wait_readable()?;
                    continue;
                }
                Err(e) => return Err(e),
            }

            // check if response is complete
            if is_complete(&self.recv_buf) {
                break;
            }
        }

        // parse response
        parse_response(&self.recv_buf)
    }

    /// reconnect to the server.
    ///
    /// call this after a connection failure to re-establish the connection.
    pub fn reconnect(&mut self) -> Result<()> {
        self.conn.reconnect()
    }

    /// check if connected to the server.
    #[inline]
    pub fn is_connected(&self) -> bool {
        self.conn.is_connected()
    }

    /// get the current request id counter.
    #[inline]
    pub fn request_id(&self) -> u64 {
        self.request_id
    }
}
