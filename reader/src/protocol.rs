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
    /// Optional Kindle identity sent as `X-YB-Kindle-Id`.
    kindle_id: Option<String>,
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
            kindle_id: None,
        }
    }

    /// Attach the shared credential (mirror.conf `SECRET=`); every
    /// subsequent request carries it as an `X-YB-Secret` header. The
    /// server opts in by requiring that header — see SECURITY.md.
    pub fn set_secret(&mut self, secret: Option<String>) {
        self.secret = secret;
    }

    /// Attach the Kindle device identifier; sent as `X-YB-Kindle-Id` on requests.
    pub fn set_kindle_id(&mut self, kindle_id: Option<String>) {
        self.kindle_id = kindle_id;
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
        let mut req = format!(
            "{} {} HTTP/1.1\r\nHost: {}:{}\r\nConnection: keep-alive\r\nContent-Length: 0\r\n",
            method, path, self.host, self.port
        );
        if let Some(sec) = &self.secret {
            req.push_str(&format!("X-YB-Secret: {}\r\n", sec));
        }
        if let Some(kid) = &self.kindle_id {
            req.push_str(&format!("X-YB-Kindle-Id: {}\r\n", kid));
        }
        req.push_str("\r\n");
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredServer {
    pub ip: String,
    pub port: u16,
    pub device_id: Option<String>,
    pub device_name: Option<String>,
}

pub fn parse_discovery_response(src_ip: &str, text: &str) -> Option<DiscoveredServer> {
    let rest = text.strip_prefix("ybmirror")?.trim_start();
    let (port_str, rest) = match rest.split_once(char::is_whitespace) {
        Some((p, r)) => (p, r.trim_start()),
        None => (rest, ""),
    };
    let port = port_str.parse::<u16>().ok()?;

    let mut device_id = None;
    let mut device_name = None;

    let mut s = rest;
    while !s.is_empty() {
        if let Some(tail) = s.strip_prefix("id=") {
            let (val, next) = parse_quoted_or_token(tail);
            if !val.is_empty() {
                device_id = Some(val);
            }
            s = next.trim_start();
        } else if let Some(tail) = s.strip_prefix("name=") {
            let (val, next) = parse_quoted_or_token(tail);
            if !val.is_empty() {
                device_name = Some(val);
            }
            s = next.trim_start();
        } else {
            // Skip unrecognized token until whitespace
            s = match s.split_once(char::is_whitespace) {
                Some((_, r)) => r.trim_start(),
                None => "",
            };
        }
    }

    Some(DiscoveredServer {
        ip: src_ip.to_string(),
        port,
        device_id,
        device_name,
    })
}

fn parse_quoted_or_token(s: &str) -> (String, &str) {
    if let Some(stripped) = s.strip_prefix('"') {
        if let Some(end_idx) = stripped.find('"') {
            return (stripped[..end_idx].to_string(), &stripped[end_idx + 1..]);
        }
    }
    match s.split_once(char::is_whitespace) {
        Some((tok, rest)) => (tok.trim_matches('"').to_string(), rest),
        None => (s.trim_matches('"').to_string(), ""),
    }
}

/// Merge a discovered server response into the list of known servers.
/// If an entry for (ip, port) already exists, fill in device_id / name when
/// the new reply carries them and the existing entry lacks them.
pub fn merge_reply(servers: &mut Vec<DiscoveredServer>, srv: DiscoveredServer) -> bool {
    if let Some(existing) = servers.iter_mut().find(|e| e.ip == srv.ip && e.port == srv.port) {
        let mut merged = false;
        if existing.device_id.is_none() && srv.device_id.is_some() {
            existing.device_id = srv.device_id;
            merged = true;
        }
        if existing.device_name.is_none() && srv.device_name.is_some() {
            existing.device_name = srv.device_name;
            merged = true;
        }
        merged
    } else {
        servers.push(srv);
        true
    }
}

/// Broadcast discovery probes and return all servers that reply within timeout.
pub fn discover_all(timeout: Duration, kindle_id: Option<&str>) -> Vec<DiscoveredServer> {
    let mut servers: Vec<DiscoveredServer> = Vec::new();
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

    let p1 = b"ybmirror-discover".to_vec();
    let p2 = match kindle_id {
        Some(id) if !id.trim().is_empty() => format!("ybmirror-discover id={}", id.trim()).into_bytes(),
        _ => Vec::new(),
    };
    let payloads: Vec<&[u8]> = if p2.is_empty() {
        vec![&p1]
    } else {
        vec![&p1, &p2]
    };

    for bcast in targets {
        if let Ok(s) = UdpSocket::bind("0.0.0.0:0") {
            let _ = s.set_broadcast(true);
            let _ = s.set_read_timeout(Some(timeout));
            for payload in &payloads {
                let _ = s.send_to(payload, (bcast.as_str(), DISCOVER_PORT));
            }
            let mut buf = [0u8; 256];
            while let Ok((n, src)) = s.recv_from(&mut buf) {
                let text = String::from_utf8_lossy(&buf[..n]);
                if let Some(srv) = parse_discovery_response(&src.ip().to_string(), &text) {
                    merge_reply(&mut servers, srv);
                }
            }
        }
    }

    servers
}

/// Decide whether a discovery reply has earned a stored control token.
///
/// Trust requires BOTH the reply's UDP source IP to resolve to a registered
/// control-scope device AND the reply's claimed device_id to match that
/// stored record. Neither alone is enough:
///   - an IP is not an identity — DHCP hands a lapsed lease to the next
///     host, so a bare legacy reply (`ybmirror 8765`, no `id=`) from a
///     trusted IP gets NO token;
///   - a claimed id is not proof — any LAN host can type `id=<victim>`.
/// Only the pairing channel (PIN-verified /api/pair) mints trust.
pub fn trust_reply(srv: &DiscoveredServer, store: &ybdev::devices::DeviceStore) -> Option<String> {
    let dev = store.find_by_ip_for_control(&srv.ip)?;
    match &srv.device_id {
        Some(claimed) if &dev.id != claimed => None, // impersonator or reinstalled host
        Some(_) => Some(dev.token.clone()),
        None => None, // legacy id-less reply: IP alone is not identity
    }
}

/// Discovers servers and resolves the target's credential from DeviceStore.
/// Cross-checks the reply IP and device_id against registered control
/// devices to prevent LAN hosts from harvesting tokens.
pub fn discover_trusted(
    timeout: Duration,
    kindle_id: Option<&str>,
    store: &ybdev::devices::DeviceStore,
) -> Option<(DiscoveredServer, Option<String>)> {
    let servers = discover_all(timeout, kindle_id);
    if servers.is_empty() {
        return None;
    }

    // 1. Attach a stored token only to replies that pass the trust check
    if !store.devices.is_empty() {
        for s in &servers {
            if let Some(token) = trust_reply(s, store) {
                return Some((s.clone(), Some(token)));
            }
        }
        // Diagnosability, not a trust grant: a reply claiming a registered
        // control id from an unrecognized IP is the DHCP-move signature.
        // Without this, a lease reshuffle silently downgrades the device to
        // unauthenticated with no on-device hint.
        if let Some(s) = servers.iter().find(|s| {
            s.device_id
                .as_ref()
                .is_some_and(|id| store.find_by_id_for_control(id).is_some())
        }) {
            static IP_MISMATCH_PLOGGED: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            if !IP_MISMATCH_PLOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                ybdev::log::plog(&format!(
                    "mirror: trusted device '{}' answered from new IP {} — open the receive page once from that device (or re-pair) to refresh its IP",
                    s.device_id.as_deref().unwrap_or("?"),
                    s.ip
                ));
            }
        }
    }

    // 2. Fallback to first discovered server (unauthenticated)
    let first = servers.first()?.clone();
    Some((first, None))
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

    #[test]
    fn parses_discovery_replies() {
        // Pure parser test — no UDP: broadcasting in a unit test both spams
        // the LAN and flips this assertion to flaky on any machine that
        // happens to run a yb-mirror responder. Trust routing is covered by
        // trust_reply_requires_ip_and_id_match below.
        let raw1 = "ybmirror 8765 id=mac_m3 name=\"MacBook\"";
        let parsed1 = parse_discovery_response("192.168.1.10", raw1).unwrap();
        assert_eq!(parsed1.ip, "192.168.1.10");
        assert_eq!(parsed1.port, 8765);
        assert_eq!(parsed1.device_id.as_deref(), Some("mac_m3"));
        assert_eq!(parsed1.device_name.as_deref(), Some("MacBook"));

        let raw2 = "ybmirror 9000";
        let parsed2 = parse_discovery_response("192.168.1.20", raw2).unwrap();
        assert_eq!(parsed2.ip, "192.168.1.20");
        assert_eq!(parsed2.port, 9000);
        assert_eq!(parsed2.device_id, None);

        // Malformed port is rejected, not defaulted.
        assert!(parse_discovery_response("192.168.1.30", "ybmirror notaport").is_none());
        // Missing the protocol prefix is rejected.
        assert!(parse_discovery_response("192.168.1.30", "8765").is_none());
    }

    #[test]
    fn packet_order_discovery_merge() {
        let legacy_reply = "ybmirror 8765";
        let new_reply = "ybmirror 8765 id=mac_studio name=\"Studio Mac\"";

        let mut servers: Vec<DiscoveredServer> = Vec::new();

        // 1. Legacy reply arrives first
        let srv1 = parse_discovery_response("192.168.1.50", legacy_reply).unwrap();
        assert_eq!(srv1.device_id, None);
        assert!(merge_reply(&mut servers, srv1));

        // 2. New id-bearing reply arrives second: merge_reply upgrades the existing entry
        let srv2 = parse_discovery_response("192.168.1.50", new_reply).unwrap();
        assert_eq!(srv2.device_id.as_deref(), Some("mac_studio"));
        assert!(merge_reply(&mut servers, srv2));

        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].device_id.as_deref(), Some("mac_studio"));
        assert_eq!(servers[0].device_name.as_deref(), Some("Studio Mac"));

        // 3. A later name-only reply still fills a missing name (id already present)
        let mut named: Vec<DiscoveredServer> = vec![DiscoveredServer {
            ip: "192.168.1.60".to_string(),
            port: 8765,
            device_id: Some("mac_air".to_string()),
            device_name: None,
        }];
        let name_only = parse_discovery_response("192.168.1.60", "ybmirror 8765 name=\"Mac Air\"").unwrap();
        assert!(merge_reply(&mut named, name_only));
        assert_eq!(named[0].device_name.as_deref(), Some("Mac Air"));
        assert_eq!(named[0].device_id.as_deref(), Some("mac_air"));
    }

    #[test]
    fn trust_reply_requires_ip_and_id_match() {
        let mut store = ybdev::devices::DeviceStore::default();
        store.add_or_update(ybdev::devices::TrustedDevice::new(
            "victim_mac",
            "Victim's Mac",
            "tok_victim_secret",
            Some("192.168.1.100"),
            "all",
        ));
        store.add_or_update(ybdev::devices::TrustedDevice::new(
            "phone_inbound",
            "Phone",
            "tok_phone",
            Some("192.168.1.101"),
            "inbound",
        ));

        // Attacker at another IP claiming the victim's id: no token.
        let spoofed_id = DiscoveredServer {
            ip: "192.168.1.200".to_string(),
            port: 8765,
            device_id: Some("victim_mac".to_string()),
            device_name: None,
        };
        assert_eq!(trust_reply(&spoofed_id, &store), None);

        // Legacy id-less reply from the victim's (or a reassigned) IP: IP
        // alone is not an identity — no token.
        let legacy_at_trusted_ip = DiscoveredServer {
            ip: "192.168.1.100".to_string(),
            port: 8765,
            device_id: None,
            device_name: None,
        };
        assert_eq!(trust_reply(&legacy_at_trusted_ip, &store), None);

        // Right IP with the WRONG id (reinstalled host / impersonator): no token.
        let wrong_id = DiscoveredServer {
            ip: "192.168.1.100".to_string(),
            port: 8765,
            device_id: Some("someone_else".to_string()),
            device_name: None,
        };
        assert_eq!(trust_reply(&wrong_id, &store), None);

        // Inbound-scope device claiming control at its own IP with its own id: no token.
        let inbound = DiscoveredServer {
            ip: "192.168.1.101".to_string(),
            port: 8765,
            device_id: Some("phone_inbound".to_string()),
            device_name: None,
        };
        assert_eq!(trust_reply(&inbound, &store), None);

        // IP + id match on a control-scope device: token attached.
        let legit = DiscoveredServer {
            ip: "192.168.1.100".to_string(),
            port: 8765,
            device_id: Some("victim_mac".to_string()),
            device_name: Some("Victim's Mac".to_string()),
        };
        assert_eq!(trust_reply(&legit, &store), Some("tok_victim_secret".to_string()));
    }
}
