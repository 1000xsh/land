//! tcp/tls connection management with mio.

use crate::error::{Error, Result};
use mio::net::TcpStream;
use mio::{Events, Interest, Poll, Token};
use rustls::crypto::ring::default_provider;
use rustls::crypto::CryptoProvider;
use rustls::{ClientConfig, ClientConnection, RootCertStore};
use std::io::{Read, Write};
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

const CONNECTION: Token = Token(0);
const DEFAULT_TIMEOUT_MS: u64 = 5000;

/// connection state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnState {
    Disconnected,
    Connecting,
    TlsHandshake,
    Connected,
}

/// parsed url info.
pub struct UrlInfo {
    pub host: String,
    pub port: u16,
    pub use_tls: bool,
}

/// parse url or host:port string.
pub fn parse_addr(addr: &str) -> Result<UrlInfo> {
    let (rest, use_tls) = if addr.starts_with("https://") {
        (&addr[8..], true)
    } else if addr.starts_with("http://") {
        (&addr[7..], false)
    } else {
        if let Some(colon_pos) = addr.rfind(':') {
            let host = &addr[..colon_pos];
            let port_str = &addr[colon_pos + 1..];
            let port: u16 = port_str.parse().map_err(|_| {
                Error::Connect(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "invalid port",
                ))
            })?;
            return Ok(UrlInfo {
                host: host.to_string(),
                port,
                use_tls: false,
            });
        }
        return Err(Error::Connect(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid address format",
        )));
    };

    let rest = rest.split('/').next().unwrap_or(rest);

    let (host, port) = if let Some(colon_pos) = rest.rfind(':') {
        let potential_port = &rest[colon_pos + 1..];
        if potential_port.chars().all(|c| c.is_ascii_digit()) && !potential_port.is_empty() {
            let port: u16 = potential_port.parse().map_err(|_| {
                Error::Connect(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "invalid port",
                ))
            })?;
            (rest[..colon_pos].to_string(), port)
        } else {
            let default_port = if use_tls { 443 } else { 80 };
            (rest.to_string(), default_port)
        }
    } else {
        let default_port = if use_tls { 443 } else { 80 };
        (rest.to_string(), default_port)
    };

    Ok(UrlInfo {
        host,
        port,
        use_tls,
    })
}

/// tcp/tls connection wrapper with mio.
pub struct Connection {
    stream: Option<TcpStream>,
    tls_conn: Option<ClientConnection>,
    poll: Poll,
    events: Events,
    state: ConnState,
    addr: SocketAddr,
    host: String,
    use_tls: bool,
}

impl Connection {
    /// create a new connection.
    pub fn connect(addr: &str) -> Result<Self> {
        let url_info = parse_addr(addr)?;

        let socket_addr = format!("{}:{}", url_info.host, url_info.port)
            .to_socket_addrs()
            .map_err(Error::Connect)?
            .next()
            .ok_or_else(|| {
                Error::Connect(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "could not resolve address",
                ))
            })?;

        let poll = Poll::new().map_err(Error::Connect)?;
        let events = Events::with_capacity(16);

        let mut conn = Self {
            stream: None,
            tls_conn: None,
            poll,
            events,
            state: ConnState::Disconnected,
            addr: socket_addr,
            host: url_info.host,
            use_tls: url_info.use_tls,
        };

