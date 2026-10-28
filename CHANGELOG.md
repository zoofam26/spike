# Changelog

All notable changes to Spike are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[SemVer](https://semver.org/).

## [0.1.0] — 2026-10-29

First public skeleton. Everything below is implemented and tested (46 tests).

### Added
- `spike-core`: domain models; extended M3U/IPTV playlist parser (attribute state
  machine, `EXTGRP`, header-less tolerance); XMLTV guide parser with timezone offset
  handling; RFC 3986 subset URL joiner.
- `spike-stream`: HLS master/media playlist parser (tag-tolerant, encrypted streams
  rejected); sliding-window `SegmentBuffer` with high/low watermarks, playhead tracking
  and health classification; EWMA `ThroughputMonitor`; pollable `Scheduler` (manifest
  refresh cadence, segment prefetch, ABR-lite variant selection, live-edge skip
  recovery); loopback `LocalStreamServer` re-serving the buffer as standard HLS.
- `spike-engine`: `MediaBackend` trait; `NullBackend`; `ExternalProcessBackend`
  (SPIKE_PLAYER env override, mpv → ffplay → vlc discovery, lifecycle events).
- `spike-app`: Slint UI (charcoal/volt broadcast design, event list, category rail,
  live buffer chip, now-playing bar); CLI (`--playlist/--url/--headless/--play/
  --duration/--backend`); HTTPS fetching via ureq/rustls; real buffer telemetry into
  the UI; `gui` feature gate so headless cross builds (musl ARM64, Android NDK)
  compile without the windowing stack.
- Brand assets: original volt S-spike icon (SVG source + 1024 px PNG).
- Documentation: README, ARCHITECTURE deep-dive, LEGAL acceptable-use policy,
  CONTRIBUTING.
- CI: fmt + clippy `-D warnings` + full test matrix on every push/PR, on Linux and
  Windows runners, plus a headless build gate.
- Release pipeline: pushing a `v*` tag builds Linux x64 (GUI), Linux ARM64 (static
  headless), Windows x64 (GUI) and Android ARM64 (headless) artifacts and publishes
  them to GitHub Releases with SHA256 checksums.

### Verified
- `cargo test --workspace`: 46/46 green.
- Live end-to-end: headless pipeline buffered an official free channel at a steady
  12.0 s watermark while the live window slid.

[0.1.0]: https://github.com/zoofam26/spike/releases/tag/v0.1.0
