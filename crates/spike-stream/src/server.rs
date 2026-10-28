//! Tiny localhost HTTP server that re-serves the buffered stream.
//!
//! The scheduler downloads segments into the [`SegmentBuffer`]; this server
//! exposes that buffer to *any* media backend as a well-formed sliding-window
//! HLS playlist on `127.0.0.1`. The player therefore always reads from local
//! memory at disk speed and never stalls on the network — that is Spike's
//! smooth-streaming trick.
//!
//! Scope: HTTP/1.1 GET only, `Connection: close`, bound to loopback. This is
//! not a general-purpose web server and deliberately stays std-only.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::buffer::SegmentBuffer;

/// A running local stream server. Stopping happens on `stop()` or `drop()`.
pub struct LocalStreamServer {
    addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl LocalStreamServer {
    /// Bind to a random loopback port and start serving the buffer.
    pub fn spawn(buffer: Arc<Mutex<SegmentBuffer>>) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let state = ServerState { buffer };
        let thread_shutdown = shutdown.clone();

        let handle = std::thread::Builder::new()
            .name("spike-local-server".into())
            .spawn(move || {
                while !thread_shutdown.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let st = state.clone();
                            let _ = std::thread::Builder::new()
                                .name("spike-conn".into())
                                .spawn(move || st.handle(stream));
                        }
                        Err(ref err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(50));
                        }
                        Err(_) => break,
                    }
                }
            })?;

        Ok(Self {
            addr,
            shutdown,
            handle: Some(handle),
        })
    }

    /// The bound loopback address.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// URL of the re-served playlist for media backends.
    pub fn playlist_url(&self) -> String {
        format!("http://{}/live.m3u8", self.addr)
    }

    /// Stop the accept loop and join the listener thread.
    pub fn stop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for LocalStreamServer {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Clone)]
struct ServerState {
    buffer: Arc<Mutex<SegmentBuffer>>,
}

impl ServerState {
    fn handle(&self, mut stream: TcpStream) {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));

        let mut head = Vec::new();
        let mut chunk = [0u8; 1024];
        let marker: &[u8] = b"\r\n\r\n";
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    head.extend_from_slice(&chunk[..n]);
                    if head.windows(4).any(|w| w == marker) || head.len() > 8192 {
                        break;
                    }
                }
                Err(_) => break,
            }
        }

        let text = String::from_utf8_lossy(&head);
        let mut parts = text.split_whitespace();
        let method = parts.next().unwrap_or("");
        let path = parts.next().unwrap_or("");

        if method != "GET" && method != "HEAD" {
            respond(
                &mut stream,
                405,
                "text/plain",
                b"method not allowed".to_vec(),
            );
            return;
        }

        let path_only = path.split(['?', '#']).next().unwrap_or("");
        if path_only == "/live.m3u8" {
            let body = {
                let buf = self.buffer.lock().unwrap_or_else(|p| p.into_inner());
                render_playlist(&buf)
            };
            respond(&mut stream, 200, "application/vnd.apple.mpegurl", body);
        } else if let Some(seq_str) = path_only.strip_prefix("/seg/") {
            let seq: u64 = match seq_str.parse() {
                Ok(seq) => seq,
                Err(_) => {
                    respond(
                        &mut stream,
                        400,
                        "text/plain",
                        b"bad sequence number".to_vec(),
                    );
                    return;
                }
            };
            let data = {
                let mut buf = self.buffer.lock().unwrap_or_else(|p| p.into_inner());
                buf.advance_playhead(seq);
                buf.data_for(seq)
            };
            match data {
                Some(bytes) if !bytes.is_empty() => {
                    respond(&mut stream, 200, "video/mp2t", (*bytes).clone())
                }
                _ => respond(
                    &mut stream,
                    404,
                    "text/plain",
                    b"segment not available".to_vec(),
                ),
            }
        } else {
            respond(&mut stream, 404, "text/plain", b"not found".to_vec());
        }
    }
}

