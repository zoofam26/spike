//! Spike — live event sports streaming app.
//!
//! `spike` ties the library crates together:
//!
//! * `spike-core`   parses playlists (M3U/IPTV) and guides (XMLTV)
//! * `spike-stream` buffers HLS with a sliding window and re-serves it on
//!   loopback so playback never stalls on the network
//! * `spike-engine` hands the buffered stream to a media backend
//!
//! Run `spike --help` for the CLI surface.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::cell::RefCell;
use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use slint::{ModelRc, VecModel};
use spike_core::{m3u, MediaKind, Playlist, PlaylistItem};
use spike_engine::create_backend;
use spike_stream::buffer::{BufferState, SegmentBuffer};
use spike_stream::error::StreamError;
use spike_stream::scheduler::{
    Scheduler, SchedulerConfig, SchedulerStats, SegmentFetcher, StdClock,
};
use spike_stream::server::LocalStreamServer;

slint::include_modules!();

const USAGE: &str = "\
Spike — live event sports streaming (Rust engine, Slint UI)

USAGE:
    spike [OPTIONS]

OPTIONS:
    --playlist <FILE>    M3U playlist to load (default: bundled sample)
    --url <M3U8>         Load a single HLS playlist instead of a file
    --headless           Print the event list and exit (no UI)
    --play <INDEX>       Headless pipeline demo: buffer event N for a while
    --duration <SECS>    How long the headless demo runs (default: 10)
    --backend <NAME>     `external` (system player, default) or `null`
    -h, --help           Print this help
";

/// Parsed command line.
#[derive(Debug, Clone, Default)]
struct Args {
    playlist: Option<PathBuf>,
    url: Option<String>,
    headless: bool,
    play: Option<usize>,
    duration_secs: u64,
    backend: Option<String>,
}

fn parse_args<I: Iterator<Item = String>>(mut args: I) -> Result<(bool, Args), String> {
    let _program_name = args.next();
    let mut out = Args {
        duration_secs: 10,
        ..Args::default()
    };
    let mut help = false;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => help = true,
            "--headless" => out.headless = true,
            "--playlist" => out.playlist = Some(PathBuf::from(next_value(&mut args, &arg)?)),
            "--url" => out.url = Some(next_value(&mut args, &arg)?),
            "--play" => {
                out.play = Some(
                    next_value(&mut args, &arg)?
                        .parse()
                        .map_err(|_| "--play expects a numeric index".to_string())?,
                );
            }
            "--duration" => {
                out.duration_secs = next_value(&mut args, &arg)?
                    .parse()
                    .map_err(|_| "--duration expects seconds".to_string())?;
            }
            "--backend" => out.backend = Some(next_value(&mut args, &arg)?),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok((help, out))
}

fn next_value<I: Iterator<Item = String>>(args: &mut I, flag: &str) -> Result<String, String> {
    args.next().ok_or_else(|| format!("{flag} expects a value"))
}

/// Production fetcher on top of `ureq` (rustls-backed HTTPS, redirects).
struct HttpFetcher;

impl SegmentFetcher for HttpFetcher {
    fn fetch(&self, url: &str) -> Result<Vec<u8>, StreamError> {
        let response = ureq::get(url)
            .timeout(Duration::from_secs(15))
            .call()
            .map_err(|err| StreamError::Network(format!("{err}")))?;
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(64 * 1024 * 1024)
            .read_to_end(&mut bytes)
            .map_err(|err| StreamError::Network(format!("{err}")))?;
        Ok(bytes)
    }
}

fn load_playlist(args: &Args) -> Result<Playlist, String> {
    if let Some(url) = &args.url {
        let bytes = HttpFetcher
            .fetch(url)
            .map_err(|err| format!("fetch failed: {err}"))?;
        let text = String::from_utf8_lossy(&bytes).to_string();
        return m3u::parse(&text, Some(url)).map_err(|err| err.to_string());
    }

    let path = args.playlist.clone().unwrap_or_else(default_sample_path);
    let text = std::fs::read_to_string(&path)
        .map_err(|err| format!("cannot read {}: {err}", path.display()))?;
    m3u::parse(&text, None).map_err(|err| err.to_string())
}

