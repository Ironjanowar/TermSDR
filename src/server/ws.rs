use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::RwLock;

use super::commands::{self, DispatchResult, SharedState};
use super::protocol::{self, ErrorMessage};
use crate::audio::AudioControl;
use crate::types::AppConfig;

/// Unique client identifier.
pub type ClientId = u64;

/// Sender half for pushing messages to a connected client.
pub type WsSender = tokio::sync::mpsc::UnboundedSender<Message>;

/// Registry of all connected WebSocket clients.
pub type ClientRegistry = Arc<RwLock<HashMap<ClientId, WsSender>>>;

/// Shared server state passed to handlers.
#[derive(Clone)]
pub struct ServerState {
    pub clients: ClientRegistry,
    pub config: AppConfig,
    pub shared: Arc<RwLock<SharedState>>,
    pub audio_control_tx: Option<crossbeam_channel::Sender<AudioControl>>,
    pub format_atomic: Option<Arc<std::sync::atomic::AtomicU8>>,
    next_id: Arc<std::sync::atomic::AtomicU64>,
}

impl ServerState {
    pub fn new(config: AppConfig) -> Self {
        let shared = SharedState::from_config(&config);
        Self {
            clients: Arc::new(RwLock::new(HashMap::new())),
            shared: Arc::new(RwLock::new(shared)),
            config,
            audio_control_tx: None,
            format_atomic: None,
            next_id: Arc::new(std::sync::atomic::AtomicU64::new(1)),
        }
    }

    pub fn with_format_atomic(mut self, atomic: Arc<std::sync::atomic::AtomicU8>) -> Self {
        self.format_atomic = Some(atomic);
        self
    }

    pub fn with_audio_control(mut self, tx: crossbeam_channel::Sender<AudioControl>) -> Self {
        self.audio_control_tx = Some(tx);
        self
    }

    fn next_client_id(&self) -> ClientId {
        self.next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }
}

/// Broadcast a binary message to all connected clients.
pub async fn broadcast_binary(clients: &ClientRegistry, data: Vec<u8>) {
    let registry = clients.read().await;
    let msg = Message::Binary(data.into());
    for sender in registry.values() {
        let _ = sender.send(msg.clone());
    }
}

/// Broadcast a text message to all connected clients.
pub async fn broadcast_text(clients: &ClientRegistry, text: String) {
    let registry = clients.read().await;
    let msg = Message::Text(text.into());
    for sender in registry.values() {
        let _ = sender.send(msg.clone());
    }
}

