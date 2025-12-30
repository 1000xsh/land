use crate::date;
use crate::error::{Result, ServerError};

/// integer to bytes conversion using itoa (faster than manual loop).
#[inline]
fn write_usize(buf: &mut Vec<u8>, n: usize) {
    let mut buffer = itoa::Buffer::new();
    buf.extend_from_slice(buffer.format(n).as_bytes());
}

/// u16 to bytes conversion for status codes.
#[inline]
fn write_u16(buf: &mut Vec<u8>, n: u16) {
    let mut buffer = itoa::Buffer::new();
    buf.extend_from_slice(buffer.format(n).as_bytes());
}

/// parsed HTTP request.
#[derive(Debug)]
pub struct HttpRequest {
    pub method: String,
    pub _path: String,
    pub _headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// whether client requested connection close.
    /// fixme
    pub connection_close: bool,
}

impl HttpRequest {
    /// parse HTTP/1.1 request from bytes.
    pub fn parse(buf: &[u8]) -> Result<Self> {
        // find end of headers (\r\n\r\n)
        let headers_end = find_headers_end(buf)
            .ok_or_else(|| ServerError::InvalidHttp("Incomplete HTTP request".to_string()))?;

        // parse headers
        let headers_bytes = &buf[..headers_end];
        let body_start = headers_end + 4; // skip \r\n\r\n

        // split into lines
        let headers_str = std::str::from_utf8(headers_bytes)
            .map_err(|_| ServerError::InvalidHttp("Invalid UTF-8 in headers".to_string()))?;

        let mut lines = headers_str.lines();

        // parse request line
        let request_line = lines
            .next()
            .ok_or_else(|| ServerError::InvalidHttp("Empty request".to_string()))?;

        let (method, path) = parse_request_line(request_line)?;

        // parse headers
        let mut headers = Vec::new();
        let mut content_length = 0;
        let mut connection_close = false;

        for line in lines {
            if line.is_empty() {
                continue;
            }

            let (name, value) = parse_header_line(line)?;

            if name.eq_ignore_ascii_case("content-length") {
                content_length = value
                    .parse()
                    .map_err(|_| ServerError::InvalidHttp("Invalid Content-Length".to_string()))?;
            } else if name.eq_ignore_ascii_case("connection") {
                connection_close = value.eq_ignore_ascii_case("close");
            }

            headers.push((name, value));
        }

        // extract body
        let body = if body_start < buf.len() {
            let available = buf.len() - body_start;
            if available >= content_length {
                buf[body_start..body_start + content_length].to_vec()
            } else {
                return Err(ServerError::InvalidHttp(format!(
                    "Incomplete body: expected {}, got {}",
                    content_length, available
                )));
            }
        } else if content_length == 0 {
            Vec::new()
        } else {
            return Err(ServerError::InvalidHttp("Missing body".to_string()));
        };

        Ok(HttpRequest {
            method,
            _path: path,
            _headers: headers,
            body,
            connection_close,
        })
    }

    /// get header value by name (case-insensitive).
    #[allow(dead_code)]
    pub fn get_header(&self, name: &str) -> Option<&str> {
        self._headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// check if body is complete given known headers_end position.
    /// uses byte-level scanning to avoid UTF-8 conversion overhead.
    #[inline]
    pub fn is_body_complete(buf: &[u8], headers_end: usize) -> bool {
        let body_start = headers_end + 4;
        let headers_bytes = &buf[..headers_end];

        // byte-level scan for content-length (no UTF-8 conversion)
        if let Some(content_length) = find_content_length_bytes(headers_bytes) {
            let available = buf.len().saturating_sub(body_start);
            return available >= content_length;
        }

        // no content-length means no body expected
        true
    }

    /// check if request is complete (has full body).
    #[allow(dead_code)]
    pub fn is_complete(buf: &[u8]) -> bool {
        if let Some(headers_end) = find_headers_end(buf) {
            return Self::is_body_complete(buf, headers_end);
        }
        false
    }
}

/// HTTP response builder with optimized serialization.
pub struct HttpResponse {
    status: u16,
    status_text: &'static str,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
    /// use keep-alive connection (default: true).
    /// fixme
    keep_alive: bool,
}

impl HttpResponse {
    pub fn ok() -> Self {
        Self {
            status: 200,
            status_text: "OK",
            headers: Vec::new(),
            body: Vec::new(),
            keep_alive: true,
        }
    }

    pub fn bad_request() -> Self {
        Self {
            status: 400,
            status_text: "Bad Request",
            headers: Vec::new(),
            body: Vec::new(),
            keep_alive: true,
        }
    }

    #[allow(dead_code)]
    pub fn internal_error() -> Self {
        Self {
            status: 500,
            status_text: "Internal Server Error",
            headers: Vec::new(),
            body: Vec::new(),
            keep_alive: true,
        }
    }

    #[allow(dead_code)]
    pub fn with_json(mut self, json: &str) -> Self {
        self.body = json.as_bytes().to_vec();
        self.headers
            .push(("Content-Type", "application/json".to_string()));
        self
    }

    /// JSON body directly from bytes (avoids extra allocation).
    pub fn with_json_bytes(mut self, json: Vec<u8>) -> Self {
        self.body = json;
        self.headers
            .push(("Content-Type", "application/json".to_string()));
        self
    }

