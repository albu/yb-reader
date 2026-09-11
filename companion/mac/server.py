#!/usr/bin/env python3
"""Kindle Mirror — Mac side.

Serves grayscale frames of the reader page to a Kindle client over HTTP,
and turns Kindle taps into page turns, clicks and scrolls. The page runs
in this server's own headless Chromium (mac/headless.py): frames are page
screenshots, input goes in through Playwright — no visible window, no
macOS permission grants.

  GET  /status            -> JSON: mode, browser info, fb params, counters
  GET  /frame?w&h&depth&stride  -> raw frame, fb-native layout
  POST /tap   {"x","y"}   -> click at the mapped point inside the page
      (x,y may also come from the query string: /tap?x=&y=&wait=1&id=)
  POST /key?k=&shift=     -> post a named key (space, left, ...) to the page
  POST /scroll?dx=&dy=    -> scroll-wheel event, deltas in client fb px
  POST /rewin            -> no-op (kept for protocol compatibility)
  POST /browser?headed=  -> show/hide the reader window

  /next, /prev, /key, /tap, /scroll share the turn semantics: `id=` makes
  them idempotent across Kindle retries, `wait=1` answers with the
  post-action settled PNG (X-Changed / X-Settled headers) instead of JSON.

Run:  uv run server.py                      # resumes the last page
      uv run server.py --url <reading-url>  # first visit
"""

import argparse
import json
import os
import signal
import socket
import struct
import subprocess
import sys
import threading
import time
import urllib.parse
import zlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import numpy as np

import pairing

# Kindles send key *names*; the values are mac virtual-key codes, which
# the headless backend translates into Playwright key names.
KEY_CODES = {
    "left": 0x7B, "right": 0x7C, "up": 0x7E, "down": 0x7D,
    "space": 0x31, "return": 0x24, "escape": 0x35, "tab": 0x30,
    "home": 0x73, "end": 0x77, "pageup": 0x74, "pagedown": 0x79,
}

LAST_URL_PATH = os.path.expanduser("~/.yb-mirror-last-url")
LAST_KINDLE_PATH = os.path.expanduser("~/.yb-mirror-last-kindle")


def log(*a):
    print(f"[{time.strftime('%H:%M:%S')}]", *a, file=sys.stderr, flush=True)


def read_last_url():
    try:
        with open(LAST_URL_PATH) as f:
            url = f.read().strip()
        return url if url.startswith("http") else None
    except OSError:
        return None


def save_last_url(url):
    if not url or not url.startswith("http"):
        return
    try:
        with open(LAST_URL_PATH, "w") as f:
            f.write(url + "\n")
        log(f"remembered page: {url}")
    except OSError as e:
        log(f"remembering page failed: {e}")


def parent_watchdog(parent_pid, interval=5):
    """Exit when the process that spawned us is gone (only used with
    --parent-pid, i.e. when started from the menu bar app). A hard app
    death (crash, force quit) reparents us to launchd; noticing that and
    leaving beats lingering as an orphan no Stop button can reach."""
    while True:
        if os.getppid() != parent_pid:
            log(f"parent {parent_pid} gone — exiting")
            # Via SIGTERM, not os._exit: the handler in main() closes the
            # headless browser first, so Chromium can't outlive us holding
            # the profile lock.
            os.kill(os.getpid(), signal.SIGTERM)
            time.sleep(5)  # only reached if the handler didn't fire
            os._exit(0)
        time.sleep(interval)


def remember_loop(mirror, interval=60):
    """Background: keep ~/.yb-mirror-last-url pointed at whatever the
    browser is reading, so the next cold start can resume the book
    (--resume, the default) instead of the catalog. The URL is a cached
    attribute of the browser object — nothing to query, nothing to
    mis-attribute. save_last_url ignores non-HTTP values (no page loaded
    yet, about:blank)."""
    while True:
        try:
            mirror.save_url(mirror.current_url())
        except Exception as e:  # never let the rememberer kill the server
            log(f"rememberer: {e}")
        time.sleep(interval)


# ------------------------------------------------------------- keep-awake ---