/// axum handler for WebSocket upgrade at `/ws`.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<ServerState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Handle a single WebSocket connection.
async fn handle_socket(socket: WebSocket, state: ServerState) {
    let client_id = state.next_client_id();
    let (mut ws_sink, mut ws_stream) = socket.split();

    // Create channel for outbound messages
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Message>();

    // Register client
    {
        let mut registry = state.clients.write().await;
        registry.insert(client_id, tx);
    }

    let client_count = state.clients.read().await.len();
    eprintln!("WS: client {} connected ({} total)", client_id, client_count);

    // Send initial state to the new client
    {
        let shared = state.shared.read().await;
        let state_msg = shared.to_state_message(client_count);
        if let Ok(json) = serde_json::to_string(&state_msg) {
            let _ = ws_sink.send(Message::Text(json.into())).await;
        }
    }

    // Broadcast updated client count to all OTHER clients
    {
        let shared = state.shared.read().await;
        let state_msg = shared.to_state_message(client_count);
        if let Ok(json) = serde_json::to_string(&state_msg) {
            let registry = state.clients.read().await;
            let msg = Message::Text(json.into());
            for (&id, sender) in registry.iter() {
                if id != client_id {
                    let _ = sender.send(msg.clone());
                }
            }
        }
    }

    // Spawn write task: forwards channel messages to WS sink
    let write_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if ws_sink.send(msg).await.is_err() {
                break;
            }
        }
    });

    // Read task: process incoming messages
    let clients_for_read = state.clients.clone();
    let shared_for_read = state.shared.clone();
    let audio_tx = state.audio_control_tx.clone();
    let format_atomic_for_read = state.format_atomic.clone();

    while let Some(result) = ws_stream.next().await {
        match result {
            Ok(Message::Text(text)) => {
                match serde_json::from_str::<protocol::Command>(&text) {
                    Ok(cmd) => {
                        eprintln!("WS: client {} sent command: {:?}", client_id, cmd);

                        let client_count = clients_for_read.read().await.len();

                        // Dispatch command
                        let (dispatch_result, json_to_send) = {
                            let mut shared = shared_for_read.write().await;
                            let (result, _audio_msgs) = commands::dispatch(
                                &cmd,
                                &mut shared,
                                audio_tx.as_ref(),
                            );

                            // Update format_atomic when SetAudioFormat succeeds
                            if let protocol::Command::SetAudioFormat { ref format } = cmd {
                                if matches!(result, DispatchResult::Ok) {
                                    if let Some(ref atomic) = format_atomic_for_read {
                                        use crate::audio::ws_sink::WsAudioFormat;
                                        let val = match format.as_str() {
                                            "opus" => WsAudioFormat::Opus as u8,
                                            _ => WsAudioFormat::PcmRaw as u8,
                                        };
                                        atomic.store(val, std::sync::atomic::Ordering::Relaxed);
                                    }
                                }
                            }

                            let json = match &result {
                                DispatchResult::Ok | DispatchResult::StateOnly => {
                                    serde_json::to_string(
                                        &shared.to_state_message(client_count),
                                    ).ok()
                                }
                                DispatchResult::Error(err) => {
                                    serde_json::to_string(err).ok()
                                }
                            };
                            (result, json)
                        };

                        if let Some(json) = json_to_send {
                            match dispatch_result {
                                DispatchResult::Ok => {
                                    // Broadcast to ALL clients
                                    broadcast_text(&clients_for_read, json).await;
                                }
                                DispatchResult::StateOnly => {
                                    // Send state to requesting client only
                                    let registry = clients_for_read.read().await;
                                    if let Some(sender) = registry.get(&client_id) {
                                        let _ = sender.send(Message::Text(json.into()));
                                    }
                                }
                                DispatchResult::Error(_) => {
                                    // Send error to requesting client only
                                    let registry = clients_for_read.read().await;
                                    if let Some(sender) = registry.get(&client_id) {
                                        let _ = sender.send(Message::Text(json.into()));
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("WS: client {} invalid command: {}", client_id, e);
                        let err = ErrorMessage::new("unknown", &format!("Invalid JSON: {}", e));
                        if let Ok(json) = serde_json::to_string(&err) {
                            let registry = clients_for_read.read().await;
                            if let Some(sender) = registry.get(&client_id) {
                                let _ = sender.send(Message::Text(json.into()));
                            }
                        }
                    }
                }
            }
            Ok(Message::Close(_)) => break,
            Err(_) => break,
            _ => {}
        }
    }

    // Cleanup: remove client
    {
        let mut registry = state.clients.write().await;
        registry.remove(&client_id);
    }
    write_task.abort();

    let remaining = state.clients.read().await.len();
    eprintln!("WS: client {} disconnected ({} remaining)", client_id, remaining);

    // Broadcast updated client count
    {
        let shared = state.shared.read().await;
        let state_msg = shared.to_state_message(remaining);
        if let Ok(json) = serde_json::to_string(&state_msg) {
            broadcast_text(&state.clients, json).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_server_state_creation() {
        let config = AppConfig::default();
        let state = ServerState::new(config);
        assert!(state.clients.read().await.is_empty());
    }

    #[tokio::test]
    async fn test_client_id_increments() {
        let state = ServerState::new(AppConfig::default());
        assert_eq!(state.next_client_id(), 1);
        assert_eq!(state.next_client_id(), 2);
        assert_eq!(state.next_client_id(), 3);
    }

    #[tokio::test]
    async fn test_broadcast_binary_no_clients() {
        let clients: ClientRegistry = Arc::new(RwLock::new(HashMap::new()));
        broadcast_binary(&clients, vec![0x01, 0x02, 0x03]).await;
    }

    #[tokio::test]
    async fn test_broadcast_binary_with_clients() {
        let clients: ClientRegistry = Arc::new(RwLock::new(HashMap::new()));
        let (tx1, mut rx1) = tokio::sync::mpsc::unbounded_channel();
        let (tx2, mut rx2) = tokio::sync::mpsc::unbounded_channel();

        {
            let mut registry = clients.write().await;
            registry.insert(1, tx1);
            registry.insert(2, tx2);
        }

        broadcast_binary(&clients, vec![0x01, 0x02]).await;

        let msg1 = rx1.recv().await.unwrap();
        let msg2 = rx2.recv().await.unwrap();
        assert!(matches!(msg1, Message::Binary(_)));
        assert!(matches!(msg2, Message::Binary(_)));
    }

    #[tokio::test]
    async fn test_broadcast_text_with_clients() {
        let clients: ClientRegistry = Arc::new(RwLock::new(HashMap::new()));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        {
            let mut registry = clients.write().await;
            registry.insert(1, tx);
        }

        broadcast_text(&clients, "hello".to_string()).await;

        let msg = rx.recv().await.unwrap();
        match msg {
            Message::Text(t) => assert_eq!(t.as_str(), "hello"),
            _ => panic!("expected text message"),
        }
    }

    #[tokio::test]
    async fn test_broadcast_handles_dropped_receiver() {
        let clients: ClientRegistry = Arc::new(RwLock::new(HashMap::new()));
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        drop(rx);

        {
            let mut registry = clients.write().await;
            registry.insert(1, tx);
        }

        broadcast_binary(&clients, vec![0x01]).await;
    }

    #[tokio::test]
    async fn test_client_registry_add_remove() {
        let clients: ClientRegistry = Arc::new(RwLock::new(HashMap::new()));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        {
            let mut registry = clients.write().await;
            registry.insert(42, tx);
            assert_eq!(registry.len(), 1);
            assert!(registry.contains_key(&42));
        }

        {
            let mut registry = clients.write().await;
            registry.remove(&42);
            assert!(registry.is_empty());
        }
    }

    #[tokio::test]
    async fn test_shared_state_initialized() {
        let config = AppConfig::default();
        let state = ServerState::new(config.clone());
        let shared = state.shared.read().await;
        assert!((shared.center_freq - config.center_freq).abs() < 1.0);
        assert_eq!(shared.volume, 80);
        assert_eq!(shared.demod_mode, crate::audio::demod::DemodMode::AM);
    }

    #[tokio::test]
    async fn test_server_state_with_audio_control() {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let config = AppConfig::default();
        let state = ServerState::new(config).with_audio_control(tx);
        assert!(state.audio_control_tx.is_some());
    }

    #[tokio::test]
    async fn test_broadcast_binary_many_clients() {
        // Verify broadcast works with many clients
        let clients: ClientRegistry = Arc::new(RwLock::new(HashMap::new()));
        let mut receivers = Vec::new();

        for i in 0..100 {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            clients.write().await.insert(i, tx);
            receivers.push(rx);
        }

        broadcast_binary(&clients, vec![0x01, 0x02]).await;

        for rx in &mut receivers {
            let msg = rx.recv().await.unwrap();
            assert!(matches!(msg, Message::Binary(_)));
        }
    }

    #[tokio::test]
    async fn test_broadcast_text_with_dropped_client_among_live() {
        // One dead client shouldn't affect others
        let clients: ClientRegistry = Arc::new(RwLock::new(HashMap::new()));
        let (tx1, rx1) = tokio::sync::mpsc::unbounded_channel();
        let (tx2, mut rx2) = tokio::sync::mpsc::unbounded_channel();
        drop(rx1); // Client 1 is dead

        {
            let mut registry = clients.write().await;
            registry.insert(1, tx1);
            registry.insert(2, tx2);
        }

        broadcast_text(&clients, "test".to_string()).await;

        // Client 2 should still receive the message
        let msg = rx2.recv().await.unwrap();
        match msg {
            Message::Text(t) => assert_eq!(t.as_str(), "test"),
            _ => panic!("expected text"),
        }
    }

    #[tokio::test]
    async fn test_client_id_uniqueness_across_clones() {
        // Cloned ServerState should share the AtomicU64 counter
        let state = ServerState::new(AppConfig::default());
        let state2 = state.clone();
        let id1 = state.next_client_id();
        let id2 = state2.next_client_id();
        assert_ne!(id1, id2);
    }

    #[tokio::test]
    async fn test_server_state_with_format_atomic() {
        let atomic = Arc::new(std::sync::atomic::AtomicU8::new(0));
        let state = ServerState::new(AppConfig::default())
            .with_format_atomic(atomic.clone());
        assert!(state.format_atomic.is_some());
        assert_eq!(state.format_atomic.unwrap().load(std::sync::atomic::Ordering::Relaxed), 0);
    }
}
