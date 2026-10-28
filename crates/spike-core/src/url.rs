//! RFC 3986 subset URL joining used by playlist and manifest resolution.

/// Resolve a possibly-relative URL against a base URL.
///
/// Implements the small subset of RFC 3986 that HLS manifests actually use:
/// absolute URLs, scheme-relative `//host/...`, root-relative `/path` and
/// plain / `../` relative paths. The base query string is intentionally not
/// inherited (signed CDNs embed per-segment tokens in the manifest itself).
pub fn join_url(base: &str, relative: &str) -> String {
    if relative.starts_with("http://") || relative.starts_with("https://") {
        return relative.to_string();
    }
    if let Some(rest) = relative.strip_prefix("//") {
        if let Some(idx) = base.find("://") {
            return format!("{}://{}", &base[..idx], rest);
        }
    }

    let (scheme_host, base_path) = match base.find("://") {
        Some(scheme_end) => {
            let after = &base[scheme_end + 3..];
            match after.find('/') {
                Some(slash) => (&base[..scheme_end + 3 + slash], &after[slash..]),
                None => (base, "/"),
            }
        }
        None => ("", base),
    };

    let base_path = base_path.split(['?', '#']).next().unwrap_or("/");

    if let Some(path) = relative.strip_prefix('/') {
        return format!("{scheme_host}/{path}");
    }

    // Directory part of the base path.
    let dir = match base_path.rfind('/') {
        Some(idx) => &base_path[..=idx],
        None => "/",
    };

    // Apply `../` and `./` normalisation.
    let mut segments: Vec<&str> = dir
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    let mut rel_parts: Vec<&str> = relative.split('/').filter(|s| !s.is_empty()).collect();
    while let Some(first) = rel_parts.first() {
        match *first {
            "." => {
                rel_parts.remove(0);
            }
            ".." => {
                rel_parts.remove(0);
                segments.pop();
            }
            _ => break,
        }
    }
    let mut joined = String::from("/");
    joined.push_str(&segments.join("/"));
    if !joined.ends_with('/') && !rel_parts.is_empty() {
        joined.push('/');
    }
    joined.push_str(&rel_parts.join("/"));
    format!("{scheme_host}{joined}")
}

#[cfg(test)]
mod tests {
    use super::join_url;

    #[test]
    fn covers_hls_cases() {
        let base = "https://cdn.example.com/live/stream/master.m3u8?token=abc";
        assert_eq!(
            join_url(base, "seg_1.ts"),
            "https://cdn.example.com/live/stream/seg_1.ts"
        );
        assert_eq!(
            join_url(base, "/abs/seg.m3u8"),
            "https://cdn.example.com/abs/seg.m3u8"
        );
        assert_eq!(
            join_url(base, "../other/seg.ts"),
            "https://cdn.example.com/live/other/seg.ts"
        );
        assert_eq!(
            join_url(base, "https://other.example/x.ts"),
            "https://other.example/x.ts"
        );
        assert_eq!(
            join_url(base, "//cdn2.example/s/seg.ts"),
            "https://cdn2.example/s/seg.ts"
        );
    }

    #[test]
    fn handles_base_without_path() {
        assert_eq!(
            join_url("https://cdn.example.com", "live/index.m3u8"),
            "https://cdn.example.com/live/index.m3u8"
        );
    }

    #[test]
    fn handles_dot_normalisation() {
        let base = "https://cdn.example.com/a/b/c.m3u8";
        assert_eq!(join_url(base, "./d.ts"), "https://cdn.example.com/a/b/d.ts");
        assert_eq!(join_url(base, "../../z.ts"), "https://cdn.example.com/z.ts");
    }
}
