# Spike Architecture

This document explains how Spike is built, why the streaming engine looks the way it
does, and where the extension points are. It targets contributors; for usage see the
[README](README.md).

## 1. Crate topology

```
            ┌─────────────────────────────────────────────┐
            │                spike-app (bin)              │
            │  Slint UI · CLI · HttpFetcher (ureq/rustls) │
            └──────┬───────────────────┬──────────────────┘
                   │                   │
        ┌──────────▼─────────┐ ┌───────▼──────────────────┐
        │    spike-engine    │ │       spike-stream       │
        │ MediaBackend trait │ │ HLS parse · SegmentBuffer│
        │ null · external    │ │ Scheduler · LocalServer  │
        └────────────────────┘ └───────────┬──────────────┘
                                           │
                               ┌───────────▼──────────────┐
                               │        spike-core        │
                               │ models · M3U · XMLTV · URL│
                               └──────────────────────────┘
```

Dependency rules:

- `spike-core` and `spike-stream` depend on **nothing outside `std`** (plus each other).
  This keeps them portable to Android and any other Rust target, and keeps the test tree
  fast and deterministic.
- `spike-app` is the only crate allowed to pull I/O-grade dependencies (`ureq` for
  HTTPS via rustls, `slint` for the UI). The `SegmentFetcher` trait is the seam.
- `spike-engine` is deliberately thin: it defines *how* playback is handed off, not how
  media is decoded.

## 2. The streaming pipeline

```
 origin CDN            spike (your machine)                player
┌────────────┐   ┌────────────────────────────────┐   ┌─────────────┐
│  master /  │   │  Scheduler thread              │   │             │
│  media     │◄──│  · refresh manifest (½ target) │   │  mpv/ffplay │
│  playlists │   │  · prefetch segments ──┐       │   │  or any HLS │
├────────────┤   │                        ▼       │   │  client     │
│  media     │◄──│  · download ──► SegmentBuffer ─┼──►│             │
│  segments  │   │   (EWMA throughput)  (12 s ring)│  └──────▲──────┘
└────────────┘   └────────────────────────────────┘          │
                     LocalStreamServer ───────────────────────┘
                     http://127.0.0.1:<port>/live.m3u8
```

The player never talks to the internet. It reads a locally rendered sliding-window
playlist from loopback and pulls already-buffered bytes from memory. This is what makes
Spike resilient: the buffer absorbs upstream jitter, and the served playlist always
describes what is *actually* in memory.

### 2.1 Scheduler

`StreamScheduler` is a **pollable state machine** — one `poll()` call performs at most one
action (refresh a manifest or download one segment) and returns a `PollOutcome`. This
design was chosen over a `tokio`-style async loop because it is:

- **deterministically testable** — tests drive `poll()` with a `ManualClock` and canned
  `MockFetcher` responses, no sleeping, no flakiness;
- **runtime-free** — the production `spawn()` wraps `poll()` in a plain thread with a
  shutdown flag; no async runtime dependency in the core.

State transitions per `poll()`:

1. `is_done()` (manifest ended + all segments downloaded) → `Finished`.
2. No current playlist yet → fetch the configured URL; if it is a master, pick a variant
   (see ABR below); if media, integrate it.
3. Refresh due (`now ≥ refresh_at`, armed at `target_duration / 2`, clamped to
   `[2 s, 15 s]`) → re-fetch the media playlist and integrate *new* segments
   (`seq ≥ next_seq`), which deduplicates the sliding window.
4. `needs_prefetch()` (an unloaded segment exists AND buffered < high watermark) →
   download exactly one segment, sample throughput, store bytes in the buffer.
5. Otherwise: if the window is short but everything is already loaded, request an
   immediate manifest re-check; else idle until the next refresh.

Failure policy: segment fetch errors are surfaced as `Err` for two retries, then the
segment is **dropped from the buffer** (`SkippedSegment`) so the pipeline keeps tracking
the live edge instead of wedging on a missing CDN object. The scheduler thread itself
gives up after ten consecutive poll errors and reports final stats.

### 2.2 SegmentBuffer

`SegmentBuffer` is an ordered, deduplicated ring of `BufferedSegment`s with:

- **watermarks** — `high_watermark` (prefetch target, 12 s) and `low_watermark`
  (UI warning, 4 s);
- **playhead tracking** — the local server advances it as the player requests segments;
  `keep_behind` (2 segments) is retained for short seeks and packet-loss recovery;
