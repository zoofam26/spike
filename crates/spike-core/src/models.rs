//! Shared domain vocabulary for Spike.
//!
//! The types here are deliberately small and serialisable-friendly so future
//! iterations can persist them or move them across thread / process borders
//! without friction.

/// How the media behind a URL is expected to be demuxed / played.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaKind {
    /// HTTP Live Streaming (`.m3u8` manifests).
    Hls,
    /// MPEG-DASH manifests (`.mpd`). Recognised but not yet played by the
    /// engine; see `ARCHITECTURE.md` for the roadmap.
    Dash,
    /// A progressive, direct media URL (mp4 / ts / mkv / radio streams...).
    Direct,
}

impl MediaKind {
    /// Best-effort sniffing from a URL. Unknown extensions fall back to
    /// [`MediaKind::Direct`], which is the safest assumption for external
    /// players.
    pub fn from_url(url: &str) -> Self {
        // Strip query strings before extension matching; tokenised CDNs are
        // extremely common in real-world playlists.
        let path = url.split(['?', '#']).next().unwrap_or(url);
        let lower = path.to_ascii_lowercase();
        if lower.ends_with(".m3u8") || lower.ends_with(".m3u") {
            MediaKind::Hls
        } else if lower.ends_with(".mpd") {
            MediaKind::Dash
        } else {
            MediaKind::Direct
        }
    }

    /// Human readable label, also used by the UI.
    pub fn label(&self) -> &'static str {
        match self {
            MediaKind::Hls => "HLS",
            MediaKind::Dash => "DASH",
            MediaKind::Direct => "DIRECT",
        }
    }
}

/// A single playable entry inside a playlist.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaylistItem {
    /// Display title, e.g. `"Arsenal vs Chelsea"`.
    pub name: String,
    /// Resolved, absolute URL.
    pub url: String,
    /// Sniffed media kind.
    pub kind: MediaKind,
    /// Optional poster / logo URL.
    pub logo: Option<String>,
    /// Free-form grouping (IPTV `group-title`), e.g. `"Football"`.
    pub group: Option<String>,
    /// EPG cross-reference id (IPTV `tvg-id`).
    pub tvg_id: Option<String>,
    /// Optional declared duration in seconds (VOD entries).
    pub duration_secs: Option<f64>,
}

/// A parsed playlist: an ordered list of items plus the group catalogue.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Playlist {
    pub items: Vec<PlaylistItem>,
    /// Distinct groups in order of first appearance, for the UI rail.
    pub groups: Vec<String>,
}

impl Playlist {
    /// All items belonging to `group`, preserving order.
    pub fn items_in_group(&self, group: &str) -> Vec<&PlaylistItem> {
        self.items
            .iter()
            .filter(|item| item.group.as_deref() == Some(group))
            .collect()
    }
}

/// A channel declaration from an XMLTV guide.
#[derive(Debug, Clone, PartialEq)]
pub struct TvChannel {
    pub id: String,
    pub display_name: Option<String>,
    pub icon: Option<String>,
}

/// A programme slot from an XMLTV guide, times normalised to unix seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct Programme {
    pub channel_id: String,
    pub start_unix: i64,
    pub stop_unix: i64,
    pub title: Option<String>,
    pub description: Option<String>,
}

impl Programme {
    /// True if `now_unix` falls inside the slot.
    pub fn is_airing(&self, now_unix: i64) -> bool {
        now_unix >= self.start_unix && now_unix < self.stop_unix
    }
}
