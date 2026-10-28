//! External-process backend: hands the stream to the system player.
//!
//! Player discovery order:
//! 1. `SPIKE_PLAYER` environment variable (full command path)
//! 2. `mpv`
//! 3. `ffplay` (ffmpeg suite)
//! 4. `vlc`
//!
//! This keeps v0.1 dependency-free and works identically on Linux, Windows
//! and macOS; the player renders its own window and its own controls. A
//! future in-process GStreamer backend will replace this for embedded
//! playback (see `ARCHITECTURE.md`).

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::{Child, Command};

use crate::{EngineError, MediaBackend, PlaybackEvent};

/// Plays a URL by spawning an external player process.
#[derive(Debug, Default)]
pub struct ExternalProcessBackend {
    url: Option<String>,
    child: Option<Child>,
    events: VecDeque<PlaybackEvent>,
    reported_exit: bool,
}

impl ExternalProcessBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// Locate a usable player, honouring `SPIKE_PLAYER` first.
    pub fn find_player() -> Option<PathBuf> {
        if let Ok(custom) = std::env::var("SPIKE_PLAYER") {
            if !custom.trim().is_empty() {
                return Some(PathBuf::from(custom));
            }
        }
        for name in ["mpv", "ffplay", "vlc"] {
            if let Some(path) = find_in_path(name) {
                return Some(path);
            }
        }
        None
    }
}

impl MediaBackend for ExternalProcessBackend {
    fn open(&mut self, url: &str) -> Result<(), EngineError> {
        self.url = Some(url.to_string());
        Ok(())
    }

    fn play(&mut self) -> Result<(), EngineError> {
        if self.child.is_some() {
            return Ok(());
        }
        let Some(url) = self.url.clone() else {
            return Err(EngineError::Unsupported(
                "open() must be called before play()".to_string(),
            ));
        };
        let Some(player) = Self::find_player() else {
            return Err(EngineError::SpawnFailed(
                "no external player found (tried mpv, ffplay, vlc; or set SPIKE_PLAYER)"
                    .to_string(),
            ));
        };

        let mut command = Command::new(&player);
        let player_name = player
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if player_name.contains("mpv") {
            command.arg("--really-quiet");
        } else if player_name.contains("ffplay") {
            command.args(["-autoexit", "-loglevel", "quiet"]);
        }
        command.arg(&url);

        let child = command.spawn().map_err(EngineError::Io)?;
        self.child = Some(child);
        self.reported_exit = false;
        self.events.push_back(PlaybackEvent::Started);
        Ok(())
    }

    fn pause(&mut self) -> Result<(), EngineError> {
        Err(EngineError::Unsupported(
            "external players are controlled from their own window".to_string(),
        ))
    }

    fn stop(&mut self) -> Result<(), EngineError> {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.child = None;
        self.events.push_back(PlaybackEvent::Stopped);
        Ok(())
    }

    fn set_volume(&mut self, volume: f32) -> Result<(), EngineError> {
        // External players own their volume; record for observability.
        self.events
            .push_back(PlaybackEvent::VolumeChanged(volume.clamp(0.0, 1.0)));
        Ok(())
    }

    fn poll_event(&mut self) -> Option<PlaybackEvent> {
        if let Some(event) = self.events.pop_front() {
            return Some(event);
        }
        if !self.reported_exit {
            let exited = self
                .child
                .as_mut()
                .and_then(|child| child.try_wait().ok())
                .flatten();
            if exited.is_some() {
                self.reported_exit = true;
                return Some(PlaybackEvent::Stopped);
            }
        }
        None
    }

    fn name(&self) -> &'static str {
        "external"
    }
}

/// Search `PATH` for an executable (adds `.exe` on Windows).
pub fn find_in_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var("PATH").ok()?;
    let exe_names: Vec<String> = if cfg!(windows) {
        vec![format!("{name}.exe"), name.to_string()]
    } else {
        vec![name.to_string()]
    };
    for dir in std::env::split_paths(&path_var) {
        for exe in &exe_names {
            let candidate = dir.join(exe);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_common_shell_in_path() {
        // POSIX environments always ship `sh`; on Windows this test would
        // look for sh.exe which may not exist, so gate on unix.
        if cfg!(unix) {
            assert!(find_in_path("sh").is_some());
        }
        assert!(find_in_path("definitely-not-a-real-binary-xyz").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn spawns_custom_player_and_reports_lifecycle() {
        // Use /bin/sh as a stand-in player: it exits immediately because the
        // "media URL" is not an executable script, which lets us observe the
        // full Started -> Stopped lifecycle without a real video stack.
        // SAFETY: tests run in-process; we restore the env var afterwards.
        let saved = std::env::var("SPIKE_PLAYER").ok();
        std::env::set_var("SPIKE_PLAYER", "/bin/sh");

        let mut backend = ExternalProcessBackend::new();
        assert!(backend.open("http://127.0.0.1:9/live.m3u8").is_ok());
        backend.play().expect("spawn /bin/sh");
        assert_eq!(backend.name(), "external");

        let mut saw_started = false;
        let mut saw_stopped = false;
        for _ in 0..50 {
            match backend.poll_event() {
                Some(PlaybackEvent::Started) => saw_started = true,
                Some(PlaybackEvent::Stopped) => {
                    saw_stopped = true;
                    break;
                }
                _ => std::thread::sleep(std::time::Duration::from_millis(20)),
            }
        }
        assert!(saw_started, "should report Started after spawn");
        assert!(saw_stopped, "should report Stopped after player exits");

        match saved {
            Some(v) => std::env::set_var("SPIKE_PLAYER", v),
            None => std::env::remove_var("SPIKE_PLAYER"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn play_without_open_is_rejected() {
        let mut backend = ExternalProcessBackend::new();
        assert!(matches!(backend.play(), Err(EngineError::Unsupported(_))));
    }
}
