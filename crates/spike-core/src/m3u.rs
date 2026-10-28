//! Extended M3U / M3U8 playlist parsing (the IPTV dialect).
//!
//! Supported directives:
//! * `#EXTM3U` — optional header (parsing is lenient)
//! * `#EXTINF:<duration>[ attrs],<title>` — the workhorse
//! * `#EXTGRP:<group>` — alternative group hint
//! * `#EXTVLCOPT:` / `#KODIPROP:` — ignored on purpose
//!
//! Attribute parsing (`tvg-id="x" tvg-logo="y" group-title="z"`) is a small
//! hand-rolled state machine, tolerant of single/double quotes and escaped
//! inner quotes.

use crate::error::{CoreError, Result};
use crate::models::{MediaKind, Playlist, PlaylistItem};
use crate::url::join_url;

/// Parse an M3U document from `text`, resolving relative URLs against
/// `base_url` when provided (pass `None` for file playlists or when entries
/// are already absolute).
pub fn parse(text: &str, base_url: Option<&str>) -> Result<Playlist> {
    let mut items: Vec<PlaylistItem> = Vec::new();
    let mut groups: Vec<String> = Vec::new();

    let mut pending_attrs: Vec<(String, String)> = Vec::new();
    let mut pending_duration: Option<f64> = None;
    let mut pending_title: Option<String> = None;
    let mut pending_group_override: Option<String> = None;

    let mut saw_extm3u = false;

    for (idx, raw_line) in text.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }

        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            // A dangling EXTINF (previous one never got its URL) is treated
            // as a recoverable blemish: drop it and start fresh.
            pending_attrs.clear();
            let (meta, title) = match rest.split_once(',') {
                Some((meta, title)) => (meta, title.trim().to_string()),
                None => (rest, String::new()),
            };
            let duration_str = meta.split(' ').next().unwrap_or("").trim();
            pending_duration = duration_str.parse::<f64>().ok().filter(|d| *d >= 0.0);
            pending_attrs = parse_attrs(meta);
            pending_title = Some(title);
            continue;
        }

        if let Some(rest) = line.strip_prefix("#EXTGRP:") {
            let group = rest.trim().to_string();
            if !group.is_empty() {
                pending_group_override = Some(group);
            }
            continue;
        }

        if line.starts_with("#EXTM3U") {
            saw_extm3u = true;
            continue;
        }

        if line.starts_with('#') {
            // Unknown directive (#EXTVLCOPT, #KODIPROP, comments...) — skip.
            continue;
        }

        // URL line.
        let title = pending_title.take();
        let attrs = std::mem::take(&mut pending_attrs);
        let duration = pending_duration.take();
        let group_override = pending_group_override.take();

        let url = match base_url {
            Some(base) => join_url(base, line),
            None => line.to_string(),
        };
        if url.is_empty() {
            return Err(CoreError::InvalidPlaylist(format!(
                "line {}: empty media URL",
                idx + 1
            )));
        }

        let get = |key: &str| -> Option<String> {
            attrs
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, v)| v.clone())
                .filter(|v| !v.is_empty())
        };

        let group = group_override.or_else(|| get("group-title"));
        if let Some(g) = group.clone() {
            if !groups.contains(&g) {
                groups.push(g);
            }
        }

        items.push(PlaylistItem {
            name: title
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| url.split('/').next_back().unwrap_or("Unnamed").to_string()),
            url,
            kind: MediaKind::from_url(line),
            logo: get("tvg-logo"),
            group,
            tvg_id: get("tvg-id"),
            duration_secs: duration,
        });
    }

    if items.is_empty() && !saw_extm3u {
        return Err(CoreError::InvalidPlaylist(
            "no #EXTM3U header and no media entries found".to_string(),
        ));
    }

    Ok(Playlist { items, groups })
}

