//! A recording no-op backend for tests and UI development.

use std::collections::VecDeque;

use crate::{EngineError, MediaBackend, PlaybackEvent};

/// Pretends to play: records state transitions as events.
#[derive(Debug, Default)]
pub struct NullBackend {
    url: Option<String>,
    playing: bool,
    events: VecDeque<PlaybackEvent>,
}

impl NullBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// The URL passed to `open`, if any.
    pub fn url(&self) -> Option<&str> {
        self.url.as_deref()
    }

    /// Whether the backend thinks it is playing.
    pub fn is_playing(&self) -> bool {
        self.playing
    }
}

impl MediaBackend for NullBackend {
    fn open(&mut self, url: &str) -> Result<(), EngineError> {
        self.url = Some(url.to_string());
        Ok(())
    }

    fn play(&mut self) -> Result<(), EngineError> {
        self.playing = true;
        self.events.push_back(PlaybackEvent::Started);
        Ok(())
    }

    fn pause(&mut self) -> Result<(), EngineError> {
        self.playing = false;
        Ok(())
    }

    fn stop(&mut self) -> Result<(), EngineError> {
        if self.playing {
            self.events.push_back(PlaybackEvent::Stopped);
        }
        self.playing = false;
        Ok(())
    }

    fn set_volume(&mut self, volume: f32) -> Result<(), EngineError> {
        self.events
            .push_back(PlaybackEvent::VolumeChanged(volume.clamp(0.0, 1.0)));
        Ok(())
    }

    fn poll_event(&mut self) -> Option<PlaybackEvent> {
        self.events.pop_front()
    }

    fn name(&self) -> &'static str {
        "null"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_lifecycle_events_in_order() {
        let mut backend = NullBackend::new();
        backend.open("http://127.0.0.1:9/live.m3u8").unwrap();
        assert_eq!(backend.url(), Some("http://127.0.0.1:9/live.m3u8"));
        assert!(!backend.is_playing());

        backend.play().unwrap();
        assert!(backend.is_playing());
        backend.set_volume(0.7).unwrap();
        backend.stop().unwrap();
        assert!(!backend.is_playing());

        assert_eq!(backend.poll_event(), Some(PlaybackEvent::Started));
        assert_eq!(
            backend.poll_event(),
            Some(PlaybackEvent::VolumeChanged(0.7))
        );
        assert_eq!(backend.poll_event(), Some(PlaybackEvent::Stopped));
        assert_eq!(backend.poll_event(), None);
    }

    #[test]
    fn stop_when_idle_emits_nothing() {
        let mut backend = NullBackend::new();
        backend.stop().unwrap();
        assert_eq!(backend.poll_event(), None);
    }

    #[test]
    fn volume_is_clamped() {
        let mut backend = NullBackend::new();
        backend.set_volume(2.0).unwrap();
        assert_eq!(
            backend.poll_event(),
            Some(PlaybackEvent::VolumeChanged(1.0))
        );
    }
}
