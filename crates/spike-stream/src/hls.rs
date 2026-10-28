//! HLS playlist parsing — master (variant) and media playlists.
//!
//! The parser is tag-tolerant: unknown `#EXT-X-*` directives are ignored so
//! newer manifest revisions keep working. Encrypted streams
//! (`#EXT-X-KEY` with `METHOD != NONE`) are rejected up front; Spike does not
//! perform any form of decryption or DRM circumvention.

use spike_core::join_url;

use crate::error::StreamError;

/// One rendition in a master playlist.
#[derive(Debug, Clone, PartialEq)]
pub struct Variant {
    pub uri: String,
    /// Declared BANDWIDTH in bits per second.
    pub bandwidth: u64,
    pub codecs: Option<String>,
    /// `(width, height)` when RESOLUTION is declared.
    pub resolution: Option<(u32, u32)>,
}

/// One media segment in a media playlist.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    /// Absolute media sequence number.
    pub seq: u64,
    /// Resolved, absolute segment URL.
    pub uri: String,
    /// EXTINF duration in seconds.
    pub duration: f64,
}

/// A parsed media playlist.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaPlaylist {
    pub target_duration: f64,
    pub media_sequence: u64,
    pub segments: Vec<Segment>,
    /// True when `#EXT-X-ENDLIST` was present (VOD or ended event).
    pub finished: bool,
}

/// Result of parsing an HLS manifest.
#[derive(Debug, Clone, PartialEq)]
pub enum ParsedPlaylist {
    Master { variants: Vec<Variant> },
    Media(MediaPlaylist),
}

/// Parse an HLS manifest, resolving relative URIs against `base_url`.
pub fn parse(text: &str, base_url: &str) -> Result<ParsedPlaylist, StreamError> {
    if text.contains("#EXT-X-STREAM-INF") {
        parse_master(text, base_url)
    } else {
        parse_media(text, base_url)
    }
}

fn parse_master(text: &str, base_url: &str) -> Result<ParsedPlaylist, StreamError> {
    let mut variants = Vec::new();
    let mut pending: Option<Variant> = None;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            if !variants_encrypted(text) {
                pending = Some(parse_stream_inf(rest, base_url)?);
            }
        } else if let Some(rest) = line.strip_prefix("#EXT-X-SESSION-KEY:") {
            let method = extract_attr(rest, "METHOD").unwrap_or_default();
            if !method.eq_ignore_ascii_case("NONE") {
                return Err(StreamError::Encrypted { method });
            }
        } else if line.starts_with('#') {
            continue;
        } else if let Some(mut variant) = pending.take() {
            variant.uri = join_url(base_url, line);
            variants.push(variant);
        }
    }

    if variants_encrypted(text) {
        // The KEY tag was found while scanning; surface a clean error.
        if let Some(line) = text.lines().find(|l| l.trim().starts_with("#EXT-X-KEY:")) {
            let rest = line.trim().trim_start_matches("#EXT-X-KEY:");
            let method = extract_attr(rest, "METHOD").unwrap_or_default();
            if !method.eq_ignore_ascii_case("NONE") {
                return Err(StreamError::Encrypted { method });
            }
        }
    }

    Ok(ParsedPlaylist::Master { variants })
}

fn variants_encrypted(text: &str) -> bool {
    text.lines().any(|l| {
        let l = l.trim();
        l.starts_with("#EXT-X-KEY:") && {
            let rest = l.trim_start_matches("#EXT-X-KEY:");
            !extract_attr(rest, "METHOD")
                .unwrap_or_default()
                .eq_ignore_ascii_case("NONE")
        }
    })
}

fn parse_stream_inf(rest: &str, base_url: &str) -> Result<Variant, StreamError> {
    let bandwidth = extract_attr(rest, "BANDWIDTH")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let codecs = extract_attr(rest, "CODECS").filter(|s| !s.is_empty());
    let resolution = extract_attr(rest, "RESOLUTION").and_then(|v| {
        let mut parts = v.splitn(2, 'x');
        let w = parts.next()?.parse().ok()?;
        let h = parts.next()?.parse().ok()?;
        Some((w, h))
    });
    if bandwidth == 0 && codecs.is_none() && resolution.is_none() {
        return Err(StreamError::Malformed(
            "STREAM-INF without BANDWIDTH attribute".to_string(),
        ));
    }
    Ok(Variant {
        uri: base_url.to_string(), // replaced by caller with the next URI line
        bandwidth,
        codecs,
        resolution,
    })
}

fn parse_media(text: &str, base_url: &str) -> Result<ParsedPlaylist, StreamError> {
    let mut target_duration = 0.0f64;
    let mut media_sequence = 0u64;
    let mut seq = 0u64;
    let mut finished = false;
    let mut pending_duration: Option<f64> = None;
    let mut segments = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            let dur_str = rest.split(',').next().unwrap_or("").trim();
            let duration: f64 = dur_str
                .parse()
                .map_err(|_| StreamError::Malformed(format!("bad EXTINF value: {rest}")))?;
            pending_duration = Some(duration);
        } else if let Some(rest) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
            target_duration = rest.trim().parse().unwrap_or(0.0);
        } else if let Some(rest) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            media_sequence = rest.trim().parse().unwrap_or(0);
            seq = media_sequence;
        } else if line.starts_with("#EXT-X-ENDLIST") {
            finished = true;
        } else if let Some(rest) = line.strip_prefix("#EXT-X-KEY:") {
            let method = extract_attr(rest, "METHOD").unwrap_or_default();
            if !method.eq_ignore_ascii_case("NONE") {
                return Err(StreamError::Encrypted { method });
            }
        } else if line.starts_with('#') {
            // Unknown/ignored tag (#EXT-X-VERSION, #EXT-X-PROGRAM-DATE-TIME...).
        } else {
            let duration = pending_duration.take().unwrap_or(target_duration);
            if duration <= 0.0 {
                return Err(StreamError::Malformed(
                    "segment without EXTINF duration".to_string(),
                ));
            }
            segments.push(Segment {
                seq,
                uri: join_url(base_url, line),
                duration,
            });
            seq += 1;
        }
    }

    Ok(ParsedPlaylist::Media(MediaPlaylist {
        target_duration,
        media_sequence,
        segments,
        finished,
    }))
}

