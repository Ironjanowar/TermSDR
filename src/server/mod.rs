//! WebSocket server for headless mode: remote control and FFT/audio streaming.

pub(crate) mod commands;
pub(crate) mod protocol;
pub(crate) mod ws;

use std::net::SocketAddr;
use std::path::Path;

use anyhow::Result;
use axum::Router;
use crossbeam_channel::Receiver;
use tokio::runtime::Runtime;

use crate::types::{AppConfig, FftResult};
use ws::{broadcast_binary, ServerState};

/// Server configuration derived from CLI args.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub bind: String,
    pub port: u16,
    pub no_web: bool,
    pub audio_format: String,
}

/// Start the WebSocket server (blocking — runs the tokio runtime).
///
/// This replaces the TUI thread in headless mode.
///
/// # Errors
/// Returns an error if the tokio runtime or WebSocket server cannot be started.
///
/// # Panics
/// Panics if the tokio runtime cannot be created.
#[allow(clippy::too_many_arguments)] // server setup requires all these parameters
pub fn run_server(
    config: AppConfig,
    server_config: ServerConfig,
    fft_rx: Receiver<FftResult>,
    audio_rx: Option<Receiver<Vec<u8>>>,
    running: std::sync::Arc<std::sync::atomic::AtomicBool>,
    format_atomic: Option<std::sync::Arc<std::sync::atomic::AtomicU8>>,
    audio_ctrl_tx: Option<crossbeam_channel::Sender<crate::audio::AudioControl>>,
    shared_center_freq: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
) -> Result<()> {
    let rt = Runtime::new()?;
    rt.block_on(async move {
        let mut state = ServerState::new(config);
        if let Some(fa) = format_atomic {
            state = state.with_format_atomic(fa);
        }
        if let Some(tx) = audio_ctrl_tx {
            state = state.with_audio_control(tx);
        }
        if let Some(sf) = shared_center_freq {
            state = state.with_shared_center_freq(sf);
        }
        let clients = state.clients.clone();

        // Build router
        let mut app = Router::new()
            .route("/ws", axum::routing::get(ws::ws_handler));

        // Serve static files from web/dist/ unless --no-web
        if !server_config.no_web {
            let web_dir = Path::new("web/dist");
            if web_dir.exists() {
                app = app.fallback_service(
                    tower_http::services::ServeDir::new(web_dir),
                );
            }
        }

        let app = app.with_state(state);

        let addr: SocketAddr = format!("{}:{}", server_config.bind, server_config.port)
            .parse()
            .expect("Invalid bind address");

        tracing::info!("TermSDR WebSocket server listening on {addr}");

        // Spawn spectrum broadcast task
        let clients_spectrum = clients.clone();
        let running_spectrum = running.clone();
        tokio::spawn(async move {
            while running_spectrum.load(std::sync::atomic::Ordering::Relaxed) {
                match fft_rx.try_recv() {
                    Ok(fft) => {
                        let frame = protocol::encode_spectrum_frame(&fft);
                        broadcast_binary(&clients_spectrum, frame).await;
                    }
                    Err(crossbeam_channel::TryRecvError::Empty) => {
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    }
                    Err(crossbeam_channel::TryRecvError::Disconnected) => break,
                }
            }
        });

        // Spawn audio broadcast task (if audio_rx provided)
        if let Some(audio_rx) = audio_rx {
            let clients_audio = clients.clone();
            let running_audio = running.clone();
            tokio::spawn(async move {
                while running_audio.load(std::sync::atomic::Ordering::Relaxed) {
                    match audio_rx.try_recv() {
                        Ok(frame) => {
                            broadcast_binary(&clients_audio, frame).await;
                        }
                        Err(crossbeam_channel::TryRecvError::Empty) => {
                            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                        }
                        Err(crossbeam_channel::TryRecvError::Disconnected) => break,
                    }
                }
            });
        }

        // Run server until shutdown
        let listener = tokio::net::TcpListener::bind(addr).await.expect("failed to bind server address");
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                while running.load(std::sync::atomic::Ordering::Relaxed) {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            })
            .await
            .expect("server error");
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_server_config_creation() {
        let cfg = ServerConfig {
            bind: "127.0.0.1".to_string(),
            port: 9090,
            no_web: true,
            audio_format: "opus".to_string(),
        };
        assert_eq!(cfg.port, 9090);
        assert!(cfg.no_web);
    }

    #[tokio::test]
    async fn test_router_builds_without_web_dir() {
        let config = AppConfig::default();
        let state = ServerState::new(config);

        let server_config = ServerConfig {
            bind: "127.0.0.1".to_string(),
            port: 0,
            no_web: true,
            audio_format: "opus".to_string(),
        };

        // Should build without panicking
        let mut app = Router::new()
            .route("/ws", axum::routing::get(ws::ws_handler));

        if !server_config.no_web {
            let web_dir = Path::new("web/dist");
            if web_dir.exists() {
                app = app.fallback_service(
                    tower_http::services::ServeDir::new(web_dir),
                );
            }
        }

        let _app: Router = app.with_state(state);
    }

    #[tokio::test]
    async fn test_server_binds_and_accepts() {
        let config = AppConfig::default();
        let state = ServerState::new(config);

        let app = Router::new()
            .route("/ws", axum::routing::get(ws::ws_handler))
            .with_state(state);

        // Bind to random port
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                })
                .await
                .unwrap();
        });

        // Try connecting
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let result = tokio::net::TcpStream::connect(addr).await;
        assert!(result.is_ok());

        let _ = handle.await;
    }
}
