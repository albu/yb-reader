//! The mirror HTTP/1.1 client + UDP discovery, ported 1:1 from
//! mirror.koplugin (Conn, discover). One kept-alive connection, strict
//! timeouts, idempotency-friendly retries.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs, UdpSocket};
use std::time::Duration;

pub const DEFAULT_PORT: u16 = 8765;
pub const DISCOVER_PORT: u16 = DEFAULT_PORT + 1;
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// Ceiling on one response body. Legit payloads are frame PNGs (well under
/// 1 MB); anything larger is hostile or broken and must fail closed rather
/// than stream into memory unbounded.
pub const MAX_BODY_BYTES: i64 = 16 * 1024 * 1024;
/// Ceiling on a single status/header line. Real lines are tiny; without a
/// cap a peer can trickle bytes with no newline forever and grow the heap
/// until the allocator aborts.
const MAX_LINE_BYTES: usize = 16 * 1024;
/// Hard wall-clock ceiling on one request()'s RECEIVE phase, checked before
/// every socket read. The per-read REQUEST_TIMEOUT restarts on each byte,
/// so a peer trickling 1 B/4.9 s could otherwise hold a request open
/// forever — and mirror.rs runs these requests inline on the UI thread,
/// where "forever" freezes the device. Legit frames land in tens of ms on
/// a warm link (radio wake adds ~0.2 s); 2.5 s is ~10× headroom.
pub const REQUEST_BUDGET: Duration = Duration::from_millis(2_500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Connect,
    Send,
    Status,
    Headers,
    Body,
}

#[derive(Debug)]
pub struct Resp {
    pub status: u16,
    pub headers: HashMap<String, String>,
}

pub struct Conn {
    host: String,
    port: u16,
    stream: Option<TcpStream>,
    buf: Vec<u8>,
    pos: usize,
    fill_len: usize,
    /// Wall-clock deadline for the in-flight request, armed by `request`
    /// and enforced before every socket read. `None` between requests.
    deadline: Option<std::time::Instant>,
    /// Budget applied per request; defaults to REQUEST_BUDGET.
    budget: Duration,
    /// Shared credential (`mirror.conf` SECRET=) sent as `X-YB-Secret`
    /// on every request when set. None = wire-identical to the
    /// unauthenticated protocol.
    secret: Option<String>,
}

impl Conn {
    pub fn new(host: &str, port: u16) -> Conn {
        Conn {
            host: host.to_string(),
            port,
            stream: None,
            buf: vec![0u8; 8192],
            pos: 0,
            fill_len: 0,
            deadline: None,
            budget: REQUEST_BUDGET,
            secret: None,
        }
    }

    /// Attach the shared credential (mirror.conf `SECRET=`); every
    /// subsequent request carries it as an `X-YB-Secret` header. The
    /// server opts in by requiring that header — see SECURITY.md.
    pub fn set_secret(&mut self, secret: Option<String>) {
        self.secret = secret;
    }

    /// Override the per-request wall-clock budget (tests shrink it).
    #[cfg(test)]
    pub fn set_budget(&mut self, budget: Duration) {
        self.budget = budget;
    }

    pub fn close(&mut self) {
        self.stream = None;
        self.pos = 0;
        self.fill_len = 0;
        self.deadline = None;
    }

