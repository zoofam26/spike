//! # spike-engine
//!
//! Pluggable playback backends behind one [`MediaBackend`] trait.
//!
//! v0.1 ships two backends:
//!
//! * [`null::NullBackend`] — a stateful no-op used for tests, CI and UI
//!   development without touching real media.
//! * [`external::ExternalProcessBackend`] — drives the system player
//!   (`mpv`, `ffplay`, `vlc`, or anything via the `SPIKE_PLAYER` env var)
//!   against Spike's locally re-served stream. This gives real, working
//!   playback on Linux, Windows and macOS without linking heavyweight
//!   media frameworks.
//!
//! The trait is designed so a future in-process backend (GStreamer paintable
//! embedded into the Slint scene) can be added without touching callers.

pub mod external;
pub mod null;

use std::fmt;

/// Errors surfaced by playback backends.
#[derive(Debug)]
pub enum EngineError {
    /// No usable player / backend could be started.
    SpawnFailed(String),
    /// The operation is not meaningful for this backend.
    Unsupported(String),
    /// Underlying process I/O failure.
    Io(std::io::Error),
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EngineError::SpawnFailed(detail) => write!(f, "cannot start backend: {detail}"),
            EngineError::Unsupported(detail) => write!(f, "unsupported: {detail}"),
            EngineError::Io(err) => write!(f, "backend io error: {err}"),
        }
    }
}

impl std::error::Error for EngineError {}

impl From<std::io::Error> for EngineError {
    fn from(err: std::io::Error) -> Self {
        EngineError::Io(err)
    }
}

/// Events a backend reports back to the application.
#[derive(Debug, Clone, PartialEq)]
pub enum PlaybackEvent {
    /// Playback actually started.
    Started,
    /// The player finished or was closed.
    Stopped,
    /// The player reported a failure.
    Failed(String),
    /// Volume changed (informational for external players).
    VolumeChanged(f32),
}

/// A media playback backend.
///
/// Call order: `open` → `play` / `pause` / `set_volume` → `stop`.
/// Poll `poll_event` to observe lifecycle transitions.
pub trait MediaBackend: Send {
    /// Point the backend at a URL (usually Spike's local re-served playlist).
    fn open(&mut self, url: &str) -> Result<(), EngineError>;
    fn play(&mut self) -> Result<(), EngineError>;
    fn pause(&mut self) -> Result<(), EngineError>;
    fn stop(&mut self) -> Result<(), EngineError>;
    /// Volume in `0.0..=1.0`.
    fn set_volume(&mut self, volume: f32) -> Result<(), EngineError>;
    /// Drain one pending lifecycle event, if any.
    fn poll_event(&mut self) -> Option<PlaybackEvent>;
    /// Backend identifier.
    fn name(&self) -> &'static str;
}

/// Create a backend by name. Unknown names fall back to the external
/// process backend.
pub fn create_backend(name: &str) -> Box<dyn MediaBackend> {
    match name {
        "null" => Box::new(null::NullBackend::new()),
        _ => Box::new(external::ExternalProcessBackend::new()),
    }
}