fn default_sample_path() -> PathBuf {
    // Prefer the repository layout (dev checkouts), fall back to cwd.
    for candidate in [
        PathBuf::from("assets/playlists/sample.m3u"),
        PathBuf::from("../assets/playlists/sample.m3u"),
        PathBuf::from("../../assets/playlists/sample.m3u"),
    ] {
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from("assets/playlists/sample.m3u")
}

/// Pure projection of playlist items into UI rows (unit-testable).
fn event_rows(playlist: &Playlist) -> Vec<(String, String, String, bool)> {
    playlist
        .items
        .iter()
        .map(|item| {
            (
                item.name.clone(),
                item.group.clone().unwrap_or_else(|| "Uncategorized".into()),
                item.kind.label().to_string(),
                matches!(item.kind, MediaKind::Hls),
            )
        })
        .collect()
}

/// Everything needed to stop a running pipeline again.
struct PipelineHandles {
    shutdown: Arc<AtomicBool>,
    server: Option<LocalStreamServer>,
    scheduler: Option<std::thread::JoinHandle<SchedulerStats>>,
}

impl PipelineHandles {
    fn stop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.scheduler.take() {
            let _ = handle.join();
        }
        if let Some(mut server) = self.server.take() {
            server.stop();
        }
    }
}

/// A started buffering pipeline.
struct StartedPipeline {
    handles: PipelineHandles,
    buffer: Arc<Mutex<SegmentBuffer>>,
    /// Loopback playlist URL a media backend should open.
    playlist_url: String,
}

/// Start buffering `item`: scheduler thread + local re-serve server.
fn start_pipeline(item: &PlaylistItem) -> Result<StartedPipeline, String> {
    let buffer = Arc::new(Mutex::new(SegmentBuffer::new(12.0, 4.0)));
    let config = SchedulerConfig {
        playlist_url: item.url.clone(),
        ..SchedulerConfig::default()
    };
    let scheduler = Scheduler::new(config, HttpFetcher, StdClock, buffer.clone());

    let server = LocalStreamServer::spawn(buffer.clone())
        .map_err(|err| format!("cannot start local stream server: {err}"))?;
    let playlist_url = server.playlist_url();

    let shutdown = Arc::new(AtomicBool::new(false));
    let handle = scheduler.spawn(shutdown.clone());

    Ok(StartedPipeline {
        handles: PipelineHandles {
            shutdown,
            server: Some(server),
            scheduler: Some(handle),
        },
        buffer,
        playlist_url,
    })
}

fn buffer_state_label(state: BufferState) -> &'static str {
    match state {
        BufferState::Empty => "ended",
        BufferState::Low => "buffer low",
        BufferState::Filling => "buffering",
        BufferState::Healthy => "buffer healthy",
        BufferState::Stalled => "stalled",
    }
}

fn run_headless(playlist: &Playlist, args: &Args) -> ExitCode {
    println!(
        "Loaded {} events across {} group(s)\n",
        playlist.items.len(),
        playlist.groups.len()
    );
    for (index, item) in playlist.items.iter().enumerate() {
        println!(
            "  [{index:>3}] {:<28} {:>7}  {}",
            item.name,
            item.kind.label(),
            item.url
        );
    }

    let Some(index) = args.play else {
        return ExitCode::SUCCESS;
    };
    let Some(item) = playlist.items.get(index) else {
        eprintln!("--play index {index} is out of range");
        return ExitCode::FAILURE;
    };
    if !matches!(item.kind, MediaKind::Hls) {
        eprintln!("--play only supports HLS events in this demo");
        return ExitCode::FAILURE;
    }

    println!("\nBuffering `{}` for {}s…", item.name, args.duration_secs);
    let Ok(mut pipeline) = start_pipeline(item) else {
        eprintln!("failed to start pipeline");
        return ExitCode::FAILURE;
    };
    println!("Local playlist URL for players: {}", pipeline.playlist_url);

    let deadline = Instant::now() + Duration::from_secs(args.duration_secs);
    while Instant::now() < deadline {
        let buffer = pipeline.buffer.lock().unwrap_or_else(|p| p.into_inner());
        println!(
            "  buffer: {:>6.1}s | {:>7} | segments: {}",
            buffer.buffered_secs_ahead(),
            format!("{:?}", buffer.health()).to_lowercase(),
            buffer.len()
        );
        drop(buffer);
        std::thread::sleep(Duration::from_millis(500));
    }

    pipeline.handles.stop();
    ExitCode::SUCCESS
}

