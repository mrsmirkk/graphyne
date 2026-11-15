//! Live-reload preview server for HTML/CSS (spec §6.2 "Web Preview").
//!
//! A tiny HTTP/1.1 server on `127.0.0.1` (random port) that serves the project folder.
//! HTML pages get a script that listens to `/__graphyne/live` (Server-Sent Events) and
//! reloads the page when [`LiveServer::reload`] is called after a save. Plain `std`
//! threads: one acceptor plus one thread per connection, nothing on the UI thread.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

const EVENTS_PATH: &str = "/__graphyne/live";
const RELOAD_SCRIPT: &str = "<script>/* Graphyne live reload */new EventSource('/__graphyne/live').onmessage=()=>location.reload();</script>";

pub struct LiveServer {
    root: PathBuf,
    port: u16,
    /// Bumped on every reload; SSE connections wait on the condvar.
    generation: Arc<(Mutex<u64>, Condvar)>,
}

impl LiveServer {
    /// Bind `127.0.0.1:0` and start serving `root` (blocking bind: call off the UI thread).
    pub fn start(root: PathBuf) -> std::io::Result<Arc<Self>> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        let root = std::fs::canonicalize(&root).unwrap_or(root);
        let server = Arc::new(Self { root, port, generation: Arc::new((Mutex::new(0), Condvar::new())) });
        let shared = server.clone();
        std::thread::Builder::new().name("graphyne-live-server".into()).spawn(move || {
            for stream in listener.incoming().flatten() {
                let s = shared.clone();
                let _ = std::thread::Builder::new().name("graphyne-live-conn".into()).spawn(move || s.handle(stream));
            }
        })?;
        Ok(server)
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// `http://127.0.0.1:<port>/<path relative to root>`, if `file` is inside the root.
    pub fn url_for(&self, file: &Path) -> Option<String> {
        let file = std::fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
        let rel = file.strip_prefix(&self.root).ok()?;
        let mut url = format!("http://127.0.0.1:{}", self.port);
        for part in rel.components() {
            url.push('/');
            url.push_str(&percent_encode(&part.as_os_str().to_string_lossy()));
        }
        Some(url)
    }

    /// Tell every open preview page to reload.
    pub fn reload(&self) {
        let (lock, cvar) = &*self.generation;
        *lock.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        cvar.notify_all();
    }

    fn handle(&self, stream: TcpStream) {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
        let mut reader = BufReader::new(match stream.try_clone() {
            Ok(s) => s,
            Err(_) => return,
        });
        let mut request_line = String::new();
        if reader.read_line(&mut request_line).is_err() {
            return;
        }
        // Drain headers.
        let mut line = String::new();
        while reader.read_line(&mut line).map(|n| n > 2).unwrap_or(false) {
            line.clear();
        }
        let mut parts = request_line.split_whitespace();
        let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or("/"));
        let mut out = stream;
        if method != "GET" && method != "HEAD" {
            let _ = respond(&mut out, "405 Method Not Allowed", "text/plain", b"method not allowed");
            return;
        }
        let path = target.split(['?', '#']).next().unwrap_or("/");
        if path == EVENTS_PATH {
            self.stream_events(out);
            return;
        }
        match self.resolve(path) {
            Some(file) => {
                let Ok(mut body) = std::fs::read(&file) else {
                    let _ = respond(&mut out, "404 Not Found", "text/plain", b"not found");
                    return;
                };
                let mime = mime_for(&file);
                if mime.starts_with("text/html") {
                    body = inject_script(body);
                }
                let _ = respond(&mut out, "200 OK", mime, if method == "HEAD" { &[] } else { &body });
            }
            None => {
                let _ = respond(&mut out, "404 Not Found", "text/plain", b"not found");
            }
        }
    }

    /// Map a URL path to a file inside the root (`/` and folders → `index.html`).
    fn resolve(&self, url_path: &str) -> Option<PathBuf> {
        let decoded = percent_decode(url_path);
        let mut file = self.root.clone();
        for part in Path::new(decoded.trim_start_matches('/')).components() {
            match part {
                Component::Normal(p) => file.push(p),
                Component::CurDir => {}
                // `..`, drive letters, roots: never leave the served folder.
                _ => return None,
            }
        }
        if file.is_dir() {
            file.push("index.html");
        }
        let file = std::fs::canonicalize(&file).ok()?;
        (file.starts_with(&self.root) && file.is_file()).then_some(file)
    }

    /// Server-Sent Events: `data: reload` after each [`reload`](Self::reload); a comment
    /// every 15 s keeps the connection alive and notices closed tabs.
    fn stream_events(&self, mut out: TcpStream) {
        let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n";
        if out.write_all(head.as_bytes()).and_then(|_| out.flush()).is_err() {
            return;
        }
        let (lock, cvar) = &*self.generation;
        let mut seen = *lock.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
            let (guard, _) = cvar.wait_timeout_while(guard, Duration::from_secs(15), |g| *g == seen).unwrap_or_else(|e| e.into_inner());
            let now = *guard;
            drop(guard);
            let msg: &[u8] = if now != seen { b"data: reload\n\n" } else { b": ping\n\n" };
            seen = now;
            if out.write_all(msg).and_then(|_| out.flush()).is_err() {
                return;
            }
        }
    }
}