/// Parse `KEY="VALUE"` pairs out of an EXTINF meta section.
///
/// Tolerates single quotes, spaces inside values and `\"` escapes.
fn parse_attrs(meta: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let chars: Vec<char> = meta.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        // Skip until a potential key start.
        while i < chars.len() && !chars[i].is_ascii_alphabetic() && chars[i] != '_' {
            i += 1;
        }
        let key_start = i;
        while i < chars.len()
            && (chars[i].is_ascii_alphanumeric() || chars[i] == '-' || chars[i] == '_')
        {
            i += 1;
        }
        if key_start == i {
            i += 1; // no progress safety
            continue;
        }
        let key: String = chars[key_start..i].iter().collect();

        // Skip whitespace before '='.
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() || chars[i] != '=' {
            continue; // bare word (e.g. the duration), skip
        }
        i += 1; // consume '='
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }

        let value: String = if i < chars.len() && (chars[i] == '"' || chars[i] == '\'') {
            let quote = chars[i];
            i += 1;
            let mut val = String::new();
            while i < chars.len() && chars[i] != quote {
                if chars[i] == '\\' && i + 1 < chars.len() && chars[i + 1] == quote {
                    i += 1;
                }
                val.push(chars[i]);
                i += 1;
            }
            i += 1; // closing quote (tolerate EOF)
            val
        } else {
            let val_start = i;
            while i < chars.len() && chars[i] != ' ' && chars[i] != '\t' {
                i += 1;
            }
            chars[val_start..i].iter().collect()
        };

        out.push((key, value));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIMPLE: &str = "\
#EXTM3U
#EXTINF:-1 tvg-id=\"nasa.tv\" tvg-logo=\"https://example.com/nasa.png\" group-title=\"Space\",NASA TV
https://example.com/nasa/master.m3u8
#EXTINF:-1 group-title=\"Space\",Rocket Launch Feed
https://example.com/launch.ts
";

    #[test]
    fn parses_basic_extended_playlist() {
        let pl = parse(SIMPLE, None).expect("parse");
        assert_eq!(pl.items.len(), 2);
        assert_eq!(pl.groups, vec!["Space".to_string()]);

        let first = &pl.items[0];
        assert_eq!(first.name, "NASA TV");
        assert_eq!(first.tvg_id.as_deref(), Some("nasa.tv"));
        assert_eq!(first.logo.as_deref(), Some("https://example.com/nasa.png"));
        assert_eq!(first.group.as_deref(), Some("Space"));
        assert_eq!(first.kind, MediaKind::Hls);

        assert_eq!(pl.items[1].kind, MediaKind::Direct);
    }

    #[test]
    fn parses_without_header_and_with_extgrp() {
        let text = "\
#EXTINF:3600,Match Of The Day
https://cdn.example.com/motd.mp4
#EXTINF:-1,Radio
#EXTGRP:Audio
https://cdn.example.com/radio.aac
";
        let pl = parse(text, None).expect("parse");
        assert_eq!(pl.items.len(), 2);
        assert_eq!(pl.items[0].duration_secs, Some(3600.0));
        assert_eq!(pl.items[0].kind, MediaKind::Direct);
        assert_eq!(pl.items[1].group.as_deref(), Some("Audio"));
        assert_eq!(pl.groups, vec!["Audio".to_string()]);
    }

    #[test]
    fn empty_document_is_an_error() {
        let err = parse("", None).unwrap_err();
        assert!(matches!(err, CoreError::InvalidPlaylist(_)));
    }

    #[test]
    fn attribute_parser_handles_quotes_and_escapes() {
        let attrs = parse_attrs(r#" -1 tvg-name="Quote \"x\"" group-title='Late Night'"#);
        assert_eq!(
            attrs,
            vec![
                ("tvg-name".to_string(), "Quote \"x\"".to_string()),
                ("group-title".to_string(), "Late Night".to_string()),
            ]
        );
    }

    #[test]
    fn relative_resolution_from_base_url() {
        let text = "#EXTM3U\n#EXTINF:-1,Rel\nseg0.ts\n";
        let pl = parse(text, Some("https://cdn.example.com/live/index.m3u8")).expect("parse");
        assert_eq!(pl.items[0].url, "https://cdn.example.com/live/seg0.ts");
    }
}
