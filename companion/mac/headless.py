#!/usr/bin/env python3
"""headless.py — headless-browser backend for the mirror server.

The mirrored web reader runs in a Chromium that this server owns outright
(via Playwright): frames are `page.screenshot()`, page turns are
`page.keyboard`, taps and swipes are `page.mouse`. Nothing is displayed
and no macOS permission grants are needed — the browser screenshots
itself, input goes through CDP, and the URL is `page.url`.

Threading: Playwright's sync API is bound to the thread that created it,
while the HTTP server answers on ThreadingHTTPServer threads. So ONE
browser thread owns every Playwright object and runs jobs off a queue;
handler threads submit and block (Mirror._call). The inherited frame
machinery (frame / frame_settled / frame_after_change / _settle /
_is_loader / gamma / resize / PNG encode) works on the ndarray _capture
returns and is reused untouched — _capture is the whole backend seam.

"Give it a head": set_headed(True) relaunches the same profile with a
visible window (menu bar "Show reader window" or POST /browser) for
logins and manual browsing; toggle back when done. Cookies and
localStorage live in the persistent profile dir and survive everything.
"""

import ctypes
import os
import queue
import sys
import threading
import time

import numpy as np
from playwright.sync_api import sync_playwright
from Quartz import (
    CGDataProviderCreateWithData,
    CGImageSourceCreateWithDataProvider,
    CGImageSourceCreateImageAtIndex,
    CGImageGetHeight,
    CGImageGetWidth,
    CGBitmapContextCreate,
    CGColorSpaceCreateDeviceRGB,
    CGContextDrawImage,
    CGRectMake,
    kCGImageAlphaPremultipliedLast,
)

# Persistent profile: logins, localStorage and the reader's own position
# sync all live here, so "resume the last book" is just re-opening the URL.
PROFILE_DIR = os.path.expanduser("~/.yb-mirror-chromium")
APP_LABEL = "chromium (headless)"

# Launch geometry: the Kindle fb (1236x1648) at 2x device pixels, i.e. the
# page lays out at 618x824 CSS — the same width it sees in the autosized
# Safari window today — and every screenshot is pixel-exact at fb size
# (no upscale, no crop, no aspect compensation).
DEFAULT_CSS_W, DEFAULT_CSS_H, SCALE = 618, 824, 2

CHROMIUM_ARGS = (
    # The page is never "focused" in a headless browser; without these,
    # Chromium throttles the timers reader sites poll their position sync on.
    "--disable-background-timer-throttling",
    "--disable-backgrounding-occluded-windows",
    "--disable-renderer-backgrounding",
    "--hide-scrollbars",           # a scrollbar eats viewport width and shows in frames
    "--disable-lcd-text",          # grayscale AA — at 2x device pixels this reads best on e-ink
    "--force-color-profile=srgb",  # a Display-P3 display must not shift the gray conversion
    "--mute-audio", "--no-first-run", "--no-default-browser-check",
)

# Injected into every page (init script + once for the document we attach
# to): window.__ybSettled flips true after 60 ms of DOM quiet and false on
# any mutation or scroll. The page knows best when it finished rendering,
# so the turn path waits for this signal with cheap evaluates instead of
# screenshot-polling (see frame_after_change). The quiet window is
# deliberately short: a change that lands after it is caught by the
# Kindle's correction poll, which is cheaper than waiting every turn.
# The guard keeps both injection paths from stacking observers.
SETTLE_SIGNAL_JS = """
if (!window.__ybArmed) {
  window.__ybArmed = true;
  window.__ybSettled = true;
  let t = null;
  let raf = null;
  const arm = () => {
    window.__ybSettled = false;
    clearTimeout(t);
    if (raf) cancelAnimationFrame(raf);
    // Double requestAnimationFrame ensures any DOM mutation was styled,
    // laid out, rasterized, and painted (~33 ms at 60 Hz). A 40 ms timeout
    // fallback guarantees completion if rAF is ever throttled.
    t = setTimeout(() => { window.__ybSettled = true; }, 40);
    raf = requestAnimationFrame(() => {
      raf = requestAnimationFrame(() => {
        clearTimeout(t);
        window.__ybSettled = true;
      });
    });
  };
  new MutationObserver(arm).observe(document,
    {childList: true, subtree: true, attributes: true, characterData: true});
  addEventListener('scroll', arm, {passive: true});
  addEventListener('load', arm);
}
"""


