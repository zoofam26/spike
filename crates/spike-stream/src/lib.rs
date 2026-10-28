//! # spike-stream
//!
//! The streaming heart of Spike. Everything here is `std`-only (no async
//! runtime, no external media libraries) so the same code runs unchanged on
//! Linux, Windows and Android.
//!
//! Building blocks:
//!
//! * [`hls`] — HLS master/media playlist parsing (encrypted streams are
//!   explicitly rejected rather than decrypted)
//! * [`buffer`] — a sliding-window [`buffer::SegmentBuffer`] with high/low
//!   watermarks and buffer-health reporting
//! * [`throughput`] — EWMA throughput estimation used for quality selection
//! * [`scheduler`] — the polling engine that refreshes manifests, prefetches
//!   segments ahead of the playhead and adapts quality
//! * [`server`] — a tiny localhost HTTP server that re-serves the buffered
//!   stream, so any media backend can play it with smooth, controlled
//!   buffering

pub mod buffer;
pub mod error;
pub mod hls;
pub mod scheduler;
pub mod server;
pub mod throughput;

pub use error::StreamError;
