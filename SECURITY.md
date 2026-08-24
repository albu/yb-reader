# Security model

yb-reader is single-user software for one jailbroken Kindle. This document
states the trust boundaries, what protects each network surface, and which
tradeoffs were accepted. It describes the code as it is; anything not
written here is not guaranteed.

## Surfaces

| Surface | Direction | Exposure |
|---|---|---|
| `receive` web uploader | inbound | LAN, only while the Receive screen is open |
| `mirror` screen stream | outbound | reader polls a server you run |
| `ai_stream` turn feed | outbound | reader polls host/port from `mirror.conf` |
| USB mass storage | local | stock Kindle MSD switch |
| SSH toggle | inbound | off by default, user-enabled via curtain |

## receive (the only inbound HTTP service)

**Credential.** Every request must present a six-digit PIN, generated from
`/dev/urandom` at listener start (`receive::generate_pin`). Three ways in:

- scan the QR — the URL already carries `?t=<pin>`;
- type the bare address — a minimal pairing page asks for the code shown
  on the Kindle screen;
- scripts/curl — `X-YB-Token:` header.

Input is normalized before comparison (`482 913`, `482-913`, lowercase all
work); the compare is length-checked constant-time folding.

**Why six digits is enough here.** Entropy alone would not be: every
failed authorization parks its connection seat for 800 ms
(`AUTH_FAIL_DELAY`) while holding one of at most `MAX_CONNS = 8`
connection threads, capping guessing around ten tries per second — about
28 h to exhaust the space, ~14 h expected hit. The decisive bound is
time, not math: the listener and its iptables ACCEPT rule exist **only
while the Receive screen is open**, a minutes-scale window in practice,
and use is visible — deliveries on screen, and auth misses land in the
persistent log (first one only; logging every miss of a sustained attack
would burn flash writes for noise).

**Hardening already in place**, mostly from audit rounds:

- upload bodies stream to disk in 64 KB chunks (no RAM-sized files);
- writes are capped to the declared Content-Length (underflow bypass fixed);
- duplicate conflicting Content-Length → 400;
- filename sanitization plus an extension allowlist (`.sh`, `.mobi`, … refused);
- path traversal neutralized (`..%2F..%2Fescape.epub` lands as `escape.epub`);
- `My Clippings.txt` / `JAILBROKEN.txt` protected from delete **and move**;
- error bodies JSON-escaped; header phase capped (16 KB / 30 s).

**Accepted residuals.**

- The PIN rides in URLs: it lands in the phone's browser history and any
  intermediate's logs — none exist on a home LAN, but don't reuse the URL
  on networks you don't control.
- No rate-limit memory across reboots or screens: closing the screen ends
  the session entirely, which is the intended lifecycle.
- No CSRF protection beyond the PIN itself; a malicious web page cannot
  read responses (no CORS) and cannot guess the PIN, but a form-based
  cross-origin POST with a known PIN is not separately blocked.
- Seat-stalling DoS: eight slow clients dripping request headers park
  every connection seat (up to 30 s each) and shed legitimate ones with
  503s. Not meaningfully defensible on a shared LAN — an attacker can jam
  the radio anyway — so it is accepted and written down here.

## mirror / ai_stream (outbound)

The reader opens no port for these. The risk is the opposite direction: a
hostile LAN host that answers the configured address feeds frames/markdown
to the device. That input is treated as untrusted — frame bodies are
capped at 4 MB, markdown nesting at depth 64, PNG decoding rejects
sub-IHDR first frames, and parsing happens inside `catch_unwind` so a
parser panic surfaces as an open error, not a crash loop.

**Optional shared credential.** `mirror.conf` accepts a `SECRET=` line;
when present, every mirror and ai_stream request carries
`X-YB-Secret: <value>` (see `protocol::Conn::set_secret`). The server side
opts in by requiring that header — until it does, the credential is inert.
Without a `SECRET=` line the wire format is byte-identical to the legacy
protocol, so old servers keep working.

**Known gap — discovery leaks the secret.** UDP discovery is
unauthenticated and first-responder-wins: on a fresh network, a hostile
host that answers before the real Mac becomes the connection target and
receives the `X-YB-Secret` header. After one success the address is pinned
into `SERVER=`, closing the race. Rule: **use SECRET only together with a
hand-pinned `SERVER=`**, never relying on discovery on networks you don't
control. Without a secret, losing the same race means only that frames are
served by a stranger — the pre-existing exposure above.

## Book parsers

Books are attacker-controlled input by definition (sideloaded files, web
uploads). Invariants held by `yread`:

- zip entries stream under a hard ceiling (`MAX_ENTRY_BYTES`); nothing is
  materialized beyond it;
- fb2 `<binary>` images above 8 MB of base64 are dropped whole and their
  ids reported in `Book::capped_binaries` (and plogged) — never decoded
  from a truncated buffer;
- epub text uses lossy UTF-8; missing spine items become placeholder
  chapters instead of shifting TOC indices.

Parser panics are caught (`catch_unwind`) and reported as open errors:
a crafted book costs a dialog, not the reading session. On-device release
builds keep `overflow-checks = true` and unwind-on-panic for exactly this
reason.

## Device-level

- TERM/INT run restore + exit 0 through the app loop (async-signal-safe
  handler: atomic store only), so shutdown cascades don't look like crashes.
- The Wi-Fi manual-off latch outranks wake intents and survives reboot.
- The persistent log never receives credentials (the receive PIN is
  explicitly excluded).

## Reporting

Personal project — no security channel. If you find something, open an
issue with reproduction steps; assume no embargo.
