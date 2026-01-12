//! zero-allocation request builder.
//!
//! builds http/json-rpc requests directly into a pre-allocated buffer.

use base64::Engine;

/// build a send transaction http request into the buffer.
///
/// format:
/// ```text
/// POST / HTTP/1.1
/// Host: {host}
/// Content-Type: application/json
/// Content-Length: {len}
/// Connection: keep-alive
///
/// {"jsonrpc":"2.0","id":{id},"method":"sendTransaction","params":{"transaction":"{base64}","fanout":{fanout},"target_slot":{slot}}}
/// ```
#[inline]
pub fn build_request(
    buf: &mut Vec<u8>,
    host: &str,
    request_id: u64,
    tx: &[u8],
    fanout: usize,
    target_slot: u64,
) {
    buf.clear();

    // build the json body to know content-length
    // at the end of buffer, then move it
    let body_start = buf.len();

    // json-rpc request body
    buf.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"id\":");
    write_u64(buf, request_id);
    buf.extend_from_slice(b",\"method\":\"sendTransaction\",\"params\":{\"transaction\":\"");

    // base64 encode transaction directly into buffer
    let base64_engine = base64::engine::general_purpose::STANDARD;
    let encoded_len = base64::encoded_len(tx.len(), true).unwrap_or(0);
    let start = buf.len();
    buf.resize(start + encoded_len, 0);
    let written = base64_engine
        .encode_slice(tx, &mut buf[start..])
        .unwrap_or(0);
    buf.truncate(start + written);

    buf.extend_from_slice(b"\",\"fanout\":");
    write_usize(buf, fanout);
    buf.extend_from_slice(b",\"target_slot\":");
    write_u64(buf, target_slot);

    buf.extend_from_slice(b"}}");

    let body_len = buf.len() - body_start;

    // now build headers at the beginning
    // we need to insert before the body, so we'll rebuild
    let body = buf[body_start..].to_vec();
    buf.truncate(body_start);

    // http request line
    buf.extend_from_slice(b"POST / HTTP/1.1\r\n");

    // host header
    buf.extend_from_slice(b"Host: ");
    buf.extend_from_slice(host.as_bytes());
    buf.extend_from_slice(b"\r\n");

    // content-type
    buf.extend_from_slice(b"Content-Type: application/json\r\n");

    // content-length
    buf.extend_from_slice(b"Content-Length: ");
    write_usize(buf, body_len);
    buf.extend_from_slice(b"\r\n");

    // connection keep-alive
    buf.extend_from_slice(b"Connection: keep-alive\r\n");

    // end headers
    buf.extend_from_slice(b"\r\n");

    // body
    buf.extend_from_slice(&body);
}

/// write usize to buffer using itoa.
#[inline]
fn write_usize(buf: &mut Vec<u8>, n: usize) {
    let mut buffer = itoa::Buffer::new();
    buf.extend_from_slice(buffer.format(n).as_bytes());
}

/// write u64 to buffer using itoa.
#[inline]
fn write_u64(buf: &mut Vec<u8>, n: u64) {
    let mut buffer = itoa::Buffer::new();
    buf.extend_from_slice(buffer.format(n).as_bytes());
}

// /// write u8 to buffer using itoa.
// #[inline]
// fn write_u8(buf: &mut Vec<u8>, n: u8) {
//     let mut buffer = itoa::Buffer::new();
//     buf.extend_from_slice(buffer.format(n).as_bytes());
// }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_request() {
        let mut buf = Vec::with_capacity(4096);
        let tx = [1u8, 2, 3, 4];

        build_request(&mut buf, "127.0.0.1:8080", 1, &tx, 3, 12345, None);

        let s = String::from_utf8_lossy(&buf);
        assert!(s.contains("POST / HTTP/1.1"));
        assert!(s.contains("Host: 127.0.0.1:8080"));
        assert!(s.contains("Content-Type: application/json"));
        assert!(s.contains("\"fanout\":3"));
        assert!(s.contains("\"target_slot\":12345"));
    }

    #[test]
    fn test_build_request_with_neighbor() {
        let mut buf = Vec::with_capacity(4096);
        let tx = [1u8, 2, 3, 4];

        build_request(&mut buf, "127.0.0.1:8080", 1, &tx, 3, 12345, Some(2));
    }
}
