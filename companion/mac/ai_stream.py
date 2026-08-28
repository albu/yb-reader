#!/usr/bin/env python3
"""ai_stream.py — Stream live AI coding sessions (Antigravity, Claude Code, etc.) to Kindle.

Watches active transcript/log files or terminal pipes, parses turns into structured
E-Ink layout blocks, and serves them over HTTP (port 8768) to yb-reader on Kindle.

Usage:
  uv run mac/ai_stream.py                     # auto-detects latest active session
  uv run mac/ai_stream.py --watch /path/to/log
  claude | uv run mac/ai_stream.py --pipe
"""

import argparse
import glob
import json
import os
import re
import socket
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import urllib.parse

import pairing

STREAM_PORT = 8768
DISCOVER_PORT = 8766
MIRROR_PORT = 8765  # the mirror server's port; probes for it must not point here


def log(*args):
    print(f"[{time.strftime('%H:%M:%S')}] [AI-Stream]", *args, file=sys.stderr, flush=True)


def strip_ansi(text: str) -> str:
    """Remove ANSI escape codes from terminal outputs."""
    ansi_regex = re.compile(r'\x1B(?:[@-Z\\-_]|\[[0-?]*[ -/]*[@-~])')
    return ansi_regex.sub('', text)


def parse_markdown_blocks(text: str):
    """Parse markdown text into structured blocks for simple e-ink rendering."""
    blocks = []
    lines = text.split('\n')
    i = 0
    while i < len(lines):
        line = lines[i]
        
        # 1. Fenced Code Block
        if line.strip().startswith('```'):
            lang = line.strip().lstrip('`').strip()
            code_lines = []
            i += 1
            while i < len(lines) and not lines[i].strip().startswith('```'):
                code_lines.append(lines[i])
                i += 1
            blocks.append({
                "type": "code",
                "lang": lang or "text",
                "code": "\n".join(code_lines)
            })
            i += 1
            continue

        # 2. Markdown Table
        if '|' in line and i + 1 < len(lines) and re.match(r'^\s*\|?\s*[-:]+[-| :]*\|\s*$', lines[i+1]):
            headers = [c.strip() for c in line.strip().strip('|').split('|')]
            i += 2  # skip header and divider
            rows = []
            while i < len(lines) and '|' in lines[i] and lines[i].strip():
                row = [c.strip() for c in lines[i].strip().strip('|').split('|')]
                rows.append(row)
                i += 1
            blocks.append({
                "type": "table",
                "headers": headers,
                "rows": rows
            })
            continue

        # 3. Headings
        if line.startswith('#'):
            match = re.match(r'^(#{1,6})\s+(.*)$', line)
            if match:
                level = len(match.group(1))
                blocks.append({
                    "type": "heading",
                    "level": level,
                    "text": match.group(2).strip()
                })
                i += 1
                continue

        # 4. Blockquotes / Alerts
        if line.startswith('>'):
            quote_lines = []
            alert_type = None
            while i < len(lines) and lines[i].startswith('>'):
                cleaned = lines[i].lstrip('>').strip()
                if cleaned.startswith('[!') and ']' in cleaned:
                    alert_type = cleaned[2:cleaned.index(']')].strip().upper()
                else:
                    quote_lines.append(cleaned)
                i += 1
            blocks.append({
                "type": "alert" if alert_type else "quote",
                "kind": alert_type or "note",
                "text": "\n".join(quote_lines)
            })
            continue

        # 5. List Items
        if re.match(r'^\s*[-*+]\s+', line) or re.match(r'^\s*\d+\.\s+', line):
            list_items = []
            while i < len(lines) and (re.match(r'^\s*[-*+]\s+', lines[i]) or re.match(r'^\s*\d+\.\s+', lines[i])):
                list_items.append(re.sub(r'^\s*([-*+]|\d+\.)\s+', '', lines[i]).strip())
                i += 1
            blocks.append({
                "type": "list",
                "items": list_items
            })
            continue

        # 6. Paragraph
        if line.strip():
            para_lines = [line.strip()]
            i += 1
            while i < len(lines) and lines[i].strip() and not lines[i].startswith(('#', '```', '>', '|', '- ', '* ', '1. ')):
                para_lines.append(lines[i].strip())
                i += 1
            blocks.append({
                "type": "paragraph",
                "text": " ".join(para_lines)
            })
            continue

        i += 1

    return blocks