fn respond(out: &mut TcpStream, status: &str, mime: &str, body: &[u8]) -> std::io::Result<()> {
    let head = format!("HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n", body.len());
    out.write_all(head.as_bytes())?;
    out.write_all(body)?;
    out.flush()
}

/// Insert the reload script before `</body>` (or append it).
fn inject_script(body: Vec<u8>) -> Vec<u8> {
    let text = String::from_utf8_lossy(&body);
    let lower = text.to_ascii_lowercase();
    match lower.rfind("</body>") {
        Some(i) => format!("{}{RELOAD_SCRIPT}{}", &text[..i], &text[i..]).into_bytes(),
        None => format!("{text}{RELOAD_SCRIPT}").into_bytes(),
    }
}

fn mime_for(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase().as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "txt" | "md" => "text/plain; charset=utf-8",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
            if let Some(b) = hex {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn get(port: u16, path: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(s, "GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    }

    #[test]
    fn serves_injects_and_reloads() {
        let dir = std::env::temp_dir().join(format!("graphyne-live-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("my site")).unwrap();
        std::fs::write(dir.join("my site").join("index.html"), "<html><body><h1>Hi</h1></body></html>").unwrap();
        std::fs::write(dir.join("style.css"), "h1{}").unwrap();
        let server = LiveServer::start(dir.clone()).unwrap();

        let url = server.url_for(&dir.join("my site").join("index.html")).unwrap();
        assert!(url.ends_with("/my%20site/index.html"), "{url}");
        let page = get(server.port(), "/my%20site/");
        assert!(page.starts_with("HTTP/1.1 200"), "{page}");
        assert!(page.contains("<h1>Hi</h1><script>"), "script goes before </body>: {page}");
        assert!(get(server.port(), "/style.css").contains("text/css"));
        assert!(get(server.port(), "/../Cargo.toml").starts_with("HTTP/1.1 404"));
        assert!(get(server.port(), "/%2e%2e/secret").starts_with("HTTP/1.1 404"));

        // SSE: a reload reaches an open event stream.
        let mut events = TcpStream::connect(("127.0.0.1", server.port())).unwrap();
        write!(events, "GET {EVENTS_PATH} HTTP/1.1\r\n\r\n").unwrap();
        events.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut buf = [0u8; 512];
        let n = events.read(&mut buf).unwrap();
        assert!(String::from_utf8_lossy(&buf[..n]).contains("text/event-stream"));
        std::thread::sleep(Duration::from_millis(100));
        server.reload();
        let n = events.read(&mut buf).unwrap();
        assert!(String::from_utf8_lossy(&buf[..n]).contains("data: reload"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
