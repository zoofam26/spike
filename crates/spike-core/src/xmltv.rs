//! Minimal XMLTV EPG parsing.
//!
//! Scope for v0.1: extract `<channel>` and `<programme>` records with their
//! `id`/`start`/`stop`/`<title>`/`<desc>` fields. Namespaced attributes,
//! CDATA and the full XMLTV surface are deliberately out of scope — real
//! guides are huge and Spike only needs a thin slice for the "what's on now"
//! strip.
//!
//! The parser is a tolerant scanner, not a validating XML parser: it walks
//! the text looking for the tags it understands and ignores everything else.

use crate::error::{CoreError, Result};
use crate::models::{Programme, TvChannel};

/// Parsed guide slice.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Guide {
    pub channels: Vec<TvChannel>,
    pub programmes: Vec<Programme>,
}

/// Parse an XMLTV document.
pub fn parse(text: &str) -> Result<Guide> {
    if !text.contains("<tv") {
        return Err(CoreError::InvalidGuide(
            "missing root <tv> element".to_string(),
        ));
    }

    let mut guide = Guide::default();

    let mut rest = text;
    while let Some(pos) = rest.find("<channel") {
        let (_, after) = rest.split_at(pos);
        let end = match after.find("</channel>") {
            Some(end) => end + "</channel>".len(),
            None => after.len(),
        };
        let block = &after[..end];
        if let Some(ch) = parse_channel(block) {
            guide.channels.push(ch);
        }
        rest = &after[end..];
    }

    let mut rest = text;
    while let Some(pos) = rest.find("<programme") {
        let (_, after) = rest.split_at(pos);
        let end = match after.find("</programme>") {
            Some(end) => end + "</programme>".len(),
            None => break,
        };
        let block = &after[..end];
        if let Some(p) = parse_programme(block) {
            guide.programmes.push(p);
        }
        rest = &after[end..];
    }

    Ok(guide)
}

fn attr_value<'a>(block: &'a str, name: &str) -> Option<&'a str> {
    let needle = format!("{name}=");
    let idx = block.find(&needle)?;
    let after = &block[idx + needle.len()..];
    let after = after.trim_start();
    let quote = after.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let rest = &after[1..];
    let end = rest.find(quote)?;
    Some(&rest[..end])
}

fn inner_text(block: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}");
    let start = block.find(&open)?;
    let after_open = &block[start..];
    let gt = after_open.find('>')?;
    let after = &after_open[gt + 1..];
    let close = format!("</{tag}>");
    let end = after.find(&close)?;
    let text = &after[..end];
    let text = text
        .strip_prefix("<![CDATA[")
        .and_then(|t| t.strip_suffix("]]>"))
        .unwrap_or(text);
    Some(decode_entities(text.trim()))
}

