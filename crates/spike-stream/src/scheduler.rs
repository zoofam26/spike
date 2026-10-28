//! The streaming scheduler: manifest refresh + segment prefetch loop.
//!
//! The scheduler is a *pollable* state machine (`poll`) so it can be driven
//! deterministically by tests with a manual clock, and wrapped in a thread
//! loop (`spawn`) for production use. One `poll` performs at most one
//! network action, keeping the logic auditable.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::buffer::SegmentBuffer;
use crate::error::StreamError;
use crate::hls::{self, MediaPlaylist, ParsedPlaylist, Variant};
use crate::throughput::ThroughputMonitor;

/// Abstract fetcher so tests can inject canned responses and production can
/// plug a real HTTP client (the app crate wires `ureq`).
pub trait SegmentFetcher: Send + Sync {
    fn fetch(&self, url: &str) -> Result<Vec<u8>, StreamError>;
}

/// Abstract monotonic clock for deterministic testing.
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}

/// Production clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct StdClock;

impl Clock for StdClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Tunables for the scheduler.
#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    /// URL of the master or media playlist.
    pub playlist_url: String,
    /// Keep this many seconds downloaded ahead of the playhead.
    pub high_watermark_secs: f64,
    /// Below this the UI should warn about possible stutter.
    pub low_watermark_secs: f64,
    /// Consecutive segment fetch failures before the segment is skipped.
    pub max_consecutive_segment_errors: u32,
    /// Variant selection headroom: pick a variant using at most
    /// `estimate * factor` bits/s to leave room for variance.
    pub variant_bandwidth_factor: f64,
    /// Clamps for the manifest re-check interval.
    pub min_refresh_interval_secs: f64,
    pub max_refresh_interval_secs: f64,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            playlist_url: String::new(),
            high_watermark_secs: 12.0,
            low_watermark_secs: 4.0,
            max_consecutive_segment_errors: 3,
            variant_bandwidth_factor: 0.9,
            min_refresh_interval_secs: 2.0,
            max_refresh_interval_secs: 15.0,
        }
    }
}

/// What one `poll` did.
#[derive(Debug, Clone, PartialEq)]
pub enum PollOutcome {
    PlaylistRefreshed {
        new_segments: usize,
    },
    Downloaded {
        seq: u64,
        bytes: usize,
    },
    /// A segment was dropped after repeated fetch failures (live edge moved).
    SkippedSegment {
        seq: u64,
    },
    /// Nothing to do; suggests how long to wait before the next poll.
    Idle {
        retry_after_secs: f64,
    },
    /// Stream fully downloaded and ended.
    Finished,
}

/// Counters for observability.
#[derive(Debug, Clone, Copy, Default)]
pub struct SchedulerStats {
    pub segments_downloaded: u64,
    pub bytes_downloaded: u64,
    pub playlist_refreshes: u64,
    pub segments_skipped: u64,
    pub variant_bandwidth: Option<u64>,
}

/// Pollable streaming scheduler.
pub struct Scheduler<F: SegmentFetcher + 'static, C: Clock + 'static> {
    fetcher: Arc<F>,
    clock: Arc<C>,
    cfg: SchedulerConfig,
    buffer: Arc<Mutex<SegmentBuffer>>,
    current_playlist: Option<String>,
    refresh_at: Instant,
    next_seq: u64,
    throughput: ThroughputMonitor,
    consecutive_errors: u32,
    pub stats: SchedulerStats,
}

impl<F: SegmentFetcher + 'static, C: Clock + 'static> Scheduler<F, C> {
    pub fn new(
        cfg: SchedulerConfig,
        fetcher: F,
        clock: C,
        buffer: Arc<Mutex<SegmentBuffer>>,
    ) -> Self {
        let clock = Arc::new(clock);
        let refresh_at = clock.now();
        Self {
            fetcher: Arc::new(fetcher),
            clock,
            cfg,
            buffer,
            current_playlist: None,
            refresh_at,
            next_seq: 0,
            throughput: ThroughputMonitor::default(),
            consecutive_errors: 0,
            stats: SchedulerStats::default(),
        }
    }

    /// Shared handle to the buffer (also given to the local server).
    pub fn buffer(&self) -> Arc<Mutex<SegmentBuffer>> {
        self.buffer.clone()
    }

