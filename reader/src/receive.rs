//! Receive over Wi-Fi — the Kindle becomes the server. A hand-rolled HTTP
//! listener (same "one static binary, no heavy deps" rule as protocol.rs's
//! client) serves a drag-drop page and streams raw POST bodies straight to
//! disk — never buffered in RAM. The screen shows a QR of the URL; any
//! phone/laptop browser on the LAN is then the client, which deletes the
//! whole "Kindle must find the Mac" discovery dance fetch.rs carries.
//!
//! Delivery semantics match fetch.rs: a file counts as received only after
//! the full body is written, fsynced and renamed into documents/.

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use qrcode::{Color, QrCode};
use ybdev::config::{sanitize_fetch_name, urldecode};
use ybdev::input::Gesture;
use ybdev::log::{now_ms, plog};

use crate::wifi;
use yui::painter::{pt, Painter, Rect};
use yui::screen::{Action, Screen};

/// Overridable so the host-side test can aim the handler at a temp dir.
fn save_dir() -> String {
    std::env::var("YB_SAVE_DIR").unwrap_or_else(|_| "/mnt/us/documents".to_string())
}

/// Same allowlist as books::list_books: anything else would land in
/// documents/ but never appear in the library.
const OK_EXTS: [&str; 7] = ["epub", "pdf", "mobi", "azw3", "fb2", "txt", "cbz"];
const MAX_BODY: u64 = 512 * 1024 * 1024;
const HDR_CAP: usize = 16 * 1024;
const PORT: u16 = 8080;
const IPTABLES: &str = "/usr/sbin/iptables";