class SessionWatcher:
    """Watches active AI sessions (Antigravity, Claude Code, or custom pipe/log)."""

    def __init__(self, watch_path=None, pipe_mode=False):
        self.watch_path = watch_path
        self.pipe_mode = pipe_mode
        self.mode = "auto"  # "auto", "antigravity", "claude"
        self.current_turn = None
        self.history = []
        self.revision = 0
        self.lock = threading.Lock()
        self.active_source = "None"
        self.force_reload = False
        self._stop = False
        self.pipe_buffer = []
        self.pipe_dirty = threading.Event()

    def set_mode(self, mode):
        with self.lock:
            self.mode = mode.lower()
            self.force_reload = True
            self.revision += 1
            log(f"AI Stream source switched to: {self.mode}")

    def find_latest_antigravity_transcript(self):
        """Find the newest Antigravity conversation transcript."""
        base = os.path.expanduser("~/.gemini/antigravity-cli/brain")
        if not os.path.exists(base):
            return None
        patterns = [
            os.path.join(base, "*/.system_generated/logs/transcript.jsonl"),
            os.path.join(base, "*/.system_generated/logs/transcript_full.jsonl")
        ]
        files = []
        for p in patterns:
            files.extend(glob.glob(p))
        if not files:
            return None
        files.sort(key=lambda f: os.path.getmtime(f), reverse=True)
        return files[0]

    def find_latest_claude_transcript(self):
        """Find the newest Claude Code conversation transcript."""
        base = os.path.expanduser("~/.claude/projects")
        if not os.path.exists(base):
            return None
        patterns = [
            os.path.join(base, "*/*.jsonl"),
            os.path.join(base, "*/*/*.jsonl"),
        ]
        files = []
        for p in patterns:
            files.extend(glob.glob(p))
        if not files:
            return None
        files.sort(key=lambda f: os.path.getmtime(f), reverse=True)
        return files[0]

    def parse_claude_transcript(self, file_path):
        """Extract the latest user prompt and model response from Claude Code jsonl."""
        if not os.path.exists(file_path):
            return None
        last_prompt = ""
        last_response = ""
        last_tool = None
        last_timestamp = ""
        try:
            with open(file_path, "r", encoding="utf-8", errors="ignore") as f:
                for line in f:
                    line = line.strip()
                    if not line:
                        continue
                    try:
                        step = json.loads(line)
                    except Exception:
                        continue
                    stype = step.get("type", "")
                    ts = step.get("timestamp", "")
                    if ts and "T" in ts:
                        time_part = ts.split("T")[1][:5]
                    else:
                        time_part = time.strftime("%H:%M")
                    
                    if stype == "user":
                        msg = step.get("message", {})
                        content = msg.get("content", "")
                        if isinstance(content, str) and content:
                            last_prompt = content.strip()
                            last_response = ""
                            last_timestamp = time_part
                    elif stype == "assistant":
                        msg = step.get("message", {})
                        content = msg.get("content", [])
                        if isinstance(content, list):
                            text_parts = []
                            for part in content:
                                if isinstance(part, dict):
                                    if part.get("type") == "text":
                                        text_parts.append(part.get("text", ""))
                                    elif part.get("type") == "tool_use":
                                        name = part.get("name", "Tool")
                                        last_tool = f"Running {name}"
                            if text_parts:
                                last_response = "\n\n".join(text_parts)
                                last_timestamp = time_part
                        elif isinstance(content, str):
                            last_response = content
                            last_timestamp = time_part
        except Exception as e:
            log("Error reading Claude transcript:", e)
            return None

        if not last_prompt and not last_response:
            return None

        blocks = parse_markdown_blocks(last_response) if last_response else []
        return {
            "id": f"claude_{int(os.path.getmtime(file_path))}",
            "assistant": "Claude Code",
            "prompt": last_prompt,
            "timestamp": last_timestamp or time.strftime("%H:%M"),
            "status": "tool_running" if last_tool else "idle",
            "tool_status": last_tool,
            "raw_markdown": last_response,
            "blocks": blocks
        }

    def parse_antigravity_transcript(self, file_path):
        """Extract the latest user prompt and model response from transcript.jsonl."""
        if not os.path.exists(file_path):
            return None
        
        last_prompt = ""
        last_response = ""
        last_tool = None
        last_timestamp = ""
        
        try:
            with open(file_path, "r", encoding="utf-8", errors="ignore") as f:
                for line in f:
                    line = line.strip()
                    if not line:
                        continue
                    try:
                        step = json.loads(line)
                    except Exception:
                        continue
                    
                    stype = step.get("type", "")
                    ts = step.get("created_at", "")
                    if ts and "T" in ts:
                        time_part = ts.split("T")[1][:5]
                    else:
                        time_part = time.strftime("%H:%M")

                    if stype == "USER_INPUT":
                        content = step.get("content", "")
                        if "<USER_REQUEST>" in content:
                            m = re.search(r'<USER_REQUEST>(.*?)</USER_REQUEST>', content, re.DOTALL)
                            if m:
                                content = m.group(1).strip()
                        content = re.sub(r'<[^>]+>.*?</[^>]+>', '', content, flags=re.DOTALL)
                        content = re.sub(r'<[^>]+>', '', content)
                        content = re.sub(r'The current local time is:.*$', '', content, flags=re.MULTILINE).strip()
                        if content:
                            last_prompt = content
                            last_response = ""  # new turn started
                            last_timestamp = time_part
                    elif stype == "PLANNER_RESPONSE":
                        content = step.get("content", "")
                        if content:
                            # Strip raw file:// URLs inside markdown links
                            content = re.sub(r'\[([^\]]+)\]\(file://[^\)]+\)', r'\1', content)
                            last_response = content
                            last_timestamp = time_part
                        tool_calls = step.get("tool_calls", [])
                        if tool_calls:
                            tc = tool_calls[-1]
                            last_tool = tc.get("toolAction") or tc.get("toolSummary") or "Running tool"
                    elif stype == "TOOL_RESULT":
                        last_tool = None
        except Exception as e:
            log("Error reading transcript:", e)
            return None

        if not last_prompt and not last_response:
            return None

        blocks = parse_markdown_blocks(last_response) if last_response else []
        return {
            "id": f"agy_{int(os.path.getmtime(file_path))}",
            "assistant": "Antigravity",
            "prompt": last_prompt,
            "timestamp": last_timestamp or time.strftime("%H:%M"),
            "status": "tool_running" if last_tool else "idle",
            "tool_status": last_tool,
            "raw_markdown": last_response,
            "blocks": blocks
        }

    def start(self):
        if self.pipe_mode:
            threading.Thread(target=self._pipe_reader, daemon=True).start()
        threading.Thread(target=self._watch_loop, daemon=True).start()

    def stop(self):
        self._stop = True

    def _pipe_reader(self):
        """Read raw markdown from stdin (--pipe) into a rolling buffer.
        Blocks on readline in this thread; the watch loop picks up whatever
        accumulated since the last tick."""
        try:
            for line in sys.stdin.buffer:
                line = line.decode("utf-8", errors="replace")
                with self.lock:
                    self.pipe_buffer.append(line)
                    if len(self.pipe_buffer) > 4000:  # bound memory
                        del self.pipe_buffer[: len(self.pipe_buffer) // 2]
                self.pipe_dirty.set()
        except Exception as e:
            log("pipe reader ended:", e)

    def _newest_jsonl(self, directory):
        """Newest *.jsonl under a directory (recursive), or None."""
        try:
            files = glob.glob(os.path.join(directory, "**", "*.jsonl"),
                              recursive=True)
            # A session file can be rotated/deleted between the glob and
            # the stat — a vanished file must not kill the watcher thread.
            files = [f for f in files if os.path.exists(f)]
            return max(files, key=os.path.getmtime) if files else None
        except OSError:
            return None

    def _parse_auto(self, path):
        """Parse a transcript in either known format (antigravity first,
        then claude); returns (turn, source_label) or (None, None)."""
        turn = self.parse_antigravity_transcript(path)
        if turn:
            return turn, "Antigravity"
        turn = self.parse_claude_transcript(path)
        if turn:
            return turn, "Claude Code"
        return None, None

    def _watch_loop(self):
        last_mtime = 0
        current_file = self.watch_path

        while not self._stop:
            # --pipe: stdin is the transcript; no file watching at all.
            if self.pipe_mode:
                if self.pipe_dirty.is_set():
                    self.pipe_dirty.clear()
                    with self.lock:
                        text = "".join(self.pipe_buffer).strip()
                    if text:
                        turn = {
                            "id": f"pipe_{int(time.time())}",
                            "assistant": "Terminal pipe",
                            "prompt": (text.splitlines() or [""])[0].strip(),
                            "timestamp": time.strftime("%H:%M"),
                            "status": "idle",
                            "tool_status": None,
                            "raw_markdown": text,
                            "blocks": parse_markdown_blocks(text),
                        }
                        with self.lock:
                            turn["source_mode"] = self.mode
                            turn["active_source"] = "stdin pipe"
                            if (not self.current_turn or
                                    self.current_turn.get("raw_markdown") != text):
                                self.revision += 1
                                turn["revision"] = self.revision
                                self.current_turn = turn
                                if (not self.history or
                                        self.history[-1].get("raw_markdown") != text):
                                    self.history.append(turn)
                                    if len(self.history) > 30:
                                        self.history.pop(0)
                                log(f"Updated pipe turn rev {self.revision}: "
                                    f"{turn['prompt'][:30]}...")
                time.sleep(0.5)
                continue

            if not self.watch_path:
                agy_f = self.find_latest_antigravity_transcript()
                claude_f = self.find_latest_claude_transcript()

                agy_mt = os.path.getmtime(agy_f) if agy_f and os.path.exists(agy_f) else 0
                claude_mt = os.path.getmtime(claude_f) if claude_f and os.path.exists(claude_f) else 0

                if self.mode == "antigravity" and agy_f:
                    current_file = agy_f
                    self.active_source = "Antigravity"
                elif self.mode == "claude" and claude_f:
                    current_file = claude_f
                    self.active_source = "Claude Code"
                else:
                    # Auto mode: pick newer
                    if claude_mt > agy_mt and claude_f:
                        current_file = claude_f
                        self.active_source = "Claude Code (Auto)"
                    elif agy_f:
                        current_file = agy_f
                        self.active_source = "Antigravity (Auto)"
            elif os.path.isdir(self.watch_path):
                # --watch DIR: follow the newest transcript under it.
                try:
                    newest = self._newest_jsonl(self.watch_path)
                    if newest and newest != current_file:
                        current_file = newest
                        last_mtime = 0
                        log(f"watch: following {current_file}")
                except Exception as e:
                    log("watch dir scan error:", e)

            if self.force_reload:
                last_mtime = 0
                self.force_reload = False

            if current_file and os.path.exists(current_file):
                try:
                    mtime = os.path.getmtime(current_file)
                    if mtime != last_mtime:
                        last_mtime = mtime
                        turn, source = self._parse_auto(current_file)
                        if turn:
                            with self.lock:
                                turn["source_mode"] = self.mode
                                turn["active_source"] = source
                                if (not self.current_turn or
                                        self.current_turn.get("raw_markdown") != turn.get("raw_markdown") or
                                        self.current_turn.get("prompt") != turn.get("prompt") or
                                        self.current_turn.get("tool_status") != turn.get("tool_status")):
                                    self.revision += 1
                                    turn["revision"] = self.revision
                                    self.current_turn = turn
                                    if not self.history or self.history[-1].get("prompt") != turn.get("prompt"):
                                        self.history.append(turn)
                                        if len(self.history) > 30:
                                            self.history.pop(0)
                                    log(f"Updated {source} turn rev {self.revision}: "
                                        f"{turn.get('prompt')[:30]}...")
                except Exception as e:
                    log("Watch error:", e)

            time.sleep(0.5)


WATCHER: SessionWatcher = None


class StreamHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def _authorized(self):
        """Same enforcement as the mirror server: off until a Kindle has
        paired, then the request must carry the paired token."""
        return pairing.authorized(
            self.headers.get("X-YB-Kindle-Id") or None,
            self.headers.get("X-YB-Secret") or "")

    def _json(self, obj, code=200, cors=False):
        body = json.dumps(obj, ensure_ascii=False).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        if cors:
            self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_OPTIONS(self):
        """Preflight for the receive page's cross-origin /api/pair POST."""
        self.send_response(204)
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
        self.send_header("Access-Control-Allow-Headers", "Content-Type")
        self.send_header("Access-Control-Allow-Private-Network", "true")
        self.send_header("Access-Control-Max-Age", "86400")
        self.send_header("Content-Length", "0")
        self.end_headers()

    @staticmethod
    def _is_loopback(client):
        ip = client[0]
        return ip.startswith("127.") or ip == "::1"

    def do_GET(self):
        parsed = urllib.parse.urlparse(self.path)
        path = parsed.path
        query = urllib.parse.parse_qs(parsed.query)
        if path != "/health" and not self._authorized():
            self._json(
                {"error": "unauthorized — open the receive page from "
                          "the Mac and re-pair this Kindle"}, 401)
            return

        if path == "/live":
            with WATCHER.lock:
                turn = dict(WATCHER.current_turn or {
                    "id": "idle",
                    "assistant": "AI Stream",
                    "prompt": "Waiting for active AI session...",
                    "timestamp": time.strftime("%H:%M"),
                    "status": "idle",
                    "tool_status": None,
                    "raw_markdown": "### No active session\n\nStart a session in Ghostty or run `claude` / `agy` on your Mac.",
                    "blocks": [
                        {"type": "heading", "level": 3, "text": "No active session"},
                        {"type": "paragraph", "text": "Start a session in Ghostty or run `claude` / `agy` on your Mac."}
                    ],
                    "revision": WATCHER.revision
                })
                turn["source_mode"] = WATCHER.mode
                turn["active_source"] = WATCHER.active_source
            self._json(turn)
            return

        if path == "/sources":
            with WATCHER.lock:
                res = {
                    "sources": ["auto", "antigravity", "claude"],
                    "mode": WATCHER.mode,
                    "active_source": WATCHER.active_source,
                }
            self._json(res)
            return

        if path == "/turn":
            try:
                idx = int(query.get("idx", ["-1"])[0])
            except ValueError:
                self._json({"error": "idx must be an integer"}, 400)
                return
            with WATCHER.lock:
                if WATCHER.history:
                    if idx < 0 or idx >= len(WATCHER.history):
                        turn = WATCHER.history[-1]
                    else:
                        turn = WATCHER.history[idx]
                    self._json(turn)
                    return
            self.send_response(404)
            self.end_headers()
            return

        if path == "/history":
            with WATCHER.lock:
                hist = [
                    {"id": t.get("id"), "prompt": t.get("prompt"), "assistant": t.get("assistant"), "timestamp": t.get("timestamp")}
                    for t in WATCHER.history
                ]
            self._json({"history": hist, "count": len(hist)})
            return

        if path == "/health":
            self._json({"ok": True, "mode": WATCHER.mode, "active_source": WATCHER.active_source, "revision": WATCHER.revision}, cors=True)
            return

        self.send_response(404)
        self.end_headers()

    def do_POST(self):
        parsed = urllib.parse.urlparse(self.path)
        path = parsed.path
        query = urllib.parse.parse_qs(parsed.query)
        if path == "/api/pair":
            # Same loopback-only pairing endpoint as the mirror server, so
            # the receive page can link a Mac where only the AI stream runs.
            if not self._is_loopback(self.client_address):
                self._json({"error": "admin endpoint — localhost only"}, 403)
                return
            try:
                n = int(self.headers.get("Content-Length", 0))
                body = self.rfile.read(n) if n else b"{}"
                p = json.loads(body)
            except (ValueError, OSError):
                self._json({"error": "bad json"}, 400)
                return
            kid = p.get("kindle_id") or ""
            name = p.get("kindle_name") or "Kindle"
            tok = p.get("token") or ""
            dev_id = p.get("device_id") or ""
            if not kid or not tok:
                self._json({"error": "kindle_id and token are required"}, 400)
                return
            ok = pairing.pair(kid, name, tok, dev_id)
            if ok:
                log(f"paired Kindle {name!r} ({kid})")
            self._json({"ok": ok}, cors=True)
            return
        if path == "/api/challenge":
            # Pairing self-heal, same as the mirror server: prove this Mac
            # still holds the pairing to a reader that found us at a new IP.
            mac = pairing.challenge(
                query.get("kindle_id", [""])[0],
                query.get("nonce", [""])[0],
            )
            if mac is None:
                self._json({"error": "unknown kindle"}, 404)
            else:
                self._json({"mac": mac})
            return
        if not self._authorized():
            self._json(
                {"error": "unauthorized — open the receive page from "
                          "the Mac and re-pair this Kindle"}, 401)
            return

        if path == "/source":
            new_source = query.get("set", ["auto"])[0]
            WATCHER.set_mode(new_source)
            self._json({"ok": True, "mode": WATCHER.mode})
            return

        self.send_response(404)
        self.end_headers()


def run_discovery(tcp_port=STREAM_PORT):
    """Answer UDP broadcast discovery requests so Kindle automatically finds this streamer."""
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
    try:
        s.bind(("", DISCOVER_PORT))
    except OSError:
        log(f"UDP discovery port {DISCOVER_PORT} in use; Kindle can connect via direct IP on TCP {tcp_port}")
        return
    log(f"UDP discovery active on :{DISCOVER_PORT}")
    while True:
        try:
            data, addr = s.recvfrom(256)
            kindle_id = None
            nonce = None
            text = data.decode("utf-8", "replace")
            if " id=" in text:
                kindle_id = text.split(" id=", 1)[1].split(" ", 1)[0].strip() or None
            if " nonce=" in text:
                nonce = text.split(" nonce=", 1)[1].split(" ", 1)[0].strip() or None
            if data.startswith(b"ybstream"):
                s.sendto(pairing.announcement(tcp_port, kindle_id, nonce).encode(),
                         addr)
            elif data.startswith(b"ybmirror"):
                # A mirror-protocol probe wants the mirror server, which
                # this process does not serve. Answer with the mirror port
                # so the reader gets a clean connection-refused instead of
                # a confusing 404 on the AI port. When the mirror server is
                # running it owns 8766, so this branch never fires.
                s.sendto(pairing.announcement(MIRROR_PORT, kindle_id, nonce).encode(),
                         addr)
        except OSError:
            continue


def main():
    global WATCHER
    parser = argparse.ArgumentParser(description="AI Companion Streamer for Kindle")
    parser.add_argument("--port", type=int, default=STREAM_PORT, help="HTTP server port")
    parser.add_argument("--watch", type=str, default=None, help="Specific file or directory to watch")
    parser.add_argument("--pipe", action="store_true", help="Read raw markdown from stdin pipe")
    args = parser.parse_args()

    WATCHER = SessionWatcher(watch_path=args.watch, pipe_mode=args.pipe)
    WATCHER.start()

    t = threading.Thread(target=run_discovery, args=(args.port,), daemon=True)
    t.start()

    server = ThreadingHTTPServer(("0.0.0.0", args.port), StreamHandler)
    log(f"AI Stream Server listening on http://0.0.0.0:{args.port}")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        log("Stopping server...")
        WATCHER.stop()
        server.server_close()


if __name__ == "__main__":
    main()
