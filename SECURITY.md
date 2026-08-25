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

**Credentials.** Two tiers, checked per request (the store is reloaded from
disk on every request, so a UI revocation is immediate):

1. *Session PIN* — six digits from `/dev/urandom`, lives only for the
   receive session. Ways in: scan the QR (URL carries `?t=<pin>`), the
   pairing page, or `X-YB-Token:` for scripts. Input is normalized before
   comparison (`482 913`, `482-913`, lowercase all work); the compare is
   length-checked constant-time folding.
2. *Paired device token* — 128-bit `tok_…` minted at pairing
   (`POST /api/pair`, PIN-verified), presented as the `yb_token` cookie
   (`Path=/; Max-Age=315360000; SameSite=Lax; HttpOnly` — set only by the
   server, never writable/readable from JS), or `X-YB-Device-Token` /
   `Authorization: Bearer` for scripts. Token comparison is constant-time.
   Scopes: `inbound` (file copy only — never used as a mirror/stream
   credential) or `all`; the server allowlists the value, everything else
   defaults to `inbound`.

`GET /api/handshake` is deliberately **unauthenticated** LAN discovery: it
exposes the Kindle's name, `kindle_id`, screen dimensions, and free space.
`kindle_id` is an identifier, not a credential — but see the outbound
section for where it travels.

**Pairing brute-force.** `POST /api/pair` fails cost 800 ms of a
connection seat and increments a counter; 10 failures lock the endpoint
with 429. The lockout decays after 5 minutes of quiet, resets on a
successful pairing, and resets when the receive screen is reopened.
A successful pairing mints a 10-year cookie — that asymmetry is why the
PIN path is the more heavily rate-limited of the two.

**Hardening already in place**, mostly from audit rounds:

- upload bodies stream to disk in 64 KB chunks (no RAM-sized files);
- writes are capped to the declared Content-Length (underflow bypass fixed);
- duplicate conflicting Content-Length → 400;
- filename sanitization plus an extension allowlist (`.sh`, `.mobi`, … refused);
- path traversal neutralized (`..%2F..%2Fescape.epub` lands as `escape.epub`);
- `My Clippings.txt` / `JAILBROKEN.txt` protected from delete **and move**;
- error bodies JSON-escaped; header phase capped (16 KB / 30 s);
- all destructive endpoints (`/api/mkdir`, `/api/move`, `/api/delete`,
  `/upload`, `/api/pair`) are POST-only — with `SameSite=Lax` a cross-site
  top-level GET carries the cookie but can only *read*.

**Token storage.** `devices.json` and `kindle_id.json` live under
`/var/local/yb-reader/` (ext filesystem, `0600` files / `0700` directory,
never exported over USB). Legacy copies at `/mnt/us/extensions/mirror/`
are migrated by rename on first run — `/mnt/us` is vfat: mode bits are
not enforced there and everything on it is visible to any computer the
Kindle is plugged into. `YB_DEVICES_PATH` / `YB_KINDLE_ID_PATH` override
the locations (tests use this).

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

- The guest PIN rides in URLs: it lands in the phone's browser history and
  any intermediate's logs — none exist on a home LAN, but don't reuse the
  QR URL on networks you don't control. Paired devices never carry their
  token in a URL (cookie only).
- Pairing-lockout DoS: ten garbage `POST /api/pair` requests disable
  pairing for five minutes (or until the receive screen is reopened) —
  cheap for a LAN attacker, bounded, and chosen over an unlimited-guess
  alternative. Guest-PIN scanning is unaffected.
- No rate-limit memory across reboots or screens: closing the screen ends
  the session entirely, which is the intended lifecycle.
- No CSRF protection beyond credentials themselves; a malicious web page
  cannot read responses (no CORS) and cannot obtain the cookie/ PIN, but a
  form-based cross-origin POST with stolen credentials is not separately
  blocked.
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

**Credentials.** Two sources, in priority order:

- `mirror.conf` `SECRET=` — static shared secret, sent as `X-YB-Secret`
  whenever present (see `protocol::Conn::set_secret`). Without it the wire
  is byte-identical to the legacy protocol, so old servers keep working.
- *Paired device tokens* (scope `all`) — attached to discovery replies
  only when **both** the reply's UDP source IP matches the device's stored
  IP **and** the reply's claimed `device_id` matches the stored record
  (`protocol::trust_reply`). Neither alone suffices: an IP is not an
  identity (DHCP hands lapsed leases to the next host — a legacy id-less
  reply from a trusted IP gets nothing), and a claimed id is not proof
  (any host can type `id=<victim>`).

**SERVER= is pinned only after proof.** A discovered host is persisted to
`mirror.conf` only after it completes a real exchange (a 200 frame fetch /
`/live`) — not on winning the UDP race. A rogue responder can win one
discovery round; it cannot fake a working stream, so it stays
session-scoped and is forgotten on restart.

**DHCP moves downgrade, loudly.** If a paired Mac changes IP, its token is
not sent (the IP+id match fails) and mirror/ai_stream degrade to
unauthenticated until the device opens the receive web page once from its
new IP (which refreshes the stored IP) or re-pairs. A one-line hint is
plogged at most once per process.

**conf.secret never reaches a discovery fallback.** The static `SECRET=`
is sent to the explicitly configured `SERVER=` host only — including when
discovery re-confirms that same host (trust-by-config). An anonymous
responder that wins the discovery race gets nothing. The trade: a legacy
un-paired Mac that changes IP stops authenticating until `SERVER=` is
re-pinned — a visible failure with an obvious fix, which the old behavior
would have silently "solved" by handing the secret to whoever inherited
the IP.

**Residual: `kindle_id` travels to any discovered host.** Every outbound
request carries `X-YB-Kindle-Id`, including to untrusted fallback servers,
so a rogue responder harvests the Kindle's stable identifier (same value
`/api/handshake` exposes). It is a routing identifier, not a credential;
nothing accepts it as one.

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
