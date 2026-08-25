//! Receive over Wi-Fi — the Kindle becomes the server. A hand-rolled HTTP
//! listener (same "one static binary, no heavy deps" rule as protocol.rs's
//! client) serves a drag-drop page and streams raw POST bodies straight to
//! disk — never buffered in RAM. The screen shows a QR of the URL; any
//! phone/laptop browser on the LAN is then the client, which deletes the
//! whole "Kindle must find the Mac" discovery dance fetch.rs carries.
//!
//! Delivery semantics match fetch.rs: a file counts as received only after
//! the full body is written, fsynced and renamed into documents/.

use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::os::unix::io::FromRawFd;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use qrcode::{Color, QrCode};
use ybdev::config::{sanitize_fetch_name, urldecode};
use ybdev::devices::{DeviceStore, KindleProfile};
use ybdev::input::Gesture;
use ybdev::log::{now_ms, plog};

use crate::wifi;
use yui::painter::{pt, Painter, Rect};
use yui::screen::{Action, Screen};

/// Overridable so the host-side test can aim the handler at a temp dir.
fn save_dir() -> String {
    std::env::var("YB_SAVE_DIR").unwrap_or_else(|_| "/mnt/us/documents".to_string())
}

/// Same allowlist as library::list_books: anything else would land in
/// documents/ but never appear in the library. (mobi/azw3 are refused —
/// there is no parser for them; converting to epub is the supported path.)
const OK_EXTS: [&str; 5] = ["epub", "pdf", "fb2", "txt", "cbz"];
/// The screensaver root accepts exactly what the sleep screen renders.
const SS_EXTS: [&str; 3] = ["png", "jpg", "jpeg"];

/// The two browsable roots, strictly enumerated — a root is never a
/// path, so a hostile ?root= cannot escape into the filesystem. The
/// screensavers dir is env-overridable for the host-side test, like
/// save_dir().
fn base_dir(root: Option<&str>) -> Option<String> {
    match root.unwrap_or("documents") {
        "documents" => Some(save_dir()),
        "screensavers" => {
            Some(std::env::var("YB_SS_DIR").unwrap_or_else(|_| "/mnt/us/screensavers".to_string()))
        }
        _ => None,
    }
}

fn root_param(query: &str) -> Option<String> {
    query_param(query, "root")
}
const MAX_BODY: u64 = 512 * 1024 * 1024;
/// Wall-clock caps bounding a whole transaction, not just one read(). The
/// per-read 60s socket timeout resets on every byte, so a client dripping
/// 1 B/59 s could otherwise pin the single-threaded accept loop forever.
/// Headers are tiny: 30 s. Bodies ride a generous half hour — enough for
/// MAX_BODY over slow Wi-Fi — after which the connection is dropped.
const HEADER_PHASE_CAP: Duration = Duration::from_secs(30);
const BODY_PHASE_CAP: Duration = Duration::from_secs(30 * 60);
const HDR_CAP: usize = 16 * 1024;
const PORT: u16 = 8080;
const IPTABLES: &str = "/usr/sbin/iptables";

const SYSTEM_FILES: [&str; 2] = ["My Clippings.txt", "JAILBROKEN.txt"];

/// Live connection threads allowed. Each costs a thread (default stack,
/// kernel task) on a ~150 MB-RAM device; eight simultaneous clients is
/// far past any single-user reality, and past it new connections get an
/// immediate 503 instead of piling threads until the OOM killer picks a
/// victim.
const MAX_CONNS: usize = 8;

/// Per-boot credential for the web receiver: six digits, shown on the
/// e-ink screen under the address and carried by the QR URL. Six digits
/// alone would fall in hours, so every miss parks its connection seat
/// for [`AUTH_FAIL_DELAY`] — through the [`MAX_CONNS`] seats that caps
/// guessing at roughly ten tries per second: ~28 h to exhaust the space,
/// ~14 h expected hit, all of it *while the screen is open*. The real
/// bound is time, not math: the listener and its firewall rule exist
/// only as long as the receive session, which is a minutes-scale window.
/// See SECURITY.md.
const PIN_LEN: usize = 6;
const AUTH_FAIL_DELAY: Duration = Duration::from_millis(800);
static FAILED_PAIR_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);
static LAST_PAIR_FAIL_SECS: AtomicUsize = AtomicUsize::new(0);
const MAX_PAIR_ATTEMPTS: usize = 10;
const PAIR_LOCKOUT_WINDOW_SECS: usize = 300; // 5 minutes decay

/// Digits only, unbiased: draws at or above the largest multiple of 10⁶
/// are redrawn so `% 1_000_000` stays flat. Falls back to a time/pid mix
/// through FNV when /dev/urandom is unreadable (host CI) — quality
/// degrades, availability doesn't.
fn generate_pin() -> String {
    const LIMIT: u64 = 1_000_000;
    const REDRAW_FROM: u64 = u64::MAX - (u64::MAX % LIMIT);

    let next = move || -> u64 {
        let mut buf = [0u8; 8];
        if std::fs::File::open("/dev/urandom")
            .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut buf))
            .is_ok()
        {
            return u64::from_be_bytes(buf);
        }
        ybdev::img::fnv1a(
            &std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0)
                .to_le_bytes(),
        ) as u64 ^ ((std::process::id() as u64) << 32)
    };

    loop {
        let v = next();
        if v < REDRAW_FROM {
            return format!("{:0>width$}", v % LIMIT, width = PIN_LEN);
        }
    }
}

/// What the user typed may come from paper transcription or a phone
/// keyboard: strip everything non-alphanumeric and uppercase. The pin is
/// digits, so this accepts "482 913", "482913", "482913 ".
fn normalize_code(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_uppercase())
        .collect()
}

