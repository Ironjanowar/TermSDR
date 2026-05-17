//! Typed error types for the WebSocket server module.

use thiserror::Error;

/// Errors that can occur in the WebSocket server.
#[derive(Error, Debug)]
#[allow(dead_code)]
pub enum ServerError {
    /// Invalid or malformed client command.
    #[error("invalid command: {0}")]
    InvalidCommand(String),

    /// WebSocket connection error.
    #[error("connection error: {0}")]
    Connection(String),

    /// Audio encoding error (e.g., Opus).
    #[error("encoding error: {0}")]
    Encoding(String),

    /// Server bind/startup error.
    #[error("server error: {0}")]
    Server(String),
}