    pub fn open(&mut self) -> bool {
        self.close();
        let addrs = (self.host.as_str(), self.port)
            .to_socket_addrs()
            .ok()
            .and_then(|mut a| a.next());
        let Some(addr) = addrs else {
            ybdev::log::plog(&format!("conn open {}: resolve empty", self.host));
            return false;
        };
        match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            Ok(s) => {
                let _ = s.set_read_timeout(Some(REQUEST_TIMEOUT));
                let _ = s.set_write_timeout(Some(REQUEST_TIMEOUT));
                let _ = s.set_nodelay(true);
                self.stream = Some(s);
                self.pos = 0;
                self.fill_len = 0;
                true
            }
            Err(e) => {
                ybdev::log::plog(&format!(
                    "conn open {}:{} failed: {} kind={:?}",
                    self.host,
                    self.port,
                    e,
                    e.kind()
                ));
                false
            }
        }
    }

    /// One HTTP/1.1 request on the kept-alive socket, streaming the body to
    /// `sink` (return false from the sink to abort early). Returns the
    /// response, or the Stage where it broke (socket is then closed, so the
    /// next request reconnects cleanly).
    pub fn request(
        &mut self,
        method: &str,
        path: &str,
        sink: &mut dyn FnMut(&[u8]) -> bool,
    ) -> Result<Resp, Stage> {
        if self.stream.is_none()
            && !self.open() {
                return Err(Stage::Connect);
            }
        let req = match &self.secret {
            Some(sec) => format!(
                "{} {} HTTP/1.1\r\nHost: {}:{}\r\nX-YB-Secret: {}\r\nConnection: keep-alive\r\nContent-Length: 0\r\n\r\n",
                method, path, self.host, self.port, sec
            ),
            None => format!(
                "{} {} HTTP/1.1\r\nHost: {}:{}\r\nConnection: keep-alive\r\nContent-Length: 0\r\n\r\n",
                method, path, self.host, self.port
            ),
        };
        {
            let Some(s) = self.stream.as_mut() else {
                return Err(Stage::Connect);
            };
            if s.write_all(req.as_bytes()).is_err() {
                self.close();
                return Err(Stage::Send);
            }
        }
        // Receive phase starts here: arm the wall-clock budget enforced by
        // budget_gate() before every subsequent socket read.
        self.deadline = Some(std::time::Instant::now() + self.budget);

        let line = self.read_line().map_err(|e| {
            ybdev::log::plog(&format!(
                "{} {} status-line read failed: {} kind={:?}",
                method,
                path,
                e,
                e.kind()
            ));
            self.close();
            Stage::Status
        })?;
        // "HTTP/1.1 200 OK" -> 200: the code is the second whitespace
        // token (the Lua original captured it with (%d+); parsing the
        // tail ("200 OK") as u16 fails on the reason phrase).
        let status = line
            .trim()
            .strip_prefix("HTTP/")
            .and_then(|r| r.split_whitespace().nth(1))
            .and_then(|code| code.parse::<u16>().ok());
        let Some(status) = status else {
            ybdev::log::plog(&format!(
                "{} {} status line not HTTP: {:?}",
                method,
                path,
                &line.as_bytes()[..line.len().min(80)]
            ));
            self.close();
            return Err(Stage::Status);
        };

        let mut headers = HashMap::new();
        loop {
            let hline = self.read_line().map_err(|_| {
                self.close();
                Stage::Headers
            })?;
            let hline = hline.trim_end_matches('\r').trim();
            if hline.is_empty() {
                break;
            }
            if let Some((k, v)) = hline.split_once(':') {
                headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
            }
        }

        let clen: i64 = headers
            .get("content-length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(-1);
        if clen < 0 {
            self.close();
            return Err(Stage::Headers);
        }
        if clen > MAX_BODY_BYTES {
            // Reject before `as usize`: on 32-bit ARM a huge i64 truncates
            // and the body loop would under-read, leaving the keep-alive
            // stream desynced instead of failing cleanly.
            self.close();
            return Err(Stage::Headers);
        }
        if clen > 0 {
            let mut remaining = clen as usize;
            let mut chunk = vec![0u8; 16384];
            while remaining > 0 {
                // Drain any bytes already buffered from header reads first.
                if self.pos < self.buf_used() {
                    let avail = self.buf_used() - self.pos;
                    let n = remaining.min(avail).min(chunk.len());
                    let part = &self.buf[self.pos..self.pos + n];
                    if !sink(part) {
                        self.close();
                        return Err(Stage::Body);
                    }
                    remaining -= n;
                    self.pos += n;
                    continue;
                }
                let want = remaining.min(chunk.len());
                if self.budget_gate().is_err() {
                    self.close();
                    return Err(Stage::Body);
                }
                let Some(s) = self.stream.as_mut() else {
                    self.close();
                    return Err(Stage::Body);
                };
                match s.read(&mut chunk[..want]) {
                    Ok(0) => {
                        self.close();
                        return Err(Stage::Body);
                    }
                    Ok(n) => {
                        if !sink(&chunk[..n]) {
                            self.close();
                            return Err(Stage::Body);
                        }
                        remaining -= n;
                    }
                    Err(_) => {
                        self.close();
                        return Err(Stage::Body);
                    }
                }
            }
        }
        // Request completed cleanly: disarm so a later idle read (there
        // are none today, but keep-alive state must not inherit a stale
        // deadline) doesn't trip the gate.
        self.deadline = None;
        Ok(Resp { status, headers })
    }

    /// Bytes currently buffered past `pos` (leftover from header reads).
    fn buf_used(&self) -> usize {
        // After a fill, buf[0..filled] holds data; pos is the cursor.
        // We track fill length implicitly: anything beyond pos is stale
        // unless a fill happened. Keep it simple: fill sets fill_len.
        self.fill_len
    }

    fn next_byte(&mut self) -> std::io::Result<u8> {
        loop {
            if self.pos < self.fill_len {
                let b = self.buf[self.pos];
                self.pos += 1;
                return Ok(b);
            }
            self.refill()?;
        }
    }

    /// Enforce the request budget before a socket read: shrink the socket
    /// timeout to the remaining budget and fail once it is spent. The
    /// per-read REQUEST_TIMEOUT restarts on every received byte, so only
    /// this wall-clock gate bounds a trickle feed.
    fn budget_gate(&mut self) -> std::io::Result<()> {
        if let Some(d) = self.deadline {
            let now = std::time::Instant::now();
            if now >= d {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "request budget exhausted",
                ));
            }
            let Some(s) = self.stream.as_mut() else {
                return Ok(());
            };
            let _ = s.set_read_timeout(Some((d - now).min(REQUEST_TIMEOUT)));
        }
        Ok(())
    }

    fn refill(&mut self) -> std::io::Result<()> {
        self.budget_gate()?;
        let Some(s) = self.stream.as_mut() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "no connection",
            ));
        };
        let n = s.read(&mut self.buf)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "connection closed",
            ));
        }
        self.fill_len = n;
        self.pos = 0;
        Ok(())
    }

    fn read_line(&mut self) -> std::io::Result<String> {
        let mut out = String::new();
        loop {
            let b = self.next_byte()?;
            if b == b'\n' {
                return Ok(out);
            }
            if out.len() >= MAX_LINE_BYTES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "line too long",
                ));
            }
            out.push(b as char);
        }
    }
}

