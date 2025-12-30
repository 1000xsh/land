use crate::error::{Result, ServerError};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: serde_json::Value,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

impl JsonRpcRequest {
    pub fn validate(&self) -> Result<()> {
        if self.jsonrpc != "2.0" {
            return Err(ServerError::InvalidRequest(format!(
                "Invalid jsonrpc version: {}",
                self.jsonrpc
            )));
        }
        Ok(())
    }
}

/// parameters for sendTransaction method.
#[derive(Debug, Clone, Deserialize)]
pub struct SendTransactionParams {
    /// base64 encoded transaction data.
    pub transaction: String,

    /// number of targets to fanout to (default: 1).
    #[serde(default = "default_fanout")]
    pub fanout: usize,

    /// target slot number.
    pub target_slot: u64,

    /// number of neighbors per leader to send to (0-8, optional).
    /// provides geographic redundancy for ultra-low latency.
    #[serde(default)]
    pub neighbor_fanout: Option<usize>,
}

fn default_fanout() -> usize {
    1
}

/// parsed and validated request ready for processing.
#[derive(Debug, Clone)]
pub struct ParsedRequest {
    /// request ID from JSON-RPC request.
    pub id: serde_json::Value,

    /// decoded transaction bytes.
    pub transaction: Vec<u8>,

    /// number of targets for fanout.
    pub fanout: usize,

    /// Target slot number.
    pub target_slot: u64,

    /// number of neighbors per leader to send to (0-8, optional).
    pub neighbor_fanout: Option<usize>,
}

impl ParsedRequest {
    /// parse and validate a JSON-RPC request.
    pub fn from_rpc_request(req: JsonRpcRequest) -> Result<Self> {
        req.validate()?;

        if req.method != "sendTransaction" {
            return Err(ServerError::MethodNotFound(req.method.clone()));
        }

        // parse params
        let params: SendTransactionParams = serde_json::from_value(req.params)
            .map_err(|e| ServerError::InvalidParams(e.to_string()))?;

        let transaction = decode_base64(&params.transaction)?;

        // validate fanout
        if params.fanout == 0 {
            return Err(ServerError::InvalidParams(
                "fanout must be greater than 0".to_string(),
            ));
        }

        // validate neighbor_fanout if provided
        if let Some(nf) = params.neighbor_fanout {
            if nf > 8 {
                return Err(ServerError::InvalidParams(
                    "neighbor_fanout must be between 0 and 8".to_string(),
                ));
            }
        }

        Ok(ParsedRequest {
            id: req.id,
            transaction,
            fanout: params.fanout,
            target_slot: params.target_slot,
            neighbor_fanout: params.neighbor_fanout,
        })
    }
}

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    pub fn success(id: serde_json::Value, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: serde_json::Value, error: JsonRpcError) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(error),
        }
    }

    /// error response from ServerError.
    pub fn from_error(id: serde_json::Value, err: ServerError) -> Self {
        Self::error(id, JsonRpcError::from_server_error(err))
    }
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
}

impl JsonRpcError {
    pub fn from_server_error(err: ServerError) -> Self {
        Self {
            code: err.to_rpc_code(),
            message: err.to_rpc_message(),
            data: err.to_rpc_data(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct SendTransactionResult {
    pub status: String,
    pub request_id: String,
}

impl SendTransactionResult {
    pub fn queued(request_id: String) -> Self {
        Self {
            status: "queued".to_string(),
            request_id,
        }
    }
}

impl fmt::Display for ParsedRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ParsedRequest(tx_len={}, fanout={}, slot={})",
            self.transaction.len(),
            self.fanout,
            self.target_slot
        )
    }
}

/// base64 string to bytes.
fn decode_base64(s: &str) -> Result<Vec<u8>> {
    // its simple
    base64_decode(s).map_err(|e| ServerError::Base64Decode(e))
}

/// base64 decoder (standard alphabet).
fn base64_decode(input: &str) -> std::result::Result<Vec<u8>, String> {
    const DECODE_TABLE: [u8; 256] = {
        let mut table = [255u8; 256];
        let mut i = 0;
        while i < 26 {
            table[(b'A' + i) as usize] = i;
            table[(b'a' + i) as usize] = i + 26;
            i += 1;
        }
        i = 0;
        while i < 10 {
            table[(b'0' + i) as usize] = i + 52;
            i += 1;
        }
        table[b'+' as usize] = 62;
        table[b'/' as usize] = 63;
        table
    };

    let input = input.trim();
    let input_bytes = input.as_bytes();

    // remove padding
    let padding = input_bytes.iter().rev().take_while(|&&b| b == b'=').count();
    let input_len = input_bytes.len() - padding;

    if input_len == 0 {
        return Ok(Vec::new());
    }

    let output_len = (input_len * 3) / 4;
    let mut output = Vec::with_capacity(output_len);

    let mut i = 0;
    while i + 4 <= input_bytes.len() {
        let a = DECODE_TABLE[input_bytes[i] as usize];
        let b = DECODE_TABLE[input_bytes[i + 1] as usize];
        let c = DECODE_TABLE[input_bytes[i + 2] as usize];
        let d = DECODE_TABLE[input_bytes[i + 3] as usize];

        if a == 255 || b == 255 {
            return Err(format!("Invalid base64 character at position {}", i));
        }

        output.push((a << 2) | (b >> 4));

        if input_bytes[i + 2] != b'=' {
            if c == 255 {
                return Err(format!("Invalid base64 character at position {}", i + 2));
            }
            output.push((b << 4) | (c >> 2));
        }

        if input_bytes[i + 3] != b'=' {
            if d == 255 {
                return Err(format!("Invalid base64 character at position {}", i + 3));
            }
            output.push((c << 6) | d);
        }

        i += 4;
    }

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base64_decode() {
        // basic encoding
        assert_eq!(base64_decode("SGVsbG8=").unwrap(), b"Hello");
        assert_eq!(base64_decode("V29ybGQ=").unwrap(), b"World");

        // no padding
        assert_eq!(base64_decode("YWJj").unwrap(), b"abc");

        // empty
        assert_eq!(base64_decode("").unwrap(), b"");
    }

    #[test]
    fn test_json_rpc_request_validation() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: serde_json::json!(1),
            method: "sendTransaction".to_string(),
            params: serde_json::json!({}),
        };
        assert!(req.validate().is_ok());

        let req = JsonRpcRequest {
            jsonrpc: "1.0".to_string(),
            id: serde_json::json!(1),
            method: "sendTransaction".to_string(),
            params: serde_json::json!({}),
        };
        assert!(req.validate().is_err());
    }
}
