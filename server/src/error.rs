use thiserror::Error;

/// errors that can occur in the RPC server.
#[derive(Debug, Error)]
pub enum ServerError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON parse error: {0}")]
    JsonParse(#[from] serde_json::Error),

    #[error("Invalid JSON-RPC request: {0}")]
    InvalidRequest(String),

    #[error("Method not found: {0}")]
    MethodNotFound(String),

    #[error("Invalid parameters: {0}")]
    InvalidParams(String),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Queue full")]
    QueueFull,

    #[error("Invalid HTTP request: {0}")]
    InvalidHttp(String),

    #[error("Base64 decode error: {0}")]
    Base64Decode(String),

    #[error("Channel send error")]
    ChannelSend,

    #[error("Channel receive error")]
    ChannelReceive,
}

impl ServerError {
    /// convert error to JSON-RPC error code.
    pub fn to_rpc_code(&self) -> i32 {
        match self {
            ServerError::JsonParse(_) => -32700, // parse error
            ServerError::InvalidRequest(_) => -32600, // invalid request
            ServerError::MethodNotFound(_) => -32601, // method not found
            ServerError::InvalidParams(_) | ServerError::Base64Decode(_) => -32602, // ivalid params
            _ => -32603, // internal error
        }
    }

    /// get error message for JSON-RPC response.
    pub fn to_rpc_message(&self) -> String {
        match self {
            ServerError::JsonParse(_) => "Parse error".to_string(),
            ServerError::InvalidRequest(_) => "Invalid Request".to_string(),
            ServerError::MethodNotFound(_) => "Method not found".to_string(),
            ServerError::InvalidParams(_) => "Invalid params".to_string(),
            ServerError::Base64Decode(_) => "Invalid params".to_string(),
            _ => "Internal error".to_string(),
        }
    }

    /// get optional error data for JSON-RPC response.
    pub fn to_rpc_data(&self) -> Option<String> {
        Some(self.to_string())
    }
}

pub type Result<T> = std::result::Result<T, ServerError>;
