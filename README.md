<div align="center">

<img src="assets/icon/spike-icon-1024.png" alt="Spike" width="140"/>

# Spike

**Live event sports streaming, powered by a Rust engine.**

Buffer-first HLS playback · Slint UI · Linux / Windows / Android

</div>

---

## What is Spike?

Spike is a live-event streaming app built around one idea: **never stall on the network**.
A purpose-written Rust engine keeps a rolling window of media prefetched ahead of the
playhead and re-serves it to the player over loopback, so playback stays smooth even when
the upstream CDN hiccups. The UI is a dense, dark, sports-broadcast-style dashboard built
with Slint — no webviews, no Electron, no template look.

> **⚖️ Read this first:** Spike is a *player and buffering engine*. It ships with no
> content and obtains none for you. Point it only at streams you are licensed to access —
> your own IPTV provider, rights-holder feeds, or official free channels. See
> [LEGAL.md](LEGAL.md).

## Highlights

- **Sliding-window buffer engine** — configurable high/low watermarks (default: keep 12 s
  of media ready), EWMA throughput tracking, live-edge resilience (skipped segments
  recover instead of stalling), and health telemetry surfaced live in the UI.
- **Local re-serve server** — the engine exposes the buffer as a standard HLS playlist on
  `127.0.0.1`, so *any* player backend can consume it: mpv, ffplay, VLC today; in-process
  backends on the roadmap.
- **Playlist + guide aware** — extended M3U/IPTV parsing (`tvg-*`, `group-title`), XMLTV
  EPG parsing, category rail, and media-kind sniffing (HLS / DASH / direct).
- **Pure Rust core** — `spike-core` and `spike-stream` are `std`-only (zero external
  dependencies), fully unit-tested, and portable to every Rust target including Android.
- **Distinctive UI** — charcoal + volt design language, dense event rows, live buffer
  chip; responsive down to phone sizes.

## Repository layout

```
spike/
├── crates/
│   ├── spike-core/     domain models, M3U + XMLTV parsers, URL joining
│   ├── spike-stream/   HLS parsing, segment buffer, scheduler, local server
│   ├── spike-engine/   MediaBackend trait, null + external-player backends
│   └── spike-app/      Slint UI, CLI, wiring (the `spike` binary)
├── assets/
│   ├── icon/           brand SVG + app icon PNG
│   └── playlists/      sample playlist with official free channels
├── .github/workflows/  CI (fmt, clippy, tests)
├── ARCHITECTURE.md     engine deep-dive
├── LEGAL.md            acceptable use
└── CHANGELOG.md
```

## Quick start

Prerequisites: [Rust 1.75+](https://rustup.rs) (stable), plus a system player
(`mpv`, `ffplay`, or `vlc`) for actual video windows.

```bash
git clone https://github.com/zoofam26/spike.git
cd spike
cargo run                                    # UI mode with the bundled sample playlist
cargo run -- --headless                      # list events in the terminal
cargo run -- --headless --play 0 --duration 12   # buffer event 0 for 12 s, watch the buffer fill
cargo run -- --url "https://example.com/live/master.m3u8"   # single stream
```

The headless demo prints the live buffer health while the engine prefetches:

```
Buffering `Red Bull TV` for 15s…
Local playlist URL for players: http://127.0.0.1:38289/live.m3u8
  buffer:   12.0s | healthy | segments: 30
  buffer:   12.0s | healthy | segments: 31
  ...
```

Point any player at the printed URL to watch the buffered stream.

## Platform support

| Platform | Status | Notes |
|----------|--------|-------|
| Linux (X11/Wayland) | ✅ primary target | tested in CI; needs OpenGL or software rendering |
| Windows 10/11 | ✅ supported | same build; external backend finds mpv/ffplay or uses `start` |
| Android | 🔧 experimental | Slint supports Android via `android-activity`; see below |
| macOS | ⚠️ untested | expected to work (winit + Slint); not validated yet |

### Building for Android (experimental)

The engine crates are `std`-only and compile for `aarch64-linux-android` out of the box.
The UI crate targets Android through Slint's `android-activity` integration:

```bash
rustup target add aarch64-linux-android
cargo install cargo-apk   # or use xbuild
# An android/ packaging skeleton is on the roadmap (see ARCHITECTURE.md §Roadmap).
```

Note: the external-player backend does not apply on Android (no arbitrary process
spawning); an in-process GStreamer backend is the planned path there.

## Configuration

| Env / flag | Meaning |
|------------|---------|
| `SPIKE_PLAYER` | Full path to your preferred player binary (overrides mpv→ffplay→vlc discovery) |
| `--backend null` | No-op backend for tests/CI |
| `--playlist FILE` | Load an M3U playlist |
| `--url M3U8` | Load a single HLS stream |

Buffer watermarks are currently engine defaults (12 s high / 4 s low); exposing them in
the UI is on the roadmap.

## Testing

```bash
cargo test --workspace        # 46 unit + integration tests
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

The streaming engine is tested against deterministic mock fetchers and a manual clock —
VOD completion, live-window sliding, duplicate suppression, segment-failure skipping,
variant selection, and the loopback server's HTTP surface are all covered.

## Roadmap

- In-process video surface (GStreamer paintable embedded in the Slint scene)
- DASH (`.mpd`) playback
- EPG-driven "now playing" strip in the UI (parser is ready)
- Android packaging skeleton + CI build
- Buffer watermark controls in the UI

## Contributing

Standard Rust workflow: fork → branch → `cargo fmt` + `cargo clippy -D warnings` +
`cargo test` → PR. CI enforces all three.

## License

MIT — see [LICENSE](LICENSE).