fn run_ui(playlist: &Playlist, _args: &Args) -> Result<(), String> {
    let ui = SpikeWindow::new().map_err(|err| format!("cannot create UI: {err}"))?;

    let rows: Vec<EventRowData> = event_rows(playlist)
        .into_iter()
        .map(|(title, group, kind, live)| EventRowData {
            title: title.into(),
            group: group.into(),
            kind: kind.into(),
            live,
        })
        .collect();
    ui.set_events(ModelRc::from(Rc::new(VecModel::from(rows))));
    let groups: Vec<slint::SharedString> = playlist
        .groups
        .iter()
        .map(|group| group.clone().into())
        .collect();
    ui.set_groups(ModelRc::from(Rc::new(VecModel::from(groups))));

    let items: Rc<Vec<PlaylistItem>> = Rc::new(playlist.items.clone());
    let state: Rc<RefCell<Option<PipelineHandles>>> = Rc::new(RefCell::new(None));

    let stop_current = |state: &Rc<RefCell<Option<PipelineHandles>>>| {
        if let Some(mut handles) = state.borrow_mut().take() {
            handles.stop();
        }
    };

    let ui_weak = ui.as_weak();
    let state_for_open = state.clone();
    ui.on_open_event(move |index| {
        let Some(item) = items.get(index as usize).cloned() else {
            return;
        };
        let Some(ui) = ui_weak.upgrade() else {
            return;
        };

        // Stop whatever is running first.
        stop_current(&state_for_open);

        ui.set_selected_index(index);
        ui.set_now_playing(item.name.clone().into());
        ui.set_status_text("starting…".into());

        match item.kind {
            MediaKind::Hls => {
                let Ok(pipeline) = start_pipeline(&item) else {
                    ui.set_status_text("failed to start pipeline".into());
                    return;
                };
                // Hand the buffered stream to the system player.
                let mut backend = create_backend("external");
                if let Err(err) = backend
                    .open(&pipeline.playlist_url)
                    .and_then(|()| backend.play())
                {
                    ui.set_status_text(format!("playback: {err}").into());
                } else {
                    ui.set_status_text("streaming through local buffer".into());
                }
                // The external player owns its own window from here on; the
                // process is detached, only the buffering pipeline stays.

                // Health telemetry thread: pushes real buffer status into
                // the UI until the pipeline is stopped.
                let shutdown = pipeline.handles.shutdown.clone();
                let buffer = pipeline.buffer.clone();
                let ui_weak = ui_weak.clone();
                std::thread::spawn(move || {
                    while !shutdown.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(500));
                        let (secs, health, level) = {
                            let buf = buffer.lock().unwrap_or_else(|p| p.into_inner());
                            (buf.buffered_secs_ahead(), buf.health(), buf.fill_level())
                        };
                        let ui_weak = ui_weak.clone();
                        let label: slint::SharedString = format!("{secs:.1}s buffered").into();
                        let status: slint::SharedString = buffer_state_label(health).into();
                        let level = level as f32;
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = ui_weak.upgrade() {
                                ui.set_buffered_label(label);
                                ui.set_buffer_level(level);
                                ui.set_status_text(status);
                            }
                        });
                    }
                });

                *state_for_open.borrow_mut() = Some(pipeline.handles);
            }
            MediaKind::Direct => {
                let mut backend = create_backend("external");
                if let Err(err) = backend.open(&item.url).and_then(|()| backend.play()) {
                    ui.set_status_text(format!("playback: {err}").into());
                } else {
                    ui.set_status_text("playing direct stream".into());
                }
            }
            MediaKind::Dash => {
                ui.set_status_text("DASH support is on the roadmap (see ARCHITECTURE.md)".into());
            }
        }
    });

    let ui_weak = ui.as_weak();
    let state_for_stop = state.clone();
    ui.on_stop_playback(move || {
        stop_current(&state_for_stop);
        if let Some(ui) = ui_weak.upgrade() {
            ui.set_status_text("stopped".into());
            ui.set_now_playing("Nothing playing".into());
            ui.set_selected_index(-1);
            ui.set_buffered_label("—".into());
            ui.set_buffer_level(0.0);
        }
    });

    ui.run().map_err(|err| format!("UI error: {err}"))
}

