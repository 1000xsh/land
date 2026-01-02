//! TLS certificate utilities for QUIC connections

use crate::error::{Error, Result};
use std::fs;
use std::path::Path;

/// ALPN protocol for solana TPU
pub const SOLANA_TPU_ALPN: &[u8] = b"solana-tpu";

/// Ed25519 keypair (64 bytes: 32 secret + 32 public)
pub struct Keypair {
    pub secret: [u8; 32],
    pub public: [u8; 32],
}

impl Keypair {
    /// load keypair from file (supports JSON array, raw bytes, or base58)
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        let data = fs::read(path).map_err(Error::Io)?;

        // try JSON array format: [1,2,3,...,64]
        if let Ok(bytes) = serde_json_parse_array(&data) {
            if bytes.len() == 64 {
                let mut secret = [0u8; 32];
                let mut public = [0u8; 32];
                secret.copy_from_slice(&bytes[0..32]);
                public.copy_from_slice(&bytes[32..64]);
                return Ok(Self { secret, public });
            }
        }

        // try raw 64 bytes
        if data.len() == 64 {
            let mut secret = [0u8; 32];
            let mut public = [0u8; 32];
            secret.copy_from_slice(&data[0..32]);
            public.copy_from_slice(&data[32..64]);
            return Ok(Self { secret, public });
        }

        // try base58 string
        if let Ok(s) = std::str::from_utf8(&data) {
            if let Ok(bytes) = bs58_decode(s.trim()) {
                if bytes.len() == 64 {
                    let mut secret = [0u8; 32];
                    let mut public = [0u8; 32];
                    secret.copy_from_slice(&bytes[0..32]);
                    public.copy_from_slice(&bytes[32..64]);
                    return Ok(Self { secret, public });
                }
            }
        }

        Err(Error::InvalidKeypair)
    }

    /// generate PEM-encoded private key (PKCS#8 format for Ed25519)
    pub fn private_key_pem(&self) -> Vec<u8> {
        // PKCS#8 v1 for Ed25519 (RFC 8410)
        const PKCS8_PREFIX: [u8; 16] = [
            0x30, 0x2e, // SEQUENCE, length 46
            0x02, 0x01, 0x00, // INTEGER version = 0
            0x30, 0x05, // SEQUENCE (AlgorithmIdentifier)
            0x06, 0x03, 0x2b, 0x65, 0x70, // OID 1.3.101.112 (Ed25519)
            0x04, 0x22, // OCTET STRING, length 34
            0x04, 0x20, // OCTET STRING, length 32 (the actual key)
        ];

        let mut der = Vec::with_capacity(48);
        der.extend_from_slice(&PKCS8_PREFIX);
        der.extend_from_slice(&self.secret);

        let b64 = base64_encode(&der);
        let mut pem = Vec::with_capacity(100);
        pem.extend_from_slice(b"-----BEGIN PRIVATE KEY-----\n");
        pem.extend_from_slice(b64.as_bytes());
        pem.extend_from_slice(b"\n-----END PRIVATE KEY-----\n");
        pem
    }

    /// generate PEM-encoded self-signed certificate
    pub fn certificate_pem(&self) -> Vec<u8> {
        // minimal self-signed X.509 certificate with Ed25519
        let mut cert_der = Vec::with_capacity(0xf4);

        // Certificate header
        cert_der.extend_from_slice(&[
            0x30, 0x81, 0xf6, // SEQUENCE
            0x30, 0x81, 0xa9, // tbsCertificate SEQUENCE
            0xa0, 0x03, 0x02, 0x01, 0x02, // version [0] INTEGER 2
            0x02, 0x08, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, // serialNumber
            0x30, 0x05, 0x06, 0x03, 0x2b, 0x65,
            0x70, // signature AlgorithmIdentifier (Ed25519)
            0x30, 0x16, // issuer Name SEQUENCE
            0x31, 0x14, 0x30, 0x12, 0x06, 0x03, 0x55, 0x04, 0x03, 0x0c, 0x0b, 0x53, 0x6f, 0x6c,
            0x61, 0x6e, 0x61, 0x20, 0x6e, 0x6f, 0x64, 0x65, // "solana node"
            0x30, 0x20, // validity
            0x17, 0x0d, 0x37, 0x30, 0x30, 0x31, 0x30, 0x31, 0x30, 0x30, 0x30, 0x30, 0x30, 0x30,
            0x5a, // notBefore
            0x18, 0x0f, 0x34, 0x30, 0x39, 0x36, 0x30, 0x31, 0x30, 0x31, 0x30, 0x30, 0x30, 0x30,
            0x30, 0x30, 0x5a, // notAfter
            0x30, 0x00, // subject (empty)
            0x30, 0x2a, // subjectPublicKeyInfo
            0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, // AlgorithmIdentifier (Ed25519)
            0x03, 0x21, 0x00, // BIT STRING
        ]);

        // public key
        cert_der.extend_from_slice(&self.public);

        // extensions and dummy signature
        cert_der.extend_from_slice(&[
            0xa3, 0x29, 0x30, 0x27, // extensions
            0x30, 0x17, 0x06, 0x03, 0x55, 0x1d, 0x11, 0x01, 0x01, 0xff, 0x04, 0x0d, 0x30, 0x0b,
            0x82, 0x09, 0x6c, 0x6f, 0x63, 0x61, 0x6c, 0x68, 0x6f, 0x73,
            0x74, // SAN: localhost
            0x30, 0x0c, 0x06, 0x03, 0x55, 0x1d, 0x13, 0x01, 0x01, 0xff, 0x04, 0x02, 0x30,
            0x00, // basicConstraints
            0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, // signatureAlgorithm
            0x03, 0x41, 0x00, // signatureValue BIT STRING
        ]);

        // dummy signature (64 bytes of 0xff)
        cert_der.extend_from_slice(&[0xff; 64]);

        let b64 = base64_encode(&cert_der);
        let mut pem = Vec::with_capacity(400);
        pem.extend_from_slice(b"-----BEGIN CERTIFICATE-----\n");
        pem.extend_from_slice(b64.as_bytes());
        pem.extend_from_slice(b"\n-----END CERTIFICATE-----\n");
        pem
    }
}