    /// Perform at most one action: refresh a manifest or download one
    /// segment. Errors are returned; callers decide on retry policy.
    pub fn poll(&mut self) -> Result<PollOutcome, StreamError> {
        if self
            .buffer()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_done()
        {
            return Ok(PollOutcome::Finished);
        }

        if self.current_playlist.is_none() {
            return self.load_initial_playlist();
        }
        if self.clock.now() >= self.refresh_at {
            return self.refresh_current_playlist();
        }
        if self
            .buffer()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .needs_prefetch()
        {
            return self.fetch_one_segment();
        }

        let buffer_arc = self.buffer();
        let buf = buffer_arc.lock().unwrap_or_else(|p| p.into_inner());
        if !buf.is_finished() && buf.pending_secs_ahead() < self.cfg.high_watermark_secs {
            // Window is short but everything buffered is already downloaded:
            // ask for a manifest refresh on the next poll.
            drop(buf);
            self.refresh_at = self.clock.now();
            return Ok(PollOutcome::Idle {
                retry_after_secs: 0.0,
            });
        }
        let remaining = self
            .refresh_at
            .saturating_duration_since(self.clock.now())
            .as_secs_f64();
        Ok(PollOutcome::Idle {
            retry_after_secs: remaining,
        })
    }

    /// Drive the scheduler on a dedicated thread until `shutdown` is set or
    /// the stream ends. Returns the final stats through the join handle.
    pub fn spawn(self, shutdown: Arc<AtomicBool>) -> std::thread::JoinHandle<SchedulerStats> {
        std::thread::Builder::new()
            .name("spike-scheduler".into())
            .spawn(move || {
                let mut sched = self;
                let mut errors = 0u32;
                loop {
                    if shutdown.load(Ordering::Relaxed) {
                        break;
                    }
                    match sched.poll() {
                        Ok(PollOutcome::Finished) => break,
                        Ok(PollOutcome::Idle { retry_after_secs }) => {
                            let delay = retry_after_secs.clamp(0.05, 1.0);
                            std::thread::sleep(Duration::from_secs_f64(delay));
                        }
                        Ok(_) => {
                            errors = 0;
                        }
                        Err(_) => {
                            errors += 1;
                            if errors >= 10 {
                                break;
                            }
                            let backoff = Duration::from_secs_f64(0.5 * f64::from(errors))
                                .min(Duration::from_secs(5));
                            std::thread::sleep(backoff);
                        }
                    }
                }
                sched.stats
            })
            .expect("failed to spawn scheduler thread")
    }

    fn load_initial_playlist(&mut self) -> Result<PollOutcome, StreamError> {
        let url = self.cfg.playlist_url.clone();
        let bytes = self.fetcher.fetch(&url)?;
        match hls::parse(&String::from_utf8_lossy(&bytes), &url)? {
            ParsedPlaylist::Master { variants } => {
                if variants.is_empty() {
                    return Err(StreamError::Malformed(
                        "master playlist lists no variants".to_string(),
                    ));
                }
                let chosen = choose_variant(
                    &variants,
                    self.throughput.estimate_bps(),
                    self.cfg.variant_bandwidth_factor,
                );
                self.stats.variant_bandwidth = Some(chosen.bandwidth);
                self.current_playlist = Some(chosen.uri.clone());
                self.refresh_at = self.clock.now();
                Ok(PollOutcome::Idle {
                    retry_after_secs: 0.0,
                })
            }
            ParsedPlaylist::Media(mp) => {
                self.current_playlist = Some(url);
                let new = self.integrate(&mp)?;
                self.refresh_at =
                    self.clock.now() + Duration::from_secs_f64(self.refresh_interval(&mp));
                Ok(PollOutcome::PlaylistRefreshed { new_segments: new })
            }
        }
    }

    fn refresh_current_playlist(&mut self) -> Result<PollOutcome, StreamError> {
        let url = self
            .current_playlist
            .clone()
            .expect("refresh without a current playlist");
        let bytes = self.fetcher.fetch(&url)?;
        match hls::parse(&String::from_utf8_lossy(&bytes), &url)? {
            ParsedPlaylist::Media(mp) => {
                let interval = self.refresh_interval(&mp);
                let new = self.integrate(&mp)?;
                self.refresh_at = self.clock.now() + Duration::from_secs_f64(interval);
                Ok(PollOutcome::PlaylistRefreshed { new_segments: new })
            }
            ParsedPlaylist::Master { .. } => Err(StreamError::Malformed(
                "expected a media playlist but got a master playlist".to_string(),
            )),
        }
    }

