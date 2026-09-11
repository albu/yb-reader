#!/usr/bin/env python3
"""menubar.py — a menu-bar control surface for the mirror + AI stream, no
Terminal.

Runs server.py and ai_stream.py as subprocesses of *this* interpreter —
which is always the project's uv venv (launch via `uv run mac/menubar.py`
or the yb-mirror.app bundle), so nothing ever installs into system python.

  ▸ Mirror: off            (status, updated every few seconds)
  ▸ Start mirror / Stop mirror
  ▸ AI Stream: off         (status, updated every few seconds)
  ▸ Start AI stream / Stop AI stream
  ▸ Open Kindle Web Manager…
  ▸ Open log
  ▸ Quit                   (stops everything it started)

The mirror server keeps its own stay-awake/idle-sleep logic; this app only
owns process lifetimes. Two deliberate details:
  - the server starts ONLY when you click Start mirror — an idle server
    must never hold the Mac awake or drain it overnight;
  - if a mirror server is already running (started from a Terminal, e.g.
    during development), Start refuses rather than fighting over the port.
"""

import atexit
import json
import os
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.request

import rumps

from AppKit import NSBezierPath, NSColor, NSFont, NSImage, \
    NSFontAttributeName, NSForegroundColorAttributeName, \
    NSParagraphStyleAttributeName, NSTextAlignmentCenter
from Foundation import NSMakeRect, NSMutableParagraphStyle, NSAttributedString


def make_icon(mirror_on=False, ai_on=False, size=18.0, with_text=True):
    """Menu-bar icon: 'YB' glyph, plus status dots:
      - Bottom-left dot: AI stream active
      - Bottom-right dot: Kindle actively using the mirror
    Template mode tints black/white for light/dark menu bars."""
    img = NSImage.alloc().initWithSize_(((size, size)))
    img.lockFocus()
    if with_text:
        style = NSMutableParagraphStyle.alloc().init()
        style.setAlignment_(NSTextAlignmentCenter)
        text = NSAttributedString.alloc().initWithString_attributes_(
            "YB",
            {NSFontAttributeName: NSFont.boldSystemFontOfSize_(size * 0.52),
             NSForegroundColorAttributeName: NSColor.blackColor(),
             NSParagraphStyleAttributeName: style})
        text.drawInRect_(NSMakeRect(0, size * 0.38, size, size * 0.60))
    NSColor.blackColor().setFill()
    if ai_on:
        # Bottom-left dot (AI Stream)
        NSBezierPath.bezierPathWithOvalInRect_(
            NSMakeRect(size * 0.06, size * 0.02,
                       size * 0.28, size * 0.28)).fill()
    if mirror_on:
        # Bottom-right dot (Mirror)
        NSBezierPath.bezierPathWithOvalInRect_(
            NSMakeRect(size * 0.66, size * 0.02,
                       size * 0.28, size * 0.28)).fill()
    img.unlockFocus()
    img.setTemplate_(True)
    return img


def _icon_png(img):
    from AppKit import NSBitmapImageRep, NSPNGFileType
    rep = NSBitmapImageRep.imageRepWithData_(img.TIFFRepresentation())
    return rep.representationUsingType_properties_(NSPNGFileType, None)


def write_icon_files():
    """Materialize all 4 state combinations as PNGs."""
    paths = {}
    for m in (False, True):
        for a in (False, True):
            p = f"/tmp/yb-mirror-icon-m{int(m)}-a{int(a)}.png"
            with open(p, "wb") as f:
                f.write(_icon_png(make_icon(mirror_on=m, ai_on=a)))
            paths[(m, a)] = p
    return paths


ICONS = write_icon_files()
ICON_OFF = ICONS[(False, False)]

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SERVER = os.path.join(REPO, "mac", "server.py")
AI_STREAM = os.path.join(REPO, "mac", "ai_stream.py")
LOG = os.path.join("/tmp", "yb-mirror-menubar.log")
PORT = 8765
AI_PORT = 8768

# The everyday session command from the README quick start: headless
# Chromium — no visible window, no permission grants.
SERVER_ARGS = []


