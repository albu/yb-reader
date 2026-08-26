#!/usr/bin/env python3
"""send.py — hand one file to the Kindle, then quit.

  uv run mac/send.py path/to/book.epub

Serves the file at http://<mac>:8767/book until KOReader's mirror plugin
fetches it (menu → Tools → Fetch book from Mac), then exits by itself —
nothing stays running. A --wait idle timeout (default 10 min) bounds its
life if nobody comes for the file.

The protocol is two headers and a byte stream, matching what the plugin's
HTTP client already speaks:
  GET /book  -> 200, X-Filename: <percent-encoded utf-8 name>,
                Content-Length: N, raw file bytes
                410 once the file has been acked as delivered
  GET /ack   -> the client confirms the body arrived intact; delivery is
                THIS, not our last write() — a whole small file can sit in
                kernel socket buffers while the reader dies, and marking
                it "delivered" then would lose the file on a retry
  GET /ping  -> {"ok": true}
Discovery: answers the same UDP probe as the mirror server (port 8766)
when that port is free, so a Kindle with no saved address finds this
server too; when the mirror server is running, it owns 8766 and the
Kindle reaches this server via its saved/discovered Mac address on 8767.
"""

import argparse
import json
import os
import socket
import sys
import threading
import time
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

FETCH_PORT = 8767  # mirror server is 8765/8766; keep the family adjacent


def log(*a):
    print(f"[{time.strftime('%H:%M:%S')}]", *a, file=sys.stderr, flush=True)


def local_ips():
    """LAN addresses of this Mac (UDP-connect trick: no packets leave)."""
    ips = set()
    for target in (("192.0.2.1", 9), ("233.252.0.1", 9)):  # TEST-NET ranges
        try:
            s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            s.connect(target)
            ips.add(s.getsockname()[0])
            s.close()
        except OSError:
            pass
    return sorted(ip for ip in ips if not ip.startswith("127."))


def run_discovery(tcp_port):
    """Answer 'ybmirror' probes exactly like the mirror server does, so the
    plugin's existing discover() finds us. The plugin only ever broadcasts
    to udp/8766, so that's the port to own; a custom --port still gets
    discovered as long as 8766 is free."""
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
    try:
        s.bind(("", 8766))
    except OSError:
        log("discovery off (udp/:8766 busy — the mirror server owns it; "
            "the Kindle can still reach this file via its saved address "
            f"on port {tcp_port})")
        return
    log("discovery listening on udp/:8766")
    while True:
        try:
            data, addr = s.recvfrom(256)
            if data.startswith(b"ybmirror"):
                s.sendto(f"ybmirror {tcp_port}".encode(), addr)
        except OSError:
            continue


class Sender:
    """Shared state: the one file, and whether it has been delivered."""

    def __init__(self, path):
        self.path = path
        self.size = os.path.getsize(path)
        self.name = os.path.basename(path)
        self.delivered = False
        self.expired = False  # --wait ran out with nobody fetching
        self.lock = threading.Lock()
        self.serve_until = time.time() + 10 * 60  # replaced by --wait


STATE: Sender = None  # set in main()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    timeout = 60  # a single write stalling this long = dead radio

    def _json(self, obj, code=200):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        st = STATE
        if self.path.startswith("/ping"):
            return self._json({"ok": True})
        if self.path.startswith("/ack"):
            with st.lock:
                if not st.delivered:
                    st.delivered = True
                    st.delivered_at = time.time()
                    log(f"delivery acked: {st.name} ({st.size} B) — "
                        f"exiting shortly")
            return self._json({"ok": True})
        if self.path.startswith("/status"):
            return self._json({"file": st.name, "size": st.size,
                               "delivered": st.delivered})
        if self.path.startswith("/book"):
            if st.delivered:
                return self._json({"error": "already delivered"}, 410)
            sent = 0
            try:
                with open(st.path, "rb") as f:
                    self.send_response(200)
                    self.send_header(
                        "Content-Type", "application/octet-stream")
                    self.send_header(
                        "X-Filename",
                        urllib.parse.quote(st.name, safe=""))
                    self.send_header("Content-Length", str(st.size))
                    self.end_headers()
                    while True:
                        chunk = f.read(65536)
                        if not chunk:
                            break
                        self.wfile.write(chunk)
                        sent += len(chunk)
            except (BrokenPipeError, ConnectionResetError):
                # The Kindle's radio dropped mid-download; a retry carries
                # the same request and the file is re-served from scratch
                # (delivery is only the /ack, never this write).
                log(f"download interrupted at {sent}/{st.size} — "
                    f"retry is fine")
                return
            return
        self._json({"error": "not found"}, 404)

    def log_message(self, fmt, *args):  # quiet default request logging
        pass


def watchdog(httpd, st):
    """Exit when the file has been delivered (grace for the last reply
    bytes to flush) or when --wait elapses with nobody fetching."""
    while True:
        time.sleep(1)
        if st.delivered and time.time() > st.delivered_at + 2:
            log("bye — file delivered")
            break
        if time.time() > st.serve_until:
            with st.lock:
                st.expired = True
            log("bye — waited long enough, nobody fetched the file")
            break
    httpd.shutdown()


def main():
    global STATE
    ap = argparse.ArgumentParser()
    ap.add_argument("file", help="the file to hand over (book, pdf, ...)")
    ap.add_argument("--port", type=int, default=FETCH_PORT)
    ap.add_argument("--wait", type=float, default=600,
                    help="give up after this many seconds (default 600)")
    args = ap.parse_args()

    if not os.path.isfile(args.file):
        log(f"no such file: {args.file}")
        return 1
    STATE = Sender(os.path.abspath(args.file))
    STATE.serve_until = time.time() + args.wait
    STATE.delivered_at = 0.0

    threading.Thread(target=run_discovery,
                     args=(args.port,), daemon=True).start()
    httpd = ThreadingHTTPServer(("0.0.0.0", args.port), Handler)
    ips = ", ".join(f"http://{ip}:{args.port}" for ip in local_ips()) or \
        f"http://<mac-ip>:{args.port}"
    log(f"serving “{STATE.name}” ({STATE.size} B) at {ips}")
    log("on the Kindle: KOReader → Tools → Fetch book from Mac")
    threading.Thread(target=watchdog, args=(httpd, STATE),
                     daemon=True).start()
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        log("bye")
    # Exit codes the menu bar app reports on: 0 = delivered (or user
    # interrupted), 2 = nobody fetched the file before --wait ran out.
    return 2 if STATE.expired else 0


if __name__ == "__main__":
    sys.exit(main())