/// Length-checked constant-time compare: equal-length strings differ in
/// at most the folded XOR. The pin is fixed-length anyway; this keeps
/// the check honest if the credential ever isn't.
fn tokens_match(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.bytes().zip(b.bytes()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Every request — static page included — must present the session PIN
/// or a registered persistent device token (via cookie, header, or ?t=).
fn authorized(
    session_pin: &str,
    query_str: &str,
    header_token: Option<&str>,
    cookie_token: Option<&str>,
    devices: &DeviceStore,
) -> bool {
    let candidate = match header_token {
        Some(h) => Some(normalize_code(h)),
        None => query_param(query_str, "t").map(|t| normalize_code(&t)),
    };
    if let Some(cand) = &candidate {
        if tokens_match(session_pin, cand) {
            return true;
        }
    }
    if let Some(cand) = query_param(query_str, "t") {
        if devices.find_by_token_for_inbound(&cand).is_some() {
            return true;
        }
    }
    if let Some(h) = header_token {
        if devices.find_by_token_for_inbound(h.trim()).is_some() {
            return true;
        }
    }
    if let Some(c) = cookie_token {
        if devices.find_by_token_for_inbound(c.trim()).is_some() {
            return true;
        }
    }
    false
}

/// Served to a browser that arrived without (or with a wrong) pin / token.
/// Provides a clean pairing form with optional persistent trust and mirror auto-link.
const PAIR_PAGE: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>yb-reader &mdash; Pair Device</title>
<style>
body { margin:0; min-height:100vh; display:flex; align-items:center; justify-content:center;
       background:#0d0e12; color:#f3f4f6; font-family:-apple-system,BlinkMacSystemFont,'SF Pro Display','Inter','Segoe UI',Roboto,sans-serif; padding:16px; box-sizing:border-box; }
.card { background:#16181f; border:1px solid rgba(255,255,255,.08); border-radius:16px; padding:32px 26px; width:min(380px,94vw); text-align:center; box-sizing:border-box; }
.badge { display:inline-flex; align-items:center; gap:6px; font-size:0.75rem; font-weight:600; color:#9ca3af; background:rgba(255,255,255,0.06); padding:4px 10px; border-radius:20px; margin-bottom:14px; border:1px solid rgba(255,255,255,0.08); }
.dot { width:6px; height:6px; border-radius:50%; background:#10b981; }
h1 { font-size:1.25rem; font-weight:700; margin:0 0 6px; letter-spacing:-0.3px; }
p { color:#9ca3af; font-size:.85rem; margin:0 0 20px; line-height:1.4; }
input.pin { width:100%; box-sizing:border-box; padding:13px; font-size:1.5rem; letter-spacing:.35em; text-align:center;
        background:#0d0e12; color:#f3f4f6; border:1px solid rgba(255,255,255,.15); border-radius:12px; outline:none; font-family:monospace; font-weight:700; }
input.pin:focus { border-color:#3b82f6; }
.opts { margin-top:18px; text-align:left; background:rgba(255,255,255,0.03); border:1px solid rgba(255,255,255,0.06); border-radius:12px; padding:14px; font-size:0.85rem; }
.row { display:flex; align-items:flex-start; gap:10px; cursor:pointer; user-select:none; }
.row input[type="checkbox"] { margin-top:3px; accent-color:#3b82f6; width:16px; height:16px; cursor:pointer; }
.row-label { font-weight:600; color:#f3f4f6; display:block; margin-bottom:2px; }
.row-sub { font-size:0.75rem; color:#9ca3af; }
.dev-input { width:100%; box-sizing:border-box; margin-top:10px; padding:9px 12px; font-size:0.85rem; background:#0d0e12; color:#f3f4f6; border:1px solid rgba(255,255,255,0.12); border-radius:8px; outline:none; }
.dev-input:focus { border-color:#3b82f6; }
.mirror-opt { margin-top:12px; padding-top:10px; border-top:1px solid rgba(255,255,255,0.06); }
button { margin-top:20px; width:100%; padding:12px; font-size:.95rem; font-weight:600;
         background:#3b82f6; color:#fff; border:none; border-radius:12px; cursor:pointer; }
button:hover { background:#2563eb; }
.err { color:#ef4444; margin-top:12px; font-size:.8rem; display:none; }
</style>
</head>
<body>
<div class="card">
  <div class="badge"><span class="dot"></span> <span id="kname">Kindle Paperwhite</span></div>
  <h1>Pair &amp; Connect</h1>
  <p>Type the 6-digit code shown on your Kindle screen.</p>
  <form id="pairForm" action="/" method="get">
    <input name="t" id="pinInput" class="pin" inputmode="numeric" pattern="[0-9 ]*" maxlength="9"
           autocomplete="off" autofocus placeholder="••••••" required>
    <div class="opts">
      <label class="row">
        <input type="checkbox" id="rememberBox" checked>
        <div>
          <span class="row-label">Trust this browser</span>
          <span class="row-sub">Instant book uploads without entering PIN</span>
        </div>
      </label>
      <input type="text" id="devName" class="dev-input" placeholder="Device Name (e.g. MacBook Pro)">
      <div class="mirror-opt">
        <label class="row">
          <input type="checkbox" id="linkMirror">
          <div>
            <span class="row-label">Also link yb-mirror</span>
            <span class="row-sub">Auto-sync token if running on this Mac</span>
          </div>
        </label>
      </div>
    </div>
    <button id="submitBtn">Open Book Manager</button>
    <div id="errMsg" class="err">Invalid code. Please check your Kindle screen.</div>
  </form>
</div>
<script>
(function() {
  const ua = navigator.userAgent;
  let defName = 'Browser';
  if (ua.includes('Macintosh')) defName = 'MacBook';
  else if (ua.includes('Windows')) defName = 'Windows PC';
  else if (ua.includes('Linux')) defName = 'Linux PC';
  else if (ua.includes('iPhone')) defName = 'iPhone';
  else if (ua.includes('iPad')) defName = 'iPad';
  else if (ua.includes('Android')) defName = 'Android Device';
  document.getElementById('devName').value = defName;

  fetch('/api/handshake').then(r => r.json()).then(d => {
    if (d && d.kindle_name) document.getElementById('kname').textContent = d.kindle_name;
  }).catch(() => {});

  const form = document.getElementById('pairForm');
  const pinInput = document.getElementById('pinInput');
  const rememberBox = document.getElementById('rememberBox');
  const devName = document.getElementById('devName');
  const linkMirror = document.getElementById('linkMirror');
  const errMsg = document.getElementById('errMsg');
  const submitBtn = document.getElementById('submitBtn');

  form.addEventListener('submit', async function(e) {
    if (!rememberBox.checked) {
      return;
    }
    e.preventDefault();
    errMsg.style.display = 'none';
    submitBtn.disabled = true;
    submitBtn.textContent = 'Pairing...';
    try {
      const pin = pinInput.value.replace(/\s+/g, '');
      const name = devName.value.trim() || defName;
      let devId = localStorage.getItem('yb_device_id') || ('dev_' + Math.random().toString(36).substring(2, 10));
      localStorage.setItem('yb_device_id', devId);

      const scope = linkMirror.checked ? 'all' : 'inbound';
      const res = await fetch('/api/pair', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ pin, name, id: devId, scope })
      });

      if (!res.ok) {
        if (res.status === 429) throw new Error('Too many failed attempts — wait 5 minutes or restart receive mode.');
        if (res.status === 500) throw new Error('Pairing failed on the Kindle — try again.');
        throw new Error('Invalid PIN');
      }

      const data = await res.json();
      if (data && data.token) {
        // No client-side document.cookie: the server's Set-Cookie already
        // carries the token with HttpOnly (JS must not be able to read it
        // back). data.token is used in-memory for the mirror-link POST only.
        if (linkMirror.checked) {
          try {
            await fetch('http://localhost:8765/api/pair', {
              method: 'POST',
              headers: { 'Content-Type': 'application/json' },
              body: JSON.stringify({
                token: data.token,
                kindle_id: data.kindle_id,
                kindle_name: data.kindle_name
              })
            }).catch(() => {});
          } catch (_) {}
        }

        location.href = '/';
        return;
      }
      throw new Error('Pairing failed');
    } catch (err) {
      errMsg.textContent = err.message || 'Invalid PIN code';
      errMsg.style.display = 'block';
      submitBtn.disabled = false;
      submitBtn.textContent = 'Open Book Manager';
    }
  });
})();
</script>
</body>
</html>
"#;

/// The web file manager: responsive mobile & desktop UI for browsing,
/// uploading, previewing, moving, creating folders, and deleting books.
const PAGE: &str = include_str!("receive_page.html");

// ---- server -------------------------------------------------------------

pub struct ReceiveServer {
    port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    /// "name (x.x MB)" of the last completed delivery, for the screen.
    last: Arc<Mutex<Option<String>>>,
    received: Arc<AtomicUsize>,
    /// Per-boot session credential. Every HTTP request must carry it
    /// (`?t=` on the QR URL, echoed by the page thereafter); the firewall
    /// rule alone only scopes exposure to the LAN, this scopes it to
    /// whoever scanned the screen.
    token: Arc<String>,
}

impl ReceiveServer {
    /// Bind 8080 when free (friendly for repeat visits), else an ephemeral
    /// port — the QR always carries the real one. `stop` is shared with the
    /// screen so shutdown works even while setup is still running.
    fn start(stop: Arc<AtomicBool>) -> Option<ReceiveServer> {
        if stop.load(Ordering::Relaxed) {
            plog("receive: setup aborted before bind");
            return None;
        }
        let listener = TcpListener::bind(("0.0.0.0", PORT))
            .or_else(|_| TcpListener::bind(("0.0.0.0", 0)))
            .ok()?;
        let port = listener.local_addr().ok()?.port();
        listener.set_nonblocking(true).ok()?;
        firewall_port(port, true);

        FAILED_PAIR_ATTEMPTS.store(0, Ordering::Relaxed);
        LAST_PAIR_FAIL_SECS.store(0, Ordering::Relaxed);

        let last = Arc::new(Mutex::new(None));
        let received = Arc::new(AtomicUsize::new(0));
        let token = Arc::new(generate_pin());
        let profile = Arc::new(KindleProfile::load_or_create(
            &ybdev::devices::kindle_id_path(),
            KindleProfile::DEFAULT_PW5_W,
            KindleProfile::DEFAULT_PW5_H,
        ));

        // Live connection seats, handed out below and reclaimed by
        // ConnSlot's Drop.
        let active = Arc::new(AtomicUsize::new(0));
        static SHED_PLOGGED: AtomicBool = AtomicBool::new(false);
        let last_t = Arc::clone(&last);
        let recv_t = Arc::clone(&received);
        let active_t = Arc::clone(&active);
        let stop_t = Arc::clone(&stop);
        let token_t = Arc::clone(&token);
        let profile_t = Arc::clone(&profile);
        let handle = std::thread::spawn(move || {
            while !stop_t.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        if active_t.load(Ordering::Relaxed) >= MAX_CONNS {
                            // Shed without spawning: an immediate 503 is
                            // cheaper for everyone than another thread.
                            if !SHED_PLOGGED.swap(true, Ordering::Relaxed) {
                                plog("receive: connection limit reached — shedding new clients");
                            }
                            respond(
                                &mut stream,
                                503,
                                "Service Unavailable",
                                "text/plain",
                                "too many connections",
                            );
                            continue;
                        }
                        // One thread per connection: a LAN client that
                        // connects and stalls (headers up to 30 s, body up
                        // to 30 min) must not block every other upload.
                        active_t.fetch_add(1, Ordering::Relaxed);
                        let last = Arc::clone(&last_t);
                        let recv = Arc::clone(&recv_t);
                        let token = Arc::clone(&token_t);
                        let profile = Arc::clone(&profile_t);
                        let slot = ConnSlot(Arc::clone(&active_t));
                        std::thread::spawn(move || {
                            let _slot = slot;
                            handle_conn(stream, &last, &recv, &token, &profile);
                        });
                    }
                    Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(200));
                    }
                    Err(e) => {
                        plog(&format!("receive: accept failed, listener stopping: {}", e));
                        break;
                    }
                }
            }
            firewall_port(port, false);
            plog("receive: listener stopped");
        });

        Some(ReceiveServer {
            port,
            stop,
            handle: Some(handle),
            last,
            received,
            token,
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The session credential for this boot of the listener — goes into
    /// the QR URL, never into the log.
    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn status(&self) -> (usize, Option<String>) {
        (
            self.received.load(Ordering::Relaxed),
            self.last.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        )
    }

    fn shutdown(&mut self) {
        firewall_port(self.port, false);
        self.stop.store(true, Ordering::Relaxed);
        self.handle.take();
    }
}

/// Ports this process currently holds an INPUT/ACCEPT rule open for.
static OPEN_RULES: std::sync::Mutex<Vec<u16>> = std::sync::Mutex::new(Vec::new());

fn track_rule(port: u16, open: bool) {
    if let Ok(mut v) = OPEN_RULES.lock() {
        if open {
            if !v.contains(&port) {
                v.push(port);
            }
        } else {
            v.retain(|&p| p != port);
        }
    }
}

fn firewall_port(port: u16, open: bool) {
    let port_str = port.to_string();
    let spec = [
        "-i",
        "wlan0",
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
            if ok {
                track_rule(port, true);
            } else {
                plog("receive: could not open firewall port — uploads will time out");
            }
        } else {
            track_rule(port, true);
        }
    } else {
        track_rule(port, false);
        let _ = Command::new(IPTABLES)
            .args(["-D", "INPUT"])
            .args(spec)
            .status();
    }
}

fn local_ip() -> Option<String> {
    let s = UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    s.connect(("8.8.8.8", 53)).ok()?;
    Some(s.local_addr().ok()?.ip().to_string())
}

pub fn emergency_cleanup() {
    let ports: Vec<u16> = OPEN_RULES
        .lock()
        .map(|v| v.as_slice().to_vec())
        .unwrap_or_default();
    for p in ports {
        firewall_port(p, false);
    }
    // No unconditional `-D PORT`: a port we never opened must not have an
    // unrelated pre-existing rule silently removed.
    if let Ok(rd) = std::fs::read_dir(save_dir()) {
        for e in rd.flatten() {
            if e.path().extension().and_then(|x| x.to_str()) == Some("part") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

fn sanitize_rel_dir(d: &str) -> Option<std::path::PathBuf> {
    let trimmed = d.trim_matches(|c| c == '/' || c == '\\' || c == ' ');
    if trimmed.is_empty() {
        return Some(std::path::PathBuf::new());
    }
    let mut clean = std::path::PathBuf::new();
    for comp in std::path::Path::new(trimmed).components() {
        match comp {
            std::path::Component::Normal(c) => {
                let s = c.to_string_lossy();
                if s.contains('\\') || s.contains('\0') || s == ".." || s == "." {
                    return None;
                }
                clean.push(c);
            }
            _ => return None,
        }
    }
    Some(clean)
}

fn sanitize_folder_name(name: &str) -> Option<String> {
    let s = name.trim();
    if s.is_empty()
        || s.len() > 100
        || s.contains('/')
        || s.contains('\\')
        || s.contains('\0')
        || s.starts_with('.')
        || s == ".."
    {
        return None;
    }
    Some(s.to_string())
}

fn query_param(query: &str, key: &str) -> Option<String> {
    let prefix = format!("{}=", key);
    for part in query.split('&') {
        if let Some(val) = part.strip_prefix(&prefix) {
            return Some(urldecode(val));
        }
    }
    None
}

fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

fn collect_all_folders(base: &std::path::Path, rel: &std::path::Path, out: &mut Vec<String>) {
    let full = base.join(rel);
    if let Ok(rd) = std::fs::read_dir(full) {
        let mut subdirs = Vec::new();
        for e in rd.flatten() {
            if let Ok(ft) = e.file_type() {
                if ft.is_dir() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if !name.starts_with('.') && !name.ends_with(".sdr") {
                        subdirs.push(name);
                    }
                }
            }
        }
        subdirs.sort();
        for sub in subdirs {
            let next_rel = if rel.as_os_str().is_empty() {
                sub.clone()
            } else {
                format!("{}/{}", rel.to_string_lossy(), sub)
            };
            out.push(next_rel.clone());
            collect_all_folders(base, std::path::Path::new(&next_rel), out);
        }
    }
}

/// RAII seat for one connection thread: the concurrency counter falls
/// even if [`handle_conn`] unwinds — otherwise a single panic would
/// permanently shed every later client.
struct ConnSlot(Arc<AtomicUsize>);

impl Drop for ConnSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

fn handle_conn(
    mut stream: TcpStream,
    last: &Arc<Mutex<Option<String>>>,
    received: &Arc<AtomicUsize>,
    pin: &str,
    profile: &Arc<KindleProfile>,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));

    let mut buf: Vec<u8> = Vec::with_capacity(2048);
    let hdr_started = Instant::now();
    let hdr_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if buf.len() > HDR_CAP {
            respond(
                &mut stream,
                431,
                "Request Header Fields Too Large",
                "text/plain",
                "too large",
            );
            return;
        }
        if hdr_started.elapsed() > HEADER_PHASE_CAP {
            plog("receive: header phase exceeded cap — dropping slow client");
            return;
        }
        let mut chunk = [0u8; 4096];
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
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
    let mut dup_cl = false;
    let mut hdr_token: Option<String> = None;
    let mut cookie_token: Option<String> = None;

    for line in lines {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let k = k.trim().to_ascii_lowercase();
        if k == "content-length" {
            match (content_length, v.trim().parse().ok()) {
                // Two Content-Length headers with different values are a
                // request-smuggling tell: reject rather than "last wins".
                (Some(a), Some(b)) if a != b => dup_cl = true,
                (None, Some(b)) => content_length = Some(b),
                _ => {}
            }
        } else if k == "expect" && v.trim().eq_ignore_ascii_case("100-continue") {
            expect_continue = true;
        } else if k == "x-yb-token" || k == "x-yb-device-token" {
            hdr_token = Some(v.trim().to_string());
        } else if k == "authorization" {
            if let Some(rest) = v.trim().strip_prefix("Bearer ") {
                hdr_token = Some(rest.trim().to_string());
            }
        } else if k == "cookie" {
            for pair in v.split(';') {
                if let Some((ck, cv)) = pair.trim().split_once('=') {
                    if ck.trim() == "yb_token" {
                        cookie_token = Some(cv.trim().to_string());
                    }
                }
            }
        }
    }

    if dup_cl {
        respond(
            &mut stream,
            400,
            "Bad Request",
            "text/plain",
            "conflicting Content-Length",
        );
        return;
    }

    let (raw_path, query_str) = path.split_once('?').unwrap_or((path, ""));

    // Handshake info is available to any LAN client to discover device details.
    if method == "GET" && raw_path == "/api/handshake" {
        let free_gb = ybdev::sysinfo::storage_free_gb().unwrap_or(0.0);
        let resp = format!(
            "{{\"kindle_id\":\"{}\",\"kindle_name\":\"{}\",\"width\":{},\"height\":{},\"bpp\":{},\"free_gb\":{:.1}}}",
            ybdev::devices::json_escape(&profile.id),
            ybdev::devices::json_escape(&profile.name),
            profile.width,
            profile.height,
            profile.bpp,
            free_gb
        );
        respond(&mut stream, 200, "OK", "application/json; charset=utf-8", &resp);
        return;
    }

    // Pairing endpoint: verifies PIN, generates persistent device token and sets cookie.
    if method == "POST" && raw_path == "/api/pair" {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as usize)
            .unwrap_or(0);
        let last_fail = LAST_PAIR_FAIL_SECS.load(Ordering::Relaxed);
        if last_fail != 0 && now.saturating_sub(last_fail) > PAIR_LOCKOUT_WINDOW_SECS {
            FAILED_PAIR_ATTEMPTS.store(0, Ordering::Relaxed);
            LAST_PAIR_FAIL_SECS.store(0, Ordering::Relaxed);
        }
        if FAILED_PAIR_ATTEMPTS.load(Ordering::Relaxed) >= MAX_PAIR_ATTEMPTS {
            plog("receive: pairing locked out due to too many failed attempts");
            std::thread::sleep(AUTH_FAIL_DELAY);
            respond(&mut stream, 429, "Too Many Requests", "application/json", "{\"error\":\"too many failed attempts - locked out\"}");
            return;
        }

        let body_bytes = read_small_body(&mut stream, &buf[hdr_end + 4..], content_length, 16384).unwrap_or_default();
        let body_str = String::from_utf8_lossy(&body_bytes);
        let cand_pin = extract_param_str(&body_str, "pin").unwrap_or_default();
        if !tokens_match(pin, &normalize_code(&cand_pin)) {
            FAILED_PAIR_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
            LAST_PAIR_FAIL_SECS.store(now, Ordering::Relaxed);
            static PAIR_MISS_PLOGGED: AtomicBool = AtomicBool::new(false);
            if !PAIR_MISS_PLOGGED.swap(true, Ordering::Relaxed) {
                plog("receive: pair request with invalid pin");
            }
            std::thread::sleep(AUTH_FAIL_DELAY);
            respond(&mut stream, 403, "Forbidden", "application/json", "{\"error\":\"invalid pin\"}");
            return;
        }
        FAILED_PAIR_ATTEMPTS.store(0, Ordering::Relaxed);
        LAST_PAIR_FAIL_SECS.store(0, Ordering::Relaxed);

        let dev_name = extract_param_str(&body_str, "name").unwrap_or_else(|| "Browser".to_string());
        let dev_id = extract_param_str(&body_str, "id").unwrap_or_else(|| format!("dev_{}", ybdev::devices::generate_random_hex(4)));
        let scope = match extract_param_str(&body_str, "scope").as_deref() {
            Some("all") => "all".to_string(),
            _ => "inbound".to_string(),
        };
        let new_token = ybdev::devices::generate_token();
        let peer_ip = stream.peer_addr().ok().map(|a| a.ip().to_string());
        let dev = ybdev::devices::TrustedDevice::new(&dev_id, &dev_name, &new_token, peer_ip.as_deref(), &scope);
        if let Err(e) = ybdev::devices::with_store_mut(|store| {
            store.add_or_update(dev);
        }) {
            // Returning 200 here would hand the client a token that was never
            // persisted — it would 403 on every subsequent request.
            plog(&format!("receive: failed to persist paired device: {}", e));
            respond(&mut stream, 500, "Internal Server Error", "application/json", "{\"error\":\"could not persist pairing\"}");
            return;
        }
        plog(&format!("receive: device paired successfully ('{}')", dev_name));
        let resp_body = format!(
            "{{\"status\":\"ok\",\"token\":\"{}\",\"device_id\":\"{}\",\"kindle_name\":\"{}\",\"kindle_id\":\"{}\"}}",
            ybdev::devices::json_escape(&new_token),
            ybdev::devices::json_escape(&dev_id),
            ybdev::devices::json_escape(&profile.name),
            ybdev::devices::json_escape(&profile.id)
        );
        let cookie_hdr = format!("Set-Cookie: yb_token={}; Path=/; Max-Age=315360000; SameSite=Lax; HttpOnly", new_token);
        respond_with_headers(&mut stream, 200, "OK", "application/json; charset=utf-8", &resp_body, &[&cookie_hdr]);
        return;
    }

    // Gate before dispatch: always reload store from disk so UI revokes take immediate effect.
    let current_store = DeviceStore::load(&ybdev::devices::devices_path());
    let is_auth = authorized(pin, query_str, hdr_token.as_deref(), cookie_token.as_deref(), &current_store);

    if !is_auth {
        // Log the first miss only: a sustained brute force must leave a
        // trace, but one plog line per guess would burn flash writes for
        // noise (same latch pattern as SHED_PLOGGED).
        static AUTH_MISS_PLOGGED: AtomicBool = AtomicBool::new(false);
        if !AUTH_MISS_PLOGGED.swap(true, Ordering::Relaxed) {
            plog("receive: auth miss — wrong or missing pin/token (further misses not logged)");
        }
        std::thread::sleep(AUTH_FAIL_DELAY);
        if method == "GET" && (raw_path == "/" || raw_path == "/index.html") {
            respond(&mut stream, 403, "Forbidden", "text/html; charset=utf-8", PAIR_PAGE);
        } else {
            respond(&mut stream, 403, "Forbidden", "text/plain", "forbidden");
        }
        return;
    }

    // Refresh client last_ip if authorized via persistent device token.
    // Gate on the freshly-loaded store: only a real device token can match,
    // so guest-PIN sessions (whose ?t= is the 6-digit PIN) never touch the
    // flash-write path. refresh_device_ip persists only when the IP changed.
    let q_tok = query_param(query_str, "t");
    let used_token = cookie_token.as_deref()
        .or(hdr_token.as_deref())
        .or(q_tok.as_deref());
    if let (Some(tok), Ok(peer_addr)) = (used_token, stream.peer_addr()) {
        if current_store.find_by_token(tok).is_some() {
            let peer_ip = peer_addr.ip().to_string();
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if let Err(e) = ybdev::devices::refresh_device_ip(&ybdev::devices::devices_path(), tok, &peer_ip, now) {
                plog(&format!("receive: device ip refresh failed: {}", e));
            }
        }
    }

    if method == "GET" {
        if raw_path == "/api/list" || raw_path == "/api/tree" {
            let Some(base) = base_dir(root_param(query_str).as_deref()) else {
                respond(
                    &mut stream,
                    400,
                    "Bad Request",
                    "application/json",
                    "{\"error\":\"bad root\"}",
                );
                return;
            };
            let base_dir = std::path::PathBuf::from(base);
            let rel_str = query_param(query_str, "dir").unwrap_or_default();
            let Some(rel_path) = sanitize_rel_dir(&rel_str) else {
                respond(
                    &mut stream,
                    400,
                    "Bad Request",
                    "application/json",
                    "{\"error\":\"bad dir\"}",
                );
                return;
            };
            let target_dir = base_dir.join(&rel_path);
            if !target_dir.exists() || !target_dir.is_dir() {
                respond(
                    &mut stream,
                    404,
                    "Not Found",
                    "application/json",
                    "{\"error\":\"not found\"}",
                );
                return;
            }

            let mut folders = Vec::new();
            let mut files = Vec::new();
            let ss_root = root_param(query_str).as_deref() == Some("screensavers");

            if let Ok(rd) = std::fs::read_dir(&target_dir) {
                for e in rd.flatten() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if name.starts_with('.')
                        || name.ends_with(".sdr")
                        || SYSTEM_FILES.contains(&name.as_str())
                    {
                        continue;
                    }
                    if let Ok(ft) = e.file_type() {
                        if ft.is_dir() {
                            folders.push(name);
                        } else if ft.is_file() {
                            let ext = e
                                .path()
                                .extension()
                                .map(|x| x.to_string_lossy().to_ascii_lowercase())
                                .unwrap_or_default();
                            let ok = if ss_root {
                                SS_EXTS.contains(&ext.as_str())
                            } else {
                                OK_EXTS.contains(&ext.as_str())
                            };
                            if ok {
                                let sz = e.metadata().map(|m| m.len()).unwrap_or(0);
                                files.push((name, sz, ext));
                            }
                        }
                    }
                }
            }
            folders.sort();
            files.sort_by_key(|a| a.0.to_lowercase());

            let mut all_folders = Vec::new();
            collect_all_folders(&base_dir, std::path::Path::new(""), &mut all_folders);

            let cur_rel_str = rel_path.to_string_lossy().into_owned();
            let mut json = String::new();
            json.push_str("{\"current_dir\":\"");
            json.push_str(&escape_json(&cur_rel_str));
            json.push_str("\",\"folders\":[");
            for (i, f) in folders.iter().enumerate() {
                if i > 0 {
                    json.push(',');
                }
                json.push('"');
                json.push_str(&escape_json(f));
                json.push('"');
            }
            json.push_str("],\"files\":[");
            for (i, (n, sz, ext)) in files.iter().enumerate() {
                if i > 0 {
                    json.push(',');
                }
                json.push_str("{\"name\":\"");
                json.push_str(&escape_json(n));
                json.push_str("\",\"size\":");
                json.push_str(&sz.to_string());
                json.push_str(",\"ext\":\"");
                json.push_str(&escape_json(ext));
                json.push_str("\"}");
            }
            json.push_str("],\"all_folders\":[");
            for (i, f) in all_folders.iter().enumerate() {
                if i > 0 {
                    json.push(',');
                }
                json.push('"');
                json.push_str(&escape_json(f));
                json.push('"');
            }
            let free_gb = ybdev::sysinfo::storage_free_gb().unwrap_or(0.0);
            json.push_str(&format!("],\"free_gb\":{:.2},\"kindle_name\":\"{}\"", free_gb, ybdev::devices::json_escape(&profile.name)));
            json.push('}');

            respond(
                &mut stream,
                200,
                "OK",
                "application/json; charset=utf-8",
                &json,
            );
            return;
        }

        if raw_path == "/api/file" || raw_path == "/api/raw" {
            let Some(base) = base_dir(root_param(query_str).as_deref()) else {
                respond(&mut stream, 400, "Bad Request", "text/plain", "bad root");
                return;
            };
            let base_dir = std::path::PathBuf::from(base);
            let rel_str = query_param(query_str, "dir").unwrap_or_default();
            let name = query_param(query_str, "name").unwrap_or_default();
            let Some(rel_path) = sanitize_rel_dir(&rel_str) else {
                respond(&mut stream, 400, "Bad Request", "text/plain", "bad dir");
                return;
            };
            if name.is_empty() || name.contains('/') || name.contains('\\') || name == ".." {
                respond(&mut stream, 400, "Bad Request", "text/plain", "bad name");
                return;
            }
            let target = base_dir.join(rel_path).join(&name);
            if !target.exists() || !target.is_file() {
                respond(&mut stream, 404, "Not Found", "text/plain", "not found");
                return;
            }
            let ext = target
                .extension()
                .and_then(|x| x.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let ctype = match ext.as_str() {
                "jpg" | "jpeg" => "image/jpeg",
                "png" => "image/png",
                "epub" => "application/epub+zip",
                "pdf" => "application/pdf",
                "txt" => "text/plain; charset=utf-8",
                "cbz" => "application/vnd.comicbook+zip",
                "fb2" => "application/x-fictionbook+xml",
                _ => "application/octet-stream",
            };
            if let Ok(mut f) = File::open(&target) {
                let sz = f.metadata().map(|m| m.len()).unwrap_or(0);
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: public, max-age=3600\r\nConnection: close\r\n\r\n",
                    ctype, sz
                );
                let _ = stream.write_all(head.as_bytes());
                let mut buf = [0u8; 65536];
                while let Ok(n) = f.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    if stream.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
                let _ = stream.flush();
            } else {
                respond(
                    &mut stream,
                    500,
                    "Server Error",
                    "text/plain",
                    "cannot read file",
                );
            }
            return;
        }

        respond(&mut stream, 200, "OK", "text/html; charset=utf-8", PAGE);
        return;
    }

    if method == "POST" {
        if raw_path == "/api/mkdir" {
            let root = root_param(query_str);
            // One flat image dir — folders don't apply.
            if root.as_deref() == Some("screensavers") {
                respond(
                    &mut stream,
                    400,
                    "Bad Request",
                    "application/json",
                    "{\"error\":\"no folders in screensavers\"}",
                );
                return;
            }
            let Some(base) = base_dir(root.as_deref()) else {
                respond(
                    &mut stream,
                    400,
                    "Bad Request",
                    "application/json",
                    "{\"error\":\"bad root\"}",
                );
                return;
            };
            let base_dir = std::path::PathBuf::from(base);
            let rel_str = query_param(query_str, "dir").unwrap_or_default();
            let name = query_param(query_str, "name").unwrap_or_default();
            let Some(rel_path) = sanitize_rel_dir(&rel_str) else {
                respond(
                    &mut stream,
                    400,
                    "Bad Request",
                    "application/json",
                    "{\"error\":\"bad dir\"}",
                );
                return;
            };
            let Some(clean_name) = sanitize_folder_name(&name) else {
                respond(
                    &mut stream,
                    400,
                    "Bad Request",
                    "application/json",
                    "{\"error\":\"bad folder name\"}",
                );
                return;
            };
            let target = base_dir.join(rel_path).join(clean_name);
            match std::fs::create_dir_all(&target) {
                Ok(()) => respond(&mut stream, 200, "OK", "application/json", "{\"ok\":true}"),
                Err(e) => respond(
                    &mut stream,
                    500,
                    "Server Error",
                    "application/json",
                    &format!("{{\"error\":\"{}\"}}", escape_json(&e.to_string())),
                ),
            }
            return;
        }

        if raw_path == "/api/move" {
            let root = root_param(query_str);
            if root.as_deref() == Some("screensavers") {
                respond(
                    &mut stream,
                    400,
                    "Bad Request",
                    "application/json",
                    "{\"error\":\"no move in screensavers\"}",
                );
                return;
            }
            let Some(base) = base_dir(root.as_deref()) else {
                respond(
                    &mut stream,
                    400,
                    "Bad Request",
                    "application/json",
                    "{\"error\":\"bad root\"}",
                );
                return;
            };
            let base_dir = std::path::PathBuf::from(base);
            let src_str = query_param(query_str, "src_dir").unwrap_or_default();
            let name = query_param(query_str, "name").unwrap_or_default();
            let dst_str = query_param(query_str, "dst_dir").unwrap_or_default();

            let Some(src_rel) = sanitize_rel_dir(&src_str) else {
                respond(
                    &mut stream,
                    400,
                    "Bad Request",
                    "application/json",
                    "{\"error\":\"bad src_dir\"}",
                );
                return;
            };
            let Some(dst_rel) = sanitize_rel_dir(&dst_str) else {
                respond(
                    &mut stream,
                    400,
                    "Bad Request",
                    "application/json",
                    "{\"error\":\"bad dst_dir\"}",
                );
                return;
            };
            if name.is_empty() || name.contains('/') || name.contains('\\') || name == ".." {
                respond(
                    &mut stream,
                    400,
                    "Bad Request",
                    "application/json",
                    "{\"error\":\"bad name\"}",
                );
                return;
            }
            // Same protected set as /api/delete: moving Amazon's own files
            // out of their expected top-level location breaks the stock
            // system, so refuse rather than let a stray drag relocate them.
            if src_rel.as_os_str().is_empty() && SYSTEM_FILES.contains(&name.as_str()) {
                respond(
                    &mut stream,
                    400,
                    "Bad Request",
                    "application/json",
                    "{\"error\":\"protected file\"}",
                );
                return;
            }

            let src_path = base_dir.join(src_rel).join(&name);
            let dst_dir = base_dir.join(&dst_rel);
            let dst_path = dst_dir.join(&name);

            if !src_path.exists() {
                respond(
                    &mut stream,
                    404,
                    "Not Found",
                    "application/json",
                    "{\"error\":\"source not found\"}",
                );
                return;
            }
            if dst_path.exists() {
                respond(
                    &mut stream,
                    409,
                    "Conflict",
                    "application/json",
                    "{\"error\":\"destination already exists\"}",
                );
                return;
            }
            let _ = std::fs::create_dir_all(&dst_dir);
            match std::fs::rename(&src_path, &dst_path) {
                Ok(()) => respond(&mut stream, 200, "OK", "application/json", "{\"ok\":true}"),
                Err(e) => respond(
                    &mut stream,
                    500,
                    "Server Error",
                    "application/json",
                    &format!("{{\"error\":\"{}\"}}", escape_json(&e.to_string())),
                ),
            }
            return;
        }

        if raw_path == "/api/delete" {
            let Some(base) = base_dir(root_param(query_str).as_deref()) else {
                respond(
                    &mut stream,
                    400,
                    "Bad Request",
                    "application/json",
                    "{\"error\":\"bad root\"}",
                );
                return;
            };
            let base_dir = std::path::PathBuf::from(base);
            let rel_str = query_param(query_str, "dir").unwrap_or_default();
            let name = query_param(query_str, "name").unwrap_or_default();

            let Some(rel_path) = sanitize_rel_dir(&rel_str) else {
                respond(
                    &mut stream,
                    400,
                    "Bad Request",
                    "application/json",
                    "{\"error\":\"bad dir\"}",
                );
                return;
            };
            if name.is_empty() || name.contains('/') || name.contains('\\') || name == ".." {
                respond(
                    &mut stream,
                    400,
                    "Bad Request",
                    "application/json",
                    "{\"error\":\"bad name\"}",
                );
                return;
            }
            // Amazon's own files (clippings, jailbreak marker) are only
            // hidden from the listing — this handler must refuse to
            // destroy them outright, not just hide them.
            if rel_path.as_os_str().is_empty() && SYSTEM_FILES.contains(&name.as_str()) {
                respond(
                    &mut stream,
                    400,
                    "Bad Request",
                    "application/json",
                    "{\"error\":\"protected file\"}",
                );
                return;
            }
            let target = base_dir.join(rel_path).join(&name);
            if !target.exists() {
                respond(
                    &mut stream,
                    404,
                    "Not Found",
                    "application/json",
                    "{\"error\":\"not found\"}",
                );
                return;
            }
            let res = if target.is_dir() {
                std::fs::remove_dir_all(&target)
            } else {
                std::fs::remove_file(&target)
            };
            match res {
                Ok(()) => respond(&mut stream, 200, "OK", "application/json", "{\"ok\":true}"),
                Err(e) => respond(
                    &mut stream,
                    500,
                    "Server Error",
                    "application/json",
                    &format!("{{\"error\":\"{}\"}}", escape_json(&e.to_string())),
                ),
            }
            return;
        }

        if raw_path == "/upload" {
            let Some(len) = content_length else {
                respond(
                    &mut stream,
                    411,
                    "Length Required",
                    "text/plain",
                    "Content-Length required",
                );
                return;
            };
            if len > MAX_BODY {
                respond(
                    &mut stream,
                    413,
                    "Payload Too Large",
                    "text/plain",
                    "too large",
                );
                return;
            }

            let root = root_param(query_str);
            let Some(base) = base_dir(root.as_deref()) else {
                respond(&mut stream, 400, "Bad Request", "text/plain", "bad root");
                return;
            };
            let ss_root = root.as_deref() == Some("screensavers");

            let rel_dir = query_param(query_str, "dir").unwrap_or_default();
            let Some(clean_rel) = sanitize_rel_dir(&rel_dir) else {
                respond(&mut stream, 400, "Bad Request", "text/plain", "bad dir");
                return;
            };

            let name = query_param(query_str, "name").and_then(|n| sanitize_fetch_name(&n));
            let Some(name) = name else {
                respond(
                    &mut stream,
                    400,
                    "Bad Request",
                    "text/plain",
                    "bad file name",
                );
                return;
            };
            // The allowlist is per-root: books in documents, exactly the
            // renderable image types in screensavers.
            let allow: &[&str] = if ss_root { &SS_EXTS } else { &OK_EXTS };
            let ext_ok = name
                .rsplit('.')
                .next()
                .map(|e| allow.contains(&e.to_ascii_lowercase().as_str()))
                .unwrap_or(false);
            if !ext_ok || name.len() > 200 {
                respond(
                    &mut stream,
                    415,
                    "Unsupported Media Type",
                    "text/plain",
                    "unsupported file type",
                );
                return;
            }

            let dir = std::path::PathBuf::from(&base).join(clean_rel);
            let _ = std::fs::create_dir_all(&dir);
            let final_path = dir.join(&name);
            let part_path = dir.join(format!("{}.part", name));
            let final_str = final_path.to_string_lossy().into_owned();
            let part_str = part_path.to_string_lossy().into_owned();

            if final_path.exists() {
                plog(&format!("receive: REFUSED {} — already exists", name));
                respond(
                    &mut stream,
                    409,
                    "Conflict",
                    "text/plain",
                    "file already exists\n",
                );
                return;
            }
            if expect_continue {
                let _ = stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n");
            }
            let body_prefix = buf[hdr_end + 4..].to_vec();
            let t0 = now_ms();

            match write_body(&mut stream, &body_prefix, len, &part_str, &final_str) {
                Ok(()) => {
                    let msg = format!("{} ({:.1} MB)", name, len as f64 / 1048576.0);
                    *last.lock().unwrap_or_else(|e| e.into_inner()) = Some(msg.clone());
                    received.fetch_add(1, Ordering::Relaxed);
                    plog(&format!(
                        "receive: saved {} in {:.1}s",
                        msg,
                        (now_ms() - t0) as f64 / 1000.0
                    ));
                    respond(
                        &mut stream,
                        200,
                        "OK",
                        "text/plain",
                        &format!("saved: {}\n", msg),
                    );
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&part_str);
                    plog(&format!("receive: FAILED {} — {}", name, e));
                    respond(
                        &mut stream,
                        500,
                        "Server Error",
                        "text/plain",
                        &format!("failed: {}", e),
                    );
                }
            }
            return;
        }
    }

    respond(&mut stream, 404, "Not Found", "text/plain", "not found");
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
    // O_NOFOLLOW: the HTTP side can't plant symlinks, but another local
    // process could pre-create `<name>.part` as one and make us write
    // through it. Fail with ELOOP instead of following.
    let c_part = std::ffi::CString::new(part).map_err(|_| "bad part path".to_string())?;
    let fd = unsafe {
        libc::open(
            c_part.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_NOFOLLOW,
            0o644,
        )
    };
    if fd < 0 {
        return Err(format!("create: {}", std::io::Error::last_os_error()));
    }
    let mut out = unsafe { File::from_raw_fd(fd) };
    let mut remaining = len;
    let mut pos = 0usize;
    let started = Instant::now();
    while remaining > 0 {
        if started.elapsed() > BODY_PHASE_CAP {
            // Caller's Err path removes the .part; the socket just dies.
            return Err("upload exceeded time cap".into());
        }
        let take = remaining.min((prefix.len() - pos) as u64) as usize;
        if take > 0 {
            out.write_all(&prefix[pos..pos + take])
                .map_err(|e| format!("write: {}", e))?;
            pos += take;
            remaining -= take as u64;
            continue;
        }
        let mut chunk = [0u8; 65536];
        let n = stream
            .read(&mut chunk)
            .map_err(|e| format!("read: {}", e))?;
        if n == 0 {
            return Err("truncated upload".into());
        }
        // Cap the write to the declared Content-Length. Without the cap,
        // `remaining -= n` underflows to u64::MAX, the loop never ends, and
        // the MAX_BODY ceiling above is bypassed by a client that lies
        // about the length while streaming gigabytes.
        let take = (n as u64).min(remaining) as usize;
        if take == 0 {
            return Err("body exceeds declared Content-Length".into());
        }
        out.write_all(&chunk[..take])
            .map_err(|e| format!("write: {}", e))?;
        remaining -= take as u64;
    }
    out.sync_all().map_err(|e| format!("fsync: {}", e))?;
    drop(out);
    // Final guard before the swap: the early exists() check happened
    // before the body arrived; re-check at commit time so the window is
    // as close to zero as the single-threaded accept loop allows.
    if std::path::Path::new(final_path).exists() {
        let _ = std::fs::remove_file(part);
        return Err("file appeared during upload".into());
    }
    std::fs::rename(part, final_path).map_err(|e| format!("rename: {}", e))?;
    Ok(())
}

fn respond(stream: &mut TcpStream, code: u16, reason: &str, ctype: &str, body: &str) {
    respond_with_headers(stream, code, reason, ctype, body, &[]);
}

fn respond_with_headers(
    stream: &mut TcpStream,
    code: u16,
    reason: &str,
    ctype: &str,
    body: &str,
    extra_headers: &[&str],
) {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        code,
        reason,
        ctype,
        body.len()
    );
    for h in extra_headers {
        head.push_str(h);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
}

fn read_small_body(
    stream: &mut TcpStream,
    initial: &[u8],
    content_length: Option<u64>,
    max_len: usize,
) -> Option<Vec<u8>> {
    let mut body = initial.to_vec();
    let needed = match content_length {
        Some(cl) => {
            if cl as usize > max_len {
                return None;
            }
            cl as usize
        }
        None => initial.len(),
    };
    while body.len() < needed {
        let mut chunk = [0u8; 1024];
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                body.extend_from_slice(&chunk[..n]);
                if body.len() > max_len {
                    return None;
                }
            }
        }
    }
    Some(body)
}

fn extract_param_str(body: &str, key: &str) -> Option<String> {
    if let Some(v) = ybdev::devices::extract_json_str(body, key) {
        return Some(v);
    }
    query_param(body, key)
}

fn current_ssid() -> Option<String> {
    if let Ok(out) = Command::new("lipc-get-prop")
        .args(["-i", "com.lab126.wifid", "essid"])
        .output()
    {
        if out.status.success() {
            if let Ok(s) = String::from_utf8(out.stdout) {
                let t = s.trim();
                if !t.is_empty() && t != "none" && t != "null" {
                    return Some(t.to_string());
                }
            }
        }
    }
    if let Ok(out) = Command::new("iwgetid").args(["-r"]).output() {
        if out.status.success() {
            if let Ok(s) = String::from_utf8(out.stdout) {
                let t = s.trim();
                if !t.is_empty() {
                    return Some(t.to_string());
                }
            }
        }
    }
    None
}

// ---- screen ---------------------------------------------------------------

enum Phase {
    /// Wi-Fi + listener setup runs on a thread so the panel still paints.
    Starting,
    Ready {
        /// Full QR payload, `?t=` included.
        url: String,
        /// The pin, shown big under the address for typed entry.
        pin: String,
        ssid: Option<String>,
    },
    Failed(String),
}

type SetupResult = Result<(ReceiveServer, String, Option<String>), String>;
type SetupSlot = Arc<Mutex<Option<SetupResult>>>;

pub struct ReceiveScreen {
    phase: Phase,
    stop: Arc<AtomicBool>,
    /// Filled by the setup thread, taken by on_tick.
    setup: SetupSlot,
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
        *self.setup.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let stop = Arc::clone(&self.stop);
        let slot = Arc::clone(&self.setup);
        std::thread::spawn(move || {
            let r = (|| -> Result<(ReceiveServer, String, Option<String>), String> {
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
                    "http://{}:{}/?t={}",
                    ip.unwrap_or_else(|| "127.0.0.1".into()),
                    srv.port(),
                    srv.token()
                );
                let ssid = current_ssid();
                Ok((srv, url, ssid))
            })();
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
        });
    }
}

impl Screen for ReceiveScreen {
    fn default_edges(&self) -> bool {
        true
    }

    fn on_enter(&mut self) -> Action {
        crate::awake::screen_wants_awake(true);
        self.start_setup();
        Action::RedrawFull
    }

    fn on_leave(&mut self) {
        crate::awake::screen_wants_awake(false);
        self.stop.store(true, Ordering::Relaxed);
        if let Some(s) = &mut self.server {
            s.shutdown();
        }
    }

    fn holds_awake(&self) -> bool {
        true
    }

    fn tick_interval(&self) -> Duration {
        Duration::from_millis(300)
    }

    fn on_tick(&mut self) -> Action {
        if matches!(self.phase, Phase::Starting) {
            match self.setup.lock().unwrap_or_else(|e| e.into_inner()).take() {
                Some(Ok((srv, url, ssid))) => {
                    self.qr = QrCode::new(url.as_bytes()).ok();
                    // The credential must not land in the persistent log —
                    // the address alone is enough to diagnose reachability.
                    plog("receive: listening (pin-gated)");
                    let pin = srv.token().to_string();
                    self.phase = Phase::Ready { url, pin, ssid };
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
            Phase::Ready { url, pin, ssid } => {
                if let Some(ssid_name) = ssid {
                    let s = format!("Wi-Fi: {}", ssid_name);
                    let trunc = p.truncate(8.5, &s, 140.0);
                    p.text_right(w - pt(16.0), pt(24.0), 8.5, 100, &trunc);
                }

                // QR: e-ink's ideal payload — static, pure black/white.
                // 4-module quiet zone, scaled to fit, drawn once.
                let top = bar_h + pt(24.0);
                let target = (w * 2 / 5).min(h - top - pt(262.0));
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
                    // Address without the credential, then the code on
                    // its own line — the two ways in, both typable.
                    let base = url.split("?t=").next().unwrap_or(url.as_str());
                    p.text_center(ty, 11.5, 0, base);
                    p.text_center(ty + pt(22.0), 13.5, 0, &format!("code {}", pin));
                    if let Some(s) = ssid {
                        p.text_center(
                            ty + pt(42.0),
                            8.5,
                            110,
                            &format!("connect phone/laptop to “{}”", s),
                        );
                    } else {
                        p.text_center(
                            ty + pt(42.0),
                            8.5,
                            110,
                            "scan QR, or open the address and type the code",
                        );
                    }
                    p.text_center(
                        ty + pt(56.0),
                        8.5,
                        130,
                        "drop books or screensavers onto the page to send them",
                    );

                    let (n, last) = self
                        .server
                        .as_ref()
                        .map(|s| s.status())
                        .unwrap_or((0, None));
                    let sy = ty + pt(86.0);
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

        p.text_center(
            h - pt(16.0),
            8.5,
            130,
            "Swipe up from bottom right corner to exit",
        );
        let cw = pt(18.0);
        let ch = pt(18.0);
        p.hline_t(h - pt(8.0), w - cw - pt(8.0), w - pt(8.0), 2, 160);
        p.rect(Rect::new(w - pt(10.0), h - ch - pt(8.0), 2, ch), 160);
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        let (vw, vh) = (self.dims.0 as u32, self.dims.1 as u32);
        if g.corner_back() || g.corner_back_in(vw, vh) {
            return Action::Pop;
        }
        match g {
            Gesture::Tap { .. } | Gesture::TwoFingerTap => {
                if matches!(self.phase, Phase::Failed(_)) {
                    self.start_setup();
                    Action::RedrawFull
                } else {
                    Action::Keep
                }
            }
            _ => Action::Keep,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ybdev::devices::TrustedDevice;

    /// The socket tests mutate process-global env (YB_SAVE_DIR / YB_SS_DIR)
    /// and start real listeners, so they must not run concurrently.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Inject `t=<pin>` into a raw test request's target so it passes the
    /// session gate. Handles targets with or without an existing query;
    /// headers and body pass through untouched.
    fn with_pin(pin: &str, raw: &str) -> String {
        let mut parts = raw.splitn(3, ' ');
        let method = parts.next().unwrap_or_default();
        let target = parts.next().unwrap_or_default();
        let rest = parts.next().unwrap_or_default();
        let sep = if target.contains('?') { '&' } else { '?' };
        format!("{} {}{}t={} {}", method, target, sep, pin, rest)
    }

    #[test]
    fn upload_roundtrip_and_extension_guard() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join("yb-receive-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("YB_SAVE_DIR", dir.to_str().unwrap());
        let ss_dir = std::env::temp_dir().join("yb-receive-ss-test");
        let _ = std::fs::remove_dir_all(&ss_dir);
        std::fs::create_dir_all(&ss_dir).unwrap();
        std::env::set_var("YB_SS_DIR", ss_dir.to_str().unwrap());

        let stop = Arc::new(AtomicBool::new(false));
        let mut srv = ReceiveServer::start(Arc::clone(&stop)).expect("server");
        let port = srv.port();
        let pin = srv.token().to_string();

        // Good upload: raw body, query-encoded name with a space, split
        // across two writes (headers first, body after).
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let body = b"fake epub bytes";
        let req = with_pin(
            &pin,
            &format!(
                "POST /upload?name=test%20book.epub HTTP/1.1\r\nHost: t\r\nContent-Length: {}\r\n\r\n",
                body.len()
            ),
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
        c.write_all(
            with_pin(&pin, "POST /upload?name=evil.sh HTTP/1.1\r\nHost: t\r\nContent-Length: 2\r\n\r\nhi")
                .as_bytes(),
        )
        .unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(resp.starts_with(b"HTTP/1.1 415"));
        assert!(!dir.join("evil.sh").exists());

        // MOBI is refused for the same reason it's not in the library
        // allowlist: no parser exists, so accepting it would only set
        // up an open error later.
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(
            with_pin(&pin, "POST /upload?name=book.mobi HTTP/1.1\r\nHost: t\r\nContent-Length: 2\r\n\r\nhi")
                .as_bytes(),
        )
        .unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(resp.starts_with(b"HTTP/1.1 415"));
        assert!(!dir.join("book.mobi").exists());

        // Path traversal is neutralized by sanitize_fetch_name: the file
        // lands under its basename inside the save dir, never outside it.
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(
            with_pin(&pin, "POST /upload?name=..%2F..%2Fescape.epub HTTP/1.1\r\nHost: t\r\nContent-Length: 1\r\n\r\nx")
                .as_bytes(),
        )
        .unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(
            resp.starts_with(b"HTTP/1.1 200"),
            "{}",
            String::from_utf8_lossy(&resp)
        );
        assert_eq!(std::fs::read(dir.join("escape.epub")).unwrap(), b"x");
        assert!(!dir.parent().unwrap().join("escape.epub").exists());

        // GET serves the upload page.
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(with_pin(&pin, "GET / HTTP/1.1\r\nHost: t\r\n\r\n").as_bytes()).unwrap();
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
        c.write_all(with_pin(&pin, "GET / HTTP/1.1\r\nHost: t\r\n\r\n").as_bytes()).unwrap();
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

        // Test /api/mkdir
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(
            with_pin(&pin, "POST /api/mkdir?name=Sci-Fi HTTP/1.1\r\nHost: t\r\nContent-Length: 0\r\n\r\n")
                .as_bytes(),
        )
        .unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(
            resp.starts_with(b"HTTP/1.1 200"),
            "mkdir failed: {}",
            String::from_utf8_lossy(&resp)
        );
        assert!(dir.join("Sci-Fi").is_dir());

        // Test /upload with dir parameter
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let body2 = b"dune content";
        let req2 = with_pin(
            &pin,
            &format!(
                "POST /upload?dir=Sci-Fi&name=Dune.epub HTTP/1.1\r\nHost: t\r\nContent-Length: {}\r\n\r\n",
                body2.len()
            ),
        );
        c.write_all(req2.as_bytes()).unwrap();
        c.write_all(body2).unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(
            resp.starts_with(b"HTTP/1.1 200"),
            "upload to dir failed: {}",
            String::from_utf8_lossy(&resp)
        );
        assert_eq!(
            std::fs::read(dir.join("Sci-Fi").join("Dune.epub")).unwrap(),
            body2
        );

        // Test /api/list
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(with_pin(&pin, "GET /api/list HTTP/1.1\r\nHost: t\r\n\r\n").as_bytes())
            .unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(resp.starts_with(b"HTTP/1.1 200"));
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(resp_str.contains("\"folders\":[\"Sci-Fi\"]"));
        assert!(resp_str.contains("\"all_folders\":[\"Sci-Fi\"]"));

        // Test /api/move
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(
            with_pin(&pin, "POST /api/move?src_dir=&name=test%20book.epub&dst_dir=Sci-Fi HTTP/1.1\r\nHost: t\r\nContent-Length: 0\r\n\r\n")
                .as_bytes(),
        )
        .unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(
            resp.starts_with(b"HTTP/1.1 200"),
            "move failed: {}",
            String::from_utf8_lossy(&resp)
        );
        assert!(dir.join("Sci-Fi").join("test book.epub").exists());
        assert!(!dir.join("test book.epub").exists());

        // Test /api/delete
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(
            with_pin(&pin, "POST /api/delete?dir=Sci-Fi&name=test%20book.epub HTTP/1.1\r\nHost: t\r\nContent-Length: 0\r\n\r\n")
                .as_bytes(),
        )
        .unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(
            resp.starts_with(b"HTTP/1.1 200"),
            "delete failed: {}",
            String::from_utf8_lossy(&resp)
        );
        assert!(!dir.join("Sci-Fi").join("test book.epub").exists());

        // ---- screensavers root ----------------------------------------
        // A PNG lands in the screensavers dir; an epub is refused there
        // for the same reason mobi is refused in documents: the consumer
        // (the sleep-screen picker) can't render it.
        let png = b"fake png bytes";
        let req = with_pin(
            &pin,
            &format!(
                "POST /upload?root=screensavers&name=cover.png HTTP/1.1\r\nHost: t\r\nContent-Length: {}\r\n\r\n",
                png.len()
            ),
        );
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(req.as_bytes()).unwrap();
        c.write_all(png).unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(
            resp.starts_with(b"HTTP/1.1 200"),
            "ss png upload: {}",
            String::from_utf8_lossy(&resp)
        );
        assert_eq!(std::fs::read(ss_dir.join("cover.png")).unwrap(), png);

        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(
            with_pin(&pin, "POST /upload?root=screensavers&name=book.epub HTTP/1.1\r\nHost: t\r\nContent-Length: 2\r\n\r\nhi")
                .as_bytes(),
        )
        .unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(
            resp.starts_with(b"HTTP/1.1 415"),
            "epub must be refused in screensavers: {}",
            String::from_utf8_lossy(&resp)
        );
        assert!(!ss_dir.join("book.epub").exists());

        // Flat root: mkdir and move are refused server-side, not just
        // hidden in the UI.
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(
            with_pin(&pin, "POST /api/mkdir?root=screensavers&name=walls HTTP/1.1\r\nHost: t\r\nContent-Length: 0\r\n\r\n")
                .as_bytes(),
        )
        .unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(resp.starts_with(b"HTTP/1.1 400"));
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(
            with_pin(&pin, "POST /api/move?root=screensavers&src_dir=&name=cover.png&dst_dir= HTTP/1.1\r\nHost: t\r\nContent-Length: 0\r\n\r\n")
                .as_bytes(),
        )
        .unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(resp.starts_with(b"HTTP/1.1 400"));

        // List shows the image, hides a macOS AppleDouble sidecar the
        // same way the device scan does, and a bogus root is rejected.
        let _ = std::fs::write(ss_dir.join("._cover.jpg"), b"finder junk");
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(
            with_pin(&pin, "GET /api/list?root=screensavers HTTP/1.1\r\nHost: t\r\n\r\n").as_bytes(),
        )
        .unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(
            resp_str.starts_with("HTTP/1.1 200"),
            "ss list: {}",
            resp_str
        );
        assert!(resp_str.contains("cover.png"));
        assert!(
            !resp_str.contains("_cover.jpg"),
            "AppleDouble sidecar leaked: {}",
            resp_str
        );

        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(
            with_pin(&pin, "GET /api/list?root=/etc HTTP/1.1\r\nHost: t\r\n\r\n").as_bytes(),
        )
        .unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(
            resp.starts_with(b"HTTP/1.1 400"),
            "bogus root accepted: {}",
            String::from_utf8_lossy(&resp)
        );

        // Test /api/file download & preview
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(
            with_pin(&pin, "GET /api/file?dir=Sci-Fi&name=Dune.epub HTTP/1.1\r\nHost: t\r\n\r\n")
                .as_bytes(),
        )
        .unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(
            resp.starts_with(b"HTTP/1.1 200"),
            "api/file failed: {}",
            String::from_utf8_lossy(&resp)
        );
        assert!(resp.windows(20).any(|w| w == b"application/epub+zip"));

        // Delete works in the screensavers root too.
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(
            with_pin(&pin, "POST /api/delete?root=screensavers&dir=&name=cover.png HTTP/1.1\r\nHost: t\r\nContent-Length: 0\r\n\r\n")
                .as_bytes(),
        )
        .unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(resp.starts_with(b"HTTP/1.1 200"));
        assert!(!ss_dir.join("cover.png").exists());

        srv.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&ss_dir);
    }

    #[test]
    fn lying_content_length_is_capped_not_underflowed() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join("yb-receive-len-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("YB_SAVE_DIR", dir.to_str().unwrap());

        let stop = Arc::new(AtomicBool::new(false));
        let mut srv = ReceiveServer::start(Arc::clone(&stop)).expect("server");
        let port = srv.port();
        let pin = srv.token().to_string();

        // Claim 1 byte, stream 100 KB. The body writer must cap the write
        // to the declared length — before the fix `remaining -= n`
        // underflowed (u64::MAX), the loop never ended, and the MAX_BODY
        // ceiling was bypassable with a single connection.
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        c.write_all(
            with_pin(
                &pin,
                "POST /upload?name=onebyte.epub HTTP/1.1\r\nHost: t\r\nExpect: 100-continue\r\nContent-Length: 1\r\n\r\n",
            )
            .as_bytes(),
        )
        .unwrap();
        // The 100-continue round-trip makes the header phase provably
        // complete, so the 1 declared byte must be satisfied by the
        // stream-read path — where the (old) `remaining -= n` underflowed.
        let mut interim = [0u8; 64];
        let n = c.read(&mut interim).unwrap();
        assert!(
            String::from_utf8_lossy(&interim[..n]).starts_with("HTTP/1.1 100"),
            "no 100-continue: {}",
            String::from_utf8_lossy(&interim[..n])
        );
        c.write_all(&[0x42; 100 * 1024]).unwrap();
        let mut resp = Vec::new();
        // The declared body is satisfied and the excess is discarded with
        // the connection; closing a socket with unread data makes the peer
        // see RST, so accept either the response or a reset. The invariant
        // under test is the bounded write, asserted on disk below.
        let _ = c.read_to_end(&mut resp);
        assert!(
            resp.is_empty() || resp.starts_with(b"HTTP/1.1 200"),
            "declared-1-body upload: {}",
            String::from_utf8_lossy(&resp)
        );
        // Exactly the declared byte is persisted; the excess is discarded
        // with the connection.
        assert_eq!(std::fs::read(dir.join("onebyte.epub")).unwrap(), b"\x42");

        srv.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn amazon_system_files_are_protected_from_delete_and_move() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join("yb-receive-sysfiles-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("YB_SAVE_DIR", dir.to_str().unwrap());
        std::fs::write(dir.join("My Clippings.txt"), b"keep me").unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let mut srv = ReceiveServer::start(Arc::clone(&stop)).expect("server");
        let port = srv.port();
        let pin = srv.token().to_string();

        let delete = with_pin(
            &pin,
            "POST /api/delete?dir=&name=My%20Clippings.txt HTTP/1.1\r\nHost: t\r\nContent-Length: 0\r\n\r\n",
        );
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(delete.as_bytes()).unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(resp.starts_with(b"HTTP/1.1 400"), "delete: {}", String::from_utf8_lossy(&resp));
        assert_eq!(std::fs::read(dir.join("My Clippings.txt")).unwrap(), b"keep me");

        let mv = with_pin(
            &pin,
            "POST /api/move?src_dir=&name=My%20Clippings.txt&dst_dir=Old HTTP/1.1\r\nHost: t\r\nContent-Length: 0\r\n\r\n",
        );
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(mv.as_bytes()).unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(resp.starts_with(b"HTTP/1.1 400"), "move: {}", String::from_utf8_lossy(&resp));
        assert_eq!(std::fs::read(dir.join("My Clippings.txt")).unwrap(), b"keep me");

        srv.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unauthenticated_requests_are_gated() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join("yb-receive-auth-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("YB_SAVE_DIR", dir.to_str().unwrap());

        let stop = Arc::new(AtomicBool::new(false));
        let mut srv = ReceiveServer::start(Arc::clone(&stop)).expect("server");
        let port = srv.port();
        let pin = srv.token().to_string();

        // Bare address (typed, not scanned): the pairing page, never the
        // file manager.
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(b"GET / HTTP/1.1\r\nHost: t\r\n\r\n").unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        let body = String::from_utf8_lossy(&resp);
        assert!(body.starts_with("HTTP/1.1 403"), "{}", body);
        assert!(body.contains("name=\"t\""), "pairing form missing: {}", body);

        // Wrong pin: same fate.
        let wrong = if pin == "000000" { "111111" } else { "000000" };
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(with_pin(wrong, "GET / HTTP/1.1\r\nHost: t\r\n\r\n").as_bytes())
            .unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(resp.starts_with(b"HTTP/1.1 403"));

        // APIs reject pinless requests too — plain text, not HTML.
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(b"GET /api/list HTTP/1.1\r\nHost: t\r\n\r\n").unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(resp.starts_with(b"HTTP/1.1 403"));
        assert!(!resp.windows(9).any(|w| w == b"text/html"));

        // X-YB-Token is the header alternative (curl / scripts).
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let h = format!("GET / HTTP/1.1\r\nHost: t\r\nX-YB-Token: {}\r\n\r\n", pin);
        c.write_all(h.as_bytes()).unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(
            resp.starts_with(b"HTTP/1.1 200"),
            "{}",
            String::from_utf8_lossy(&resp)
        );

        srv.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pin_helpers() {
        // Shape: six digits.
        let a = generate_pin();
        assert_eq!(a.len(), PIN_LEN, "{a}");
        assert!(a.chars().all(|c| c.is_ascii_digit()), "{a}");

        // Transcription tolerance: spaces, dashes and case fold away.
        assert_eq!(normalize_code("482 913"), "482913");
        assert_eq!(normalize_code("482-913"), "482913");
        assert_eq!(normalize_code("ab12"), "AB12");

        // Compare: exact true; single-digit difference false; length
        // mismatch false.
        assert!(tokens_match("482913", "482913"));
        assert!(!tokens_match("482913", "482914"));
        assert!(!tokens_match("482913", "4829"));

        let mut devices = DeviceStore::default();
        devices.add_or_update(TrustedDevice::new(
            "test_dev",
            "Test Device",
            "tok_sec_trusted_token_123",
            None,
            "inbound",
        ));

        // Gate decisions: query param, header, transcription tolerance,
        // wrong value, absence, device store tokens, cookie tokens.
        assert!(authorized("482913", "dir=&t=482913", None, None, &devices));
        assert!(authorized("482913", "dir=x&t=482%20913", None, None, &devices));
        assert!(authorized("482913", "", Some(" 482-913"), None, &devices));
        assert!(authorized("482913", "", None, Some("tok_sec_trusted_token_123"), &devices));
        assert!(authorized("482913", "dir=&t=tok_sec_trusted_token_123", None, None, &devices));
        assert!(!authorized("482913", "dir=&t=111111", None, None, &devices));
        assert!(!authorized("482913", "dir=&root=books", None, None, &devices));
    }

    #[test]
    fn handshake_and_pairing_api() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("yb_handshake_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("YB_SAVE_DIR", dir.to_str().unwrap());
        let dev_file = dir.join("devices.json");
        let kindle_file = dir.join("kindle_id.json");
        std::env::set_var("YB_DEVICES_PATH", dev_file.to_str().unwrap());
        std::env::set_var("YB_KINDLE_ID_PATH", kindle_file.to_str().unwrap());

        let stop = Arc::new(AtomicBool::new(false));
        let mut srv = ReceiveServer::start(Arc::clone(&stop)).expect("server");
        let port = srv.port();
        let pin = srv.token().to_string();

        // 1. GET /api/handshake works unauthenticated
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(b"GET /api/handshake HTTP/1.1\r\nHost: t\r\n\r\n").unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(resp_str.starts_with("HTTP/1.1 200 OK"), "{}", resp_str);
        assert!(resp_str.contains("kindle_id"));
        assert!(resp_str.contains("kindle_name"));

        // 2. POST /api/pair with valid PIN registers device
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let pair_body = format!("{{\"pin\":\"{}\",\"name\":\"MacBook Air\",\"id\":\"mac_air_1\"}}", pin);
        let req = format!(
            "POST /api/pair HTTP/1.1\r\nHost: t\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            pair_body.len(),
            pair_body
        );
        c.write_all(req.as_bytes()).unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(resp_str.starts_with("HTTP/1.1 200 OK"), "{}", resp_str);
        assert!(resp_str.contains("Set-Cookie: yb_token="));
        assert!(resp_str.contains("HttpOnly"), "session cookie must be HttpOnly");
        let token = ybdev::devices::extract_json_str(&resp_str, "token").unwrap();

        // 3. GET / with cookie token is immediately authorized (200, not 403 pairing page)
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let req = format!("GET / HTTP/1.1\r\nHost: t\r\nCookie: yb_token={}\r\n\r\n", token);
        c.write_all(req.as_bytes()).unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(resp_str.starts_with("HTTP/1.1 200 OK"), "{}", resp_str);
        assert!(resp_str.contains("Kindle File Manager"));

        // 4. Revoking devices on disk immediately blocks the token on the running server
        let empty_store = DeviceStore::default();
        empty_store.save(dev_file.to_str().unwrap()).unwrap();

        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let req = format!("GET / HTTP/1.1\r\nHost: t\r\nCookie: yb_token={}\r\n\r\n", token);
        c.write_all(req.as_bytes()).unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(resp_str.starts_with("HTTP/1.1 403 Forbidden"), "revoked token was not rejected: {}", resp_str);

        // 5. Pairing rate limiter locks out after too many failed attempts
        let now_sec = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as usize)
            .unwrap_or(0);
        LAST_PAIR_FAIL_SECS.store(now_sec, Ordering::Relaxed);
        FAILED_PAIR_ATTEMPTS.store(MAX_PAIR_ATTEMPTS, Ordering::Relaxed);
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let bad_body = "{\"pin\":\"000000\",\"name\":\"Attacker\"}";
        let req = format!(
            "POST /api/pair HTTP/1.1\r\nHost: t\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            bad_body.len(),
            bad_body
        );
        c.write_all(req.as_bytes()).unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(resp_str.starts_with("HTTP/1.1 429 Too Many Requests"), "lockout failed: {}", resp_str);

        // 6. Pairing after Revoke All registers new device without resurrecting the revoked one
        FAILED_PAIR_ATTEMPTS.store(0, Ordering::Relaxed);
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let pair_body2 = format!("{{\"pin\":\"{}\",\"name\":\"Linux PC\",\"id\":\"linux_pc_1\"}}", pin);
        let req2 = format!(
            "POST /api/pair HTTP/1.1\r\nHost: t\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            pair_body2.len(),
            pair_body2
        );
        c.write_all(req2.as_bytes()).unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        let resp_str2 = String::from_utf8_lossy(&resp);
        assert!(resp_str2.starts_with("HTTP/1.1 200 OK"), "{}", resp_str2);
        let token2 = ybdev::devices::extract_json_str(&resp_str2, "token").unwrap();

        // New token works
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let req = format!("GET / HTTP/1.1\r\nHost: t\r\nCookie: yb_token={}\r\n\r\n", token2);
        c.write_all(req.as_bytes()).unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(resp.starts_with(b"HTTP/1.1 200 OK"));

        // Old revoked token is still rejected (never resurrected)
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let req = format!("GET / HTTP/1.1\r\nHost: t\r\nCookie: yb_token={}\r\n\r\n", token);
        c.write_all(req.as_bytes()).unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(resp.starts_with(b"HTTP/1.1 403 Forbidden"), "revoked token was resurrected!");

        srv.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn renders_receive_screen_to_png() {
        let font = yui::font::Font::load().unwrap();
        let mut s = ReceiveScreen::new();
        let url = "http://192.168.1.50:8080/?t=482913".to_string();
        s.qr = QrCode::new(url.as_bytes()).ok();
        s.phase = Phase::Ready {
            url,
            pin: "482913".to_string(),
            ssid: Some("HomeStudio_5G".to_string()),
        };
        let mut canvas = vec![0u8; 1236 * 1648];
        let mut panel = vec![255u8; 1248 * 1648];
        let mut p = yui::Painter::new(
            &mut panel,
            1236,
            1648,
            1248,
            yui::Orientation::Portrait,
            &mut canvas,
            &font,
        );
        s.draw(&mut p);
        // Temp dir, not a personal path: this test must run on any machine.
        let artifact_path = std::env::temp_dir().join("yb_receive_screen_device.png");
        let file = std::fs::File::create(&artifact_path).unwrap();
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), 1236, 1648);
        enc.set_color(png::ColorType::Grayscale);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header()
            .unwrap()
            .write_image_data(&canvas)
            .unwrap();
    }
}