def log(*a):
    with open(LOG, "a") as f:
        f.write(f"[{time.strftime('%H:%M:%S')}] " + " ".join(map(str, a))
                + "\n")


def notify(title, msg):
    # NOT rumps.notification: rumps needs a real bundle identifier and
    # this app runs through an exec chain (launcher → uv → venv python)
    # whose mainBundle() is nil — rumps' notification path dies with a
    # "no Info.plist" note and takes the whole app with it (that crash
    # orphaned a server once). AppleScript notifications work anywhere.
    esc = lambda s: s.replace("\\", "\\\\").replace('"', '\\"')
    try:
        subprocess.run(["osascript", "-e",
                        f'display notification "{esc(msg)}" '
                        f'with title "{esc(title)}"'],
                       capture_output=True, timeout=5)
    except OSError as e:
        log("notification failed:", e)


def ping_ok(port=PORT):
    try:
        with urllib.request.urlopen(f"http://127.0.0.1:{port}/health",
                                    timeout=1.5) as r:
            return r.status == 200
    except OSError:
        return False


def get_status(port=PORT, timeout=2.0):
    """/status JSON, or None. Loopback-only on the server side, so this
    always talks to a server on this Mac."""
    try:
        with urllib.request.urlopen(f"http://127.0.0.1:{port}/status",
                                    timeout=timeout) as r:
            return json.loads(r.read())
    except Exception:
        return None


def kill_script(script_path):
    """Kill every process running <script_path>, however it was launched.
    The menu bar spawns scripts with an absolute path, but a Terminal
    `uv run mac/foo.py` uses a relative one — so pkill must match on the
    'mac/<name>.py' substring (present in both) rather than the absolute
    path, or a pkill against an orphaned Terminal-launched process silently
    matches nothing."""
    subprocess.run(["pkill", "-f",
                    os.path.join("mac", os.path.basename(script_path))],
                   capture_output=True)


def stop_any_server(timeout=5):
    """Kill a yb-mirror server even if this app didn't spawn it — a
    previous app instance that crashed hard leaves an orphan behind
    (PPID 1), and 'Stop mirror' should mean stop *the mirror*, full stop.
    Returns True if something was killed."""
    if not ping_ok():
        return False
    kill_script(SERVER)
    deadline = time.time() + timeout
    while time.time() < deadline and ping_ok():
        time.sleep(0.25)
    return not ping_ok()


