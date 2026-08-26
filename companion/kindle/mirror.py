#!/usr/bin/env python3
"""Kindle Mirror — Kindle side client.

Pulls framebuffer-native grayscale frames from the Mac server and writes
them straight to /dev/fb0; forwards touchscreen taps as HTTP POSTs which
the server replays as real mouse clicks inside the mirrored window.

Usage:  mirror.py SERVER [SETTLE_SECONDS]
        SERVER e.g. http://192.168.1.42:8765

Depends on: python3 (KUAL "Python 3" extension).
"""
import json
import os
import select
import struct
import subprocess
import sys
import time
import urllib.request

FB_DEV = "/dev/fb0"
EV_DIR = "/dev/input"
LOG = "/mnt/us/extensions/mirror/mirror.log"

# Set True if probe.sh shows X/Y arriving swapped for the touchscreen.
SWAP_AXES = False

EV_SYN, EV_KEY, EV_ABS = 0x00, 0x01, 0x03
BTN_TOUCH, ABS_MT_POSITION_X, ABS_MT_POSITION_Y = 0x14A, 0x35, 0x36
ABS_X, ABS_Y, ABS_MT_TRACKING_ID = 0x00, 0x01, 0x39

FMT_OLD = struct.Struct("=llHHi")   # 2.6-era kernels: 32-bit timeval, 16 bytes
FMT_NEW = struct.Struct("=qqHHi")   # newer kernels: 64-bit time_t, 24 bytes


def log(*a):
    line = "[%s] %s" % (time.strftime("%H:%M:%S"), " ".join(str(x) for x in a))
    try:
        with open(LOG, "a") as f:
            f.write(line + "\n")
    except OSError:
        pass
    print(line)


# ------------------------------------------------------------- framebuffer --

def fb_params():
    base = "/sys/class/graphics/" + os.path.basename(FB_DEV)
    with open(base + "/virtual_size") as f:
        w, h = (int(x) for x in f.read().strip().split(","))
    with open(base + "/bits_per_pixel") as f:
        depth = int(f.read().strip())
    try:
        with open(base + "/stride") as f:
            stride = int(f.read().strip())
    except OSError:
        stride = w * depth // 8
    return w, h, depth, stride


# ------------------------------------------------------------- touch input --

def find_touch_device():
    devs = {}
    try:
        with open("/proc/bus/input/devices") as f:
            cur = None
            for line in f:
                if line.startswith("N: "):
                    cur = line.split("Name=", 1)[-1].strip('" \n')
                elif line.startswith("H: ") and cur is not None:
                    for node in line.split("Handlers=")[-1].split():
                        if node.startswith("event"):
                            devs["/dev/input/" + node] = cur
    except OSError:
        pass
    for path, name in devs.items():
        low = name.lower()
        if "touch" in low or "cyttsp" in low or "mt" in low or "cap" in low:
            return path
    return None


def scale(v, dim):
    """Touch controllers report either 0..dim-1 or 0..4095; handle both."""
    return int(v * dim / 4096) if v > dim else v


class Touch:
    def __init__(self, path):
        self.fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK)
        self.fmt = None            # decided on first successful read
        self.x = self.y = None
        self.pressed = False

    def _events(self, data):
        """Yield (type, code, value); pick struct layout from the first block."""
        if self.fmt is None:
            for fmt in (FMT_OLD, FMT_NEW):
                try:
                    _, _, t, c, _ = fmt.unpack_from(data, 0)
                    if t in (EV_SYN, EV_KEY, EV_ABS) and 0 <= c < 0x400:
                        self.fmt = fmt
                        break
                except struct.error:
                    continue
            if self.fmt is None:
                return
        for i in range(0, len(data) - self.fmt.size + 1, self.fmt.size):
            _, _, t, c, v = self.fmt.unpack_from(data, i)
            if t in (EV_SYN, EV_KEY, EV_ABS):
                yield t, c, v

    def poll_tap(self, fbw, fbh, timeout=0.2):
        """Return (x, y) in framebuffer coords if a completed tap is seen."""
        try:
            r, _, _ = select.select([self.fd], [], [], timeout)
            if not r:
                return None
            data = os.read(self.fd, 4096)
        except (BlockingIOError, OSError):
            return None
        for t, c, v in self._events(data):
            if t == EV_ABS:
                if c in (ABS_MT_POSITION_X, ABS_X):
                    self.x = v
                elif c in (ABS_MT_POSITION_Y, ABS_Y):
                    self.y = v
                elif c == ABS_MT_TRACKING_ID and v == -1:
                    return self._pos(fbw, fbh)
            elif t == EV_KEY and c == BTN_TOUCH:
                if v == 1:
                    self.pressed = True
                elif v == 0 and self.pressed:
                    self.pressed = False
                    return self._pos(fbw, fbh)
        return None

    def _pos(self, fbw, fbh):
        if self.x is None or self.y is None:
            return None
        x, y = (self.y, self.x) if SWAP_AXES else (self.x, self.y)
        return scale(x, fbw), scale(y, fbh)


# ------------------------------------------------------------------- main --

def http(url, data=None, timeout=15):
    req = urllib.request.Request(url, data=data,
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return r.read(), dict(r.headers)


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    server = sys.argv[1].rstrip("/")
    settle = float(sys.argv[2]) if len(sys.argv) > 2 else 0.9

    w, h, depth, stride = fb_params()
    log("fb %dx%d depth=%d stride=%d" % (w, h, depth, stride))

    fbink = None
    for cand in ("/mnt/us/koreader/fbink",
                 "/mnt/us/system/bin/fbink",
                 "/mnt/us/extensions/mirror/fbink"):
        if os.path.isfile(cand):
            fbink = cand

    def refresh():
        """Best-effort e-ink kick; plain fb writes usually auto-refresh."""
        if fbink:
            subprocess.Popen([fbink, "-f", "-q"], stdout=subprocess.DEVNULL,
                             stderr=subprocess.DEVNULL)

    def pull_frame(fb):
        url = "%s/frame?w=%d&h=%d&depth=%d&stride=%d" % (server, w, h, depth, stride)
        body, hdrs = http(url, timeout=20)
        fb.seek(0)
        fb.write(body)
        fb.flush()
        log("frame %s seq=%s (%dB)" % (hdrs.get("X-Win-Size"),
                                       hdrs.get("X-Seq"), len(body)))
        refresh()

    ev = find_touch_device()
    touch = Touch(ev) if ev else None
    log("touch: %s" % (ev or "none found — frame-pull only"))
    if touch is None:
        log("hint: run probe.sh and check the touchscreen evdev name")

    with open(FB_DEV, "wb", buffering=0) as fb:
        try:
            pull_frame(fb)
            while True:
                tap = touch.poll_tap(w, h) if touch else None
                if tap:
                    # tap zones: left third = previous page, rest = next
                    ep = "/prev" if tap[0] < w // 3 else "/next"
                    log("tap %s -> %s" % (tap, ep))
                    try:
                        http(server + ep)
                    except Exception as e:
                        log("%s failed: %s" % (ep, e))
                    time.sleep(settle)
                    try:
                        pull_frame(fb)
                    except Exception as e:
                        log("frame pull failed: %s" % e)
                elif touch is None:
                    time.sleep(settle)
                    try:
                        pull_frame(fb)
                    except Exception as e:
                        log("frame pull failed: %s" % e)
        except KeyboardInterrupt:
            log("bye")


if __name__ == "__main__":
    main()