def system_keepawake_loop(mirror, idle_grace, tick=5):
    """Headless twin of stay_awake, activity-scoped: hold the no-idle-sleep
    assertion only while the Kindle is in contact, and release it
    idle_grace seconds after the last request. A Mac left on the nightstand
    then sleeps itself (its own idle timer takes over; closing the lid
    works as always), instead of burning the night on the chance of one
    more page. Sleep is a pause, not session loss: the server and its
    browser wake with the Mac and the next Kindle tap just works.

    The Kindle's /ping heartbeat counts as contact — an open-but-idle
    mirror screen keeps the Mac awake by design (it IS about to be read).
    Putting the Kindle to sleep kills its radio, which starts the
    countdown. Same -t/-w bounds as the display loop: this thread dying
    can't hold the Mac forever, and a restarted server can't leak children.
    """
    proc = None
    while True:
        active = mirror.last_activity > 0 and \
            time.time() - mirror.last_activity < idle_grace
        if active and (proc is None or proc.poll() is not None):
            proc = subprocess.Popen(
                ["caffeinate", "-i", "-w", str(os.getpid()),
                 "-t", str(idle_grace * 2)],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            mirror.keep_awake = True
            mirror.idle_grace = idle_grace
            log("system keep-awake: on (Kindle mirror active)")
        elif not active and proc is not None and proc.poll() is None:
            proc.terminate()
            proc = None
            mirror.keep_awake = False
            log("system keep-awake: off (mirror idle) — the Mac may sleep")
        time.sleep(tick)


def stay_awake():
    """Hold a no-idle-sleep assertion for this process's lifetime. A Mac
    that dozes off mid-read drops Wi-Fi and the mirror with it; running
    caffeinate ourselves means the launch command needs no wrapper (and a
    forgotten wrapper can't sink the session)."""
    try:
        subprocess.Popen(["caffeinate", "-i", "-w", str(os.getpid())],
                         stdout=subprocess.DEVNULL,
                         stderr=subprocess.DEVNULL)
        log("stay-awake: on (caffeinate -i follows this process)")
    except OSError as e:
        log(f"stay-awake failed: {e}")


# ---------------------------------------------------------------- frames ---

def png_gray(gray, bits=8, level=1):
    """Grayscale PNG from a uint8 array, stdlib only (zlib + chunks).

    bits=4 packs two samples per byte (e-ink only shows 16 levels anyway),
    halving the payload the Kindle has to download over Wi-Fi. Levels are
    rounded to nearest (not truncated): `>> 4` biased every pixel down by
    up to 15/255, greying out near-white anti-aliasing and paper tones.
    level=1 trades ~15% bigger payloads for ~2.5x faster encoding — the
    Kindle waits on this per page turn.
    """
    h, w = gray.shape
    if bits == 8:
        raw = b"".join(b"\x00" + gray[i].tobytes() for i in range(h))
    elif bits == 4:
        g4 = np.minimum((gray.astype(np.uint16) + 8) >> 4, 15)
        wpad = (w + 1) // 2
        packed = np.zeros((h, wpad), np.uint8)
        hi = g4[:, 0::2] << 4
        if w % 2 == 0:
            lo = g4[:, 1::2]
        else:
            lo = np.zeros((h, wpad), np.uint16)
            lo[:, : w // 2] = g4[:, 1::2]
        packed[:, :] = (hi | lo).astype(np.uint8)
        raw = b"".join(b"\x00" + packed[i].tobytes() for i in range(h))
    else:
        raise ValueError(f"unsupported PNG bit depth {bits}")

    def chunk(tag, data):
        body = tag + data
        return (struct.pack(">I", len(data)) + body +
                struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF))

    ihdr = struct.pack(">IIBBBBB", w, h, bits, 0, 0, 0, 0)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) +
            chunk(b"IDAT", zlib.compress(raw, level)) + chunk(b"IEND", b""))