class MirrorBar(rumps.App):
    def __init__(self):
        super().__init__("YB", quit_button="Quit")
        self.server_proc = None
        self.ai_proc = None
        self._last_state = None
        # Icon (drawn YB glyph + status dot) replaces the "YB" text title;
        # template mode so it tints correctly in dark menu bars.
        self.icon = ICON_OFF
        self.template = True
        self.title = None
        self.state = rumps.MenuItem("Mirror: …", callback=None)
        self.ai_state = rumps.MenuItem("AI Stream: …", callback=None)

        self.start_item = rumps.MenuItem("Start mirror",
                                         callback=self.start_mirror)
        self.stop_item = rumps.MenuItem("Stop mirror",
                                        callback=self.stop_mirror)
        # Headless mode's "give it a head": the server relaunches its own
        # Chromium with a visible window for logins and manual browsing.
        # Label tracks /status in _tick, so it self-heals a lost POST.
        self.head_item = rumps.MenuItem("Show reader window",
                                        callback=self.toggle_headed)
        self.start_ai_item = rumps.MenuItem("Start AI stream",
                                            callback=self.start_ai)
        self.stop_ai_item = rumps.MenuItem("Stop AI stream",
                                           callback=self.stop_ai)
        self.web_item = rumps.MenuItem("Open Kindle Web Manager…",
                                       callback=self.open_web_manager)
        self.menu = [
            self.state,
            self.start_item,
            self.stop_item,
            self.head_item,
            None,
            self.ai_state,
            self.start_ai_item,
            self.stop_ai_item,
            None,
            self.web_item,
            None,
            rumps.MenuItem("Open log", callback=self.open_log),
        ]
        rumps.Timer(self.tick, 5).start()
        log("menu bar app started (pid", os.getpid(), ")")

    # ------------------------------------------------------------ mirror ---

    def start_mirror(self, _):
        if self.server_proc and self.server_proc.poll() is None:
            notify("Mirror", "already running from the menu bar")
            return
        if ping_ok():
            notify("Mirror", "a server is already running elsewhere "
                             "(Terminal?) — not starting a second one")
            return
        log("starting server:", SERVER_ARGS)
        self.server_proc = subprocess.Popen(
            [sys.executable, SERVER, "--parent-pid", str(os.getpid())]
            + SERVER_ARGS,
            stdout=open(LOG, "a"), stderr=subprocess.STDOUT, cwd=REPO)
        notify("Mirror", "starting — headless Chromium opens your last book")

    def toggle_headed(self, _):
        """Show/Hide reader window: POST /browser and let the server do the
        relaunch (same profile, so logins survive). Label truth comes from
        /status in _tick, not from this POST's success."""
        if not ping_ok():
            notify("Mirror", "not running")
            return
        try:
            # data=b"" makes urllib POST (the server routes on the method)
            with urllib.request.urlopen(
                    f"http://127.0.0.1:{PORT}/browser?headed=toggle",
                    data=b"", timeout=20) as r:
                d = json.loads(r.read())
            notify("Mirror", "reader window shown — log in if needed"
                   if d.get("headed") else "reader window hidden")
        except Exception as e:
            body = getattr(e, "read", lambda: b"")()
            notify("Mirror", f"toggle failed: {e} {body[:120]}".strip())

    def stop_mirror(self, _):
        stopped_here = self._stop(self.server_proc)
        self.server_proc = None
        if stopped_here or not ping_ok():
            notify("Mirror", "stopped")
            return
        notify("Mirror", "stopping a server this app didn't start…")
        stop_any_server()
        notify("Mirror", "stopped" if not ping_ok() else
               "still responding — see log (/tmp/yb-mirror-menubar.log)")

    @staticmethod
    def _stop(proc):
        if proc and proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
            log("stopped subprocess pid", proc.pid)
            return True
        return False

    # ------------------------------------------------------- web manager ---

    def open_web_manager(self, _):
        # The Kindle's receive page serves port 8080. Its address comes from
        # the handshake, never from a hardcoded IP: the mirror server learns
        # it from the Kindle's own requests (and persists it), so this works
        # as long as the two have talked once. mDNS is the last resort.
        ip = self._kindle_ip()
        if ip:
            if self._probe_web_manager(f"http://{ip}:8080/", timeout=1.5):
                subprocess.Popen(["open", f"http://{ip}:8080/"])
                return
            notify("Web Manager",
                   f"kindle at {ip} isn't answering :8080 — is the reader "
                   "receiving?")
            return
        if self._probe_web_manager("http://kindle.local:8080/", timeout=1.0):
            subprocess.Popen(["open", "http://kindle.local:8080/"])
            return
        notify("Web Manager",
               "start the mirror once so the Mac learns the Kindle's "
               "address (or open the receive page from the Kindle's QR code)")

    @staticmethod
    def _probe_web_manager(url, timeout):
        """True when *anything* answers HTTP on the Kindle's 8080. The
        receive page itself often answers 403 (the PIN screen) when not yet
        authorized — that is the normal state, so any HTTP response counts;
        only a connection failure means the Kindle isn't there."""
        try:
            with urllib.request.urlopen(url, timeout=timeout) as r:
                return True
        except urllib.error.HTTPError:
            return True
        except Exception:
            return False

    def _kindle_ip(self):
        """Learned Kindle address: the running mirror server's memory first
        (it refreshes on every Kindle request), then the persisted handshake
        record from previous sessions."""
        if ping_ok(PORT):
            d = get_status()
            ip = (d or {}).get("kindle", {}).get("ip")
            if ip:
                return ip
        p = os.path.expanduser("~/.yb-mirror-last-kindle")
        try:
            with open(p) as f:
                ip = f.read().strip()
            if ip:
                return ip
        except OSError:
            pass
        return None

    # ----------------------------------------------------------- ai stream ---

    def start_ai(self, _):
        if self.ai_proc and self.ai_proc.poll() is None:
            notify("AI Stream", "already running from the menu bar")
            return
        if ping_ok(AI_PORT):
            notify("AI Stream", "an AI stream server is already running elsewhere")
            return
        log("starting AI stream server...")
        self.ai_proc = subprocess.Popen(
            [sys.executable, AI_STREAM],
            stdout=open(LOG, "a"), stderr=subprocess.STDOUT, cwd=REPO)
        notify("AI Stream", "started — listening for Antigravity & Claude Code")

    def stop_ai(self, _):
        if self.ai_proc:
            self._stop(self.ai_proc)
            self.ai_proc = None
        kill_script(AI_STREAM)
        deadline = time.time() + 5
        while time.time() < deadline and ping_ok(AI_PORT):
            time.sleep(0.25)
        notify("AI Stream", "stopped" if not ping_ok(AI_PORT) else
               "still responding — see log (/tmp/yb-mirror-menubar.log)")

    # ------------------------------------------------------------- house ---

    def tick(self, _):
        try:
            self._tick()
        except Exception as e:
            log("tick error:", repr(e))

    def _tick(self):
        # Server state line
        if self.server_proc and self.server_proc.poll() is not None:
            code = self.server_proc.returncode
            self.server_proc = None
            notify("Mirror", f"server exited (code {code})")
        if self.ai_proc and self.ai_proc.poll() is not None:
            code = self.ai_proc.returncode
            self.ai_proc = None
            notify("AI Stream", f"AI stream server exited (code {code})")
        mirror_up = ping_ok(PORT)
        ai_up = ping_ok(AI_PORT)
        reading = False  # Kindle in contact right now?
        keep = False     # server holding its no-sleep assertion? (dot)

        if ai_up:
            ai_state = "AI Stream: on 🟢"
        else:
            ai_state = "AI Stream: off"

        # Reader-window toggle label + status line. The DOT tracks live
        # Kindle contact (last_seen within 120 s — rides out the radio's
        # 20-30 s naps). The LINE tracks the keep-awake contract: "(reading)"
        # while in contact, then a live countdown of the server's remaining
        # no-sleep grace, and "(idle)" only once the assertion is actually
        # released — never claim idle while the Mac is still held awake.
        if mirror_up:
            st = get_status()
            browser = (st or {}).get("browser") or {}
            last_seen = ((st or {}).get("kindle") or {}).get("last_seen") or 0
            age = time.time() - last_seen
            reading = 0 < age < 120
            keep = bool(browser.get("keep_awake"))
            grace = browser.get("idle_grace") or 600
            if st is not None and st.get("mode") == "headless":
                self.head_item.title = ("Hide reader window"
                                        if browser.get("headed")
                                        else "Show reader window")
                self.head_item.hidden = False
            else:
                # older/external server without the /browser endpoint
                self.head_item.hidden = True
            if reading:
                mirror_state = "Mirror: on (reading)"
            elif keep and grace == 0:
                mirror_state = "Mirror: on (staying awake)"
            elif keep:
                mins = max(1, int((grace - age) // 60) + 1)
                mirror_state = f"Mirror: on (sleep in ~{mins}m)"
            else:
                mirror_state = "Mirror: on (idle)"
            if not self.server_proc:
                mirror_state += " (external)"
        else:
            self.head_item.hidden = True
            mirror_state = "Mirror: off"

        self.state.title = mirror_state
        self.ai_state.title = ai_state

        state_key = (mirror_state, ai_state)
        if state_key != self._last_state:
            log("state:", mirror_state, "|", ai_state)
            self._last_state = state_key
            self.icon = ICONS[(keep, ai_up)]

    def open_log(self, _):
        subprocess.Popen(["open", "-t", LOG])

    def on_quit_cleanup(self):
        self._stop(self.server_proc)
        self._stop(self.ai_proc)


if __name__ == "__main__":
    bar = MirrorBar()
    atexit.register(bar.on_quit_cleanup)

    def _graceful(signum, _frame):
        bar.on_quit_cleanup()
        os._exit(0)

    signal.signal(signal.SIGTERM, _graceful)
    signal.signal(signal.SIGINT, _graceful)
    bar.run()