/// Extract `KEY="VALUE"` / `KEY=VALUE` from an HLS attribute list.
pub fn extract_attr(attrs: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=");
    let start = attrs.find(&needle)? + needle.len();
    let rest = &attrs[start..];
    let bytes = rest.as_bytes();
    if bytes.first() == Some(&b'"') {
        let end = rest[1..].find('"')? + 1;
        Some(rest[1..end].to_string())
    } else {
        let end = rest.find([',', ' ']).unwrap_or(rest.len());
        Some(rest[..end].to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MEDIA: &str = "\
#EXTM3U
#EXT-X-VERSION:3
#EXT-X-TARGETDURATION:4
#EXT-X-MEDIA-SEQUENCE:2680
#EXTINF:4.000,
seg_2680.ts
#EXTINF:4.000,
seg_2681.ts
#EXTINF:4.166,
https://other.example/absolute.ts
";

    #[test]
    fn parses_media_playlist() {
        let base = "https://cdn.example.com/live/index.m3u8";
        let parsed = parse(MEDIA, base).expect("parse");
        let ParsedPlaylist::Media(mp) = parsed else {
            panic!("expected media playlist");
        };
        assert_eq!(mp.target_duration, 4.0);
        assert_eq!(mp.media_sequence, 2680);
        assert!(!mp.finished);
        assert_eq!(mp.segments.len(), 3);
        assert_eq!(mp.segments[0].seq, 2680);
        assert_eq!(
            mp.segments[0].uri,
            "https://cdn.example.com/live/seg_2680.ts"
        );
        assert_eq!(mp.segments[1].seq, 2681);
        assert_eq!(mp.segments[2].uri, "https://other.example/absolute.ts");
        assert!((mp.segments[2].duration - 4.166).abs() < 1e-9);
    }

    #[test]
    fn parses_vod_endlist() {
        let text = "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\na.ts\n#EXT-X-ENDLIST\n";
        let parsed = parse(text, "https://x.example/vod.m3u8").expect("parse");
        let ParsedPlaylist::Media(mp) = parsed else {
            panic!("expected media playlist");
        };
        assert!(mp.finished);
    }

    #[test]
    fn parses_master_playlist() {
        let text = "\
#EXTM3U
#EXT-X-STREAM-INF:BANDWIDTH=1280000,RESOLUTION=640x360,CODECS=\"avc1.640029,mp4a.40.2\"
low/index.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=4128000,RESOLUTION=1920x1080
high/index.m3u8
";
        let parsed = parse(text, "https://cdn.example.com/event/master.m3u8").expect("parse");
        let ParsedPlaylist::Master { variants } = parsed else {
            panic!("expected master playlist");
        };
        assert_eq!(variants.len(), 2);
        assert_eq!(variants[0].bandwidth, 1_280_000);
        assert_eq!(variants[0].resolution, Some((640, 360)));
        assert_eq!(variants[0].codecs.as_deref(), Some("avc1.640029,mp4a.40.2"));
        assert_eq!(
            variants[0].uri,
            "https://cdn.example.com/event/low/index.m3u8"
        );
        assert_eq!(
            variants[1].uri,
            "https://cdn.example.com/event/high/index.m3u8"
        );
    }

    #[test]
    fn rejects_encrypted_media() {
        let text = "\
#EXTM3U
#EXT-X-TARGETDURATION:4
#EXT-X-KEY:METHOD=AES-128,URI=\"https://keys.example/key\"
#EXTINF:4,\nseg.ts\n";
        let err = parse(text, "https://x.example/m.m3u8").unwrap_err();
        assert!(matches!(err, StreamError::Encrypted { .. }));
    }

    #[test]
    fn allows_method_none() {
        let text = "#EXTM3U\n#EXT-X-KEY:METHOD=NONE\n#EXTINF:4,\nseg.ts\n";
        assert!(parse(text, "https://x.example/m.m3u8").is_ok());
    }

    #[test]
    fn extracts_unquoted_and_quoted_attrs() {
        let attrs = "BANDWIDTH=1280000,CODECS=\"avc1,mp4a\",SUBTITLES=\"subs\"";
        assert_eq!(extract_attr(attrs, "BANDWIDTH").as_deref(), Some("1280000"));
        assert_eq!(extract_attr(attrs, "CODECS").as_deref(), Some("avc1,mp4a"));
        assert_eq!(extract_attr(attrs, "SUBTITLES").as_deref(), Some("subs"));
        assert_eq!(extract_attr(attrs, "MISSING"), None);
    }
}