fn respond(stream: &mut TcpStream, code: u16, content_type: &str, body: Vec<u8>) {
    let reason = match code {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    if code == 200 {
        let _ = stream.write_all(&body);
    }
    let _ = stream.flush();
}

/// Render the buffered window as an HLS media playlist.
///
/// Public so tests and future in-app backends can reuse the exact bytes a
/// real player would receive.
pub fn render_playlist(buffer: &SegmentBuffer) -> Vec<u8> {
    let segments: Vec<_> = buffer.window().collect();
    let mut out = String::from("#EXTM3U\n#EXT-X-VERSION:3\n");

    if segments.is_empty() {
        out.push_str("#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:0\n");
        return out.into_bytes();
    }

    let target = segments
        .iter()
        .map(|s| s.duration)
        .fold(0.0f64, f64::max)
        .ceil()
        .max(1.0);
    out.push_str(&format!("#EXT-X-TARGETDURATION:{target}\n"));
    out.push_str(&format!("#EXT-X-MEDIA-SEQUENCE:{}\n", segments[0].seq));
    out.push_str("#EXT-X-DISCONTINUITY-SEQUENCE:0\n");

    for seg in &segments {
        out.push_str(&format!("#EXTINF:{:.3},\n/seg/{}\n", seg.duration, seg.seq));
    }
    if buffer.is_finished() && buffer.next_unloaded().is_none() {
        out.push_str("#EXT-X-ENDLIST\n");
    }
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hls::Segment;

    fn make_buffer() -> Arc<Mutex<SegmentBuffer>> {
        let mut buf = SegmentBuffer::new(12.0, 4.0);
        for i in 0..3u64 {
            buf.enqueue(Segment {
                seq: i,
                uri: format!("https://origin/seg_{i}.ts"),
                duration: 4.0,
            });
            buf.store_data(i, Arc::new(vec![i as u8; 16]));
        }
        buf.set_finished();
        Arc::new(Mutex::new(buf))
    }

    fn http_get(addr: SocketAddr, path: &str) -> (u16, String, Vec<u8>) {
        let mut stream = TcpStream::connect(addr).expect("connect");
        let req = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n");
        stream.write_all(req.as_bytes()).expect("write");
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).expect("read");
        let text = String::from_utf8_lossy(&raw).to_string();
        let status: u16 = text
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let header_end = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|p| p + 4)
            .unwrap_or(raw.len());
        let body = raw[header_end..].to_vec();
        (status, text, body)
    }

    #[test]
    fn serves_playlist_and_segments() {
        let buffer = make_buffer();
        let mut server = LocalStreamServer::spawn(buffer).expect("spawn");
        let addr = server.addr();

        let (status, _head, body) = http_get(addr, "/live.m3u8");
        assert_eq!(status, 200);
        let playlist = String::from_utf8(body).expect("utf8");
        assert!(playlist.starts_with("#EXTM3U"));
        assert!(playlist.contains("#EXT-X-MEDIA-SEQUENCE:0"));
        assert!(playlist.contains("#EXT-X-ENDLIST"));
        assert!(playlist.contains("/seg/0"));

        let (status, head, body) = http_get(addr, "/seg/1");
        assert_eq!(status, 200);
        assert!(head.contains("video/mp2t"));
        assert_eq!(body, vec![1u8; 16]);

        // Serving segment 1 advanced the playhead, so the window slides.
        let (status, _head, body) = http_get(addr, "/live.m3u8");
        assert_eq!(status, 200);
        let playlist = String::from_utf8(body).expect("utf8");
        assert!(playlist.contains("#EXT-X-MEDIA-SEQUENCE:1"));

        let (status, _, _) = http_get(addr, "/seg/999");
        assert_eq!(status, 404);
        let (status, _, _) = http_get(addr, "/nope");
        assert_eq!(status, 404);

        server.stop();
    }

    #[test]
    fn empty_buffer_still_serves_valid_playlist() {
        let buffer = Arc::new(Mutex::new(SegmentBuffer::new(12.0, 4.0)));
        let server = LocalStreamServer::spawn(buffer).expect("spawn");

        let (status, _head, body) = http_get(server.addr(), "/live.m3u8");
        assert_eq!(status, 200);
        let playlist = String::from_utf8(body).expect("utf8");
        assert!(playlist.contains("#EXTM3U"));
    }

    #[test]
    fn render_playlist_direct() {
        let buffer = make_buffer();
        let buf = buffer.lock().unwrap();
        let playlist = String::from_utf8(render_playlist(&buf)).unwrap();
        assert_eq!(
            playlist
                .lines()
                .filter(|l| l.starts_with("#EXTINF"))
                .count(),
            3
        );
    }
}
