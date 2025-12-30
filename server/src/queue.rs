use crate::request::ParsedRequest;
use land_channel::{mpsc, spsc};
use land_cpu::SpinLoopHintWait;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// request ID generator.
static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// generate a unique request ID.
pub fn generate_request_id() -> u64 {
    NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
}

/// item queued for worker processing.
#[derive(Clone)]
pub struct QueuedRequest {
    /// unique request ID for tracking.
    /// fixme
    pub request_id: u64,

    /// connection token for direct response routing (avoids HashMap lookup).
    pub token: usize,

    /// parsed request data.
    pub request: ParsedRequest,

    /// response channel sender.
    pub response_tx: Arc<ResponseSender>,
}

impl Default for QueuedRequest {
    fn default() -> Self {
        // create a dummy response queue for default
        let (dummy_tx, _) = mpsc::channel(1);

        Self {
            request_id: 0,
            token: 0,
            request: ParsedRequest {
                id: serde_json::Value::Null,
                transaction: Vec::new(),
                fanout: 1,
                target_slot: 0,
                neighbor_fanout: None,
            },
            response_tx: Arc::new(ResponseSender::new(dummy_tx, 0, 0)),
        }
    }
}

/// response from worker to HTTP handler.
#[derive(Debug, Clone)]
pub struct QueuedResponse {
    /// request ID this response is for.
    pub request_id: u64,

    /// connection token for direct routing (avoids HashMap lookup).
    pub token: usize,

    /// result: either success (request_id) or error message.
    pub result: Result<String, String>,
}

impl Default for QueuedResponse {
    fn default() -> Self {
        Self {
            request_id: 0,
            token: 0,
            result: Err("Uninitialized".to_string()),
        }
    }
}

/// response sender wrapper with embedded token.
pub struct ResponseSender {
    sender: mpsc::Producer<QueuedResponse>,
    request_id: u64,
    token: usize,
}

impl ResponseSender {
    /// create a new response sender with token for direct routing.
    pub fn new(sender: mpsc::Producer<QueuedResponse>, request_id: u64, token: usize) -> Self {
        Self {
            sender,
            request_id,
            token,
        }
    }

    /// send response back to HTTP handler with embedded token.
    pub fn send(&self, result: Result<String, String>) -> Result<(), String> {
        let response = QueuedResponse {
            request_id: self.request_id,
            token: self.token,
            result,
        };

        self.sender
            .send(response)
            .map_err(|_| "Failed to send response".to_string())
    }
}

/// request queue (HTTP -> worker).
pub type RequestQueue = (spsc::Producer<QueuedRequest>, spsc::Consumer<QueuedRequest, SpinLoopHintWait>);

/// response queue (worker -> HTTP).
pub type ResponseQueue = (mpsc::Producer<QueuedResponse>, mpsc::Consumer<QueuedResponse, SpinLoopHintWait>);

/// create a request queue with given size.
pub fn create_request_queue(size: usize) -> RequestQueue {
    spsc::channel(size)
}

/// create a response queue with given size.
pub fn create_response_queue(size: usize) -> ResponseQueue {
    mpsc::channel(size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_id_generation() {
        let id1 = generate_request_id();
        let id2 = generate_request_id();
        assert!(id2 > id1);
    }

    #[test]
    fn test_queue_send_receive() {
        let (mut tx, mut rx) = create_request_queue(16);

        let req_id = generate_request_id();
        let token = 42usize;
        let parsed = ParsedRequest {
            id: serde_json::json!(1),
            transaction: vec![1, 2, 3],
            fanout: 1,
            target_slot: 12345,
            neighbor_fanout: None,
        };

        let (resp_tx, _resp_rx) = create_response_queue(16);
        let response_sender = Arc::new(ResponseSender::new(resp_tx, req_id, token));

        let queued = QueuedRequest {
            request_id: req_id,
            token,
            request: parsed.clone(),
            response_tx: response_sender,
        };

        tx.send(queued).unwrap();

        let received = rx.recv().unwrap();
        assert_eq!(received.request_id, req_id);
        assert_eq!(received.token, token);
        assert_eq!(received.request.transaction, vec![1, 2, 3]);
    }
}