def log(*a):
    print(f"[{time.strftime('%H:%M:%S')}]", *a, file=sys.stderr, flush=True)


def png_to_gray(png):
    """PNG bytes -> (gray uint8 [h, w], width, height).

    Playwright only emits PNG/JPEG bytes — there is no raw-pixel API, and
    the venv has no image decoder (server.py's paths are encode-only).
    Rather than pull in Pillow, decode through the ImageIO framework
    pyobjc already ships: draw into a preallocated RGBA bitmap context
    (which normalizes whatever pixel layout Chromium encoded) and take the
    same luma as the window capture (quartz_capture_gray). ~10 ms/frame.
    Note CGImageSource must be built from a *data provider* —
    CGImageSourceCreateWithData does not accept a Python bytes buffer.
    """
    src = CGDataProviderCreateWithData(None, png, len(png), None)
    img = CGImageSourceCreateImageAtIndex(
        CGImageSourceCreateWithDataProvider(src, None), 0, None)
    w, h = CGImageGetWidth(img), CGImageGetHeight(img)
    buf = ctypes.create_string_buffer(w * h * 4)
    ctx = CGBitmapContextCreate(buf, w, h, 8, w * 4,
                                CGColorSpaceCreateDeviceRGB(),
                                kCGImageAlphaPremultipliedLast)
    CGContextDrawImage(ctx, CGRectMake(0, 0, w, h), img)
    rgba = np.frombuffer(buf, np.uint8, w * h * 4).reshape(h, w, 4)
    r, g, b = (rgba[..., i].astype(np.float32) for i in range(3))
    gray = np.clip(0.299 * r + 0.587 * g + 0.114 * b, 0, 255).astype(np.uint8)
    return gray, w, h


# The server's key names (the yb wire vocabulary: "left", "pagedown", ...)
# translated into Playwright's. Mac keycodes arrive via the injected
# KEY_CODES and are reversed in __init__; this is the one vocabulary step
# between them.
PLAYWRIGHT_KEYS = {
    "left": "ArrowLeft", "right": "ArrowRight",
    "up": "ArrowUp", "down": "ArrowDown",
    "space": "Space", "return": "Enter", "escape": "Escape",
    "tab": "Tab", "home": "Home", "end": "End",
    "pageup": "PageUp", "pagedown": "PageDown",
}


def _closed_browser(e):
    """True when the error means the context/page died under us. Playwright
    spells this several ways (and moved the exception class between
    versions), so match the strings instead of importing a name."""
    s = str(e)
    return ("Target closed" in s or "Browser closed" in s
            or "Target page, context or browser has been closed" in s)