/// simple JSON array parser (no allocation for parsing)
fn serde_json_parse_array(data: &[u8]) -> std::result::Result<Vec<u8>, ()> {
    let s = std::str::from_utf8(data).map_err(|_| ())?;
    let s = s.trim();

    if !s.starts_with('[') || !s.ends_with(']') {
        return Err(());
    }

    let inner = &s[1..s.len() - 1];
    let mut result = Vec::with_capacity(64);

    for part in inner.split(',') {
        let num: u8 = part.trim().parse().map_err(|_| ())?;
        result.push(num);
    }

    Ok(result)
}

/// simple base58 decoder
fn bs58_decode(s: &str) -> std::result::Result<Vec<u8>, ()> {
    const ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

    let mut result = vec![0u8; s.len()];
    let mut result_len = 0;

    for c in s.bytes() {
        let mut val = ALPHABET.iter().position(|&x| x == c).ok_or(())? as u32;

        for i in 0..result_len {
            val += (result[i] as u32) * 58;
            result[i] = (val & 0xff) as u8;
            val >>= 8;
        }

        while val > 0 {
            result[result_len] = (val & 0xff) as u8;
            result_len += 1;
            val >>= 8;
        }
    }

    // handle leading zeros
    for c in s.bytes() {
        if c == b'1' {
            result[result_len] = 0;
            result_len += 1;
        } else {
            break;
        }
    }

    result.truncate(result_len);
    result.reverse();
    Ok(result)
}

/// simple base64 encoder
fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut result = String::with_capacity((data.len() + 2) / 3 * 4);

    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;

        let triple = (b0 << 16) | (b1 << 8) | b2;

        result.push(ALPHABET[((triple >> 18) & 0x3f) as usize] as char);
        result.push(ALPHABET[((triple >> 12) & 0x3f) as usize] as char);

        if chunk.len() > 1 {
            result.push(ALPHABET[((triple >> 6) & 0x3f) as usize] as char);
        } else {
            result.push('=');
        }

        if chunk.len() > 2 {
            result.push(ALPHABET[(triple & 0x3f) as usize] as char);
        } else {
            result.push('=');
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base64_encode() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
    }
}