/// Broadcast a probe; the first ybmirror server to answer wins. The Mac's
/// IP is the reply's *source address*; its tcp port comes from the reply
/// text. Tries the limited broadcast first, then the local /24.
pub fn discover(timeout: Duration) -> Option<(String, u16)> {
    let mut targets = vec!["255.255.255.255".to_string()];
    // Best-effort local subnet broadcast, like the Lua version.
    if let Ok(s) = UdpSocket::bind("0.0.0.0:0") {
        if let Ok(addr) = "192.0.2.1:9".parse::<std::net::SocketAddr>() {
            if s.connect(addr).is_ok() {
                if let Ok(local) = s.local_addr() {
                    if local.ip().is_ipv4() {
                        let ip = local.ip().to_string();
                        let mut parts: Vec<&str> = ip.split('.').collect();
                        if let Some(last) = parts.last_mut() {
                            *last = "255";
                        }
                        let sub = parts.join(".");
                        if sub != "255.255.255.255" {
                            targets.push(sub);
                        }
                    }
                }
            }
        }
    }

    for bcast in targets {
        if let Ok(s) = UdpSocket::bind("0.0.0.0:0") {
            let _ = s.set_broadcast(true);
            let _ = s.set_read_timeout(Some(timeout));
            if s.send_to(b"ybmirror-discover", (bcast.as_str(), DISCOVER_PORT))
                .is_ok()
            {
                let mut buf = [0u8; 128];
                if let Ok((n, src)) = s.recv_from(&mut buf) {
                    let text = String::from_utf8_lossy(&buf[..n]);
                    let port = text
                        .strip_prefix("ybmirror")
                        .and_then(|rest| rest.split_whitespace().next())
                        .and_then(|p| p.parse::<u16>().ok())
                        .unwrap_or(DEFAULT_PORT);
                    return Some((src.ip().to_string(), port));
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serve one canned response on a loopback listener, hand the client a
    /// Conn pointed at it.
    fn conn_against(response: Vec<u8>) -> Conn {
        use std::io::Write as _;
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = std::io::Read::read(&mut s, &mut buf);
            s.write_all(&response).unwrap();
            // Hold the socket open briefly so the client sees EOF only
            // after processing, not a race with accept.
            std::thread::sleep(std::time::Duration::from_millis(50));
        });
        let mut c = Conn::new(&addr.ip().to_string(), addr.port());
        assert!(c.open());
        c
    }

    #[test]
    fn rejects_oversized_content_length() {
        let resp = b"HTTP/1.1 200 OK\r\nContent-Length: 999999999\r\n\r\n";
        let mut c = conn_against(resp.to_vec());
        let mut sink = |_: &[u8]| true;
        assert_eq!(
            c.request("GET", "/frame.png", &mut sink).err(),
            Some(Stage::Headers)
        );
    }

    #[test]
    fn rejects_unterminated_line_instead_of_growing_forever() {
        // No newline anywhere: read_line must give up at MAX_LINE_BYTES,
        // not buffer until the heap dies. The response is bigger than the
        // cap precisely so the old code would have tripped it.
        let resp = vec![b'a'; 64 * 1024];
        let mut c = conn_against(resp);
        let mut sink = |_: &[u8]| true;
        assert_eq!(c.request("GET", "/", &mut sink).err(), Some(Stage::Status));
    }

    #[test]
    fn happy_path_still_works() {
        let body = b"hello frame";
        let resp = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
        let mut resp = resp.into_bytes();
        resp.extend_from_slice(body);
        let mut c = conn_against(resp);
        let mut got = Vec::new();
        let mut sink = |chunk: &[u8]| {
            got.extend_from_slice(chunk);
            true
        };
        let r = c.request("GET", "/frame.png", &mut sink).unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(got, body);
    }

    #[test]
    fn secret_header_sent_when_configured_and_absent_when_not() {
        use std::io::Write as _;
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || {
            // Two sequential connections: configured client first, then
            // the unconfigured one.
            for _ in 0..2 {
                let (mut s, _) = l.accept().unwrap();
                let mut buf = vec![0u8; 2048];
                let n = std::io::Read::read(&mut s, &mut buf).unwrap_or(0);
                let _ = tx.send(String::from_utf8_lossy(&buf[..n]).to_string());
                let resp = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
                let _ = s.write_all(resp.as_bytes());
            }
        });
        let mut sink = |_: &[u8]| true;

        let mut with = Conn::new(&addr.ip().to_string(), addr.port());
        with.set_secret(Some("hunter2".into()));
        assert!(with.open());
        with.request("GET", "/frame", &mut sink).unwrap();

        let mut without = Conn::new(&addr.ip().to_string(), addr.port());
        assert!(without.open());
        without.request("GET", "/frame", &mut sink).unwrap();

        let r1 = rx.recv().unwrap();
        let r2 = rx.recv().unwrap();
        assert!(r1.contains("X-YB-Secret: hunter2"), "{r1}");
        assert!(!r2.contains("X-YB-Secret"), "{r2}");
    }

    #[test]
    fn request_budget_bounds_a_trickling_peer() {
        use std::io::Write as _;
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = std::io::Read::read(&mut s, &mut buf);
            // Drip the status line one byte at a time, each inside the
            // (shrunken) per-read timeout window: socket timeouts never
            // fire, the wall-clock budget must.
            for b in b"HTTP/1.1 200 OK\r\n" {
                s.write_all(&[*b]).unwrap();
                let _ = s.flush();
                std::thread::sleep(std::time::Duration::from_millis(40));
            }
            std::thread::sleep(std::time::Duration::from_millis(300));
        });
        let mut c = Conn::new(&addr.ip().to_string(), addr.port());
        c.set_budget(std::time::Duration::from_millis(150));
        assert!(c.open());
        let mut sink = |_: &[u8]| true;
        assert_eq!(
            c.request("GET", "/frame.png", &mut sink).err(),
            Some(Stage::Status)
        );
    }
}