def make_headless_mirror(Mirror, resize_gray):
    """Build HeadlessMirror on top of the caller's Mirror class.

    A factory, not a plain `from server import Mirror`, because server.py
    runs as __main__: an import would execute it a second time as module
    'server' and fork its module state. Passing the base class (and
    resize_gray) in keeps exactly one instance of everything.
    """

    class HeadlessMirror(Mirror):
        mode = "headless"

        def __init__(self, contrast=2.0, css_width=DEFAULT_CSS_W,
                     headed=False, key_codes=None, read_url=None,
                     save_url=None):
            # crop/aspect knobs died with the window backend; the viewport
            # IS the served rect.
            super().__init__(app=APP_LABEL, contrast=contrast)
            self.headed = headed
            self.scale = SCALE
            self.css_w = css_width
            self.css_h = round(css_width * DEFAULT_CSS_H / DEFAULT_CSS_W)
            self._vp_for = None  # client fb (w, h) the viewport is fitted to
            self._key_names = {code: PLAYWRIGHT_KEYS[name]
                               for name, code in (key_codes or {}).items()
                               if name in PLAYWRIGHT_KEYS}
            self.read_url, self.save_url = read_url, save_url
            self.url = None      # cached page URL (captures + goto keep it true)
            self.start_error = None
            self.pw = self.ctx = self.page = None
            self.ready = threading.Event()
            self.q = queue.Queue()
            self.thread = threading.Thread(target=self._run, daemon=True)

        # ------------------------------------------------------ lifecycle ---

        def start(self, url="", resume=True):
            """Launch the browser in the background and navigate. All
            fire-and-forget: the HTTP port must bind immediately, or the
            menu bar's 1.5 s /health probe would race a ~3 s Chromium
            start. Early frame requests get a clean 'starting' error and
            the Kindle just retries."""
            self.thread.start()
            if not url and resume and self.read_url:
                url = self.read_url()
                if url:
                    log(f"resuming last page: {url}")
            self.goto(url)

        def _run(self):
            try:
                self.pw = sync_playwright().start()
                self._launch()
            except Exception as e:
                # Surface the real cause (playwright missing, chromium not
                # installed, profile locked by a zombie) — a generic
                # "browser did not answer" timeout would send the user
                # hunting in the wrong place.
                self.start_error = (
                    f"{type(e).__name__}: {e} — headless mode needs "
                    "`uv sync` + `uv run playwright install chromium`")
                log("browser launch failed:", e)
            finally:
                self.ready.set()
            while True:
                fn, reply, _what = self.q.get()
                try:
                    reply.put((fn(), None))
                except BaseException as e:
                    if _closed_browser(e):
                        # The context died under us: forget it, and the
                        # next request relaunches (_ensure). The request
                        # that hit the corpse still 503s.
                        self.ctx = self.page = None
                    reply.put((None, e))

        def _launch(self):
            """(Re)create the persistent context. sync_playwright itself is
            started once for the process lifetime — only the context is
            recycled (headed toggles, crash recovery); tearing the driver
            down would break the next relaunch."""
            self.ctx = self.pw.chromium.launch_persistent_context(
                PROFILE_DIR, headless=not self.headed,
                args=list(CHROMIUM_ARGS),
                viewport={"width": self.css_w, "height": self.css_h},
                device_scale_factor=self.scale,
                color_scheme="light",   # a dark-mode Mac must not flip the
                                        # page to white-on-black and wreck
                                        # the gamma curve
                accept_downloads=False)
            self.page = self.ctx.pages[0] if self.ctx.pages else self.ctx.new_page()
            self.page.set_default_timeout(5000)
            # Render sentinel: init script covers every future navigation,
            # the direct evaluate covers the document we're attaching to
            # right now. Running both is harmless (guard in the script).
            self.page.add_init_script(SETTLE_SIGNAL_JS)
            try:
                self.page.evaluate(SETTLE_SIGNAL_JS)
            except Exception:
                pass  # the init script covers the next navigation anyway
            self._vp_for = None
            self.app = "chromium (headed)" if self.headed else APP_LABEL
            log(f"browser: {'headed' if self.headed else 'headless'} chromium, "
                f"profile {PROFILE_DIR}, viewport "
                f"{self.css_w}x{self.css_h} @ {self.scale}x")

        def _ensure(self):
            """Browser-thread only: relaunch + re-goto when the context or
            page died (crash, headed-toggle race). Direct navigation here —
            calling goto() would queue behind ourselves (rule 2 below)."""
            if self.ctx is not None and self.page is not None \
                    and not self.page.is_closed():
                return
            log("browser: (re)launching")
            self._launch()
            url = self.url or (self.read_url() if self.read_url else None)
            if url:
                try:
                    self.page.goto(url, timeout=8000)
                except Exception as e:
                    log(f"relaunch goto: {e}")

        def close(self):
            """Bounded best-effort teardown for SIGTERM / Ctrl-C. Without
            closing the context, Chromium outlives us as an orphan holding
            the profile's ProcessSingleton lock and the next start fails.
            Runs on the main thread (signal handler), so the close itself
            goes through the job queue — never wait unbounded on it."""
            if self.ctx is None:
                return
            try:
                reply = queue.Queue()
                self.q.put((self.ctx.close, reply, "close"))
                reply.get(timeout=5.0)
            except Exception:
                pass

        def goto(self, url):
            """Navigate, fire-and-forget. Safe from any thread: the job
            serializes on the browser thread, so a screenshot queued behind
            a slow load simply waits it out (bounded by the nav timeout)."""
            if not url:
                return
            self.url = url
            def nav():
                if self.page is None:
                    return
                try:
                    self.page.goto(url, timeout=8000)
                    self.url = self.page.url
                except Exception as e:
                    # a slow or bad site must never wedge the job queue
                    log(f"goto {url[:60]!r}: {e}")
            self.q.put((nav, queue.Queue(), "goto"))

        # ------------------------------------------------- job protocol ---

        def _call(self, fn, timeout=15.0, what="?"):
            """Run fn on the browser thread and wait. Deadlock rules the
            whole design rests on:
              1. The browser thread never takes self.lock and never calls a
                 public Mirror method — jobs touch raw Playwright objects
                 and plain attributes only.
              2. _call never runs ON the browser thread (it would queue
                 behind itself and block on its own reply). True by
                 construction: every caller is an HTTP handler thread,
                 including the inherited frame/frame_after_change loop via
                 _capture.
              3. Overrides MAY hold self.lock across the job-wait — it
                 blocks only on the lock-free browser thread. The forbidden
                 direction is a job waiting on self.lock or calling _call.
            """
            if not self.ready.wait(3.0):
                raise RuntimeError("browser is still starting — retry")
            if self.start_error:
                raise RuntimeError(self.start_error)
            reply = queue.Queue()
            self.q.put((fn, reply, what))
            try:
                value, err = reply.get(timeout=timeout)
            except queue.Empty:
                raise RuntimeError(
                    f"browser did not answer in {timeout:.0f}s ({what})")
            if err is not None:
                raise err
            return value

        # ------------------------------------------------ Mirror overrides --

        def rewin(self):
            """Nothing to re-find — the browser relaunches itself if dead
            (_ensure, on the next frame request)."""
            return True

        def _capture(self):
            """One screenshot job -> final gray frame, mirroring the base
            contract: fb-size ndarray, self.geo set for tap()/scroll()."""
            fb = self.fb or {}
            png, css_w, css_h, url = self._call(
                lambda: self._shot(fb.get("w"), fb.get("h")),
                timeout=10.0, what="screenshot")
            return self._gray_from(png, fb, css_w, css_h, url)

        def _gray_from(self, png, fb, css_w, css_h, url):
            gray, w, h = png_to_gray(png)
            fbw, fbh = fb.get("w", w), fb.get("h", h)
            # geo in CSS px: the viewport IS the served rect, so x/y are 0
            # and fb->css is a pure scale — the linear map tap()/scroll()
            # invert.
            self.geo = {"x": 0, "y": 0, "w": css_w, "h": css_h,
                        "fbw": fbw, "fbh": fbh}
            self.url = url
            return self.lut[resize_gray(gray, fbw, fbh)]

        def _fit_viewport(self, fbw, fbh):
            """Fit the viewport to the client's fb once per distinct size.
            device_scale_factor is fixed at context creation (Playwright
            won't change it on a live context), so a non-default fb gets
            fb/scale CSS px and the <=1 px rounding is absorbed by the
            bilinear resize_gray in _gray_from."""
            if fbw and fbh and (fbw, fbh) != self._vp_for:
                css = (max(1, round(fbw / self.scale)),
                       max(1, round(fbh / self.scale)))
                if css != (self.css_w, self.css_h):
                    self.page.set_viewport_size(
                        {"width": css[0], "height": css[1]})
                    log(f"viewport: {css[0]}x{css[1]} @ {self.scale}x "
                        f"for fb {fbw}x{fbh}")
                self.css_w, self.css_h = css
                self._vp_for = (fbw, fbh)

        def _shot(self, fbw, fbh):
            """Browser thread only. Returns (png, css_w, css_h, url)."""
            self._ensure()
            self._fit_viewport(fbw, fbh)
            # animations="disabled" snaps a reader's turn animation to its
            # end state — strictly helps settle/loader detection.
            return (self.page.screenshot(type="png", timeout=4000,
                                         caret="hide",
                                         animations="disabled"),
                    self.css_w, self.css_h, self.page.url)

        def _turn_shot(self, fbw, fbh, deadline):
            """Browser thread only: the TURN capture. Waits for the page's
            own render signal (__ybSettled — double-rAF DOM quiet) with
            wait_for_function instead of polling sleep, then takes exactly one
            screenshot. Deadline-bounded: a page that never goes quiet
            (spinner, live clock) still ships one frame at the deadline."""
            self._ensure()
            self._fit_viewport(fbw, fbh)
            rem_ms = max(10, int((deadline - time.time()) * 1000))
            try:
                self.page.wait_for_function(
                    "() => window.__ybSettled === true",
                    timeout=rem_ms, polling="raf")
            except Exception:
                pass
            return (self.page.screenshot(type="png", timeout=4000,
                                         caret="hide",
                                         animations="disabled"),
                    self.css_w, self.css_h, self.page.url)

        def frame_after_change(self, timeout=3.0, poll=0.02,
                               settle_tries=4, settle_gap=0.03, trust=False):
            """Turn path — the reason the sentinel exists. Wait for the page
            to announce it finished rendering, then ship ONE capture. The
            signal (DOM quiet for 120 ms, animations disabled so end state
            == quiet state) replaces both the screenshot-poll loop and the
            stability double-capture; loader detection is kept. Anything
            going wrong (sentinel missing, page wedged) falls back to the
            base implementation for the remaining budget — today's
            behavior, byte for byte."""
            with self.lock:
                start = time.time()
                deadline = start + timeout
                fb = self.fb or {}
                prev = self.last_gray
                polls = 0
                try:
                    while True:
                        png, css_w, css_h, url = self._call(
                            lambda: self._turn_shot(fb.get("w"), fb.get("h"),
                                                    deadline),
                            timeout=max(5.0, deadline - time.time() + 2.0),
                            what="turn capture")
                        polls += 1
                        gray = self._gray_from(png, fb, css_w, css_h, url)
                        if prev is None or not np.array_equal(gray, prev):
                            break
                        if time.time() >= deadline:
                            # the turn didn't change anything (chapter
                            # boundary, no-op key): report like the base's
                            # timeout arm does
                            return self._png_reply(gray, False,
                                                   not self._is_loader(gray),
                                                   start, polls)
                        # unchanged: pace the re-check like the base's poll
                        # loop instead of screenshot-spamming a settled page
                        time.sleep(poll)
                    gray = self._wait_out_loader(
                        gray, min(deadline, time.time() + self.LOADER_WAIT_S))
                    settled = not self._is_loader(gray)
                    return self._png_reply(gray, True, settled, start, polls)
                except Exception as e:
                    remaining = max(0.1, deadline - time.time())
                    log(f"render signal unavailable ({type(e).__name__}) — "
                        f"poll fallback for {remaining:.1f}s")
                    return super().frame_after_change(
                        timeout=remaining, poll=poll,
                        settle_tries=settle_tries, settle_gap=settle_gap,
                        trust=trust)

        def key(self, keycode, shift=False):
            """Page turn (or any named key) straight into the page via CDP —
            no OS focus, no Accessibility, never steals the Mac's keyboard."""
            name = self._key_names.get(keycode)
            if name is None:
                raise RuntimeError(
                    f"no headless mapping for keycode {keycode:#x}")
            def press():
                if shift:
                    self.page.keyboard.down("Shift")
                self.page.keyboard.press(name)  # press() has no modifiers arg
                if shift:
                    self.page.keyboard.up("Shift")
            self._call(press, timeout=8.0, what=f"key {name}")

        def tap(self, x, y):
            """Click at client-fb (x, y) — the geo map from _capture is a
            pure fb->CSS scale, so this is one multiply, then a CDP click."""
            if self.geo is None:
                with self.lock:      # rule 3: legal, see _call
                    self._capture()
            g = self.geo
            cx = g["x"] + (x + 0.5) * g["w"] / g["fbw"]
            cy = g["y"] + (y + 0.5) * g["h"] / g["fbh"]
            self._call(lambda: self.page.mouse.click(cx, cy),
                       timeout=8.0, what=f"tap {cx:.0f},{cy:.0f}")
            return int(cx), int(cy)

        def scroll(self, dx, dy):
            """Kindle swipe -> wheel event, client fb px scaled to CSS px.

            Sign: the client sends dy = ey - y (finger up ends ABOVE its
            start, so dy < 0) and the contract is 'finger up moves the
            content up' = one screen forward. A browser wheel with a
            POSITIVE deltaY scrolls forward, so the CSS deltas are
            negated here (verified against the swipe test page)."""
            if self.geo is None:
                with self.lock:      # rule 3: legal, see _call
                    self._capture()
            g = self.geo
            sx = round(dx * g["w"] / g["fbw"])
            sy = round(dy * g["h"] / g["fbh"])
            def wheel():
                # a wheel event lands at the pointer, so park it mid-page
                self.page.mouse.move(self.css_w / 2, self.css_h / 2)
                self.page.mouse.wheel(-sx, -sy)
            self._call(wheel, timeout=8.0, what=f"scroll {sx},{sy}")
            return sx, sy

        def autosize(self):
            """'Window size' here = refit the viewport to the client's fb.
            Returns True for parity with the /autosize endpoint reply."""
            fb = self.fb or {}
            if not (fb.get("w") and fb.get("h")):
                return False
            def refit():
                css = (max(1, round(fb["w"] / self.scale)),
                       max(1, round(fb["h"] / self.scale)))
                if css != (self.css_w, self.css_h):
                    self.page.set_viewport_size(
                        {"width": css[0], "height": css[1]})
                    log(f"viewport: {css[0]}x{css[1]} @ {self.scale}x")
                self.css_w, self.css_h = css
                self._vp_for = (fb["w"], fb["h"])
            self._call(refit, what="autosize")
            return True

        # --------------------------------------------------- head control --

        def set_headed(self, headed):
            """Relaunch the SAME profile with/without a visible window —
            the 'log in once' path (menu bar Show/Hide reader window, or
            POST /browser?headed=). One job, so it serializes against
            in-flight screenshots for free. Accepted artifact: a
            frame_after_change straddling the toggle sees the fresh context
            as 'changed' and reports one spurious X-Changed."""
            if headed == self.headed:
                return self.status()
            def flip():
                try:
                    url = (self.page.url
                           if self.page and not self.page.is_closed()
                           else self.url)
                except Exception:
                    url = self.url
                if url:
                    self.url = url
                    if self.save_url:
                        self.save_url(url)   # survives even a crash here
                if self.ctx:
                    self.ctx.close()         # releases the profile lock
                self.ctx = self.page = None
                self.headed = headed
                self._launch()
                if self.url:
                    try:
                        self.page.goto(self.url, timeout=8000)
                    except Exception as e:
                        log(f"headed relaunch goto: {e}")
            self._call(flip, timeout=30.0, what="headed toggle")
            return self.status()

        # ------------------------------------------------------- reading --

        def current_url(self):
            """Cached — no browser round trip (the rememberer polls this
            every minute; the value refreshes on every capture anyway)."""
            return self.url

        def status(self):
            """Plain attributes only: /status is polled by the menu bar, so
            this must never block on the browser thread. keep_awake/idle_grace
            are maintained by the server's keep-awake loop (whether the
            no-sleep assertion is currently held, and its grace length) —
            the menu bar's status line displays exactly that contract."""
            return {
                "headed": self.headed,
                "url": self.url,
                "viewport": {"w": self.css_w, "h": self.css_h,
                             "scale": self.scale},
                "alive": self.ctx is not None and self.start_error is None,
                "keep_awake": getattr(self, "keep_awake", False),
                "idle_grace": getattr(self, "idle_grace", None),
            }

    return HeadlessMirror