fn decode_entities(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

fn parse_channel(block: &str) -> Option<TvChannel> {
    let id = attr_value(block, "id")?.to_string();
    Some(TvChannel {
        id,
        display_name: inner_text(block, "display-name"),
        icon: attr_value(block, "src").map(str::to_string),
    })
}

fn parse_programme(block: &str) -> Option<Programme> {
    let channel_id = attr_value(block, "channel")?.to_string();
    let start_unix = parse_xmltv_time(attr_value(block, "start")?)?;
    let stop_unix = parse_xmltv_time(attr_value(block, "stop").unwrap_or("")).unwrap_or(i64::MAX);
    Some(Programme {
        channel_id,
        start_unix,
        stop_unix,
        title: inner_text(block, "title"),
        description: inner_text(block, "desc"),
    })
}

/// Parse XMLTV timestamps like `20260101203000 +0000` or `20260101203000`.
///
/// The embedded timezone offset (if present) is applied so the result is a
/// true unix timestamp. Naive timestamps are treated as UTC — documented
/// behaviour for v0.1.
fn parse_xmltv_time(raw: &str) -> Option<i64> {
    let mut parts = raw.trim().split(' ');
    let digits: String = parts
        .next()?
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect();
    if digits.len() < 14 {
        return None;
    }
    let year: i32 = digits[0..4].parse().ok()?;
    let month: u32 = digits[4..6].parse().ok()?;
    let day: u32 = digits[6..8].parse().ok()?;
    let hour: u32 = digits[8..10].parse().ok()?;
    let minute: u32 = digits[10..12].parse().ok()?;
    let second: u32 = digits[12..14].parse().ok()?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }

    let days = days_from_civil(year, month, day);
    let mut unix = days * 86_400 + hour as i64 * 3600 + minute as i64 * 60 + second as i64;

    // Apply the trailing offset (e.g. `+0300`, `-0800`) to reach UTC.
    if let Some(tz) = parts.next() {
        let tz = tz.trim();
        let (sign, digits) = match tz.as_bytes().first() {
            Some(b'+') => (1i64, &tz[1..]),
            Some(b'-') => (-1i64, &tz[1..]),
            _ => (0i64, ""),
        };
        let clean: String = digits.chars().filter(|c| c.is_ascii_digit()).collect();
        if clean.len() >= 4 {
            let hh: i64 = clean[0..2].parse().ok()?;
            let mm: i64 = clean[2..4].parse().ok()?;
            unix -= sign * (hh * 3600 + mm * 60);
        }
    }

    Some(unix)
}

/// Howard Hinnant's `days_from_civil` algorithm — days since 1970-01-01.
fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = i64::from(if m <= 2 { y - 1 } else { y });
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = i64::from(m);
    let d = i64::from(d);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    const GUIDE: &str = r#"<?xml version="1.0"?>
<tv generator-info-name="spike-test">
  <channel id="nasa.tv">
    <display-name>NASA TV</display-name>
    <icon src="https://example.com/nasa.png"/>
  </channel>
  <programme start="20260101203000 +0000" stop="20260101213000 +0000" channel="nasa.tv">
    <title>Launch Window Live</title>
    <desc>Coverage &amp; analysis.</desc>
  </programme>
  <programme start="20260101213000 +0000" stop="20260101223000 +0000" channel="nasa.tv">
    <title><![CDATA[Deep Space Q&A]]></title>
  </programme>
</tv>"#;

    #[test]
    fn parses_channels_and_programmes() {
        let guide = parse(GUIDE).expect("parse");
        assert_eq!(guide.channels.len(), 1);
        assert_eq!(guide.channels[0].id, "nasa.tv");
        assert_eq!(guide.channels[0].display_name.as_deref(), Some("NASA TV"));
        assert_eq!(guide.programmes.len(), 2);
        assert_eq!(
            guide.programmes[0].title.as_deref(),
            Some("Launch Window Live")
        );
        assert_eq!(
            guide.programmes[0].description.as_deref(),
            Some("Coverage & analysis.")
        );
        assert_eq!(guide.programmes[1].title.as_deref(), Some("Deep Space Q&A"));
    }

    #[test]
    fn is_airing_respects_window() {
        let guide = parse(GUIDE).expect("parse");
        let p = &guide.programmes[0];
        assert!(p.is_airing(p.start_unix));
        assert!(p.is_airing(p.stop_unix - 1));
        assert!(!p.is_airing(p.stop_unix));
    }

    #[test]
    fn timezone_offsets_are_applied() {
        // 20:30 at +0300 is 17:30 UTC.
        let t = parse_xmltv_time("20260101203000 +0300").expect("time");
        let utc = parse_xmltv_time("20260101173000 +0000").expect("time");
        assert_eq!(t, utc);

        // 20:30 at -0500 is 01:30 UTC next day.
        let t = parse_xmltv_time("20260101203000 -0500").expect("time");
        let utc = parse_xmltv_time("20260102013000 +0000").expect("time");
        assert_eq!(t, utc);
    }

    #[test]
    fn known_epoch_matches() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(parse_xmltv_time("19700101000000 +0000"), Some(0));
    }

    #[test]
    fn missing_root_is_rejected() {
        assert!(parse("<html></html>").is_err());
    }
}
