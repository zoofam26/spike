//! Error types for `spike-stream`.

use std::fmt;

/// Everything that can go wrong inside the streaming pipeline.
#[derive(Debug, Clone)]
pub enum StreamError {
    /// The manifest asks for an encrypted stream. Spike deliberately refuses
    /// to touch stream encryption/DRM — see `LEGAL.md`.
    Encrypted { method: String },
    /// The manifest is structurally broken.
    Malformed(String),
    /// Fetching a manifest or segment failed.
    Network(String),
    /// The requested resource does not exist (or slid out of the live window).
    NotFound(String),
}

impl fmt::Display for StreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StreamError::Encrypted { method } => {
                write!(f, "encrypted streams are not supported (method: {method})")
            }
            StreamError::Malformed(detail) => write!(f, "malformed stream data: {detail}"),
            StreamError::Network(detail) => write!(f, "network error: {detail}"),
            StreamError::NotFound(detail) => write!(f, "not found: {detail}"),
        }
    }
}

impl std::error::Error for StreamError {}
