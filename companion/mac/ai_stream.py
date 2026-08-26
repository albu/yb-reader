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

STREAM_PORT = 8768
DISCOVER_PORT = 8766


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
        t = threading.Thread(target=self._watch_loop, daemon=True)
        t.start()

    def stop(self):
        self._stop = True

    def _watch_loop(self):
        last_mtime = 0
        current_file = self.watch_path
        current_type = "agy"

        while not self._stop:
            if not self.watch_path and not self.pipe_mode:
                agy_f = self.find_latest_antigravity_transcript()
                claude_f = self.find_latest_claude_transcript()
                
                agy_mt = os.path.getmtime(agy_f) if agy_f and os.path.exists(agy_f) else 0
                claude_mt = os.path.getmtime(claude_f) if claude_f and os.path.exists(claude_f) else 0

                if self.mode == "antigravity" and agy_f:
                    current_file = agy_f
                    current_type = "agy"
                    self.active_source = "Antigravity"
                elif self.mode == "claude" and claude_f:
                    current_file = claude_f
                    current_type = "claude"
                    self.active_source = "Claude Code"
                else:
                    # Auto mode: pick newer
                    if claude_mt > agy_mt and claude_f:
                        current_file = claude_f
                        current_type = "claude"
                        self.active_source = "Claude Code (Auto)"
                    elif agy_f:
                        current_file = agy_f
                        current_type = "agy"
                        self.active_source = "Antigravity (Auto)"
            
            if self.force_reload:
                last_mtime = 0
                self.force_reload = False

            if current_file and os.path.exists(current_file):
                try:
                    mtime = os.path.getmtime(current_file)
                    if mtime != last_mtime:
                        last_mtime = mtime
                        if current_type == "claude":
                            turn = self.parse_claude_transcript(current_file)
                        else:
                            turn = self.parse_antigravity_transcript(current_file)
                            
                        if turn:
                            with self.lock:
                                turn["source_mode"] = self.mode
                                turn["active_source"] = self.active_source
                                if not self.current_turn or self.current_turn.get("raw_markdown") != turn.get("raw_markdown") or self.current_turn.get("prompt") != turn.get("prompt") or self.current_turn.get("tool_status") != turn.get("tool_status"):
                                    self.revision += 1
                                    turn["revision"] = self.revision
                                    self.current_turn = turn
                                    if not self.history or self.history[-1].get("prompt") != turn.get("prompt"):
                                        self.history.append(turn)
                                        if len(self.history) > 30:
                                            self.history.pop(0)
                                    log(f"Updated {self.active_source} turn rev {self.revision}: {turn.get('prompt')[:30]}...")
                except Exception as e:
                    log("Watch error:", e)

            time.sleep(0.5)


WATCHER: SessionWatcher = None


class StreamHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def _json(self, obj, code=200):
        body = json.dumps(obj, ensure_ascii=False).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Access-Control-Allow-Origin", "*")
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        parsed = urllib.parse.urlparse(self.path)
        path = parsed.path
        query = urllib.parse.parse_qs(parsed.query)

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
            idx = int(query.get("idx", ["-1"])[0])
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
            self._json({"ok": True, "mode": WATCHER.mode, "active_source": WATCHER.active_source, "revision": WATCHER.revision})
            return

        self.send_response(404)
        self.end_headers()

    def do_POST(self):
        parsed = urllib.parse.urlparse(self.path)
        path = parsed.path
        query = urllib.parse.parse_qs(parsed.query)

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
            if data.startswith(b"ybmirror") or data.startswith(b"ybstream"):
                s.sendto(f"ybmirror {tcp_port}".encode(), addr)
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