        conn.do_connect()?;
        Ok(conn)
    }

    fn do_connect(&mut self) -> Result<()> {
        let mut stream = TcpStream::connect(self.addr).map_err(Error::Connect)?;
        stream.set_nodelay(true).ok();

        self.poll
            .registry()
            .register(
                &mut stream,
                CONNECTION,
                Interest::READABLE | Interest::WRITABLE,
            )
            .map_err(Error::Connect)?;

        self.stream = Some(stream);
        self.state = ConnState::Connecting;

        // wait for tcp connect
        self.wait_connected()?;

        if self.use_tls {
            self.start_tls_handshake()?;
        }

        self.state = ConnState::Connected;
        Ok(())
    }

    fn wait_connected(&mut self) -> Result<()> {
        let timeout = Duration::from_millis(DEFAULT_TIMEOUT_MS);

        loop {
            self.poll
                .poll(&mut self.events, Some(timeout))
                .map_err(Error::Connect)?;

            for event in self.events.iter() {
                if event.token() == CONNECTION {
                    if event.is_writable() {
                        // check for connect error
                        if let Some(stream) = &self.stream {
                            if let Err(e) = stream.take_error() {
                                return Err(Error::Connect(e));
                            }
                        }
                        return Ok(());
                    }
                    if event.is_error() {
                        return Err(Error::Connect(std::io::Error::new(
                            std::io::ErrorKind::ConnectionRefused,
                            "connection failed",
                        )));
                    }
                }
            }

            if self.events.is_empty() {
                return Err(Error::Connect(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "connection timeout",
                )));
            }
        }
    }

    fn start_tls_handshake(&mut self) -> Result<()> {
        // install crypto provider if not already installed
        let _ = CryptoProvider::install_default(default_provider());

        let mut root_store = RootCertStore::empty();
        root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

        let config = ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();

        let server_name = self.host.clone().try_into().map_err(|_| {
            Error::Connect(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid server name",
            ))
        })?;

        let tls_conn = ClientConnection::new(Arc::new(config), server_name).map_err(|e| {
            Error::Connect(std::io::Error::new(
                std::io::ErrorKind::Other,
                e.to_string(),
            ))
        })?;

        self.tls_conn = Some(tls_conn);
        self.state = ConnState::TlsHandshake;

        // complete handshake
        self.complete_tls_handshake()
    }

    fn complete_tls_handshake(&mut self) -> Result<()> {
        let timeout = Duration::from_millis(DEFAULT_TIMEOUT_MS);
        let stream = self.stream.as_mut().unwrap();
        let tls = self.tls_conn.as_mut().unwrap();

        loop {
            if !tls.is_handshaking() {
                return Ok(());
            }

            // write tls data
            while tls.wants_write() {
                match tls.write_tls(stream) {
                    Ok(_) => {}
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => return Err(Error::Connect(e)),
                }
            }

            // read tls data
            if tls.wants_read() {
                match tls.read_tls(stream) {
                    Ok(0) => {
                        return Err(Error::Connect(std::io::Error::new(
                            std::io::ErrorKind::ConnectionReset,
                            "connection closed during handshake",
                        )));
                    }
                    Ok(_) => {
                        tls.process_new_packets().map_err(|e| {
                            Error::Connect(std::io::Error::new(
                                std::io::ErrorKind::Other,
                                e.to_string(),
                            ))
                        })?;
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        self.poll
                            .poll(&mut self.events, Some(timeout))
                            .map_err(Error::Connect)?;
                        if self.events.is_empty() {
                            return Err(Error::Connect(std::io::Error::new(
                                std::io::ErrorKind::TimedOut,
                                "tls handshake timeout",
                            )));
                        }
                    }
                    Err(e) => return Err(Error::Connect(e)),
                }
            }
        }
    }

    #[inline]
    pub fn send(&mut self, data: &[u8]) -> Result<()> {
        if self.stream.is_none() {
            return Err(Error::Send(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "not connected",
            )));
        }

        if self.use_tls {
            self.send_tls(data)
        } else {
            self.send_plain(data)
        }
    }

    fn send_plain(&mut self, data: &[u8]) -> Result<()> {
        let mut written = 0;
        while written < data.len() {
            let result = {
                let stream = self.stream.as_mut().unwrap();
                stream.write(&data[written..])
            };

            match result {
                Ok(0) => {
                    return Err(Error::Send(std::io::Error::new(
                        std::io::ErrorKind::WriteZero,
                        "write returned 0",
                    )));
                }
                Ok(n) => written += n,
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    self.wait_writable()?;
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(Error::Send(e)),
            }
        }
        Ok(())
    }

    fn send_tls(&mut self, data: &[u8]) -> Result<()> {
        // write plaintext to tls buffer
        {
            let tls = self.tls_conn.as_mut().unwrap();
            tls.writer().write_all(data).map_err(Error::Send)?;
        }

        // flush tls to network
        self.flush_tls_write()
    }

    fn flush_tls_write(&mut self) -> Result<()> {
        loop {
            let wants_write = self.tls_conn.as_ref().unwrap().wants_write();
            if !wants_write {
                return Ok(());
            }

            let result = {
                let stream = self.stream.as_mut().unwrap();
                let tls = self.tls_conn.as_mut().unwrap();
                tls.write_tls(stream)
            };

            match result {
                Ok(_) => {}
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    self.wait_writable()?;
                }
                Err(e) => return Err(Error::Send(e)),
            }
        }
    }

    #[inline]
    pub fn recv(&mut self, buf: &mut Vec<u8>) -> Result<usize> {
        if self.stream.is_none() {
            return Err(Error::Recv(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "not connected",
            )));
        }

        if self.use_tls {
            self.recv_tls(buf)
        } else {
            self.recv_plain(buf)
        }
    }

    fn recv_plain(&mut self, buf: &mut Vec<u8>) -> Result<usize> {
        let start_len = buf.len();
        buf.resize(start_len + 4096, 0);

        let result = {
            let stream = self.stream.as_mut().unwrap();
            stream.read(&mut buf[start_len..])
        };

        match result {
            Ok(0) => {
                buf.truncate(start_len);
                Err(Error::Recv(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "connection closed",
                )))
            }
            Ok(n) => {
                buf.truncate(start_len + n);
                Ok(n)
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                buf.truncate(start_len);
                Err(Error::WouldBlock)
            }
            Err(e) => {
                buf.truncate(start_len);
                Err(Error::Recv(e))
            }
        }
    }

    fn recv_tls(&mut self, buf: &mut Vec<u8>) -> Result<usize> {
        let start_len = buf.len();
        buf.resize(start_len + 4096, 0);

        // read tls records from network
        let read_result = {
            let stream = self.stream.as_mut().unwrap();
            let tls = self.tls_conn.as_mut().unwrap();
            tls.read_tls(stream)
        };

        match read_result {
            Ok(0) => {
                buf.truncate(start_len);
                return Err(Error::Recv(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "connection closed",
                )));
            }
            Ok(_) => {
                let tls = self.tls_conn.as_mut().unwrap();
                tls.process_new_packets().map_err(|e| {
                    Error::Recv(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        e.to_string(),
                    ))
                })?;
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => {
                buf.truncate(start_len);
                return Err(Error::Recv(e));
            }
        }

        // read decrypted data
        let result = {
            let tls = self.tls_conn.as_mut().unwrap();
            tls.reader().read(&mut buf[start_len..])
        };

        match result {
            Ok(0) => {
                buf.truncate(start_len);
                Err(Error::Recv(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "connection closed",
                )))
            }
            Ok(n) => {
                buf.truncate(start_len + n);
                Ok(n)
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                buf.truncate(start_len);
                Err(Error::WouldBlock)
            }
            Err(e) => {
                buf.truncate(start_len);
                Err(Error::Recv(e))
            }
        }
    }

    fn wait_writable(&mut self) -> Result<()> {
        let timeout = Duration::from_millis(DEFAULT_TIMEOUT_MS);

        loop {
            self.poll
                .poll(&mut self.events, Some(timeout))
                .map_err(Error::Send)?;

            for event in self.events.iter() {
                if event.token() == CONNECTION && event.is_writable() {
                    return Ok(());
                }
            }

            if self.events.is_empty() {
                return Err(Error::Send(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "write timeout",
                )));
            }
        }
    }

    pub fn wait_readable(&mut self) -> Result<()> {
        let timeout = Duration::from_millis(DEFAULT_TIMEOUT_MS);

        loop {
            self.poll
                .poll(&mut self.events, Some(timeout))
                .map_err(Error::Recv)?;

            for event in self.events.iter() {
                if event.token() == CONNECTION && event.is_readable() {
                    return Ok(());
                }
            }

            if self.events.is_empty() {
                return Err(Error::Recv(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "read timeout",
                )));
            }
        }
    }

    pub fn reconnect(&mut self) -> Result<()> {
        if let Some(mut stream) = self.stream.take() {
            self.poll.registry().deregister(&mut stream).ok();
        }
        self.tls_conn = None;
        self.state = ConnState::Disconnected;
        self.do_connect()
    }

    #[inline]
    pub fn is_connected(&self) -> bool {
        self.state == ConnState::Connected
    }
}
