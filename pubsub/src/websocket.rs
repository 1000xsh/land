use crate::error::{Error, Result};
use std::io::{Read, Write};
use std::net::TcpStream;

const WEBSOCKET_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

// websocket frame opcodes
const OPCODE_TEXT: u8 = 0x1;
const OPCODE_CLOSE: u8 = 0x8;
const OPCODE_PING: u8 = 0x9;
const OPCODE_PONG: u8 = 0xA;

#[derive(Debug)]
pub enum Frame {
    Text(String),
    Ping(Vec<u8>),
    Pong(()),
    Close,
}

pub struct WebSocket {
    stream: TcpStream,
    read_buf: Vec<u8>,
}

impl WebSocket {
    // connect to websocket server and perform handshake
    pub fn connect(url: &str) -> Result<Self> {
        // parse url: ws://host:port/path
        let (host, port, path) = parse_ws_url(url)?;

        // connect tcp
        let addr = format!("{}:{}", host, port);
        let mut stream = TcpStream::connect(&addr)?;

        // generate random websocket key
        let key = generate_ws_key();

        // send http upgrade request
        let handshake = format!(
            "GET {} HTTP/1.1\r\n\
             Host: {}\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Key: {}\r\n\
             Sec-WebSocket-Version: 13\r\n\
             \r\n",
            path, host, key
        );

        stream.write_all(handshake.as_bytes())?;

        // read http response
        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf)?;
        let response = String::from_utf8_lossy(&buf[..n]);

        // verify handshake response
        if !response.contains("HTTP/1.1 101") {
            return Err(Error::InvalidHandshake(
                "server did not accept upgrade".to_string(),
            ));
        }

        // verify sec-websocket-accept header
        let expected_accept = compute_accept_key(&key);
        if !response.contains(&expected_accept) {
            return Err(Error::InvalidHandshake(
                "invalid sec-websocket-accept".to_string(),
            ));
        }

        Ok(Self {
            stream,
            read_buf: Vec::with_capacity(4096),
        })
    }

    // read next websocket frame
    pub fn read_frame(&mut self) -> Result<Frame> {
        // read first 2 bytes (header)
        let mut header = [0u8; 2];
        self.stream.read_exact(&mut header)?;

        let _fin = (header[0] & 0x80) != 0;
        let opcode = header[0] & 0x0F;
        let masked = (header[1] & 0x80) != 0;
        let mut payload_len = (header[1] & 0x7F) as u64;

        // read extended payload length if needed
        if payload_len == 126 {
            let mut len_bytes = [0u8; 2];
            self.stream.read_exact(&mut len_bytes)?;
            payload_len = u16::from_be_bytes(len_bytes) as u64;
        } else if payload_len == 127 {
            let mut len_bytes = [0u8; 8];
            self.stream.read_exact(&mut len_bytes)?;
            payload_len = u64::from_be_bytes(len_bytes);
        }

        // read mask key if present (server-to-client frames should not be masked)
        if masked {
            let mut mask = [0u8; 4];
            self.stream.read_exact(&mut mask)?;
            // note: server should not send masked frames, but we'll handle it
        }

        // read payload
        self.read_buf.clear();
        self.read_buf.resize(payload_len as usize, 0);
        self.stream.read_exact(&mut self.read_buf)?;

        // process based on opcode
        match opcode {
            OPCODE_TEXT => {
                let text = String::from_utf8_lossy(&self.read_buf).to_string();
                Ok(Frame::Text(text))
            }
            OPCODE_PING => Ok(Frame::Ping(self.read_buf.clone())),
            OPCODE_PONG => Ok(Frame::Pong(())),
            OPCODE_CLOSE => Ok(Frame::Close),
            _ => Err(Error::InvalidFrame(format!("unknown opcode: {}", opcode))),
        }
    }

    // write text frame (client-to-server must be masked)
    pub fn write_text(&mut self, text: &str) -> Result<()> {
        self.write_frame(OPCODE_TEXT, text.as_bytes())
    }

    // write pong frame
    pub fn write_pong(&mut self, data: &[u8]) -> Result<()> {
        self.write_frame(OPCODE_PONG, data)
    }

    // write close frame
    pub fn write_close(&mut self) -> Result<()> {
        self.write_frame(OPCODE_CLOSE, &[])
    }

    // write websocket frame with masking
    fn write_frame(&mut self, opcode: u8, payload: &[u8]) -> Result<()> {
        let mut frame = Vec::with_capacity(14 + payload.len());

        // first byte: FIN=1, RSV=0, opcode
        frame.push(0x80 | opcode);

        // second byte: MASK=1, payload length
        let len = payload.len();
        if len < 126 {
            frame.push(0x80 | len as u8);
        } else if len < 65536 {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(len as u64).to_be_bytes());
        }

        // generate random mask key
        let mask = generate_mask_key();
        frame.extend_from_slice(&mask);

        // mask and append payload
        for (i, &byte) in payload.iter().enumerate() {
            frame.push(byte ^ mask[i % 4]);
        }

        self.stream.write_all(&frame)?;
        Ok(())
    }
}

// parse ws://host:port/path url
fn parse_ws_url(url: &str) -> Result<(String, u16, String)> {
    let url = url.strip_prefix("ws://").ok_or_else(|| {
        Error::InvalidHandshake("url must start with ws://".to_string())
    })?;

    // find path separator
    let (host_port, path) = if let Some(idx) = url.find('/') {
        (&url[..idx], &url[idx..])
    } else {
        (url, "/")
    };

    // parse host:port
    let (host, port) = if let Some(idx) = host_port.find(':') {
        let host = &host_port[..idx];
        let port = host_port[idx + 1..]
            .parse::<u16>()
            .map_err(|_| Error::InvalidHandshake("invalid port".to_string()))?;
        (host.to_string(), port)
    } else {
        (host_port.to_string(), 80)
    };

    Ok((host, port, path.to_string()))
}

// generate random websocket key (16 bytes base64)
fn generate_ws_key() -> String {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use std::time::{SystemTime, UNIX_EPOCH};

    // use timestamp as seed for randomness (good enough for websocket key)
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;

    let bytes: [u8; 16] = [
        (nonce >> 56) as u8,
        (nonce >> 48) as u8,
        (nonce >> 40) as u8,
        (nonce >> 32) as u8,
        (nonce >> 24) as u8,
        (nonce >> 16) as u8,
        (nonce >> 8) as u8,
        nonce as u8,
        (nonce ^ 0xFF) as u8,
        (nonce ^ 0xAA) as u8,
        (nonce ^ 0x55) as u8,
        (nonce ^ 0x0F) as u8,
        (nonce ^ 0xF0) as u8,
        (nonce ^ 0x33) as u8,
        (nonce ^ 0xCC) as u8,
        (nonce ^ 0x99) as u8,
    ];

    STANDARD.encode(&bytes)
}

// compute sec-websocket-accept header value
fn compute_accept_key(key: &str) -> String {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use sha1::{Digest, Sha1};

    let concat = format!("{}{}", key, WEBSOCKET_GUID);
    let hash = Sha1::digest(concat.as_bytes());
    STANDARD.encode(&hash)
}

// generate random 4-byte mask key
fn generate_mask_key() -> [u8; 4] {
    use std::time::{SystemTime, UNIX_EPOCH};

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u32;

    [
        (nonce >> 24) as u8,
        (nonce >> 16) as u8,
        (nonce >> 8) as u8,
        nonce as u8,
    ]
}