    /// set custom body.
    #[allow(dead_code)]
    pub fn with_body(mut self, body: Vec<u8>) -> Self {
        self.body = body;
        self
    }

    #[allow(dead_code)]
    pub fn with_header(mut self, name: &'static str, value: String) -> Self {
        self.headers.push((name, value));
        self
    }

    /// connection to close after response.
    pub fn with_connection_close(mut self) -> Self {
        self.keep_alive = false;
        self
    }

    /// response bytes using direct Vec writes (no format! overhead).
    pub fn build(self) -> Vec<u8> {
        // pre-allocate buffer: status line (~30) + headers (~150) + body
        let mut buf = Vec::with_capacity(200 + self.body.len());

        // status line: "HTTP/1.1 200 OK\r\n"
        buf.extend_from_slice(b"HTTP/1.1 ");
        write_u16(&mut buf, self.status);
        buf.push(b' ');
        buf.extend_from_slice(self.status_text.as_bytes());
        buf.extend_from_slice(b"\r\n");

        // content-length header
        buf.extend_from_slice(b"Content-Length: ");
        write_usize(&mut buf, self.body.len());
        buf.extend_from_slice(b"\r\n");

        // date header (cached, updated every 500ms in background)
        buf.extend_from_slice(b"Date: ");
        date::append_date(&mut buf);
        buf.extend_from_slice(b"\r\n");

        // custom headers
        for (name, value) in &self.headers {
            buf.extend_from_slice(name.as_bytes());
            buf.extend_from_slice(b": ");
            buf.extend_from_slice(value.as_bytes());
            buf.extend_from_slice(b"\r\n");
        }

        // connection header
        if self.keep_alive {
            buf.extend_from_slice(b"Connection: keep-alive\r\n");
        } else {
            buf.extend_from_slice(b"Connection: close\r\n");
        }

        // end headers
        buf.extend_from_slice(b"\r\n");

        // body
        buf.extend_from_slice(&self.body);
        buf
    }
}

/// end of HTTP headers (\r\n\r\n).
fn find_headers_end(buf: &[u8]) -> Option<usize> {
    for i in 0..buf.len().saturating_sub(3) {
        if &buf[i..i + 4] == b"\r\n\r\n" {
            return Some(i);
        }
    }
    None
}

/// find Content-Length value using byte-level scanning (no UTF-8 conversion).
/// case-insensitive search for "Content-Length:" header.
#[inline]
fn find_content_length_bytes(headers: &[u8]) -> Option<usize> {
    // "Content-Length:" is 15 bytes
    if headers.len() < 15 {
        return None;
    }

    let mut i = 0;
    while i + 15 <= headers.len() {
        // check for 'C' or 'c' at start of line (after \n or at position 0)
        let at_line_start = i == 0 || headers[i - 1] == b'\n';
        if at_line_start
            && (headers[i] == b'C' || headers[i] == b'c')
            && headers[i + 1..i + 15].eq_ignore_ascii_case(b"ontent-length:")
        {
            // skip whitespace after colon
            let mut j = i + 15;
            while j < headers.len() && headers[j] == b' ' {
                j += 1;
            }

            // parse digits
            let mut val = 0usize;
            while j < headers.len() && headers[j].is_ascii_digit() {
                val = val * 10 + (headers[j] - b'0') as usize;
                j += 1;
            }

            return Some(val);
        }
        i += 1;
    }
    None
}

/// parse HTTP request line (e.g., "GET / HTTP/1.1").
fn parse_request_line(line: &str) -> Result<(String, String)> {
    let parts: Vec<&str> = line.split_whitespace().collect();

    if parts.len() < 3 {
        return Err(ServerError::InvalidHttp(format!(
            "Invalid request line: {}",
            line
        )));
    }

    let method = parts[0].to_string();
    let path = parts[1].to_string();

    Ok((method, path))
}

/// parse HTTP header line (e.g., "Content-Type: application/json").
fn parse_header_line(line: &str) -> Result<(String, String)> {
    let pos = line
        .find(':')
        .ok_or_else(|| ServerError::InvalidHttp(format!("Invalid header: {}", line)))?;

    let name = line[..pos].trim().to_string();
    let value = line[pos + 1..].trim().to_string();

    Ok((name, value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_post() {
        let raw = b"POST / HTTP/1.1\r\nContent-Length: 5\r\n\r\nHello";
        let req = HttpRequest::parse(raw).unwrap();

        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/");
        assert_eq!(req.body, b"Hello");
    }

    #[test]
    fn test_is_complete() {
        let raw = b"POST / HTTP/1.1\r\nContent-Length: 5\r\n\r\nHello";
        assert!(HttpRequest::is_complete(raw));

        let raw = b"POST / HTTP/1.1\r\nContent-Length: 10\r\n\r\nHello";
        assert!(!HttpRequest::is_complete(raw));

        let raw = b"POST / HTTP/1.1\r\n";
        assert!(!HttpRequest::is_complete(raw));
    }

    #[test]
    fn test_response_build() {
        let resp = HttpResponse::ok().with_json(r#"{"result":"ok"}"#);
        let bytes = resp.build();
        let text = String::from_utf8(bytes).unwrap();

        assert!(text.contains("HTTP/1.1 200 OK"));
        assert!(text.contains("Content-Type: application/json"));
        assert!(text.contains(r#"{"result":"ok"}"#));
    }
}