def z4_compress(gray, level=1):
    """Packed 4-bit grayscale compressed with zlib.

    Drops PNG headers, chunking, CRC32, and row-filter bytes completely.
    On the Kindle side, miniz_oxide decompresses directly into the packed
    buffer and unpacks in ~10 ms total (vs ~90 ms for png_gray).
    """
    h, w = gray.shape
    g4 = np.minimum((gray.astype(np.uint16) + 8) >> 4, 15)
    wpad = (w + 1) // 2
    packed = np.zeros((h, wpad), np.uint8)
    hi = g4[:, 0::2] << 4
    if w % 2 == 0:
        lo = g4[:, 1::2]
    else:
        lo = np.zeros((h, wpad), np.uint16)
        lo[:, : w // 2] = g4[:, 1::2]
    packed[:, :] = (hi | lo).astype(np.uint8)
    return zlib.compress(packed.tobytes(), level)


def resize_gray(gray, fbw, fbh):
    """Resize to (fbh, fbw) with bilinear interpolation.

    The previous nearest-neighbour sampler made anti-aliased text look dirty:
    every pixel maps to exactly one source pixel, so a mild upscale (e.g.
    1148x1682 retina -> 1236x1648) doubles some columns/rows but not others,
    producing jagged, uneven letters. Bilinear interpolation keeps text
    smooth for roughly the same cost.
    """
    h, w = gray.shape
    if (w, h) == (fbw, fbh):
        return gray
    ys = (np.arange(fbh) + 0.5) * (h / fbh) - 0.5
    xs = (np.arange(fbw) + 0.5) * (w / fbw) - 0.5
    y0 = np.floor(ys).astype(int).clip(0, h - 1)
    x0 = np.floor(xs).astype(int).clip(0, w - 1)
    y1 = (y0 + 1).clip(0, h - 1)
    x1 = (x0 + 1).clip(0, w - 1)
    fy = (ys - y0).astype(np.float32)[:, None]
    fx = (xs - x0).astype(np.float32)[None, :]
    g = gray.astype(np.float32)
    top = g[y0][:, x0] * (1.0 - fx) + g[y0][:, x1] * fx
    bot = g[y1][:, x0] * (1.0 - fx) + g[y1][:, x1] * fx
    out = top * (1.0 - fy) + bot * fy
    return np.clip(out, 0, 255).astype(np.uint8)


def gamma_lut(gamma):
    """Darken mid-tones so text reads black instead of gray on e-ink.

    Real pages rarely contain true black: anti-aliased text and dark-gray
    body copy (e.g. #333) convert to gray ~40-70, and e-ink's low native
    contrast then makes everything look washed out. A gamma curve keeps
    white at 255 but maps those grays far closer to black:
      out = 255 * (in / 255) ^ gamma
    gamma == 1.0 gives an identity table. Returned as a 256-entry LUT
    because indexing the final frame with it is ~2x faster than np.power
    over the 2M-pixel image.
    """
    return np.clip(255.0 * (np.arange(256) / 255.0) ** gamma,
                   0, 255).astype(np.uint8)


def to_fb_layout(gray, fbw, fbh, depth, stride):
    """Nearest-neighbour resize + pack to framebuffer-native bytes."""
    gray = resize_gray(gray, fbw, fbh)
    if depth == 16:  # RGB565 little-endian
        g = gray.astype(np.uint16)
        packed = ((g >> 3) << 11) | ((g >> 2) << 5) | (g >> 3)
        stride_px = stride // 2
        buf = np.zeros((fbh, stride_px), np.uint16)
        buf[:, :fbw] = packed
    elif depth == 32:  # gray replicated; channel order is irrelevant then
        quad = np.repeat(gray, 4, axis=1)  # R,G,B,X each = gray
        buf = np.zeros((fbh, stride), np.uint8)
        buf[:, : fbw * 4] = quad
    else:
        raise ValueError(f"unsupported fb depth {depth}")
    return buf.tobytes()


# ---------------------------------------------------------------- server ---

class Mirror:
    """Frame machinery shared by the headless backend: everything between
    "one gray ndarray" (the subclass's _capture) and "a settled PNG reply
    on the wire" — gamma, resize, fb layout, loader detection, stability
    confirmation, turn idempotency, fb parameter memory. The window-capture
    backend this once subclassed directly is gone; _capture/key/tap/
    scroll/autosize live only on HeadlessMirror now."""

    def __init__(self, app, contrast=2.0):
        self.app = app
        self.contrast = contrast
        self.png_bits = 8
        self.fmt = "png"
        self.lut = gamma_lut(contrast)
        self.wait_stats = {}  # last frame_after_change timings, for the log
        self.last_turn_id = None  # idempotency key of the last page turn
        self.last_gray = None  # last served frame, for wait-for-change
        self.geo = None  # served rect (page CSS px) of the last capture
        self.last_activity = 0.0  # last Kindle contact; drives keep-awake
        self.lock = threading.Lock()
        self.kindle_lock = threading.Lock()
        self.last_kindle_ip = None   # learned from the Kindle's own requests
        self.last_kindle_seen = 0.0
        self.fb = None          # dict from client: w,h,depth,stride
        self.seq = 0

    def note_client(self, ip):
        """Remember the Kindle's address from the handshake: every request
        the Kindle makes (frame polls, page turns, pings) carries its source
        IP, so no address ever has to be configured or hardcoded. Persisted
        so the menu bar can reopen the Kindle's web manager later, and the
        paired-device record can attach a name once /api/pair has seen it.
        Loopback (our own /status probes) is ignored."""
        if not ip or ip.startswith("127.") or ip == "::1":
            return
        with self.kindle_lock:
            if ip == self.last_kindle_ip:
                return
            self.last_kindle_ip = ip
            self.last_kindle_seen = time.time()
        try:
            with open(LAST_KINDLE_PATH, "w") as f:
                f.write(ip + "\n")
            log(f"remembered Kindle at {ip}")
        except OSError as e:
            log(f"remembering kindle ip failed: {e}")

    def frame(self, fmt="raw"):
        # One _capture() round trip per poll: the subclass (HeadlessMirror)
        # submits a screenshot job to the browser thread and returns the
        # final gray ndarray; everything here stays engine-agnostic.
        with self.lock:
            gray = self._capture()
            self.last_gray = gray
            fbw, fbh = gray.shape[1], gray.shape[0]
            if fmt == "png":
                payload = png_gray(gray, bits=self.png_bits)
            else:
                depth = (self.fb or {}).get("depth", 16)
                stride = (self.fb or {}).get("stride", fbw * depth // 8)
                payload = to_fb_layout(gray, fbw, fbh, depth, stride)
            self.seq += 1
            return payload, fbw, fbh

    LOADER_RAW_FRACTION = 0.03  # PNG below 3% of packed size = loader screen
    # Cap on the loader-wait phase. _is_loader cannot tell a sparse FINAL
    # page (a chapter end with a few rows of text, ~20 KB encoded) from a
    # real loading screen — both encode tiny — so the wait must be short:
    # waiting the full change deadline on a final sparse page stalled
    # every chapter-end turn for seconds. The Kindle shows the early
    # frame and polls cheaply; a real loader is corrected within a tick.
    LOADER_WAIT_S = 1.2

    def _is_loader(self, gray):
        """True if the frame looks like a loading screen rather than a page.

        Measured from live logs: real text pages encode to 137-160 KB,
        sparse chapter-end pages to ~73 KB, loading screens to ~10 KB — a
        7x gap, far more robust than any pixel heuristic (a static loader
        defeats frame-stability checks; a sparse title page defeats
        ink-coverage ones).
        """
        if getattr(self, "fmt", "png") == "z4":
            probe = z4_compress(gray)
        else:
            probe = png_gray(gray, bits=self.png_bits)
        packed = gray.shape[0] * gray.shape[1] // 2
        return len(probe) < self.LOADER_RAW_FRACTION * packed

    def _settle(self, gray, tries=4, gap=0.03):
        """Wait for frame stability: two consecutive equal captures. A
        finished page is stable on the very next capture, so this costs one
        confirmation capture on normal turns; it only runs long when the
        screen is genuinely mid-transition (animated spinner etc.)."""
        for _ in range(tries):
            time.sleep(gap)
            nxt = self._capture()
            if np.array_equal(nxt, gray):
                return gray, True
            gray = nxt
        return gray, False

    def _wait_out_loader(self, gray, deadline):
        """A chapter boundary renders a static 'loading' screen before the
        text; stability alone can't tell it from a finished (sparse) page,
        but its encoded size can. Wait for real content, deadline-bounded."""
        while time.time() < deadline and self._is_loader(gray):
            time.sleep(0.12)
            gray = self._capture()
        return gray

    def _png_reply(self, gray, changed, settled, start, polls):
        """Encode a frame as PNG or z4, update state, and record wait_stats."""
        self.last_gray = gray
        fbw, fbh = gray.shape[1], gray.shape[0]
        t_enc = time.time()
        if getattr(self, "fmt", "png") == "z4":
            payload = z4_compress(gray)
        else:
            payload = png_gray(gray, bits=self.png_bits)
        self.seq += 1
        self.wait_stats = {
            "polls": polls, "changed": changed, "settled": settled,
            "detect_s": round(time.time() - start, 3),
            "encode_ms": round((time.time() - t_enc) * 1000, 1),
        }
        return payload, fbw, fbh

    def frame_after_change(self, timeout=3.0, poll=0.02,
                           settle_tries=4, settle_gap=0.03, trust=False):
        """Capture until the frame differs from the last served one, then
        wait out loading screens and confirm stability before returning the
        PNG. All under one deadline (the client's request timeout is only a
        couple of seconds beyond it). Falls back to the current frame on
        timeout, so the client always gets something back.

        `trust` (scrolls): report X-Settled even without stability
        confirmation — the page is *expected* to still be animating, and
        the client's fallback poll would cost far more than a frame a few
        px behind the final position. The log records the truth. Scrolls
        also take a short timeout: at a scroll boundary no change ever
        comes, and the default 3 s would hang.
        """
        with self.lock:
            start = time.time()
            deadline = start + timeout
            prev = self.last_gray
            polls = 0
            while True:
                gray = self._capture()
                polls += 1
                if prev is None or not np.array_equal(gray, prev):
                    break
                if time.time() >= deadline:
                    return self._png_reply(gray, False,
                                           not self._is_loader(gray),
                                           start, polls)
                time.sleep(poll)
            gray = self._wait_out_loader(
                gray, min(deadline, time.time() + self.LOADER_WAIT_S))
            gray, stable = self._settle(gray, tries=settle_tries,
                                        gap=settle_gap)
            settled = (stable or trust) and not self._is_loader(gray)
            return self._png_reply(gray, True, settled, start, polls)

    def frame_settled(self):
        """Current frame as PNG, loader-waited and stability-confirmed
        (plain frame GETs and duplicate-turn replies, where shipping a
        mid-transition capture would leave the Kindle a page behind)."""
        with self.lock:
            start = time.time()
            gray = self._capture()
            # An unchanged frame cannot benefit from the loader wait: the
            # page has not moved since we served it, and a sparse final
            # page (which _is_loader misreads as a loader) would burn the
            # full wait on EVERY poll — 3 s per request, the amplifier
            # behind the chapter-end stalls.
            unchanged = (self.last_gray is not None
                         and np.array_equal(gray, self.last_gray))
            if not unchanged:
                gray = self._wait_out_loader(gray,
                                             start + self.LOADER_WAIT_S)
            gray, stable = self._settle(gray)
            settled = stable and not self._is_loader(gray)
            return self._png_reply(gray, True, settled, start, 1)


def run_discovery(tcp_port, udp_port):
    """Answer 'ybmirror' UDP probes so the Kindle can find this Mac with no
    configuration at all: the plugin broadcasts on udp/<port+1> and uses the
    reply's *source address* as the server IP (nothing in the payload to
    misparse), learning the TCP port from the reply body. The reply also
    announces id=/name= (pairing.announcement) so the reader's trust check
    can attach a paired token to this server."""
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
    try:
        s.bind(("", udp_port))
    except OSError as e:
        log(f"discovery off (udp/:{udp_port} bind failed: {e})")
        return
    log(f"discovery listening on udp/:{udp_port}")
    while True:
        try:
            data, addr = s.recvfrom(256)
            if data.startswith(b"ybmirror"):
                # The reader's second probe carries its own identity:
                # "ybmirror-discover id=<kindle_id> nonce=<hex>". Answer
                # with THAT Kindle's paired device_id (so a Mac serving
                # several Kindles announces the right identity to each) and
                # mac=HMAC-SHA256(token, nonce) — the proof that this Mac
                # holds the pairing, so the reader can trust the reply from
                # any IP (a host that merely echoes the id gets nothing).
                kindle_id = None
                nonce = None
                text = data.decode("utf-8", "replace")
                if " id=" in text:
                    kindle_id = text.split(" id=", 1)[1].split(" ", 1)[0].strip() or None
                if " nonce=" in text:
                    nonce = text.split(" nonce=", 1)[1].split(" ", 1)[0].strip() or None
                s.sendto(pairing.announcement(tcp_port, kindle_id, nonce).encode(),
                         addr)
        except OSError:
            continue


class QuietHTTPServer(ThreadingHTTPServer):
    """The Kindle's power-saving radio drops TCP connections with a reset
    every ~20-30 idle seconds; socketserver's default reaction is a full
    traceback per drop, which reads as an crash. Only unexpected errors
    keep their traceback."""

    def handle_error(self, request, client_address):
        exc = sys.exc_info()[1]
        if isinstance(exc, (ConnectionResetError, BrokenPipeError,
                            TimeoutError)):
            log(f"client {client_address[0]} dropped the connection "
                f"({type(exc).__name__}) — routine, the Kindle re-connects")
            return
        super().handle_error(request, client_address)


class Handler(BaseHTTPRequestHandler):
    mirror: Mirror = None
    # HTTP/1.1 keep-alive: every response path sets Content-Length, so the
    # Kindle can hold one TCP connection for the whole reading session
    # instead of paying a TCP handshake + slow start on every page turn.
    # Idle connections are reaped after 30 s (Kindle asleep, Wi-Fi gone).
    protocol_version = "HTTP/1.1"
    timeout = 30

    def _json(self, obj, code=200, cors=False):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        if cors:
            self._cors_headers()
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _cors_headers(self):
        """CORS for the receive page's pairing handshake: the page runs on
        http://<kindle-ip>:8080 and talks to http://localhost:8765. Only the
        pairing endpoints get this; /status and the frame endpoints stay
        CORS-free so a hostile web page can't read them."""
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
        self.send_header("Access-Control-Allow-Headers", "Content-Type")
        self.send_header("Access-Control-Allow-Private-Network", "true")
        self.send_header("Access-Control-Max-Age", "86400")

    def do_OPTIONS(self):
        """Preflight for the browser's cross-origin /api/pair POST."""
        self.send_response(204)
        self._cors_headers()
        self.send_header("Content-Length", "0")
        self.end_headers()

    @staticmethod
    def _is_loopback(client):
        ip = client[0]
        return ip.startswith("127.") or ip == "::1"

    def _admin_only(self):
        """Mac-side admin endpoints (/status, /rewin, /autosize, /browser,
        /api/pair) are localhost-only: the pairing browser runs on this
        Mac, and the reader never calls them, so a LAN peer must not be
        able to read the paired identities or drive the reader browser."""
        if not self._is_loopback(self.client_address):
            self._json({"error": "admin endpoint — localhost only"}, 403)
            return False
        return True

    def _query(self):
        """First value of every query-string param, as a plain dict."""
        if "?" not in self.path:
            return {}
        return {k: v[0] for k, v in urllib.parse.parse_qs(
            self.path.split("?", 1)[1]).items()}

    def _apply_frame_params(self):
        """Frame geometry/depth from the query string, so any frame-bearing
        endpoint carries them (the client shouldn't depend on an earlier
        /frame request having configured the server, e.g. after a restart)."""
        q = self._query()
        if not q:
            return
        m = self.mirror
        m.fb = {k: int(q[k]) for k in ("w", "h", "depth", "stride") if k in q}
        if "bpp" in q:
            m.png_bits = int(q["bpp"])
        if "fmt" in q:
            m.fmt = q["fmt"]

    def _authorized(self):
        """True when a Kindle-facing request may proceed. Enforcement is
        OFF until at least one Kindle has paired (see pairing.authorized);
        after that, the request must carry the paired token as
        X-YB-Secret (X-YB-Kindle-Id says which Kindle it is, optional)."""
        return pairing.authorized(
            self.headers.get("X-YB-Kindle-Id") or None,
            self.headers.get("X-YB-Secret") or "")

    def do_GET(self):
        m = self.mirror
        # Kindle-facing endpoints refresh the activity clock that scopes
        # the display keep-awake; /status (our own curl probes) does not.
        if self.path.startswith(("/ping", "/frame")):
            if not self._authorized():
                return self._json(
                    {"error": "unauthorized — open the receive page from "
                              "the Mac and re-pair this Kindle"}, 401)
            m.last_activity = time.time()
            m.note_client(self.client_address[0])
        if self.path.startswith("/ping"):
            return self._json({"ok": True})
        if self.path.startswith("/health"):
            # Liveness for probes (menu bar app, curl): side-effect free.
            # Deliberately does NOT touch last_activity — /ping is the
            # Kindle heartbeat and the keep-awake signal; a probe
            # refreshing it would keep the Mac awake with no Kindle
            # anywhere near.
            return self._json({"ok": True}, cors=True)
        if self.path.startswith("/status"):
            if not self._admin_only():
                return
            paired = pairing.load()
            return self._json({
                "mode": m.mode,
                "app": m.app,
                "browser": m.status(),
                "fb": m.fb, "seq": m.seq, "geo": m.geo,
                "kindle": {
                    "ip": m.last_kindle_ip,
                    "last_seen": m.last_kindle_seen or None,
                },
                "paired": [{"id": k, "name": v.get("name")}
                           for k, v in paired.items()],
            })
        if self.path.startswith("/frame"):
            self._apply_frame_params()
            if self.path.startswith("/frame.png") and "fmt" not in self._query():
                m.fmt = "png"
            elif self.path.startswith("/frame.z4"):
                m.fmt = "z4"
            if self.path.startswith(("/frame.png", "/frame.z4")) or "fmt" in self._query():
                try:
                    payload, w, h = m.frame_settled()
                except Exception as e:
                    return self._json({"error": str(e)}, 503)
                self._png(payload, w, h, {
                    "X-Settled": "1" if m.wait_stats["settled"] else "0",
                }, f"frame #{m.seq} {len(payload)}B "
                   f"settled={m.wait_stats['settled']}")
                return
            try:
                payload, w, h = m.frame("raw")
            except Exception as e:
                return self._json({"error": str(e)}, 503)
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Content-Length", str(len(payload)))
            self.send_header("X-Seq", str(m.seq))
            self.send_header("X-Win-Size", f"{w}x{h}")
            self.end_headers()
            self.wfile.write(payload)
            log(f"frame #{m.seq} {len(payload)}B")
            return
        self._json({"error": "not found"}, 404)

    def _png(self, payload, w, h, extra=None, log_line=None):
        """Send a frame reply: body plus the X-* headers clients poll on."""
        m = self.mirror
        fmt = getattr(m, "fmt", "png")
        content_type = "application/x-z4" if fmt == "z4" else "image/png"
        self.send_response(200)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(payload)))
        self.send_header("X-Seq", str(m.seq))
        if fmt == "z4":
            self.send_header("X-Fmt", "z4")
        for k, v in (extra or {}).items():
            self.send_header(k, v)
        self.send_header("X-Win-Size", f"{w}x{h}")
        self.end_headers()
        self.wfile.write(payload)
        if log_line:
            log(log_line)

    def _turn_reply(self, act, label, turn_id, wait, fast=False):
        """Run one Kindle action (key press / click / scroll) with the turn
        semantics shared by /next, /prev, /key, /tap and /scroll:

        - `id=` idempotency: a retried request (Kindle radio dropped the
          response) carries the same id — repeating the action would skip
          a page, so answer with the current frame instead.
        - `wait=1`: one-request action — do it, wait for the window
          content to actually change, return the new PNG.
        - `fast` (scrolls): short change timeout, light settle, trusted
          X-Settled — see frame_after_change.
        """
        m = self.mirror
        dup = turn_id is not None and turn_id == m.last_turn_id
        if wait and dup:
            # Capturing "right now" can catch a chapter loading screen —
            # which used to ship as final truth (X-Changed: 1, no
            # polling) and left the Kindle a page behind Safari. The
            # settled capture fixes that; X-Settled lets the plugin keep
            # polling if even this didn't land on stable content.
            try:
                self._apply_frame_params()
                payload, w, h = m.frame_settled()
            except Exception as e:
                return self._json({"error": str(e)}, 503)
            self._png(payload, w, h, {
                "X-Changed": "1",  # no slow-site polling
                "X-Settled": "1" if m.wait_stats["settled"] else "0",
                "X-Dup": "1",
            }, f"turn: duplicate id={turn_id} — frame only, "
               f"settled={m.wait_stats['settled']}")
            return
        if not dup:
            try:
                act()
            except Exception as e:
                return self._json({"error": str(e)}, 503)
            if turn_id is not None:
                m.last_turn_id = turn_id
        if wait:
            self._apply_frame_params()
            try:
                if fast:
                    payload, w, h = m.frame_after_change(
                        timeout=0.6, settle_tries=2, settle_gap=0.05,
                        trust=True)
                else:
                    # Not first-change speculation: measured 2026-08-22
                    # on web reader pages with turn animations, the first capture that differs
                    # after a key is the OLD page plus the opening sliver
                    # of the turn animation (early payloads matched the
                    # previous settled frame's size, turn after turn), so
                    # the client gained a serial correction round trip
                    # and never a usable early paint.
                    payload, w, h = m.frame_after_change(timeout=3.0)
            except Exception as e:
                return self._json({"error": str(e)}, 503)
            st = m.wait_stats
            self._png(payload, w, h, {
                "X-Changed": "1" if st["changed"] else "0",
                "X-Settled": "1" if st["settled"] else "0",
            }, f"turn: site+capture={st['detect_s']:.2f}s "
               f"({st['polls']} polls, changed={st['changed']}, "
               f"settled={st['settled']}) "
               f"encode={st['encode_ms']:.0f}ms {len(payload)}B")
            return
        return self._json(label)

    def do_POST(self):
        m = self.mirror
        q = self._query()
        if self.path.startswith(("/next", "/prev", "/key", "/scroll", "/tap")):
            if not self._authorized():
                return self._json(
                    {"error": "unauthorized — open the receive page from "
                              "the Mac and re-pair this Kindle"}, 401)
            m.last_activity = time.time()
            m.note_client(self.client_address[0])
        wait = "wait" in q
        turn_id = q.get("id")
        n = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(n) if n else b""
        if self.path.startswith("/api/pair"):
            if not self._admin_only():
                return
            # Completes the pairing the Kindle's receive page starts: its
            # browser POSTs the PIN-minted token here after /api/pair on
            # the Kindle succeeds (see receive_page.html — this endpoint is
            # what that fetch() is talking to).
            try:
                p = json.loads(body or b"{}")
            except ValueError:
                return self._json({"error": "bad json"}, 400)
            kid = p.get("kindle_id") or ""
            name = p.get("kindle_name") or "Kindle"
            tok = p.get("token") or ""
            dev_id = p.get("device_id") or ""
            if not kid or not tok:
                return self._json(
                    {"error": "kindle_id and token are required"}, 400)
            ok = pairing.pair(kid, name, tok, dev_id)
            if ok:
                log(f"paired Kindle {name!r} ({kid})")
            return self._json({"ok": ok}, cors=True)
        if self.path.startswith("/api/challenge"):
            # Pairing self-heal: the reader proves this Mac still holds the
            # pairing by challenging it with a fresh nonce (HMAC-SHA256 of
            # the nonce with the paired token). A matching answer lets the
            # reader refresh its stored IP after a DHCP change — nothing
            # secret travels in the request.
            mac = pairing.challenge(q.get("kindle_id", ""), q.get("nonce", ""))
            if mac is None:
                return self._json({"error": "unknown kindle"}, 404)
            return self._json({"mac": mac})
        if self.path.startswith("/tap"):
            # Coordinates from the query string (the Rust client's HTTP
            # layer sends no bodies) or the legacy JSON body.
            p = json.loads(body or b"{}")
            x = int(q.get("x", p.get("x", 0)))
            y = int(q.get("y", p.get("y", 0)))
            clicked = []

            def act():
                clicked[:] = m.tap(x, y)

            return self._turn_reply(act, {"clicked": clicked}, turn_id, wait)
        if self.path.startswith(("/next", "/prev", "/key")):
            if self.path.startswith("/next"):
                name, shift = "right", False
            elif self.path.startswith("/prev"):
                name, shift = "left", False
            else:
                name = q.get("k", "")
                if name not in KEY_CODES:
                    return self._json(
                        {"error": f"unknown key {name!r}; "
                                  f"one of {sorted(KEY_CODES)}"}, 400)
                shift = q.get("shift") in ("1", "true")
            return self._turn_reply(
                lambda: m.key(KEY_CODES[name], shift),
                {"key": name, "shift": shift}, turn_id, wait)
        if self.path.startswith("/scroll"):
            try:
                dx = int(q.get("dx", 0))
                dy = int(q.get("dy", 0))
            except ValueError:
                return self._json({"error": "dx/dy must be integers"}, 400)
            if dx == 0 and dy == 0:
                return self._json({"error": "zero scroll"}, 400)
            scrolled = []

            def act():
                scrolled[:] = m.scroll(dx, dy)

            return self._turn_reply(act, {"scroll": scrolled}, turn_id, wait,
                                    fast=True)
        if self.path.startswith("/rewin"):
            # Kept for protocol compatibility: nothing to re-find, the
            # browser relaunches itself if dead (HeadlessMirror.rewin).
            if not self._admin_only():
                return
            return self._json({"window": bool(m.rewin())})
        if self.path.startswith("/browser"):
            # Headed toggle: relaunch the server's own Chromium with/without
            # a visible window — the log-in-and-browse path (menu bar "Show
            # reader window"). Loopback-only like every admin endpoint.
            # Blocks a few seconds inside the handler's 30 s timeout;
            # ?headed=1|0 forces a side, bare = toggle.
            if not self._admin_only():
                return
            want = {"1": True, "0": False}.get(q.get("headed"),
                                               not m.status()["headed"])
            try:
                return self._json(m.set_headed(want))
            except Exception as e:
                return self._json({"error": str(e)}, 503)
        if self.path.startswith("/autosize"):
            if not self._admin_only():
                return
            return self._json({"autosized": bool(m.autosize())})
        self._json({"error": "not found"}, 404)

    def log_message(self, *a):  # quiet default request logging
        pass


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--css-width", type=int, default=618,
                    help="CSS viewport width; device scale is 2, so a 618 "
                         "viewport screenshots at 1236 px — the Kindle fb "
                         "width (default 618)")
    ap.add_argument("--headed", action="store_true",
                    help="launch with a visible window (the CLI form of the "
                         "menu bar's Show reader window; toggle back any "
                         "time via POST /browser)")
    ap.add_argument("--contrast", type=float, default=2.0,
                    help="gamma applied to grayscale frames; higher = darker "
                         "text/blacks, 1.0 = unchanged (default 2.0)")
    ap.add_argument("--url", default="",
                    help="open this URL at startup (before serving)")
    ap.add_argument("--no-resume", action="store_true",
                    help="don't auto-open the last remembered page at "
                         "startup (resuming is the default; an explicit "
                         "--url always wins)")
    ap.add_argument("--idle-sleep", type=float, default=600,
                    help="seconds of Kindle quiet before the server "
                         "releases its no-sleep assertion and the Mac may "
                         "sleep again (default 600 — matched to the "
                         "Kindle's own sleep rhythm; 0 = hold for the "
                         "server's lifetime). Contact = any request, "
                         "heartbeat included; sleep is a pause, waking "
                         "the Mac resumes the session")
    ap.add_argument("--no-stay-awake", action="store_true",
                    help="don't hold a caffeinate no-idle-sleep assertion "
                    "at all (the default is activity-scoped)")
    ap.add_argument("--port", type=int, default=8765)
    ap.add_argument("--parent-pid", type=int, default=0,
                    help="exit when this pid disappears (used by the "
                         "menu bar app so a crashed app can't leave an "
                         "orphaned server behind)")
    args = ap.parse_args()

    def _bye(*_):
        # SIGTERM (menu bar stop, pkill, parent watchdog) must close the
        # browser: an orphaned Chromium keeps the profile's ProcessSingleton
        # lock and every later start fails. Bounded close, then a hard exit
        # — half-alive is worse than dead.
        try:
            if Handler.mirror is not None:
                Handler.mirror.close()
        except Exception:
            pass
        os._exit(0)

    signal.signal(signal.SIGTERM, _bye)

    if args.parent_pid:
        threading.Thread(target=parent_watchdog, args=(args.parent_pid,),
                         daemon=True).start()

    try:
        from headless import make_headless_mirror
    except ImportError as e:
        log(f"the server needs playwright ({e}) — run `uv sync`")
        raise SystemExit(1)
    # The factory passes our Mirror + resize_gray in: importing them
    # from headless would fork the __main__ module's state.
    Handler.mirror = make_headless_mirror(Mirror, resize_gray)(
        contrast=args.contrast, css_width=args.css_width,
        headed=args.headed, key_codes=KEY_CODES,
        read_url=read_last_url, save_url=save_last_url)
    Handler.mirror.start(args.url, resume=not args.no_resume)
    # Night reading: hold the no-sleep assertion only while the Kindle is
    # in contact. Seeded so a just-started server can't be slept out from
    # under the first tap; after --idle-sleep seconds of quiet the Mac's
    # own idle timer (or the lid) takes over.
    Handler.mirror.last_activity = time.time()
    if not args.no_stay_awake:
        if args.idle_sleep > 0:
            threading.Thread(target=system_keepawake_loop,
                             args=(Handler.mirror, args.idle_sleep),
                             daemon=True).start()
        else:
            stay_awake()
    threading.Thread(target=remember_loop, args=(Handler.mirror,),
                     daemon=True).start()
    try:
        srv = QuietHTTPServer(("0.0.0.0", args.port), Handler)
    except OSError as e:
        if e.errno == 48:
            log(f"port {args.port} is already taken — another server instance "
                f"is running. Find it with `lsof -iTCP:{args.port} -sTCP:LISTEN` "
                f"and kill it, or start this one with --port.")
        raise
    threading.Thread(target=run_discovery, args=(args.port, args.port + 1),
                     daemon=True).start()
    log(f"listening on :{args.port}  browser={Handler.mirror.app}")
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        log("bye")
        _bye()


if __name__ == "__main__":
    main()