    fn refresh_interval(&self, mp: &MediaPlaylist) -> f64 {
        let half = mp.target_duration / 2.0;
        half.clamp(
            self.cfg.min_refresh_interval_secs,
            self.cfg.max_refresh_interval_secs,
        )
    }

    fn integrate(&mut self, mp: &MediaPlaylist) -> Result<usize, StreamError> {
        let buffer_arc = self.buffer();
        let mut buf = buffer_arc.lock().unwrap_or_else(|p| p.into_inner());
        let mut new = 0usize;
        for seg in &mp.segments {
            if seg.seq >= self.next_seq && buf.enqueue(seg.clone()) {
                new += 1;
            }
        }
        self.next_seq = buf.last_seq().map(|s| s + 1).unwrap_or(self.next_seq);
        if mp.finished {
            buf.set_finished();
        }
        self.stats.playlist_refreshes += 1;
        Ok(new)
    }

    fn fetch_one_segment(&mut self) -> Result<PollOutcome, StreamError> {
        let target = self
            .buffer()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .next_unloaded()
            .map(|s| (s.seq, s.uri.clone()));
        let Some((seq, uri)) = target else {
            return Ok(PollOutcome::Idle {
                retry_after_secs: 0.0,
            });
        };

        let t0 = self.clock.now();
        match self.fetcher.fetch(&uri) {
            Ok(bytes) => {
                let elapsed = self.clock.now().duration_since(t0).as_secs_f64().max(1e-6);
                self.throughput.sample(bytes.len() as u64, elapsed);
                self.buffer()
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .store_data(seq, Arc::new(bytes.clone()));
                self.stats.segments_downloaded += 1;
                self.stats.bytes_downloaded += bytes.len() as u64;
                self.consecutive_errors = 0;
                Ok(PollOutcome::Downloaded {
                    seq,
                    bytes: bytes.len(),
                })
            }
            Err(err) => {
                self.consecutive_errors += 1;
                if self.consecutive_errors >= self.cfg.max_consecutive_segment_errors {
                    self.buffer()
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .remove(seq);
                    self.stats.segments_skipped += 1;
                    self.consecutive_errors = 0;
                    Ok(PollOutcome::SkippedSegment { seq })
                } else {
                    Err(err)
                }
            }
        }
    }
}

