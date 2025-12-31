use crate::error::{Error, Result};
use crate::parser::{parse_slot_notification, parse_subscription_response};
use crate::types::{ConnectionState, SlotInfo, SubscriptionId};
use crate::websocket::{Frame, WebSocket};
use land_channel::spsc::{self, Consumer, Producer};
use land_cpu::SpinLoopHintWait;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub struct SlotSubscriber {
    ws_url: String,
    state: Arc<ConnectionState>,
    running: Arc<AtomicBool>,
    thread_handle: Option<JoinHandle<()>>,
    producer: Option<Producer<SlotInfo>>,
}

impl SlotSubscriber {
    // create subscriber and return consumer handle
    pub fn new(ws_url: impl Into<String>, capacity: usize) -> (Self, Consumer<SlotInfo, SpinLoopHintWait>) {
        // create spsc channel with factory for zero allocation
        let (tx, rx) = spsc::channel_with_factory(
            capacity,
            || SlotInfo::default(),
            SpinLoopHintWait,
        );

        let subscriber = Self {
            ws_url: ws_url.into(),
            state: Arc::new(ConnectionState::new()),
            running: Arc::new(AtomicBool::new(false)),
            thread_handle: None,
            producer: Some(tx),
        };

        (subscriber, rx)
    }

    // start websocket thread
    pub fn start(&mut self) -> Result<()> {
        if self.running.load(Ordering::Acquire) {
            return Err(Error::AlreadyStarted);
        }

        let tx = self.producer.take().ok_or(Error::AlreadyStarted)?;

        self.running.store(true, Ordering::Release);

        let ws_url = self.ws_url.clone();
        let state = Arc::clone(&self.state);
        let running = Arc::clone(&self.running);

        let handle = thread::Builder::new()
            .name("slot-ws".into())
            .spawn(move || {
                websocket_thread(ws_url, tx, state, running);
            })?;

        self.thread_handle = Some(handle);
        Ok(())
    }

    // stop websocket thread
    pub fn stop(&mut self) {
        self.running.store(false, Ordering::Release);
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }

    // check connection status
    #[inline]
    pub fn is_connected(&self) -> bool {
        self.state.is_connected()
    }

    // get last received slot
    #[inline]
    pub fn last_slot(&self) -> u64 {
        self.state.last_slot()
    }

    // get connection state for monitoring
    #[inline]
    pub fn connection_state(&self) -> Arc<ConnectionState> {
        Arc::clone(&self.state)
    }
}

impl Drop for SlotSubscriber {
    fn drop(&mut self) {
        self.stop();
    }
}

// websocket thread main loop
fn websocket_thread(
    ws_url: String,
    mut tx: Producer<SlotInfo>,
    state: Arc<ConnectionState>,
    running: Arc<AtomicBool>,
) {
    let mut backoff_ms = 100;
    const MAX_BACKOFF: u64 = 5000;

    while running.load(Ordering::Acquire) {
        match connect_and_subscribe(&ws_url) {
            Ok((mut ws, sub_id)) => {
                // reset backoff on successful connection
                backoff_ms = 100;
                state.set_connected(true);

                // handle messages
                handle_messages(&mut ws, sub_id, &mut tx, &state, &running);

                // disconnected
                state.set_connected(false);
            }
            Err(_) => {
                state.set_connected(false);
                thread::sleep(Duration::from_millis(backoff_ms));
                backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF);
            }
        }
    }
}

// connect to websocket and send subscription request
fn connect_and_subscribe(ws_url: &str) -> Result<(WebSocket, SubscriptionId)> {
    let mut ws = WebSocket::connect(ws_url)?;

    // send slot subscription request
    let request = r#"{"jsonrpc":"2.0","id":1,"method":"slotSubscribe","params":[]}"#;
    ws.write_text(request)?;

    // read subscription response
    match ws.read_frame()? {
        Frame::Text(text) => {
            let sub_id = parse_subscription_response(&text)
                .ok_or_else(|| Error::InvalidJson("failed to parse subscription response".to_string()))?;
            Ok((ws, sub_id))
        }
        _ => Err(Error::InvalidHandshake("expected text frame for subscription response".to_string())),
    }
}

// handle incoming websocket messages
fn handle_messages(
    ws: &mut WebSocket,
    _sub_id: SubscriptionId,
    tx: &mut Producer<SlotInfo>,
    state: &Arc<ConnectionState>,
    running: &Arc<AtomicBool>,
) {
    while running.load(Ordering::Acquire) {
        match ws.read_frame() {
            Ok(Frame::Text(text)) => {
                // parse slot notification
                if let Some(info) = parse_slot_notification(&text) {
                    // try to claim slot in channel
                    match tx.try_claim() {
                        Ok(slot) => {
                            *slot = info;
                            tx.publish();
                            state.set_last_slot(info.slot);
                        }
                        Err(_) => {
                            // channel full or disconnected, drop update
                        }
                    }
                }
            }
            Ok(Frame::Ping(data)) => {
                // respond to ping with pong
                let _ = ws.write_pong(&data);
            }
            Ok(Frame::Close) => {
                // server closed connection
                let _ = ws.write_close();
                break;
            }
            Ok(Frame::Pong(_)) => {
                // ignore pong frames
            }
            Err(_) => {
                // connection error, break and reconnect
                break;
            }
        }
    }
}