- **health classification** — `Healthy / Filling / Low / Stalled / Empty`, plus a
  normalized `fill_level()` for the UI chip.

Key subtlety: `finished` (manifest has `#EXT-X-ENDLIST`) does **not** stop prefetching —
VOD content still needs its bytes. Termination is `is_done() = finished && nothing
unloaded`. This distinction was found by tests and is documented in the code.

### 2.3 ABR-lite

Variant selection is deliberately simple: with no throughput history pick the highest
bandwidth; afterwards pick the highest variant with `bandwidth ≤ estimate × 0.9`
(headroom absorbs variance), falling back to the lowest variant when nothing fits.
EWMA α = 0.2 smooths per-segment noise. Full timestamp-aligned switching (matching
`EXT-X-DISCONTINUITY` rules) is future work — v0.1 selects one variant per session.

### 2.4 LocalStreamServer

A `std`-only HTTP/1.1 server bound to a random loopback port:

- `GET /live.m3u8` → renders the current window (`render_playlist` is public and unit
  tested): `TARGETDURATION` = max segment duration, `MEDIA-SEQUENCE` = first buffered
  seq, `#EXTINF` per segment pointing at `/seg/<seq>`, `ENDLIST` when done;
- `GET /seg/<seq>` → payload bytes; serving a segment also advances the playhead;
- anything else → 404/405. Connections are closed after one response (player clients
  reconnect per manifest refresh anyway).

## 3. UI

Slint compiles `ui/main.slint` at build time (`slint-build`). The design system is
deliberately *not* a generic template: charcoal surfaces (`#0d1017`/`#10141c`), a single
volt accent (`#b6f542`), dense 60 px event rows, a live buffer-health chip, and a
now-playing bar. Responsive behaviour comes from layout constraints (`min-width`,
`horizontal-stretch`, `ListView` scrolling) rather than fixed pixel canvases; the window
min size is 520×420, comfortable for phone-scale rendering.

State flow: the UI thread owns `Rc<RefCell<Option<PipelineHandles>>>`; selecting an event
starts the pipeline (scheduler thread + server) and spawns a 500 ms telemetry loop that
pushes real buffer stats into the UI via `slint::invoke_from_event_loop` (UI updates never
happen off-thread). Stop reverses everything: shutdown flag → join scheduler → stop
server (drop-safe via `Drop` impls).

## 4. Error strategy

- No panics on network/parse paths: crates return typed errors (`CoreError`,
  `StreamError`, `EngineError`) with `Display` + `std::error::Error`.
- Mutex poisoning is recovered with `unwrap_or_else(|p| p.into_inner())` rather than
  unwrapping — a panicking telemetry thread must not take playback down.
- The UI degrades loudly: failures set the status line, the buffer chip reflects health.

## 5. Testing strategy

| Layer | What is covered |
|-------|-----------------|
| `spike-core` | M3U attrs/quotes/`EXTGRP`, header-less tolerance, URL joining (HLS cases incl. `../`, scheme-relative), XMLTV channels/programmes/offsets/epochs |
| `spike-stream` | buffer dedupe/watermarks/health/trimming; HLS master+media parsing, encrypted-stream rejection; scheduler: VOD completion, master→variant selection, live sliding+dedupe, segment skip-after-3; loopback server: playlist rendering, segment serving, playhead advance, 404s |
| `spike-engine` | null-backend lifecycle ordering; PATH discovery; external-player spawn/exit events (unix, `/bin/sh` stand-in) |
| `spike-app` | arg parsing, row projection, bundled sample playlist validity |

Live validation (not part of CI): `spike --headless --play 0` against an official free
stream — the buffer was observed holding a steady 12.0 s ("healthy") while the live
window slid.

## 6. Roadmap

1. **In-process video surface** — implement a `MediaBackend` over GStreamer
   (`gstreamer-play` or paintable-style integration with Slint's renderer). The trait
   already isolates callers from this change; Android requires it (no process spawning).
2. **DASH support** — new `dash` module mirroring `hls`, same buffer/scheduler contract.
3. **EPG strip** — `spike-core::xmltv` is ready; the UI needs a per-channel join on
   `tvg-id`.
4. **Android packaging** — `android/` skeleton with `cargo-apk`/`xbuild` config and an
   emulator CI job.
5. **Seek + watermark controls** — expose `keep_behind`/watermarks in the UI; wire
   `advance_playhead` for real seeking.