/// Pick the highest-bandwidth variant that fits `estimate * factor`;
/// fall back to the lowest variant when nothing fits.
pub fn choose_variant(variants: &[Variant], estimate_bps: Option<f64>, factor: f64) -> &Variant {
    debug_assert!(!variants.is_empty(), "caller guarantees non-empty variants");
    match estimate_bps {
        None => variants
            .iter()
            .max_by_key(|v| v.bandwidth)
            .unwrap_or(&variants[0]),
        Some(estimate) => {
            let cap = estimate * factor;
            variants
                .iter()
                .filter(|v| (v.bandwidth as f64) <= cap)
                .max_by_key(|v| v.bandwidth)
                .unwrap_or_else(|| variants.iter().min_by_key(|v| v.bandwidth).unwrap())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, VecDeque};

    /// Deterministic fetcher: per-URL queue of canned responses; the last
    /// response repeats once the queue drains.
    type CannedQueue = VecDeque<Result<Vec<u8>, StreamError>>;
    struct MockFetcher {
        responses: Mutex<HashMap<String, CannedQueue>>,
    }

    impl MockFetcher {
        fn new() -> Self {
            Self {
                responses: Mutex::new(HashMap::new()),
            }
        }

        fn queue(&mut self, url: &str, body: &str) {
            self.queue_bytes(url, Ok(body.as_bytes().to_vec()));
        }

        fn queue_bytes(&mut self, url: &str, result: Result<Vec<u8>, StreamError>) {
            self.responses
                .lock()
                .unwrap()
                .entry(url.to_string())
                .or_default()
                .push_back(result);
        }
    }

    impl SegmentFetcher for MockFetcher {
        fn fetch(&self, url: &str) -> Result<Vec<u8>, StreamError> {
            let mut map = self.responses.lock().unwrap();
            let queue = map.entry(url.to_string()).or_insert_with(|| {
                VecDeque::from(vec![Err(StreamError::NotFound(url.to_string()))])
            });
            if queue.len() > 1 {
                queue.pop_front().expect("len checked")
            } else {
                // The last queued response repeats forever.
                queue.front().expect("len checked").clone()
            }
        }
    }

    /// Manual clock so tests control time exactly.
    struct ManualClock(Mutex<Instant>);

    impl ManualClock {
        fn new() -> Self {
            Self(Mutex::new(Instant::now()))
        }
        fn advance(&self, d: Duration) {
            *self.0.lock().unwrap() += d;
        }
    }

    impl Clock for ManualClock {
        fn now(&self) -> Instant {
            *self.0.lock().unwrap()
        }
    }

    const MASTER: &str = "\
#EXTM3U
#EXT-X-STREAM-INF:BANDWIDTH=500000
low/index.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=2000000
high/index.m3u8
";

    fn media(seq_start: u64, from: u64, count: u64, finished: bool) -> String {
        let mut text =
            format!("#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:{seq_start}\n");
        for i in from..from + count {
            text.push_str(&format!("#EXTINF:4.0,\nseg_{i}.ts\n"));
        }
        if finished {
            text.push_str("#EXT-X-ENDLIST\n");
        }
        text
    }

    fn make_scheduler_at(url: &str, fetcher: MockFetcher) -> Scheduler<MockFetcher, ManualClock> {
        let cfg = SchedulerConfig {
            playlist_url: url.to_string(),
            ..Default::default()
        };
        Scheduler::new(
            cfg,
            fetcher,
            ManualClock::new(),
            Arc::new(Mutex::new(SegmentBuffer::new(12.0, 4.0))),
        )
    }

    fn make_scheduler(fetcher: MockFetcher) -> Scheduler<MockFetcher, ManualClock> {
        make_scheduler_at("https://cdn.example/event/master.m3u8", fetcher)
    }

    fn seg_url(base: &str, i: u64) -> String {
        format!("{base}seg_{i}.ts")
    }

    #[test]
    fn vod_stream_downloads_to_completion() {
        let mut fetcher = MockFetcher::new();
        let base = "https://cdn.example/vod/";
        fetcher.queue(
            "https://cdn.example/vod.m3u8",
            &media(0, 0, 3, true)
                .lines()
                .map(|l| {
                    if l.starts_with("seg_") {
                        format!("{base}{l}")
                    } else {
                        l.to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n"),
        );
        for i in 0..3 {
            fetcher.queue_bytes(&seg_url(base, i), Ok(vec![i as u8; 100]));
        }
        let mut sched = make_scheduler_at("https://cdn.example/vod.m3u8", fetcher);

        let mut finished = false;
        for _ in 0..64 {
            match sched.poll().expect("poll ok") {
                PollOutcome::Finished => {
                    finished = true;
                    break;
                }
                PollOutcome::Idle { retry_after_secs } => {
                    sched
                        .clock
                        .advance(Duration::from_secs_f64(retry_after_secs.max(0.5)));
                }
                _ => {}
            }
        }

        assert!(finished);
        assert_eq!(sched.stats.segments_downloaded, 3);
        assert_eq!(sched.stats.bytes_downloaded, 300);
        let buffer_arc = sched.buffer();
        let buf = buffer_arc.lock().unwrap();
        assert!(buf.is_done());
        assert_eq!(buf.buffered_secs_ahead(), 12.0);
    }

    #[test]
    fn master_playlist_selects_highest_variant_first() {
        let mut fetcher = MockFetcher::new();
        fetcher.queue("https://cdn.example/event/master.m3u8", MASTER);
        let mut sched = make_scheduler(fetcher);

        sched.poll().expect("first poll loads master");
        assert_eq!(sched.stats.variant_bandwidth, Some(2_000_000));
        assert_eq!(
            sched.current_playlist.as_deref(),
            Some("https://cdn.example/event/high/index.m3u8")
        );
    }

    #[test]
    fn live_window_slides_and_dedupes() {
        let mut fetcher = MockFetcher::new();
        let media_url = "https://cdn.example/event/high/index.m3u8";
        fetcher.queue("https://cdn.example/event/master.m3u8", MASTER);
        // First refresh: segments 0..3. Second refresh: window slid to 2..6.
        fetcher.queue(media_url, &media(0, 0, 3, false));
        fetcher.queue(media_url, &media(2, 2, 4, false));
        for i in 0..6 {
            fetcher.queue_bytes(
                &seg_url("https://cdn.example/event/high/", i),
                Ok(vec![i as u8; 50]),
            );
        }
        let mut sched = make_scheduler(fetcher);

        // Load master, then first media playlist.
        sched.poll().unwrap();
        let refreshed = sched.poll().unwrap();
        assert_eq!(
            refreshed,
            PollOutcome::PlaylistRefreshed { new_segments: 3 }
        );

        // Download 0..3, playing each segment right away (the local server
        // advances the playhead when a player requests it).
        let buffer_arc = sched.buffer();
        for i in 0..3u64 {
            assert_eq!(
                sched.poll().unwrap(),
                PollOutcome::Downloaded { seq: i, bytes: 50 }
            );
            buffer_arc.lock().unwrap().advance_playhead(i);
        }

        // Force a refresh: manifest slid to segments 2..6. Segments 3,4,5
        // are new (2 was known and deduped against next_seq=3).
        sched.clock.advance(Duration::from_secs(3));
        let refreshed = sched.poll().unwrap();
        assert_eq!(
            refreshed,
            PollOutcome::PlaylistRefreshed { new_segments: 3 }
        );

        // Download the newly announced 3,4,5 (2 was known and deduped).
        let mut seen = Vec::new();
        for _ in 0..16 {
            match sched.poll().unwrap() {
                PollOutcome::Downloaded { seq, .. } => {
                    seen.push(seq);
                    buffer_arc.lock().unwrap().advance_playhead(seq);
                }
                PollOutcome::Idle { retry_after_secs } => {
                    sched
                        .clock
                        .advance(Duration::from_secs_f64(retry_after_secs.max(0.5)));
                }
                other => panic!("unexpected outcome: {other:?}"),
            }
            if seen.len() == 3 {
                break;
            }
        }
        seen.sort_unstable();
        assert_eq!(seen, vec![3, 4, 5]);
        assert_eq!(sched.stats.segments_skipped, 0);
    }

    #[test]
    fn repeated_segment_failures_skip_after_threshold() {
        let mut fetcher = MockFetcher::new();
        let base = "https://cdn.example/vod/";
        fetcher.queue(
            "https://cdn.example/vod.m3u8",
            &media(0, 0, 3, false)
                .lines()
                .map(|l| {
                    if l.starts_with("seg_") {
                        format!("{base}{l}")
                    } else {
                        l.to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n"),
        );
        fetcher.queue_bytes(&seg_url(base, 0), Ok(vec![1; 10]));
        for _ in 0..3 {
            fetcher.queue_bytes(&seg_url(base, 1), Err(StreamError::NotFound("gone".into())));
        }
        fetcher.queue_bytes(&seg_url(base, 2), Ok(vec![2; 10]));
        let mut sched = make_scheduler_at("https://cdn.example/vod.m3u8", fetcher);
        sched.cfg.max_consecutive_segment_errors = 3;

        sched.poll().unwrap(); // load playlist
        assert!(matches!(
            sched.poll().unwrap(),
            PollOutcome::Downloaded { seq: 0, .. }
        ));

        let mut skipped = false;
        let mut downloaded_after_skip = None;
        for _ in 0..16 {
            match sched.poll() {
                Ok(PollOutcome::SkippedSegment { seq }) => {
                    skipped = true;
                    assert_eq!(seq, 1);
                }
                Ok(PollOutcome::Downloaded { seq, .. }) => {
                    if skipped {
                        downloaded_after_skip = Some(seq);
                        break;
                    }
                }
                Ok(PollOutcome::Idle { retry_after_secs }) => {
                    sched
                        .clock
                        .advance(Duration::from_secs_f64(retry_after_secs.max(0.5)));
                }
                Ok(other) => panic!("unexpected outcome: {other:?}"),
                Err(_) => { /* first two failures are surfaced as errors */ }
            }
        }
        assert!(skipped);
        assert_eq!(downloaded_after_skip, Some(2));
        assert_eq!(sched.stats.segments_skipped, 1);
    }

    #[test]
    fn choose_variant_respects_estimate() {
        let variants = vec![
            Variant {
                uri: "low".into(),
                bandwidth: 500_000,
                codecs: None,
                resolution: None,
            },
            Variant {
                uri: "mid".into(),
                bandwidth: 2_000_000,
                codecs: None,
                resolution: None,
            },
            Variant {
                uri: "high".into(),
                bandwidth: 8_000_000,
                codecs: None,
                resolution: None,
            },
        ];
        // No estimate -> highest.
        assert_eq!(choose_variant(&variants, None, 0.9).uri, "high");
        // 3 mbps estimate * 0.9 = 2.7 mbps cap -> mid.
        assert_eq!(choose_variant(&variants, Some(3_000_000.0), 0.9).uri, "mid");
        // Tiny estimate -> lowest.
        assert_eq!(choose_variant(&variants, Some(100_000.0), 0.9).uri, "low");
    }
}
