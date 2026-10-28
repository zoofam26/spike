//! Error types for `spike-core`.

use std::fmt;

/// Convenient result alias used across the crate.
pub type Result<T> = std::result::Result<T, CoreError>;

/// Everything that can go wrong while parsing user media data.
#[derive(Debug)]
pub enum CoreError {
    /// The playlist text violates the M3U structure in an unrecoverable way.
    InvalidPlaylist(String),
    /// The XMLTV guide is malformed beyond recovery.
    InvalidGuide(String),
    /// Wrapped I/O failure (file reads and similar).
    Io(std::io::Error),
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CoreError::InvalidPlaylist(detail) => write!(f, "invalid playlist: {detail}"),
            CoreError::InvalidGuide(detail) => write!(f, "invalid guide: {detail}"),
            CoreError::Io(err) => write!(f, "io error: {err}"),
        }
    }
}

impl std::error::Error for CoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CoreError::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for CoreError {
    fn from(err: std::io::Error) -> Self {
        CoreError::Io(err)
    }
}
