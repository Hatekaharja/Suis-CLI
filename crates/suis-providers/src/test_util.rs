//! Test-only helpers: a tiny dependency-free HTTP server that serves one canned
//! response to every request, so transport/discovery tests stay hermetic
//! without pulling in a mock-HTTP crate.
#![cfg(test)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

static CACHE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique temporary directory, removed on drop. Used as a capability cache
/// directory in detection tests.
pub(crate) struct TempCacheDir {
    path: PathBuf,
}

impl TempCacheDir {
    pub fn new() -> Self {
        let n = CACHE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "suis-providers-test-{}-{}-{}",
            std::process::id(),
            n,
            nanos
        ));
        std::fs::create_dir_all(&path).unwrap();
        TempCacheDir { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempCacheDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// A localhost HTTP server that replies to every connection with the same
/// status, headers, and body until it is dropped. The raw request head of each
/// connection is captured so tests can assert on received headers (e.g. an
/// `Authorization` bearer).
pub(crate) struct MockServer {
    port: u16,
    shutdown: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    captured: Arc<Mutex<Vec<String>>>,
}

impl MockServer {
    /// Serve `body` with a `200 OK` / `application/json` response.
    pub fn json(body: &str) -> Self {
        Self::new(200, "application/json", body)
    }

    /// Serve `body` with a `200 OK` / `text/event-stream` response.
    pub fn sse(body: &str) -> Self {
        Self::new(200, "text/event-stream", body)
    }

    /// Serve `body` with an arbitrary status code.
    pub fn status(code: u16, body: &str) -> Self {
        Self::new(code, "text/plain", body)
    }

    /// Serve `body` with an arbitrary status code and `application/json`. Used
    /// to exercise auth (401/403) and other error classifications.
    pub fn json_status(code: u16, body: &str) -> Self {
        Self::new(code, "application/json", body)
    }

    /// A server that accepts connections but never sends a response, holding
    /// each open until shutdown — to exercise client read/request timeouts. The
    /// accepted streams are stashed (not dropped) so the peer sees a silent,
    /// stalled connection rather than an immediate close.
    pub fn stalling() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let port = listener.local_addr().unwrap().port();
        listener
            .set_nonblocking(true)
            .expect("set_nonblocking on listener");

        let shutdown = Arc::new(AtomicBool::new(false));
        let stop = shutdown.clone();

        let handle = std::thread::spawn(move || {
            let mut held: Vec<TcpStream> = Vec::new();
            while !stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => held.push(stream),
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
            drop(held);
        });

        MockServer {
            port,
            shutdown,
            handle: Some(handle),
            captured: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn new(code: u16, content_type: &str, body: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let port = listener.local_addr().unwrap().port();
        listener
            .set_nonblocking(true)
            .expect("set_nonblocking on listener");

        let response = build_response(code, content_type, body);
        let shutdown = Arc::new(AtomicBool::new(false));
        let stop = shutdown.clone();
        let captured = Arc::new(Mutex::new(Vec::new()));
        let sink = captured.clone();

        let handle = std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => handle_connection(stream, &response, &sink),
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });

        MockServer {
            port,
            shutdown,
            handle: Some(handle),
            captured,
        }
    }

    /// Base URL clients should target, e.g. `http://127.0.0.1:54321`.
    pub fn endpoint(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// The value of a received request header (case-insensitive name) from the
    /// most recent request, if any request carried it. Returns `None` when no
    /// request has arrived or the header is absent.
    pub fn received_header(&self, name: &str) -> Option<String> {
        let needle = format!("{}:", name.to_ascii_lowercase());
        let captured = self.captured.lock().unwrap();
        let head = captured.last()?;
        for line in head.lines() {
            if line.to_ascii_lowercase().starts_with(&needle) {
                return Some(line[needle.len()..].trim().to_string());
            }
        }
        None
    }

    /// Number of requests received so far.
    pub fn request_count(&self) -> usize {
        self.captured.lock().unwrap().len()
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn build_response(code: u16, content_type: &str, body: &str) -> Vec<u8> {
    let reason = match code {
        200 => "OK",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "Status",
    };
    format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

/// Read the full request (headers + any Content-Length body) so the client
/// finishes sending before we reply, record the request head for assertions,
/// then write the canned response.
fn handle_connection(mut stream: TcpStream, response: &[u8], sink: &Arc<Mutex<Vec<String>>>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];

    loop {
        // Headers complete?
        if let Some(header_end) = find_header_end(&buf) {
            let content_length = parse_content_length(&buf[..header_end]);
            let body_have = buf.len() - header_end;
            if body_have >= content_length {
                break;
            }
        }
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => break,
        }
    }

    if let Some(header_end) = find_header_end(&buf) {
        let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
        sink.lock().unwrap().push(head);
    }

    let _ = stream.write_all(response);
    let _ = stream.flush();
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

fn parse_content_length(headers: &[u8]) -> usize {
    let text = String::from_utf8_lossy(headers);
    for line in text.lines() {
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            return value.trim().parse().unwrap_or(0);
        }
    }
    0
}
