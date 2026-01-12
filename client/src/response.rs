//! response parser.
//!
//! parses http/json-rpc responses using byte-level scanning.

use crate::error::{Error, Result};
use crate::types::SendResult;

/// check if response is complete (has full body).
#[inline]
pub fn is_complete(buf: &[u8]) -> bool {
    if let Some(headers_end) = find_headers_end(buf) {
        return is_body_complete(buf, headers_end);
    }
    false
}

/// check if body is complete given headers_end position.
#[inline]
fn is_body_complete(buf: &[u8], headers_end: usize) -> bool {
    let body_start = headers_end + 4; // skip \r\n\r\n
    if let Some(content_length) = find_content_length(buf, headers_end) {
        let available = buf.len().saturating_sub(body_start);
        return available >= content_length;
    }
    // no content-length means no body or chunked (assume complete)
    true
}

/// find end of http headers (\r\n\r\n).
#[inline]
fn find_headers_end(buf: &[u8]) -> Option<usize> {
    if buf.len() < 4 {
        return None;
    }
    for i in 0..buf.len() - 3 {
        if &buf[i..i + 4] == b"\r\n\r\n" {
            return Some(i);
        }
    }
    None
}

/// find content-length header value using byte-level scanning.
#[inline]
fn find_content_length(buf: &[u8], headers_end: usize) -> Option<usize> {
    let headers = &buf[..headers_end];
    if headers.len() < 15 {
        return None;
    }

    let mut i = 0;
    while i + 15 <= headers.len() {
        // check for 'C' or 'c' at start of line
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

/// parse response and extract send result.
#[inline]
pub fn parse_response(buf: &[u8]) -> Result<SendResult> {
    let headers_end = find_headers_end(buf).ok_or(Error::InvalidHttp)?;

    // check http status (accept 200, 201, etc.)
    let status = get_http_status(buf);
    if status < 200 || status >= 300 {
        // print response for debugging
        eprintln!("http status: {}", status);
        if let Ok(s) = std::str::from_utf8(&buf[..buf.len().min(500)]) {
            eprintln!("response: {}", s);
        }
        return Err(Error::InvalidHttp);
    }

    let body_start = headers_end + 4;
    let body = &buf[body_start..];

    // parse json-rpc response
    parse_json_rpc_response(body)
}

/// get http status code from response.
#[inline]
fn get_http_status(buf: &[u8]) -> u16 {
    // HTTP/1.1 200 OK or HTTP/1.0 200 OK
    if buf.len() < 12 {
        return 0;
    }
    // find first space, then parse 3 digits
    if let Some(pos) = buf[..12].iter().position(|&b| b == b' ') {
        if pos + 4 <= buf.len() {
            let d1 = buf[pos + 1];
            let d2 = buf[pos + 2];
            let d3 = buf[pos + 3];
            if d1.is_ascii_digit() && d2.is_ascii_digit() && d3.is_ascii_digit() {
                return ((d1 - b'0') as u16 * 100)
                    + ((d2 - b'0') as u16 * 10)
                    + ((d3 - b'0') as u16);
            }
        }
    }
    0
}

// /// check if http status is 200 ok.
// #[inline]
// fn check_http_ok(buf: &[u8]) -> bool {
//     // HTTP/1.1 200 OK
//     buf.len() >= 12 && &buf[9..12] == b"200"
// }

/// parse json-rpc response body (minimal parsing).
///
/// we look for:
/// - "result" with "request_id" -> success
/// - "error" with "code" -> rpc error
#[inline]
fn parse_json_rpc_response(body: &[u8]) -> Result<SendResult> {
    // check for error first
    if let Some(code) = find_error_code(body) {
        return Err(Error::Rpc { code });
    }

    // find request_id in result
    if let Some(request_id) = find_request_id(body) {
        return Ok(SendResult { request_id });
    }

    Err(Error::InvalidResponse)
}

/// find error code in json-rpc error response.
/// looks for "error":{..."code":123...}
#[inline]
fn find_error_code(body: &[u8]) -> Option<i32> {
    // find "error"
    let error_pos = find_key(body, b"\"error\"")?;

    // find "code" after error
    let code_pos = find_key(&body[error_pos..], b"\"code\"")?;
    let abs_pos = error_pos + code_pos;

    // parse number after "code":
    parse_number_after(&body[abs_pos..])
}

/// find request_id in json-rpc result.
/// looks for "request_id":"hexstring" and parses as hex.
#[inline]
fn find_request_id(body: &[u8]) -> Option<u64> {
    // find "request_id"
    let key_pos = find_key(body, b"\"request_id\"")?;

    // find the value (after colon and quote)
    let rest = &body[key_pos..];
    let colon = find_byte(rest, b':')?;
    let quote_start = find_byte(&rest[colon..], b'"')?;
    let value_start = colon + quote_start + 1;

    if value_start >= rest.len() {
        return None;
    }

    // find end quote
    let value_rest = &rest[value_start..];
    let quote_end = find_byte(value_rest, b'"')?;

    // parse hex string
    let hex_str = &value_rest[..quote_end];
    parse_hex_u64(hex_str)
}

/// find a json key in bytes.
#[inline]
fn find_key(buf: &[u8], key: &[u8]) -> Option<usize> {
    if buf.len() < key.len() {
        return None;
    }
    for i in 0..=buf.len() - key.len() {
        if &buf[i..i + key.len()] == key {
            return Some(i);
        }
    }
    None
}

/// find a byte in buffer.
#[inline]
fn find_byte(buf: &[u8], needle: u8) -> Option<usize> {
    buf.iter().position(|&b| b == needle)
}

/// parse a number after the current position (skipping colon and whitespace).
#[inline]
fn parse_number_after(buf: &[u8]) -> Option<i32> {
    // skip "code" key
    let colon = find_byte(buf, b':')?;
    let rest = &buf[colon + 1..];

    // skip whitespace
    let mut i = 0;
    while i < rest.len() && (rest[i] == b' ' || rest[i] == b'\t') {
        i += 1;
    }

    if i >= rest.len() {
        return None;
    }

    // check for negative
    let (negative, start) = if rest[i] == b'-' {
        (true, i + 1)
    } else {
        (false, i)
    };

    // parse digits
    let mut val = 0i32;
    let mut j = start;
    while j < rest.len() && rest[j].is_ascii_digit() {
        val = val * 10 + (rest[j] - b'0') as i32;
        j += 1;
    }

    if j == start {
        return None; // no digits found
    }

    Some(if negative { -val } else { val })
}

/// parse hex string as u64.
#[inline]
fn parse_hex_u64(hex: &[u8]) -> Option<u64> {
    let mut val = 0u64;
    for &b in hex {
        let digit = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            _ => return None,
        };
        val = val.checked_mul(16)?.checked_add(digit as u64)?;
    }
    Some(val)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_complete() {
        let resp = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nHello";
        assert!(is_complete(resp));

        let resp = b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nHello";
        assert!(!is_complete(resp));
    }

    #[test]
    fn test_parse_response_success() {
        let resp = b"HTTP/1.1 200 OK\r\nContent-Length: 66\r\n\r\n{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"status\":\"queued\",\"request_id\":\"1a\"}}";
        let result = parse_response(resp).unwrap();
        assert_eq!(result.request_id, 0x1a);
    }

    #[test]
    fn test_parse_response_error() {
        let resp = b"HTTP/1.1 200 OK\r\nContent-Length: 50\r\n\r\n{\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"code\":-32602}}";
        let err = parse_response(resp).unwrap_err();
        match err {
            Error::Rpc { code } => assert_eq!(code, -32602),
            _ => panic!("expected rpc error"),
        }
    }

    #[test]
    fn test_parse_hex() {
        assert_eq!(parse_hex_u64(b"1a"), Some(26));
        assert_eq!(parse_hex_u64(b"ff"), Some(255));
        assert_eq!(parse_hex_u64(b"000000000000001a"), Some(26));
    }
}