/// The page IS the client: drag-drop / file picker that POSTs the raw File
/// body (no multipart at all — the name rides in the query string).
const PAGE: &str = r#"<!doctype html><meta charset=utf-8>
<meta name=viewport content="width=device-width,initial-scale=1">
<title>Send to yb-reader</title>
<style>
body{font-family:system-ui,sans-serif;max-width:34em;margin:2em auto;padding:0 1em;color:#111}
h1{font-size:1.3em}
#d{border:2px dashed #999;border-radius:14px;padding:3em 1em;text-align:center;margin:1.5em 0;color:#555}
#d.ok{border-color:#060;color:#060}
button{font-size:1.1em;padding:.55em 1.3em}
#s{min-height:1.4em;color:#060;white-space:pre-wrap}
</style>
<h1>yb-reader &mdash; send books</h1>
<div id=d>Drop files here<br><br>EPUB &middot; PDF &middot; MOBI &middot; AZW3 &middot; FB2 &middot; TXT &middot; CBZ</div>
<button onclick=f.click()>Choose files&hellip;</button>
<input id=f type=file multiple hidden>
<pre id=s></pre>
<script>
const d=document.getElementById('d'),f=document.getElementById('f'),s=document.getElementById('s');
document.addEventListener('dragover',e=>e.preventDefault());
document.addEventListener('drop',e=>e.preventDefault());
d.addEventListener('drop',e=>{e.preventDefault();go(e.dataTransfer.files)});
d.addEventListener('dragover',()=>d.classList.add('ok'));
d.addEventListener('dragleave',()=>d.classList.remove('ok'));
f.onchange=()=>{go(f.files);f.value=''};
async function go(files){
 for(const file of files){
  s.textContent='Sending '+file.name+'…';
  try{
   const r=await fetch('/upload?name='+encodeURIComponent(file.name),{method:'POST',body:file});
   s.textContent=file.name+': '+(await r.text());
  }catch(e){s.textContent=file.name+': failed ('+e+')';}
 }
}
</script>"#;

// ---- server -------------------------------------------------------------

pub struct ReceiveServer {
    port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    /// "name (x.x MB)" of the last completed delivery, for the screen.
    last: Arc<Mutex<Option<String>>>,
    received: Arc<AtomicUsize>,
}

impl ReceiveServer {
    /// Bind 8080 when free (friendly for repeat visits), else an ephemeral
    /// port — the QR always carries the real one. `stop` is shared with the
    /// screen so shutdown works even while setup is still running.
    fn start(stop: Arc<AtomicBool>) -> Option<ReceiveServer> {
        let listener = TcpListener::bind(("0.0.0.0", PORT))
            .or_else(|_| TcpListener::bind(("0.0.0.0", 0)))
            .ok()?;
        let port = listener.local_addr().ok()?.port();
        listener.set_nonblocking(true).ok()?;
        firewall_port(port, true);

        let last = Arc::new(Mutex::new(None));
        let received = Arc::new(AtomicUsize::new(0));
        let last_t = Arc::clone(&last);
        let recv_t = Arc::clone(&received);
        let stop_t = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            while !stop_t.load(Ordering::Relaxed) {
                match listener.accept() {
                    // One connection at a time: this is a single-user drop
                    // zone, and sequential handling bounds RAM on a device
                    // with ~150 MB free.
                    Ok((stream, _)) => {
                        // accept() inherits the listener's O_NONBLOCK on
                        // Linux: without this, a read that races the
                        // client's first packet returns WouldBlock, which
                        // the header loop treats as a dead client — the
                        // connection is closed with no response. That was
                        // the host-test flake, and it drops real uploads
                        // whenever Wi-Fi latency lets accept() beat the
                        // request bytes.
                        let _ = stream.set_nonblocking(false);
                        handle_conn(stream, &last_t, &recv_t);
                    }
                    Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(200));
                    }
                    Err(e) => {
                        // A dead listener with no trace is undebuggable on a
                        // headless device — leave the reason in the log.
                        plog(&format!("receive: accept failed, listener stopping: {}", e));
                        break;
                    }
                }
            }
            plog("receive: listener stopped");
        });

        Some(ReceiveServer {
            port,
            stop,
            handle: Some(handle),
            last,
            received,
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn status(&self) -> (usize, Option<String>) {
        (
            self.received.load(Ordering::Relaxed),
            self.last.lock().unwrap().clone(),
        )
    }

    /// Flag the accept loop down and detach it: joining could block for the
    /// length of an in-flight upload, and Pop must never hang the UI. The
    /// loop polls the flag every 200ms, so the listener frees itself soon
    /// after it goes idle.
    fn shutdown(&mut self) {
        firewall_port(self.port, false);
        self.stop.store(true, Ordering::Relaxed);
        self.handle.take();
    }
}

/// The lab126 firewall is default-DROP on INPUT (`iptables -P INPUT DROP`,
/// only ESTABLISHED flows and a few lab126 service ports pass) — a fresh
/// listener is unreachable no matter how correctly it binds: the inbound
/// SYN is silently dropped and clients just time out. Verified on-device:
/// curl to a bound :8080 timed out until this rule existed, then answered
/// 200. Scoped like the listener itself — open on start, closed on
/// teardown. The reader runs as root, so this is permitted; the rule is
/// also idempotent (-C before -I) and vanishes on reboot anyway.
fn firewall_port(port: u16, open: bool) {
    let port_str = port.to_string();
    let spec = [
        "-i".as_ref(),
        "wlan0".as_ref(),
        "-p",
        "tcp",
        "--dport",
        port_str.as_str(),
        "-j",
        "ACCEPT",
    ];
    if open {
        let exists = Command::new(IPTABLES)
            .args(["-C", "INPUT"])
            .args(spec)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !exists {
            let ok = Command::new(IPTABLES)
                .args(["-I", "INPUT", "1"])
                .args(spec)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !ok {
                plog("receive: could not open firewall port — uploads will time out");
            }
        }
    } else {
        // Tolerate an absent rule (teardown after a reboot resets).
        let _ = Command::new(IPTABLES)
            .args(["-D", "INPUT"])
            .args(spec)
            .status();
    }
}

/// The interface IP the default route would use: connect() on a UDP socket
/// only selects the route (nothing is sent), then local_addr() reports it.
/// No /proc parsing, no busybox tools.
fn local_ip() -> Option<String> {
    let s = UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    s.connect(("8.8.8.8", 53)).ok()?;
    Some(s.local_addr().ok()?.ip().to_string())
}

/// Last-ditch cleanup for the emergency exit path (panic / terminating
/// signal): drop the firewall rule and any half-written upload, so a dead
/// process leaves the network closed and documents/ clean.
pub fn emergency_cleanup() {
    firewall_port(PORT, false);
    if let Ok(rd) = std::fs::read_dir(save_dir()) {
        for e in rd.flatten() {
            if e.path().extension().and_then(|x| x.to_str()) == Some("part") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

fn handle_conn(mut stream: TcpStream, last: &Arc<Mutex<Option<String>>>, received: &Arc<AtomicUsize>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));

    // Accumulate until the blank line; the tail of the buffer (if any) is
    // the start of the body — browsers may send both in one packet.
    let mut buf: Vec<u8> = Vec::with_capacity(2048);
    let hdr_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if buf.len() > HDR_CAP {
            respond(&mut stream, 431, "Request Header Fields Too Large", "text/plain", "too large");
            return;
        }
        let mut chunk = [0u8; 4096];
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return, // dead client mid-headers
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    };

    let head = String::from_utf8_lossy(&buf[..hdr_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut parts = lines.next().unwrap_or("").split(' ');
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");

    let mut content_length: Option<u64> = None;
    let mut expect_continue = false;
    for line in lines {
        let Some((k, v)) = line.split_once(':') else { continue };
        let k = k.trim().to_ascii_lowercase();
        if k == "content-length" {
            content_length = v.trim().parse().ok();
        } else if k == "expect" && v.trim().eq_ignore_ascii_case("100-continue") {
            expect_continue = true; // curl sends this on large bodies
        }
    }

    if method != "POST" {
        respond(&mut stream, 200, "OK", "text/html; charset=utf-8", PAGE);
        return;
    }
    if expect_continue {
        let _ = stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n");
    }

    let Some(len) = content_length else {
        respond(&mut stream, 411, "Length Required", "text/plain", "Content-Length required");
        return;
    };
    if len > MAX_BODY {
        respond(&mut stream, 413, "Payload Too Large", "text/plain", "too large");
        return;
    }

    let name = path
        .split('?')
        .nth(1)
        .and_then(|q| q.split('&').find(|p| p.starts_with("name=")))
        .map(|p| urldecode(&p[5..]))
        .and_then(|n| sanitize_fetch_name(&n));
    let Some(name) = name else {
        respond(&mut stream, 400, "Bad Request", "text/plain", "bad file name");
        return;
    };
    let ext_ok = name
        .rsplit('.')
        .next()
        .map(|e| OK_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false);
    if !ext_ok || name.len() > 200 {
        respond(&mut stream, 415, "Unsupported Media Type", "text/plain", "unsupported file type");
        return;
    }

    let dir = save_dir();
    let final_path = format!("{}/{}", dir, name);
    let part_path = format!("{}.part", final_path);
    let body_prefix = buf[hdr_end + 4..].to_vec();
    let t0 = now_ms();

    match write_body(&mut stream, &body_prefix, len, &part_path, &final_path) {
        Ok(()) => {
            let msg = format!("{} ({:.1} MB)", name, len as f64 / 1048576.0);
            *last.lock().unwrap() = Some(msg.clone());
            received.fetch_add(1, Ordering::Relaxed);
            plog(&format!(
                "receive: saved {} in {:.1}s",
                msg,
                (now_ms() - t0) as f64 / 1000.0
            ));
            respond(&mut stream, 200, "OK", "text/plain", &format!("saved: {}\n", msg));
        }
        Err(e) => {
            // A failed delivery leaves nothing behind.
            let _ = std::fs::remove_file(&part_path);
            plog(&format!("receive: FAILED {} — {}", name, e));
            respond(&mut stream, 500, "Server Error", "text/plain", &format!("failed: {}", e));
        }
    }
}

/// Socket → disk in 64 KB chunks: the file is never held in RAM (the device
/// has ~150 MB free; a big PDF must not OOM it). Counts as delivered only
/// after fsync + rename, exactly like fetch.rs's .part discipline.
fn write_body(
    stream: &mut TcpStream,
    prefix: &[u8],
    len: u64,
    part: &str,
    final_path: &str,
) -> Result<(), String> {
    let mut out = std::fs::File::create(part).map_err(|e| format!("create: {}", e))?;
    let mut remaining = len;
    let mut pos = 0usize;
    while remaining > 0 {
        let take = remaining.min((prefix.len() - pos) as u64) as usize;
        if take > 0 {
            out.write_all(&prefix[pos..pos + take])
                .map_err(|e| format!("write: {}", e))?;
            pos += take;
            remaining -= take as u64;
            continue;
        }
        let mut chunk = [0u8; 65536];
        let n = stream.read(&mut chunk).map_err(|e| format!("read: {}", e))?;
        if n == 0 {
            return Err("truncated upload".into());
        }
        out.write_all(&chunk[..n]).map_err(|e| format!("write: {}", e))?;
        remaining -= n as u64;
    }
    out.sync_all().map_err(|e| format!("fsync: {}", e))?;
    drop(out);
    std::fs::rename(part, final_path).map_err(|e| format!("rename: {}", e))?;
    Ok(())
}

fn respond(stream: &mut TcpStream, code: u16, reason: &str, ctype: &str, body: &str) {
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        code,
        reason,
        ctype,
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
}

// ---- screen ---------------------------------------------------------------

enum Phase {
    /// Wi-Fi + listener setup runs on a thread so the panel still paints.
    Starting,
    Ready { url: String },
    Failed(String),
}

pub struct ReceiveScreen {
    phase: Phase,
    stop: Arc<AtomicBool>,
    /// Filled by the setup thread, taken by on_tick.
    setup: Arc<Mutex<Option<Result<(ReceiveServer, String), String>>>>,
    server: Option<ReceiveServer>,
    qr: Option<QrCode>,
    seen_count: usize,
    dims: (i32, i32),
}

impl ReceiveScreen {
    pub fn new() -> ReceiveScreen {
        let stop = Arc::new(AtomicBool::new(false));
        ReceiveScreen {
            phase: Phase::Starting,
            stop: Arc::clone(&stop),
            setup: Arc::new(Mutex::new(None)),
            server: None,
            qr: None,
            seen_count: 0,
            dims: (1236, 1648),
        }
    }

    /// Wi-Fi bring-up can take ~20s on a cold radio; doing it on the UI
    /// thread would freeze the panel mid-Pop. The thread parks its result
    /// in `setup` and on_tick promotes it.
    fn start_setup(&mut self) {
        self.stop.store(false, Ordering::Relaxed);
        self.phase = Phase::Starting;
        *self.setup.lock().unwrap() = None;
        let stop = Arc::clone(&self.stop);
        let slot = Arc::clone(&self.setup);
        std::thread::spawn(move || {
            let r = (|| -> Result<(ReceiveServer, String), String> {
                wifi::ensure_wifi();
                if !wifi::wait_for_wifi(Duration::from_secs(10)) {
                    return Err("Wi-Fi not connected".into());
                }
                let Some(srv) = ReceiveServer::start(Arc::clone(&stop)) else {
                    return Err("cannot bind listener".into());
                };
                // The route can lag association by a moment.
                let mut ip = local_ip();
                for _ in 0..10 {
                    if ip.is_some() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(300));
                    ip = local_ip();
                }
                let url = format!(
                    "http://{}:{}/",
                    ip.unwrap_or_else(|| "127.0.0.1".into()),
                    srv.port()
                );
                Ok((srv, url))
            })();
            *slot.lock().unwrap() = Some(r);
        });
    }
}

impl Screen for ReceiveScreen {
    fn default_edges(&self) -> bool {
        false
    }

    fn on_enter(&mut self) -> Action {
        wifi::keep_awake(true);
        self.start_setup();
        Action::RedrawFull
    }

    fn on_leave(&mut self) {
        wifi::keep_awake(false);
        self.stop.store(true, Ordering::Relaxed);
        if let Some(s) = &mut self.server {
            s.shutdown();
        }
    }

    fn tick_interval(&self) -> Duration {
        Duration::from_millis(300)
    }

    fn on_tick(&mut self) -> Action {
        if matches!(self.phase, Phase::Starting) {
            match self.setup.lock().unwrap().take() {
                Some(Ok((srv, url))) => {
                    self.qr = QrCode::new(url.as_bytes()).ok();
                    plog(&format!("receive: listening on {}", url));
                    self.phase = Phase::Ready { url };
                    self.server = Some(srv);
                    Action::RedrawFull
                }
                Some(Err(e)) => {
                    self.phase = Phase::Failed(e);
                    Action::RedrawFull
                }
                None => Action::Keep,
            }
        } else if let Some(srv) = &self.server {
            let (n, _) = srv.status();
            if n != self.seen_count {
                self.seen_count = n;
                return Action::Redraw;
            }
            Action::Keep
        } else {
            Action::Keep
        }
    }

    fn draw(&mut self, p: &mut Painter) {
        let (w, h) = p.size();
        self.dims = (w, h);
        p.clear(255);

        let bar_h = pt(40.0);
        p.rect(Rect::new(0, 0, w, bar_h), 245);
        p.hline_t(bar_h, 0, w, 1, 200);
        p.text(pt(16.0), pt(24.0), 10.5, 0, "Receive over Wi-Fi");

        match &self.phase {
            Phase::Starting => {
                p.text_center(h / 2, 11.0, 0, "Turning on Wi-Fi…");
                p.text_center(h / 2 + pt(18.0), 8.5, 130, "the address will appear here");
            }
            Phase::Failed(e) => {
                p.text_center(h / 2, 11.0, 0, e);
                p.text_center(h / 2 + pt(18.0), 9.0, 0, "tap to retry");
            }
            Phase::Ready { url } => {
                // QR: e-ink's ideal payload — static, pure black/white.
                // 4-module quiet zone, scaled to fit, drawn once.
                let top = bar_h + pt(24.0);
                let target = (w * 2 / 5).min(h - top - pt(230.0));
                if let Some(qr) = &self.qr {
                    let qw = qr.width() as i32;
                    let total = qw + 8;
                    let scale = (target / total).max(2);
                    let size = total * scale;
                    let x0 = (w - size) / 2;
                    p.rect(Rect::new(x0, top, size, size), 255);
                    for my in 0..qw {
                        for mx in 0..qw {
                            if qr[(mx as usize, my as usize)] == Color::Dark {
                                p.rect(
                                    Rect::new(
                                        x0 + (mx + 4) * scale,
                                        top + (my + 4) * scale,
                                        scale,
                                        scale,
                                    ),
                                    0,
                                );
                            }
                        }
                    }

                    let ty = top + size + pt(34.0);
                    p.text_center(ty, 11.0, 0, url);
                    p.text_center(ty + pt(20.0), 8.5, 130,
                        "scan, or open the address in any browser —");
                    p.text_center(ty + pt(34.0), 8.5, 130,
                        "drop books onto the page to send them here");

                    let (n, last) = self.server.as_ref().map(|s| s.status()).unwrap_or((0, None));
                    let sy = ty + pt(62.0);
                    if n > 0 {
                        let line = format!("received {} — last: {}", n, last.unwrap_or_default());
                        let trunc = p.truncate(9.0, &line, p.width_pt() - 20.0);
                        p.text_center(sy, 9.0, 0, &trunc);
                    } else {
                        p.text_center(sy, 9.0, 130, "waiting for files…");
                    }
                } else {
                    p.text_center(h / 2, 11.0, 0, url);
                }
            }
        }

        p.text_center(h - pt(12.0), 8.0, 130, "tap anywhere to close");
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        match g {
            Gesture::Tap { .. } | Gesture::TwoFingerTap => {
                if matches!(self.phase, Phase::Failed(_)) {
                    self.start_setup();
                    Action::RedrawFull
                } else {
                    Action::Pop
                }
            }
            Gesture::Swipe { .. } => Action::Pop,
            _ => Action::Keep,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_roundtrip_and_extension_guard() {
        let dir = std::env::temp_dir().join("yb-receive-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("YB_SAVE_DIR", dir.to_str().unwrap());

        let stop = Arc::new(AtomicBool::new(false));
        let mut srv = ReceiveServer::start(Arc::clone(&stop)).expect("server");
        let port = srv.port();

        // Good upload: raw body, query-encoded name with a space, split
        // across two writes (headers first, body after).
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let body = b"fake epub bytes";
        let req = format!(
            "POST /upload?name=test%20book.epub HTTP/1.1\r\nHost: t\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        c.write_all(req.as_bytes()).unwrap();
        c.write_all(body).unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(
            resp.starts_with(b"HTTP/1.1 200"),
            "{}",
            String::from_utf8_lossy(&resp)
        );
        assert_eq!(std::fs::read(dir.join("test book.epub")).unwrap(), body);
        assert!(!dir.join("test book.epub.part").exists());

        // Headers + body in ONE packet must work too (prefix path), and a
        // disallowed extension must be refused without leaving a file.
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(b"POST /upload?name=evil.sh HTTP/1.1\r\nHost: t\r\nContent-Length: 2\r\n\r\nhi")
            .unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(resp.starts_with(b"HTTP/1.1 415"));
        assert!(!dir.join("evil.sh").exists());

        // Path traversal is neutralized by sanitize_fetch_name: the file
        // lands under its basename inside the save dir, never outside it.
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(b"POST /upload?name=..%2F..%2Fescape.epub HTTP/1.1\r\nHost: t\r\nContent-Length: 1\r\n\r\nx")
            .unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(resp.starts_with(b"HTTP/1.1 200"), "{}", String::from_utf8_lossy(&resp));
        assert_eq!(std::fs::read(dir.join("escape.epub")).unwrap(), b"x");
        assert!(!dir.parent().unwrap().join("escape.epub").exists());

        // GET serves the upload page.
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(b"GET / HTTP/1.1\r\nHost: t\r\n\r\n").unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(resp.starts_with(b"HTTP/1.1 200"));
        assert!(resp.windows(9).any(|w| w == b"text/html"));

        // Regression for the O_NONBLOCK race: a client that connects and
        // only sends its request a beat later must NOT be treated as dead.
        // Before the set_nonblocking(false) fix, the server's first read
        // returned WouldBlock and closed the connection with no response.
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        c.write_all(b"GET / HTTP/1.1\r\nHost: t\r\n\r\n").unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(
            resp.starts_with(b"HTTP/1.1 200"),
            "delayed-request client was dropped: {}",
            String::from_utf8_lossy(&resp)
        );

        let (n, last) = srv.status();
        assert_eq!(n, 2);
        assert!(last.unwrap().contains("escape.epub"));

        srv.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
