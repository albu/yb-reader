#!/usr/bin/env python3
"""Kindle Mirror — Mac side.

Serves grayscale frames of one application window to a Kindle client over
HTTP, and turns Kindle taps into real mouse clicks in that window.

  GET  /status            -> JSON: window info, client fb params, counters
  GET  /frame?w&h&depth&stride[&win_w&win_h]  -> raw frame, fb-native layout
  POST /tap   {"x","y"}   -> click at the mapped point inside the window
      (x,y may also come from the query string: /tap?x=&y=&wait=1&id=)
  POST /key?k=&shift=     -> post a named key (space, left, ...) to the app
  POST /scroll?dx=&dy=    -> scroll-wheel event, deltas in client fb px
  POST /rewin            -> re-discover the window (after switching books)

  /next, /prev, /key, /tap, /scroll share the turn semantics: `id=` makes
  them idempotent across Kindle retries, `wait=1` answers with the
  post-action settled PNG (X-Changed / X-Settled headers) instead of JSON.

Run:  uv run server.py [--app "Google Chrome"] [--title substr] [--port 8765]

macOS permissions (System Settings -> Privacy & Security):
  - Screen Recording  : for `screencapture` of the window
  - Accessibility     : for posting synthetic mouse events
"""

import argparse
import ctypes
import json
import os
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time
import urllib.parse
import zlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import numpy as np
from Quartz import (
    CGDisplayBounds,
    CGEventCreateKeyboardEvent,
    CGEventCreateMouseEvent,
    CGEventCreateScrollWheelEvent,
    CGEventPost,
    CGEventPostToPid,
    CGEventSetFlags,
    CGMainDisplayID,
    CGDataProviderCopyData,
    CGImageGetBitsPerPixel,
    CGImageGetBytesPerRow,
    CGImageGetDataProvider,
    CGImageGetHeight,
    CGImageGetWidth,
    CGWindowListCopyWindowInfo,
    CGWindowListCreateImage,
    CGRectNull,
    kCGEventFlagMaskShift,
    kCGEventKeyDown,
    kCGEventKeyUp,
    kCGEventLeftMouseDown,
    kCGEventLeftMouseUp,
    kCGEventMouseMoved,
    kCGHIDEventTap,
    kCGScrollEventUnitPixel,
    kCGWindowBounds,
    kCGWindowImageBoundsIgnoreFraming,
    kCGWindowLayer,
    kCGWindowListOptionAll,
    kCGWindowListOptionIncludingWindow,
    kCGWindowListOptionOnScreenOnly,
    kCGWindowName,
    kCGWindowNumber,
    kCGWindowOwnerName,
    CGPointMake,
)

import pairing

# Kindles send key *names*; the values are mac virtual-key codes.
KEY_CODES = {
    "left": 0x7B, "right": 0x7C, "up": 0x7E, "down": 0x7D,
    "space": 0x31, "return": 0x24, "escape": 0x35, "tab": 0x30,
    "home": 0x73, "end": 0x77, "pageup": 0x74, "pagedown": 0x79,
}

BMP_PATH = tempfile.gettempdir() + "/ybm_frame.bmp"
LAST_URL_PATH = os.path.expanduser("~/.yb-mirror-last-url")
LAST_KINDLE_PATH = os.path.expanduser("~/.yb-mirror-last-kindle")


def log(*a):
    print(f"[{time.strftime('%H:%M:%S')}]", *a, file=sys.stderr, flush=True)


def get_tab_urls(app):
    """[(window name, front-tab URL)] for every window of the app, via
    AppleScript (needs macOS Automation permission for this terminal -> the
    app; static script, nothing interpolated). Empty list on any failure."""
    script = (
        'tell application "' + app + '"\n'
        "    set out to \"\"\n"
        "    repeat with w in windows\n"
        "        set out to out & (name of w) & \"\\t\" & "
        "(URL of document of w) & \"\\n\"\n"
        "    end repeat\n"
        "    return out\n"
        "end tell")
    try:
        r = subprocess.run(["osascript", "-e", script],
                           capture_output=True, text=True, timeout=5)
    except (subprocess.TimeoutExpired, OSError):
        return []
    if r.returncode != 0:
        return []
    out = []
    for line in r.stdout.splitlines():
        name, _, url = line.partition("\t")
        if url.startswith("http"):
            out.append((name, url))
    return out


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
            os._exit(0)
        time.sleep(interval)