fn main() -> ExitCode {
    let (help, args) = match parse_args(std::env::args()) {
        Ok(pair) => pair,
        Err(err) => {
            eprintln!("{USAGE}\nerror: {err}");
            return ExitCode::FAILURE;
        }
    };
    if help {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }

    let playlist = match load_playlist(&args) {
        Ok(playlist) => playlist,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::FAILURE;
        }
    };

    if args.headless {
        return run_headless(&playlist, &args);
    }

    match run_ui(&playlist, &args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn argv(items: &[&str]) -> Vec<String> {
        std::iter::once("spike".to_string())
            .chain(items.iter().map(|s| s.to_string()))
            .collect()
    }

    #[test]
    fn args_parse_all_flags() {
        let (help, args) = parse_args(
            argv(&[
                "--playlist",
                "/tmp/x.m3u",
                "--headless",
                "--play",
                "2",
                "--duration",
                "5",
                "--backend",
                "null",
            ])
            .into_iter(),
        )
        .expect("parse");
        assert!(!help);
        assert!(args.headless);
        assert_eq!(args.playlist.as_deref(), Some(Path::new("/tmp/x.m3u")));
        assert_eq!(args.play, Some(2));
        assert_eq!(args.duration_secs, 5);
        assert_eq!(args.backend.as_deref(), Some("null"));
    }

    #[test]
    fn args_reject_unknown_and_missing_values() {
        assert!(parse_args(argv(&["--wat"]).into_iter()).is_err());
        assert!(parse_args(argv(&["--play"]).into_iter()).is_err());
        let (help, _) = parse_args(argv(&["-h"]).into_iter()).expect("help");
        assert!(help);
    }

    #[test]
    fn event_rows_project_playlist() {
        let text = "\
#EXTM3U
#EXTINF:-1 group-title=\"Football\",Match A
https://cdn.example/a.m3u8
#EXTINF:-1,Direct Feed
https://cdn.example/b.ts
";
        let playlist = m3u::parse(text, None).expect("parse");
        let rows = event_rows(&playlist);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0],
            (
                "Match A".to_string(),
                "Football".to_string(),
                "HLS".to_string(),
                true
            )
        );
        assert_eq!(
            rows[1],
            (
                "Direct Feed".to_string(),
                "Uncategorized".to_string(),
                "DIRECT".to_string(),
                false
            )
        );
    }

    #[test]
    fn sample_playlist_parses_and_has_groups() {
        let path = default_sample_path();
        let text = std::fs::read_to_string(&path).expect("sample playlist readable");
        let playlist = m3u::parse(&text, None).expect("parse");
        assert!(!playlist.items.is_empty());
        assert!(playlist.groups.contains(&"Space & Exploration".to_string()));
        assert!(playlist
            .items
            .iter()
            .all(|item| matches!(item.kind, MediaKind::Hls)));
    }
}
