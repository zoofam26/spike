//! Sliding-window segment buffer.
//!
//! The buffer is the reason Spike streams smoothly: the scheduler keeps
//! `high_watermark` seconds of media prefetched ahead of the playhead, and
//! the player drains from it through the local re-serve server. If the
//! network hiccups, playback continues out of the buffer instead of
//! stuttering.

use std::collections::VecDeque;
use std::sync::Arc;

use crate::hls::Segment;

/// A segment held by the buffer, with its payload once downloaded.
#[derive(Debug, Clone)]
pub struct BufferedSegment {
    pub seq: u64,
    pub uri: String,
    pub duration: f64,
    pub data: Option<Arc<Vec<u8>>>,
}

/// Coarse buffer-health classification used by the UI and the scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferState {
    /// Nothing buffered at all and nothing pending.
    Empty,
    /// Buffer below the low watermark — playback is at risk.
    Low,
    /// Buffer filling up towards the high watermark.
    Filling,
    /// Buffer at/above the high watermark — ideal.
    Healthy,
    /// Playback wants data but the buffer has none.
    Stalled,
}

/// Ordered, deduplicated window of segments with playhead tracking.
#[derive(Debug)]
pub struct SegmentBuffer {
    high_watermark: f64,
    low_watermark: f64,
    keep_behind: u64,
    segments: VecDeque<BufferedSegment>,
    playhead: u64,
    finished: bool,
}

impl SegmentBuffer {
    /// Create a buffer with the given watermarks (seconds of media).
    pub fn new(high_watermark_secs: f64, low_watermark_secs: f64) -> Self {
        Self {
            high_watermark: high_watermark_secs.max(0.0),
            low_watermark: low_watermark_secs.max(0.0),
            keep_behind: 2,
            segments: VecDeque::new(),
            playhead: 0,
            finished: false,
        }
    }

    /// Override how many already-played segments are retained for
    /// short seeks / packet loss recovery. Defaults to 2.
    pub fn set_keep_behind(&mut self, keep: u64) {
        self.keep_behind = keep.max(1);
    }