def display_keepawake_loop(mirror, idle_grace=1800, tick=5):
    """Hold the display awake exactly while the Kindle is actively using
    the mirror, and stop the moment it goes quiet.

    Mirroring needs a live rendering window, and a sleeping display kills
    window capture outright (verified: with the display asleep, requests
    fail with "no window" — and page-turn keys posted to Safari do NOT
    count as display activity, so ~displaysleep minutes into Kindle-only
    reading the mirror used to die mid-book). While the plugin is pinging
    or turning pages we hold a timed `caffeinate -d` and respawn it just
    before it lapses; when the Kindle stops (mirror closed), the assertion
    is dropped immediately and the normal display-sleep timer takes over.
    The -t lifetime also bounds the damage if this thread dies: the screen
    can never be held more than idle_grace*2 by a stale process."""
    proc = None
    while True:
        active = mirror.last_activity > 0 and \
            time.time() - mirror.last_activity < idle_grace
        if active and (proc is None or proc.poll() is not None):
            # -d only *prevents* display sleep, it doesn't undo one. After
            # a released gap the display may already be asleep (distraction
            # longer than the grace, Apple-menu sleep) — nudge it awake so
            # the mirror resumes by itself when the Kindle comes back.
            if mirror.display_released:
                subprocess.Popen(["caffeinate", "-u", "-t", "2"],
                                 stdout=subprocess.DEVNULL,
                                 stderr=subprocess.DEVNULL)
                log("display keep-awake: on (Kindle mirror active) "
                    "— waking display")
            else:
                log("display keep-awake: on (Kindle mirror active)")
            mirror.display_released = False
            # -w ties the assertion to this server's lifetime: a restarted
            # server used to orphan its caffeinate children (-t up to an
            # hour each) and they piled up across restarts.
            proc = subprocess.Popen(
                ["caffeinate", "-d", "-w", str(os.getpid()),
                 "-t", str(idle_grace * 2)],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        elif not active and proc is not None and proc.poll() is None:
            proc.terminate()
            proc = None
            mirror.display_released = True
            log("display keep-awake: off (mirror idle) — screen may sleep")
        time.sleep(tick)


def remember_loop(mirror, interval=60):
    """Background: keep ~/.yb-mirror-last-url pointed at whatever the
    mirrored window is reading, so the next cold start can resume the book
    (--resume, the default) instead of the catalog. Follows book switches:
    the window title *is* the book name, so when the remembered title stops
    matching (new book opened in the same window), the single-window case
    adopts the new title + URL; with several windows and no match, keep the
    old memory rather than risk saving the wrong window."""
    while True:
        try:
            if mirror.win is not None:
                tabs = get_tab_urls(mirror.app)
                pick = None
                for name, url in tabs:
                    if name == mirror.win[5]:
                        pick = (name, url)
                        break
                if pick is None and len(tabs) == 1:
                    pick = tabs[0]
                    if pick[0] != mirror.win[5]:
                        log(f"window now shows {pick[0]!r} — following")
                        mirror.win = mirror.win[:5] + (pick[0],)
                if pick:
                    save_last_url(pick[1])
        except Exception as e:  # never let the rememberer kill the server
            log(f"rememberer: {e}")
        time.sleep(interval)


# ---------------------------------------------------------------- window ---

def find_window(app, title_substr):
    """Return (win_id, x, y, w, h, name) for the best-matching window.

    Apps keep offscreen helper windows around, so we filter those out and
    take the largest real window rather than trusting list order. If a
    title substring is given, windows are first narrowed to those whose
    name contains it.
    """
    wins = CGWindowListCopyWindowInfo(kCGWindowListOptionAll, 0)
    matches = []
    for w in wins:
        if w.get(kCGWindowOwnerName) != app:
            continue
        if w.get(kCGWindowLayer, 1) != 0:      # desktop/widgets live on other layers
            continue
        b = w.get(kCGWindowBounds, {})
        if b.get("Width", 0) < 400 or b.get("Height", 0) < 300:
            continue
        if b.get("X", 0) < -50 or b.get("Y", 0) < -50:  # parked offscreen
            continue
        name = w.get(kCGWindowName) or ""
        if title_substr and title_substr.lower() not in name.lower():
            continue
        matches.append((w.get(kCGWindowNumber), b.get("X", 0), b.get("Y", 0),
                        int(b["Width"]), int(b["Height"]), name))
    if not matches:
        return None
    # A window can be found even when occluded, but never when it is
    # minimized; log what we see so the wrong-tab/wrong-window case is obvious.
    log("candidate windows: "
        + "; ".join(f"{m[5][:36]!r} {m[3]}x{m[4]} @({m[1]:.0f},{m[2]:.0f})"
                    for m in matches))
    return max(matches, key=lambda m: m[3] * m[4])


def activate_front(app):
    """Bring the app (and its front window) forward. With a single
    tab/window this is all that is needed to make the mirror show the same
    content the user is looking at, and it ensures the synthetic arrow
    keys land in the right app. Returns True on success, False if the
    script failed (missing Automation permission, ...)."""
    script = f'''
    tell application "{app}"
        activate
        set index of front window to 1
        return "ok"
    end tell'''
    r = subprocess.run(["osascript", "-e", script],
                       capture_output=True, text=True)
    if r.returncode == 0 and "ok" in r.stdout:
        log(f"activated {app}")
        return True
    log("activate failed: "
        + (r.stderr.strip() or r.stdout.strip() or f"rc={r.returncode}"))
    return False


def activate_app(app):
    """Bring the app to the foreground so synthetic keys land in it."""
    script = f'tell application "{app}" to activate'
    r = subprocess.run(["osascript", "-e", script],
                       capture_output=True, text=True)
    if r.returncode == 0:
        return True
    log("activate failed: "
        + (r.stderr.strip() or r.stdout.strip() or f"rc={r.returncode}"))
    return False


def _set_window_bounds(app, w, h):
    disp = CGDisplayBounds(CGMainDisplayID())
    x = int((disp.size.width - w) / 2)
    y = int((disp.size.height - h) / 2)
    script = f'''
    tell application "{app}"
        set bounds of front window to {{{x}, {y}, {x + w}, {y + h}}}
        set index of front window to 1
        return "ok"
    end tell'''
    r = subprocess.run(["osascript", "-e", script],
                       capture_output=True, text=True)
    if r.returncode == 0 and "ok" in r.stdout:
        return True
    log("set bounds failed: "
        + (r.stderr.strip() or r.stdout.strip() or f"rc={r.returncode}"))
    return False


def _get_window_bounds(app):
    script = f'tell application "{app}" to get bounds of front window'
    r = subprocess.run(["osascript", "-e", script],
                       capture_output=True, text=True)
    if r.returncode == 0:
        parts = r.stdout.strip().split(", ")
        if len(parts) == 4:
            left, top, right, bottom = map(int, parts)
            return right - left, bottom - top
    log("get bounds failed: "
        + (r.stderr.strip() or r.stdout.strip() or f"rc={r.returncode}"))
    return None


def open_url(app, url, timeout=8):
    """Open the reader in the app via LaunchServices (`open` needs no
    Automation permission, unlike AppleScript tab juggling), then wait for
    a qualifying window to exist so --autosize can size it."""
    subprocess.Popen(["open", "-a", app, url])
    deadline = time.time() + timeout
    while time.time() < deadline:
        time.sleep(0.5)
        if find_window(app, ""):
            return True
    return False


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


def autosize_window(app, crop):
    """Resize the app's front window to the optimal Kindle-mirror size.

    The Kindle screen is 1236x1648 (aspect 0.75), and the server resizes the
    *cropped* window content to exactly that. The ideal window is the largest
    one whose content has 0.75 aspect after the configured crop, so that
    nothing needs to be cropped or stretched. We set the size, re-read the
    actual bounds (the window server may clamp the height), then fix the
    width to match 0.75 of the real content height.
    """
    disp = CGDisplayBounds(CGMainDisplayID())
    usable_w = max(1, int(disp.size.width) - 80)    # side margins
    usable_h = max(1, int(disp.size.height) - 90)   # menu bar + dock
    aspect = 1236.0 / 1648.0
    content_w = min(usable_w, int(usable_h * aspect), 1100)
    content_h = int(content_w / aspect)
    ctop, cbottom, cleft, cright = crop
    win_w = content_w + int(cleft + cright)
    win_h = content_h + int(ctop + cbottom)
    if not _set_window_bounds(app, win_w, win_h):
        return False
    actual = _get_window_bounds(app) or (win_w, win_h)
    actual_w, actual_h = actual
    content_h2 = max(1, actual_h - int(ctop + cbottom))
    content_w2 = min(int(content_h2 * aspect), actual_w)
    win_w2 = content_w2 + int(cleft + cright)
    if win_w2 != actual_w:
        _set_window_bounds(app, win_w2, actual_h)
    log(f"autosized {app} window to {win_w2}x{actual_h} "
        f"(content {content_w2}x{content_h2})")
    return True


def _read_bmp():
    try:
        with open(BMP_PATH, "rb") as f:
            return f.read()
    except FileNotFoundError:
        return b""


def app_pid(app):
    """PID of a running app by name, via NSWorkspace (in-process, ~1 ms —
    an osascript or pgrep subprocess costs 15-100 ms)."""
    try:
        from AppKit import NSWorkspace
        for a in NSWorkspace.sharedWorkspace().runningApplications():
            if a.localizedName() == app:
                return a.processIdentifier()
    except Exception:
        pass
    return None


def app_is_frontmost(app):
    """True if `app` is the frontmost app (in-process, ~1 ms) — lets
    tap/scroll skip the ~100 ms osascript activation when the reader app
    is already front, which is the steady state while mirroring."""
    try:
        from AppKit import NSWorkspace
        return NSWorkspace.sharedWorkspace() \
            .frontmostApplication().localizedName() == app
    except Exception:
        return False


def quartz_capture_gray(win):
    """In-process window capture -> (gray, w, h, opaque_bbox), or None.

    CGWindowListCreateImage grabs the window by ID (works while occluded,
    same as `screencapture -l`), but needs no subprocess and no temp file:
    ~15 ms vs ~80 ms per capture on this window — and that cost is paid on
    every poll iteration while waiting for the page to change. With
    kCGWindowImageBoundsIgnoreFraming the shadow is never captured, so the
    alpha-bbox below only trims the rounded corners. Output verified
    pixel-identical to the screencapture path after its shadow crop.
    """
    cg = CGWindowListCreateImage(CGRectNull, kCGWindowListOptionIncludingWindow,
                                 win[0], kCGWindowImageBoundsIgnoreFraming)
    if cg is None:
        return None
    w, h = CGImageGetWidth(cg), CGImageGetHeight(cg)
    row, bpp = CGImageGetBytesPerRow(cg), CGImageGetBitsPerPixel(cg)
    if bpp != 32:
        return None
    buf = np.frombuffer(CGDataProviderCopyData(CGImageGetDataProvider(cg)),
                        np.uint8, h * row)
    bgra = buf.reshape(h, row)[:, : w * 4].reshape(h, w, 4)
    b, g, r, a = (bgra[..., i].astype(np.float32) for i in range(4))
    # premultiplied color over white, same math as the BMP path
    gray = np.clip((0.299 * r + 0.587 * g + 0.114 * b) + (255.0 - a),
                   0, 255).astype(np.uint8)
    opaque = bgra[..., 3] > 128
    if opaque.any():
        ys, xs = np.where(opaque)
        bbox = (int(xs.min()), int(ys.min()),
                int(xs.max()) + 1, int(ys.max()) + 1)
    else:
        bbox = (0, 0, w, h)
    return gray, w, h, bbox


def capture_gray(win):
    """Capture the window -> (gray, w, h, opaque_bbox), Quartz first."""
    r = quartz_capture_gray(win)
    if r is not None:
        return r
    return bmp_to_gray(capture_window(win))


def capture_window(win):
    """Screenshot the window (by CGWindowID, works while occluded) -> BMP bytes.

    Bounded by a timeout: an unbounded subprocess here (e.g. screencapture
    blocked on a permissions prompt) would hold the frame lock and wedge
    every request, which looks from the client exactly like a dead server."""
    win_id, x, y, w, h, _ = win
    data = b""
    try:
        subprocess.run(["screencapture", "-x", "-t", "bmp", "-l", str(win_id),
                        BMP_PATH], check=True, capture_output=True, timeout=5)
        data = _read_bmp()
    except subprocess.CalledProcessError:
        pass
    except subprocess.TimeoutExpired:
        raise RuntimeError("screencapture timed out — check Screen Recording "
                           "permission (no dialog waiting?)")
    if len(data) < 1000:  # window-id capture failed -> region fallback
        subprocess.run(["screencapture", "-x", "-t", "bmp",
                        "-R", f"{x},{y},{w},{h}", BMP_PATH],
                       check=True, capture_output=True, timeout=5)
        data = _read_bmp()
    if len(data) < 1000:
        raise RuntimeError(
            "screencapture produced nothing — check that the window is "
            "visible (not minimized) and that this terminal has Screen "
            "Recording permission (System Settings -> Privacy & Security)")
    return data


# ---------------------------------------------------------------- frames ---

def bmp_to_gray(data):
    """24/32bpp BMP bytes -> (gray uint8 [h,w], width, height, opaque_bbox).

    opaque_bbox is the bounding box of pixels with meaningful alpha, or None
    for formats without alpha. It lets the caller crop away the window's
    shadow and rounded corners before resizing.
    """
    off = struct.unpack_from("<I", data, 10)[0]
    w, h_signed = struct.unpack_from("<ii", data, 18)
    bpp = struct.unpack_from("<H", data, 28)[0]
    top_down = h_signed < 0
    h = abs(h_signed)
    row = ((w * bpp // 8 + 3) // 4) * 4
    img = np.frombuffer(data, np.uint8, h * row, off).reshape(h, row)
    if bpp == 24:
        rgb = img[:, : w * 3].reshape(h, w, 3)[:, :, ::-1]  # BGR -> RGB
        if not top_down:
            rgb = rgb[::-1]
        gray = (0.299 * rgb[..., 0] + 0.587 * rgb[..., 1] + 0.114 * rgb[..., 2])
        return gray.astype(np.uint8), w, h, None
    elif bpp == 32:
        # macOS screencapture writes 32bpp BI_BITFIELDS, stored as
        # premultiplied BGRA (masks R=0x00ff0000 G=0x0000ff00 B=0x000000ff
        # A=0xff000000). The old code treated the first three bytes as B,G,R
        # but then applied the 0.299/0.587/0.114 weights as if they were
        # R,G,B, and left premultiplied alpha in the values, so blue was
        # over-weighted, red under-weighted and transparent pixels (shadows,
        # rounded corners, un-captured windows) went black.
        # Premultiplied color over a white background is simply
        #   gray = premultiplied_gray + (255 - alpha)
        # which turns window shadows into light gray instead of black.
        bgra = img[:, : w * 4].reshape(h, w, 4)
        if not top_down:
            bgra = bgra[::-1]
        b, g, r, a = (bgra[..., i].astype(np.float32) for i in range(4))
        gray = (0.299 * r + 0.587 * g + 0.114 * b) + (255.0 - a)
        opaque = bgra[..., 3] > 128
        if opaque.any():
            ys, xs = np.where(opaque)
            bbox = (int(xs.min()), int(ys.min()),
                    int(xs.max()) + 1, int(ys.max()) + 1)
        else:
            bbox = (0, 0, w, h)
        return np.clip(gray, 0, 255).astype(np.uint8), w, h, bbox
    else:
        raise ValueError(f"unexpected BMP bpp {bpp}")


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
    def __init__(self, app, title, activate=False, contrast=2.0,
                 crop=(0.0, 0.0, 0.0, 0.0), no_shadow_crop=False,
                 no_aspect_crop=False):
        self.app, self.title, self.activate = app, title, activate
        self.contrast = contrast
        self.crop = crop              # (top, bottom, left, right) in points
        self.no_shadow_crop = no_shadow_crop
        self.no_aspect_crop = no_aspect_crop
        self.png_bits = 8
        self.lut = gamma_lut(contrast)
        self.wait_stats = {}  # last frame_after_change timings, for the log
        self.last_turn_id = None  # idempotency key of the last page turn
        self.last_gray = None  # last served frame, for wait-for-change
        self.geo = None  # crop+aspect rect (window points) of last capture
        self.last_activity = 0.0  # last Kindle contact; drives display keep-awake
        self.display_released = True  # display may be asleep; wake on next contact
        self.win = None
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

    def rewin(self):
        if self.activate:
            activate_front(self.app)
        self.win = find_window(self.app, self.title)
        if self.win:
            log(f"window: id={self.win[0]} {self.win[3]}x{self.win[4]} "
                f"at ({self.win[1]},{self.win[2]}) '{self.win[5][:40]}'")
        else:
            log(f"no window found for app='{self.app}'"
                + (f" title~'{self.title}'" if self.title else ""))
        return self.win

    def _capture(self):
        """Capture the window and return the final gray frame (fbw x fbh)."""
        if self.win is None and not self.rewin():
            raise RuntimeError("no window")
        r = quartz_capture_gray(self.win)
        if r is None:
            # The window id can die under us (window closed and reopened,
            # app restarted). Re-find it once instead of falling through to
            # the region capture with stale coordinates, which would
            # silently mirror whatever now sits at those screen coords.
            log("capture: window id dead — re-finding window")
            if self.rewin():
                r = quartz_capture_gray(self.win)
        if r is None:
            r = bmp_to_gray(capture_window(self.win))
        gray, w, h, bbox = r
        fbw, fbh = (self.fb or {}).get("w", w), (self.fb or {}).get("h", h)
        # Crop the window chrome: the shadow and rounded corners come off
        # automatically via the alpha bounding box; --crop-* shave extra
        # (e.g. the browser tab bar with its close button), in points.
        if bbox and not self.no_shadow_crop:
            x0, y0, x1, y1 = bbox
        else:
            x0, y0, x1, y1 = 0, 0, w, h
        _, _, _, ww, _, _ = self.win
        scale = (x1 - x0) / ww if ww else 1.0  # capture px per window point
        ctop, cbottom, cleft, cright = self.crop
        x0 = max(0, x0 + int(cleft * scale))
        y0 = max(0, y0 + int(ctop * scale))
        x1 = min(w, x1 - int(cright * scale))
        y1 = min(h, y1 - int(cbottom * scale))
        if x1 <= x0 or y1 <= y0:
            raise RuntimeError("crop removed the whole window")
        gray = gray[y0:y1, x0:x1]
        # Preserve aspect exactly: the cropped window content rarely has
        # the Kindle's 1236x1648 (0.75) ratio, so center-crop the excess
        # dimension before the final stretch. Without this, a taller
        # window gets stretched more vertically than horizontally and
        # text looks elongated (and vice versa for wider windows).
        if not self.no_aspect_crop:
            cw, ch = gray.shape[1], gray.shape[0]
            target = fbw / fbh
            cur = cw / ch
            if cur > target:
                new_w = max(1, int(ch * target))
                xoff = (cw - new_w) // 2
                gray = gray[:, xoff:xoff + new_w]
                x0, x1 = x0 + xoff, x0 + xoff + new_w
            elif cur < target:
                new_h = max(1, int(cw / target))
                yoff = (ch - new_h) // 2
                gray = gray[yoff:yoff + new_h, :]
                y0, y1 = y0 + yoff, y0 + yoff + new_h
        # Pixel-perfect path: when the cropped content already has the
        # framebuffer's aspect but is smaller (a 2x retina capture of a
        # window the display caps below the fb size), resampling smears
        # glyph edges — the x1.03 bilinear upscale this replaced cost ~22%
        # of the edge gradient (measured 2026-08-22 on the same page:
        # mean grad 110.7 resized vs 142.0 at 1:1). Center the content on
        # the page's own background instead: 1:1 pixels, borders that
        # vanish into the site's margins.
        ch2, cw2 = gray.shape
        if (cw2, ch2) != (fbw, fbh) and cw2 <= fbw and ch2 <= fbh \
                and abs(cw2 * fbh - ch2 * fbw) <= 2 * max(1, ch2):
            bg = int(np.median(gray[:40, :40]))
            canvas = np.full((fbh, fbw), bg, np.uint8)
            offx, offy = (fbw - cw2) // 2, (fbh - ch2) // 2
            canvas[offy:offy + ch2, offx:offx + cw2] = gray
            # Widen the served rect so tap()'s linear fb->window map stays
            # true for the padded borders (they land in the site margins).
            x0, x1 = x0 - offx, x1 + (fbw - cw2 - offx)
            y0, y1 = y0 - offy, y1 + (fbh - ch2 - offy)
            gray = canvas
        # Remember the final served rect (in global window points) so tap()
        # can invert fb px -> window point. The px-per-point scale uses the
        # full capture width: the alpha bbox trims the rounded corners, so
        # it would read slightly small.
        _, wx, wy, ww, _, _ = self.win
        scale = w / ww if ww else 1.0  # capture px per window point
        self.geo = {"x": wx + x0 / scale, "y": wy + y0 / scale,
                    "w": (x1 - x0) / scale, "h": (y1 - y0) / scale,
                    "fbw": fbw, "fbh": fbh}
        return self.lut[resize_gray(gray, fbw, fbh)]

    def frame(self, fmt="raw"):
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
        """Encode a frame as PNG, update state, and record wait_stats."""
        self.last_gray = gray
        fbw, fbh = gray.shape[1], gray.shape[0]
        t_enc = time.time()
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

    def tap(self, x, y):
        """Click at client-fb (x, y), mapped through the *same* crop chain
        the frames went through (shadow bbox, --crop-*, aspect center-crop,
        resize) — the inverse of _capture(). geo is refreshed by every
        capture; if none exists yet, take one. Caveat: a window moved after
        the last frame fetch taps its old position; every action replies
        with a fresh frame, so the cache is at most one action old."""
        if self.win is None and not self.rewin():
            raise RuntimeError("no window")
        if not app_is_frontmost(self.app):
            activate_app(self.app)
        if self.geo is None:
            with self.lock:
                self._capture()
        g = self.geo
        cx = g["x"] + (x + 0.5) * g["w"] / g["fbw"]
        cy = g["y"] + (y + 0.5) * g["h"] / g["fbh"]
        pt = CGPointMake(cx, cy)
        for kind in (kCGEventMouseMoved, kCGEventLeftMouseDown, kCGEventLeftMouseUp):
            CGEventPost(kCGHIDEventTap, CGEventCreateMouseEvent(None, kind, pt, 0))
        return int(cx), int(cy)

    def scroll(self, dx, dy):
        """Post a scroll-wheel event for a Kindle swipe, deltas in client
        fb px, scaled to window points via the capture geometry — wheel
        pixel units are screen points (verified: 300 units scrolls a page
        exactly 300 pt), so a full-screen swipe scrolls one window-screen
        whatever the fb size. Raw, natural-scroll sign: finger up (dy<0)
        moves the content up.

        Unlike keys (which a pid-posted event reaches even in a background
        app), a scroll wheel event is routed by *cursor position*, and
        WebKit drops pid-posted wheels outright (tested) — so the pointer
        must sit over our window and the app must be active, the same
        dance as tap()."""
        if self.geo is None:
            with self.lock:
                self._capture()
        g = self.geo
        sy = round(dy * g["h"] / g["fbh"])
        sx = round(dx * g["w"] / g["fbw"])
        if not app_is_frontmost(self.app):
            activate_app(self.app)
        pt = CGPointMake(g["x"] + g["w"] / 2, g["y"] + g["h"] / 2)
        CGEventPost(kCGHIDEventTap,
                    CGEventCreateMouseEvent(None, kCGEventMouseMoved, pt, 0))
        if sx:
            e = CGEventCreateScrollWheelEvent(None, kCGScrollEventUnitPixel, 2,
                                              sy, sx)
        else:
            e = CGEventCreateScrollWheelEvent(None, kCGScrollEventUnitPixel, 1,
                                              sy)
        CGEventPost(kCGHIDEventTap, e)
        return sx, sy

    def key(self, keycode, shift=False):
        """Post a key press straight to the reader app's process.

        CGEventPostToPid delivers to a background app too (verified: a
        background Safari hands the keystroke to the page's JS), so page
        turns need no ~80 ms osascript activation and never steal the
        Mac's focus. Falls back to the system-wide HID post if the pid
        can't be resolved (app-specific path requires the frontmost app,
        but it's better than dropping the key)."""
        pid = app_pid(self.app)
        try:
            # Diagnostic: silent no-op is how a stale Accessibility grant
            # shows up; this line pins the blame in the log.
            h = ctypes.CDLL(
                "/System/Library/Frameworks/ApplicationServices.framework"
                "/Frameworks/HIServices.framework/HIServices")
            trusted = h.AXIsProcessTrusted()
        except Exception:
            trusted = "?"
        log(f"key {keycode} shift={int(shift)}: pid={pid} ax_trusted={trusted}")
        flags = kCGEventFlagMaskShift if shift else 0
        if pid is not None:
            for down in (True, False):
                e = CGEventCreateKeyboardEvent(None, keycode, down)
                CGEventSetFlags(e, flags)
                CGEventPostToPid(pid, e)
        else:
            for down in (True, False):
                e = CGEventCreateKeyboardEvent(None, keycode, down)
                CGEventSetFlags(e, flags)
                CGEventPost(kCGHIDEventTap, e)

    def autosize(self):
        """Resize the window to the optimal mirror size, then re-discover it."""
        if autosize_window(self.app, self.crop):
            self.rewin()
            return True
        return False


def ax_prompt_if_untrusted():
    """If Accessibility is not granted, pop macOS's own prompt (with the
    "Open System Settings" button). Without this, a missing grant is a
    *silent* no-op: keys get posted and vanish, and nothing ever asks."""
    try:
        import objc
        from CoreFoundation import (CFDictionaryCreate,
                                    CFStringCreateWithCString,
                                    kCFBooleanTrue)
        h = ctypes.CDLL(
            "/System/Library/Frameworks/ApplicationServices.framework"
            "/Frameworks/HIServices.framework/HIServices")
        h.AXIsProcessTrustedWithOptions.restype = ctypes.c_bool
        h.AXIsProcessTrustedWithOptions.argtypes = [ctypes.c_void_p]
        key = CFStringCreateWithCString(None, b"AXTrustedCheckOptionPrompt", 0)
        opts = CFDictionaryCreate(None, (key,), (kCFBooleanTrue,),
                                  1, None, None)
        trusted = h.AXIsProcessTrustedWithOptions(
            ctypes.c_void_p(objc.pyobjc_id(opts)))
        log(f"accessibility trusted={trusted}")
        return trusted
    except Exception as e:
        log(f"ax check failed: {e}")
        return None


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
        """Mac-side admin endpoints (/status, /rewin, /autosize, /api/pair)
        are localhost-only: the pairing browser runs on this Mac, and the
        reader never calls them, so a LAN peer must not be able to read the
        paired identities or re-resize the window."""
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
            # Kindle heartbeat and the display keep-awake signal; a probe
            # refreshing it would keep the screen lit with no Kindle
            # anywhere near.
            return self._json({"ok": True}, cors=True)
        if self.path.startswith("/status"):
            if not self._admin_only():
                return
            if m.win is None:
                m.rewin()
            w = m.win or (None, None, None, None, None, None)
            paired = pairing.load()
            return self._json({
                "app": m.app, "title_substr": m.title,
                "window": {"id": w[0], "x": w[1], "y": w[2], "w": w[3], "h": w[4],
                           "name": w[5]} if m.win else None,
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
            if self.path.startswith("/frame.png"):
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
        """Send a frame reply: PNG body plus the X-* headers clients poll on."""
        m = self.mirror
        self.send_response(200)
        self.send_header("Content-Type", "image/png")
        self.send_header("Content-Length", str(len(payload)))
        self.send_header("X-Seq", str(m.seq))
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
            if not self._admin_only():
                return
            return self._json({"window": bool(m.rewin())})
        if self.path.startswith("/autosize"):
            if not self._admin_only():
                return
            return self._json({"window": bool(m.autosize()),
                               "info": ("id", m.win[0], m.win[3], m.win[4])
                               if m.win else None})
        self._json({"error": "not found"}, 404)

    def log_message(self, *a):  # quiet default request logging
        pass


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--app", default="Safari")
    ap.add_argument("--title", default="")
    ap.add_argument("--activate", action="store_true",
                    help="bring the app (and its front window) to the "
                         "foreground before each capture (use with a "
                         "single-tab browser; needs macOS Automation "
                         "permission for osascript)")
    ap.add_argument("--contrast", type=float, default=2.0,
                    help="gamma applied to grayscale frames; higher = darker "
                         "text/blacks, 1.0 = unchanged (default 2.0)")
    ap.add_argument("--crop-top", type=float, default=0.0,
                    help="shave this many window points off the top "
                         "(Safari/Chrome tab bar is ~55-90; default 0)")
    ap.add_argument("--crop-bottom", type=float, default=0.0,
                    help="shave this many window points off the bottom")
    ap.add_argument("--crop-left", type=float, default=0.0,
                    help="shave this many window points off the left")
    ap.add_argument("--crop-right", type=float, default=0.0,
                    help="shave this many window points off the right")
    ap.add_argument("--no-shadow-crop", action="store_true",
                    help="disable automatic removal of the window shadow and "
                         "rounded corners")
    ap.add_argument("--no-aspect-crop", action="store_true",
                    help="disable center-cropping to the Kindle's 0.75 aspect "
                         "(text may look stretched/elongated)")
    ap.add_argument("--autosize", action="store_true",
                    help="resize the app window to the optimal mirror size "
                         "(largest 0.75-aspect content that fits the screen) "
                         "at startup")
    ap.add_argument("--url", default="",
                    help="open this URL in the app at startup before autosizing and serving")
    ap.add_argument("--no-resume", action="store_true",
                    help="don't auto-open the last remembered page when the "
                    "app has no window at startup (resuming is the default; "
                    "an explicit --url always wins)")
    ap.add_argument("--no-stay-awake", action="store_true",
                    help="don't hold a caffeinate no-idle-sleep assertion "
                    "for the server's lifetime (on by default)")
    ap.add_argument("--port", type=int, default=8765)
    ap.add_argument("--parent-pid", type=int, default=0,
                    help="exit when this pid disappears (used by the "
                         "menu bar app so a crashed app can't leave an "
                         "orphaned server behind)")
    ap.add_argument("--display-idle", type=float, default=1800,
                    help="seconds of Kindle quiet before the display is "
                         "allowed to sleep again (default 1800 = the "
                         "distraction-proof reading session; 0 = never "
                         "hold it awake). While the mirror is in active "
                         "use the display is held awake, because window "
                         "capture fails on a sleeping display")
    args = ap.parse_args()

    if not args.no_stay_awake:
        stay_awake()
    if args.parent_pid:
        threading.Thread(target=parent_watchdog, args=(args.parent_pid,),
                         daemon=True).start()
    if args.url:
        if open_url(args.app, args.url):
            log(f"opened {args.url}")
        else:
            log(f"opened {args.url}, but no qualifying window appeared — "
                f"check the app/URL")
    elif not args.no_resume and not find_window(args.app, args.title):
        saved = read_last_url()
        if saved and open_url(args.app, saved):
            log(f"resumed last page: {saved}")

    Handler.mirror = Mirror(args.app, args.title, args.activate, args.contrast,
                            (args.crop_top, args.crop_bottom,
                             args.crop_left, args.crop_right),
                            args.no_shadow_crop, args.no_aspect_crop)
    # Ask for Accessibility up front: without the grant every posted key is
    # a silent no-op, and macOS never prompts on its own from CGEventPost.
    ax_prompt_if_untrusted()
    if args.autosize:
        Handler.mirror.autosize()
    threading.Thread(target=remember_loop, args=(Handler.mirror,),
                     daemon=True).start()
    if args.display_idle > 0:
        threading.Thread(target=display_keepawake_loop,
                         args=(Handler.mirror, args.display_idle),
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
    log(f"listening on :{args.port}  app='{args.app}'"
        + (f" title~'{args.title}'" if args.title else ""))
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        log("bye")


if __name__ == "__main__":
    main()
