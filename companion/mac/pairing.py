"""Shared pairing state for server.py and ai_stream.py.

The Kindle's receive page completes pairing by POSTing the PIN-minted
token to the Mac's local mirror server (http://localhost:8765/api/pair);
both stream servers read the same record and require X-YB-Secret on
Kindle-facing requests once at least one Kindle has paired.

The announcement identity is also shared: the Mac announces id=/name= in
its UDP discovery replies, which is what the reader's trust check compares
against before attaching a paired token (an IP alone is not an identity).
The announced id is the pairing browser's device id when we have one (the
Kindle stores that same id for the device, so the match holds), else a
locally generated stable id.
"""

import json
import hashlib
import hmac as _hmac
import os
import socket
import threading
import time

DEVICES_PATH = os.path.expanduser("~/.yb-mirror-devices.json")
ID_PATH = os.path.expanduser("~/.yb-mirror-id")
CONF_PATH = os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "mirror.conf")
_lock = threading.Lock()
_cached_mtime = None
_cached = {}
_secret_mtime = None
_secret = None


def load():
    """Paired devices, cached by file mtime — authorized() runs on every
    frame poll and heartbeat, so re-reading + json.loads per request would
    be ~2-4 disk reads/sec for no benefit."""
    global _cached_mtime, _cached
    try:
        mtime = os.stat(DEVICES_PATH).st_mtime_ns
    except OSError:
        _cached_mtime = None
        _cached = {}
        return {}
    with _lock:
        if _cached_mtime == mtime:
            return _cached
        try:
            with open(DEVICES_PATH) as f:
                _cached = json.load(f)
        except (OSError, ValueError):
            _cached = {}
        _cached_mtime = mtime
        return _cached


def save(devices):
    """Atomic write (tmp + rename) so concurrent readers never see a
    half-written file, 0600 because the file holds bearer tokens."""
    global _cached_mtime
    with _lock:
        tmp = DEVICES_PATH + ".tmp"
        with open(tmp, "w") as f:
            json.dump(devices, f, indent=2)
        os.replace(tmp, DEVICES_PATH)
        _cached_mtime = None  # next load() re-reads
        try:
            os.chmod(DEVICES_PATH, 0o600)
        except OSError:
            pass


def pair(kindle_id, kindle_name, token, device_id=None):
    devices = load()
    entry = dict(devices.get(kindle_id, {}))
    entry.update({"name": kindle_name, "token": token,
                  "paired_at": time.time()})
    if device_id:
        entry["device_id"] = device_id
    devices[kindle_id] = entry
    save(devices)
    return True


def challenge(kindle_id, nonce):
    """Proof that this Mac still holds a pairing, without revealing the
    token: HMAC-SHA256(token, nonce). The reader sends a fresh nonce when
    it discovers us from a *new* IP and refreshes its stored address on a
    match — a DHCP change self-heals with no manual steps. Returns the
    lowercase hex MAC, or None when the Kindle id is not paired."""
    entry = load().get(kindle_id or "")
    if not entry:
        return None
    token = (entry.get("token") or "").encode()
    if not token:
        return None
    return _hmac.new(token, (nonce or "").encode(), hashlib.sha256).hexdigest()


def identity():
    """(announced_id, announced_name) for discovery replies."""
    for entry in load().values():
        if entry.get("device_id"):
            return entry["device_id"], (entry.get("name") or "Mac")
    try:
        with open(ID_PATH) as f:
            mac_id = f.read().strip()
        if mac_id:
            return mac_id, socket.gethostname()
    except OSError:
        pass
    mac_id = "mac_" + os.urandom(4).hex()
    try:
        with open(ID_PATH, "w") as f:
            f.write(mac_id + "\n")
        os.chmod(ID_PATH, 0o600)
    except OSError:
        pass
    return mac_id, socket.gethostname()


def static_secret():
    """Optional static secret from the repo's mirror.conf (SECRET=) — the
    documented alternative credential path. Set the same SECRET= in the
    Kindle's mirror.conf and in companion/mirror.conf, and requests
    carrying it are accepted alongside paired tokens. Cached by mtime."""
    global _secret_mtime, _secret
    try:
        mtime = os.stat(CONF_PATH).st_mtime_ns
    except OSError:
        return None
    with _lock:
        if mtime == _secret_mtime:
            return _secret
        secret = None
        try:
            with open(CONF_PATH) as f:
                for line in f:
                    line = line.strip()
                    if line.startswith("SECRET="):
                        secret = line.split("=", 1)[1].strip() or None
                        break
        except OSError:
            pass
        _secret_mtime = mtime
        _secret = secret
        return secret


def identity_for(kindle_id):
    """Announced (id, name) for one specific paired Kindle: its own
    device_id (the browser id registered on *that* Kindle), so a Mac
    serving several Kindles announces each Kindle's own identity instead of
    the first pairing's. Falls back to identity() for unknown ids."""
    entry = load().get(kindle_id or "")
    if entry and entry.get("device_id"):
        return entry["device_id"], (entry.get("name") or "Mac")
    return identity()


def announcement(tcp_port, kindle_id=None, nonce=None):
    """The UDP discovery reply: `ybmirror <port> id=... name="..."`.
    Quoting matches the reader's parse_quoted_or_token; the name is
    sanitized so a hostile device name can't smuggle quotes into the
    reply (it is only ever a label, but keep the wire clean).

    When the probe carried a `nonce=`, the reply also carries
    `mac=HMAC-SHA256(token, nonce)` — the proof that this Mac holds the
    pairing, so the reader can trust it from any IP (DHCP moves included)
    and a host that merely echoes the id gets nothing. The token itself is
    never transmitted."""
    mac_id, name = identity_for(kindle_id) if kindle_id else identity()
    name = name.replace("\\", "").replace('"', "")
    base = f'ybmirror {tcp_port} id={mac_id} name="{name}"'
    mac = challenge(kindle_id, nonce) if (kindle_id and nonce) else None
    return base + f" mac={mac}" if mac else base


def authorized(kindle_id, secret):
    """True when a request's credential satisfies the enforcement policy.

    Enforcement is OFF while no Kindle has paired (the legacy wire stays
    byte-identical, so old unpaired setups keep working). Once a paired
    Kindle exists, the secret must match that Kindle's paired token — or
    any paired token when the request doesn't say which Kindle it is.
    """
    devices = load()
    static = static_secret()
    if not devices and not static:
        return True
    if not secret:
        return False
    if static and _hmac.compare_digest(secret, static):
        return True
    if kindle_id and kindle_id in devices:
        return _hmac.compare_digest(secret, devices[kindle_id].get("token") or "")
    return any(_hmac.compare_digest(secret, dev.get("token") or "")
               for dev in devices.values())