    /// Mark the source playlist as ended (`#EXT-X-ENDLIST` seen).
    pub fn set_finished(&mut self) {
        self.finished = true;
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Fully downloaded *and* ended — nothing left to do for this stream.
    pub fn is_done(&self) -> bool {
        self.finished && self.next_unloaded().is_none()
    }

    /// Append a segment. Returns false (and does nothing) when the segment
    /// is a duplicate or belongs to the already-played past.
    pub fn enqueue(&mut self, seg: Segment) -> bool {
        if seg.seq < self.playhead {
            return false;
        }
        if let Some(last) = self.segments.back() {
            if seg.seq <= last.seq {
                return false;
            }
        }
        self.segments.push_back(BufferedSegment {
            seq: seg.seq,
            uri: seg.uri,
            duration: seg.duration,
            data: None,
        });
        true
    }

    /// Attach downloaded bytes to a previously enqueued segment.
    pub fn store_data(&mut self, seq: u64, data: Arc<Vec<u8>>) -> bool {
        for seg in &mut self.segments {
            if seg.seq == seq {
                seg.data = Some(data);
                return true;
            }
        }
        false
    }

    /// Drop a segment entirely (live-edge resilience after repeated
    /// fetch failures).
    pub fn remove(&mut self, seq: u64) -> bool {
        let before = self.segments.len();
        self.segments.retain(|s| s.seq != seq);
        self.segments.len() != before
    }

    /// First buffered segment that still needs downloading, in order.
    pub fn next_unloaded(&self) -> Option<&BufferedSegment> {
        self.segments
            .iter()
            .find(|s| s.seq >= self.playhead && s.data.is_none())
    }

    /// Advance the playhead (called when a segment is served to the player).
    pub fn advance_playhead(&mut self, seq: u64) {
        if seq > self.playhead {
            self.playhead = seq;
        }
        let cutoff = self.playhead.saturating_sub(self.keep_behind);
        self.segments.retain(|s| s.seq >= cutoff);
    }

    pub fn playhead(&self) -> u64 {
        self.playhead
    }

    pub fn first_seq(&self) -> Option<u64> {
        self.segments.front().map(|s| s.seq)
    }

    pub fn last_seq(&self) -> Option<u64> {
        self.segments.back().map(|s| s.seq)
    }

    pub fn len(&self) -> usize {
        self.segments.len()
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// Payload for a served segment, if it is loaded.
    pub fn data_for(&self, seq: u64) -> Option<Arc<Vec<u8>>> {
        self.segments
            .iter()
            .find(|s| s.seq == seq)
            .and_then(|s| s.data.clone())
    }

    /// All loaded segments at/after the playhead — what the local server
    /// exposes as the (sliding) playlist window.
    pub fn window(&self) -> impl Iterator<Item = &BufferedSegment> {
        let playhead = self.playhead;
        self.segments
            .iter()
            .filter(move |s| s.seq >= playhead && s.data.is_some())
    }

    /// Seconds of *downloaded* media at/after the playhead.
    pub fn buffered_secs_ahead(&self) -> f64 {
        self.window().map(|s| s.duration).sum()
    }

    /// Seconds of media at/after the playhead, downloaded or not.
    pub fn pending_secs_ahead(&self) -> f64 {
        let playhead = self.playhead;
        self.segments
            .iter()
            .filter(move |s| s.seq >= playhead)
            .map(|s| s.duration)
            .sum()
    }

    /// The scheduler should keep downloading while this is true.
    ///
    /// Note: finished (VOD/ended) streams still prefetch — "finished" only
    /// means the manifest will not grow. Termination is governed by
    /// [`SegmentBuffer::is_done`].
    pub fn needs_prefetch(&self) -> bool {
        self.next_unloaded().is_some() && self.buffered_secs_ahead() < self.high_watermark
    }

    /// Coarse health classification for UI display.
    pub fn health(&self) -> BufferState {
        let buffered = self.buffered_secs_ahead();
        if buffered <= f64::EPSILON {
            if self.finished {
                BufferState::Empty
            } else if self.pending_secs_ahead() > 0.0 {
                BufferState::Filling
            } else {
                BufferState::Stalled
            }
        } else if buffered < self.low_watermark {
            BufferState::Low
        } else if buffered < self.high_watermark {
            BufferState::Filling
        } else {
            BufferState::Healthy
        }
    }

    /// Normalised 0..1 fill level against the high watermark.
    pub fn fill_level(&self) -> f64 {
        if self.high_watermark <= 0.0 {
            return 0.0;
        }
        (self.buffered_secs_ahead() / self.high_watermark).clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(seq: u64, duration: f64) -> Segment {
        Segment {
            seq,
            uri: format!("https://cdn.example/seg_{seq}.ts"),
            duration,
        }
    }

    #[test]
    fn enqueue_dedupes_and_respects_playhead() {
        let mut buf = SegmentBuffer::new(12.0, 4.0);
        assert!(buf.enqueue(seg(0, 4.0)));
        assert!(buf.enqueue(seg(1, 4.0)));
        assert!(!buf.enqueue(seg(1, 4.0)), "duplicate must be rejected");
        buf.advance_playhead(1);
        // keep_behind=2 retains the played segment for short seeks.
        assert_eq!(buf.first_seq(), Some(0));
        assert!(!buf.enqueue(seg(0, 4.0)), "past segment must be rejected");
        assert!(buf.enqueue(seg(2, 4.0)));
        assert_eq!(buf.len(), 3);
    }

    #[test]
    fn watermarks_drive_prefetch_and_health() {
        let mut buf = SegmentBuffer::new(12.0, 4.0);
        for i in 0..3 {
            buf.enqueue(seg(i, 4.0));
        }
        assert_eq!(buf.health(), BufferState::Filling);
        assert!(buf.needs_prefetch());

        for i in 0..3 {
            buf.store_data(i, Arc::new(vec![i as u8; 10]));
        }
        assert_eq!(buf.health(), BufferState::Healthy);
        assert!(!buf.needs_prefetch(), "12s buffered == high watermark");
        assert_eq!(buf.fill_level(), 1.0);
    }

    #[test]
    fn playhead_and_trimming() {
        let mut buf = SegmentBuffer::new(30.0, 5.0);
        buf.set_keep_behind(2);
        for i in 0..8 {
            buf.enqueue(seg(i, 4.0));
            buf.store_data(i, Arc::new(vec![0u8; 4]));
        }
        buf.advance_playhead(5);
        assert_eq!(buf.playhead(), 5);
        assert_eq!(buf.first_seq(), Some(3), "keep_behind=2 should retain 3..");
        assert!((buf.buffered_secs_ahead() - 4.0 * 3.0).abs() < 1e-9);
    }

    #[test]
    fn done_requires_endlist_and_full_download() {
        let mut buf = SegmentBuffer::new(12.0, 4.0);
        buf.enqueue(seg(0, 4.0));
        buf.set_finished();
        assert!(!buf.is_done(), "still missing data");
        buf.store_data(0, Arc::new(vec![1]));
        assert!(buf.is_done());
    }

    #[test]
    fn stalled_when_nothing_pending() {
        let mut buf = SegmentBuffer::new(12.0, 4.0);
        assert_eq!(buf.health(), BufferState::Stalled);
        buf.enqueue(seg(0, 4.0));
        assert_eq!(buf.health(), BufferState::Filling);
        // Ended but nothing ever downloaded: over, not stalled.
        buf.set_finished();
        assert_eq!(buf.health(), BufferState::Empty);
        buf.store_data(0, Arc::new(vec![1]));
        assert_eq!(buf.health(), BufferState::Filling);
    }

    #[test]
    fn remove_drops_failed_segment() {
        let mut buf = SegmentBuffer::new(12.0, 4.0);
        buf.enqueue(seg(0, 4.0));
        buf.enqueue(seg(1, 4.0));
        assert!(buf.remove(0));
        assert!(!buf.remove(0));
        assert_eq!(buf.next_unloaded().map(|s| s.seq), Some(1));
    }
}
