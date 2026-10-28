//! # spike-core
//!
//! Domain models and parsers for the Spike live-event streaming app.
//!
//! This crate is intentionally dependency-free (std only) so it can be reused
//! on every platform Spike targets, including Android, without dragging in
//! heavy transitive trees.
//!
//! * [`m3u`] — extended M3U / M3U8 playlist parsing (IPTV flavour included)
//! * [`xmltv`] — minimal XMLTV EPG guide parsing
//! * [`models`] — the shared domain vocabulary

pub mod error;
pub mod m3u;
pub mod models;
pub mod url;
pub mod xmltv;

pub use error::{CoreError, Result};
pub use models::{MediaKind, Playlist, PlaylistItem, Programme, TvChannel};
pub use url::join_url;
