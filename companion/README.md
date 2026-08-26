# yb-mirror — Kindle as a mirror screen for your Mac

Your Mac runs everything (browser, reader, whatever); the Kindle shows a
grayscale copy of one window; taps become real ←/→ arrow keys on the Mac.

```
Mac (server.py)                        Kindle (KOReader plugin)
  Quartz window capture ── PNG gray ─▶  ImageWidget fullscreen
  CGEventPostToPid ←/→  ◀── tap zones ──  tap/swipe gestures
```

Page-turn budget: one request tells the server to press the key and return
the new frame as soon as the window content changes *and settles* — chapter
boundaries render a loading screen first, and the server waits it out before
answering. The Mac's own share is ~0.05–0.15 s per turn (the key goes
straight to the reader app's process via `CGEventPostToPid` — no osascript
activation, no focus steal; capture is in-process Quartz; PNGs encode at
zlib level 1); what you feel on top of that is the site's own re-render
after the keypress, Wi-Fi + Kindle-side PNG decode, and the e-ink partial
refresh (~0.1–0.25 s, panel physics). 4-bit grayscale PNGs halve the Wi-Fi
transfer; levels are rounded, not truncated, so paper stays pure white. The
plugin keeps one HTTP/1.1 connection open for the whole session (a `/ping`
heartbeat every 15 s keeps it — and the Kindle's power-saving radio — warm)
and finds the Mac by itself: it broadcasts a UDP probe on `<port>+1`, and
whatever answers becomes `SERVER=` in `mirror.conf`.

## Getting it

**From your Kindle** — the yb-reader receive page (the one you use to send
books over Wi-Fi) offers a *source* zip in its "Companion (macOS)" card
(`make dist` in the yb-reader repo builds it; deploy carries it to the
device). It is small on purpose, and the app is built **on your Mac**, which
sidesteps the whole download-and-trust dance: locally built apps carry no
quarantine attribute, so there are no Gatekeeper warnings and no signing
certificates to buy.

```bash
unzip yb-mirror.zip
cd yb-mirror
uv sync                      # installs deps into a local venv (needs uv once)
bash mac/make-app.sh         # builds ~/Applications/yb-mirror.app (needs clang)
open ~/Applications/yb-mirror.app
```

Then grant the three privacy permissions once (below). Requirements: macOS,
[`uv`](https://docs.astral.sh/uv/) and Xcode Command Line Tools.

## Quick start (Mac)

```bash
cd yb-mirror
uv sync
uv run mac/server.py --app Safari --autosize --crop-top 55
```

One command starts the whole session: the server reopens your last book
(see below), sizes the window to the Kindle's aspect, and holds a
`caffeinate` no-idle-sleep assertion for its own lifetime (so the Mac can't
doze off mid-read; opt out with `--no-stay-awake`). The **display** is
held awake only while the Kindle is actually using the mirror and released
`--display-idle` seconds after the last contact (default 30 min —
distraction-proof); if it did sleep, the next Kindle tap wakes it and the
session resumes by itself. Still run it in a normal
Terminal window of your own — processes left running from detached/
background contexts get demoted by macOS after a while and then accept
connections without ever answering them.

First-ever run (or when you want a *new* reader window): add
`--url https://books.example.com`. It opens a *new* frontmost window, which
the server then mirrors. Without `--url`, an already-open reader window is
mirrored as-is, and a cold start resumes the last book.

## Menu bar app (no Terminal)

```bash
bash mac/make-app.sh     # once; needs Xcode Command Line Tools (clang)
open ~/Applications/yb-mirror.app
```

A `YB` icon in the menu bar — with a **dot in its bottom-right corner
while the mirror is on** (template-mode, adapts to dark menu bars):
**Start mirror** / **Stop mirror** (same
flags as the quick start; refuses politely if a server already runs in a
Terminal), **Send book to Kindle…** (file picker → send.py → notification;
click it again to cancel), **Open log**, **Quit** — quitting stops
everything it started. No Dock icon (`LSUIElement`). Everything runs
inside the project's uv venv — the launcher only `cd`s to the repo and
execs `uv run`; nothing ever installs into system python. To start it at
login: System Settings → General → Login Items → add `yb-mirror.app`.
If the repo moves, rebuild with `make-app.sh` (the path is baked into the
compiled launcher — a *script* executable there is refused by macOS
LaunchServices with `-10669`, which is why it's C).

**Last-book memory**: a background job follows the mirrored window and
remembers its URL in `~/.yb-mirror-last-url` (refreshed every minute; it
follows book switches, since the window title *is* the book name). On a
cold start — no window open, no `--url` given — the server reopens the
last page automatically: The server encodes the book in the URL
(`/reader/<id>`) and syncs your position to your account, so the book
comes back exactly where you left it. `--no-resume` disables this.

- `--autosize` resizes Safari so its content is exactly the Kindle's 0.75
  aspect (1236×1648) — no text is cropped and nothing is stretched. It first
  sets the window height, reads back the real (possibly clamped) height, then
  sets the width to match.
- `--crop-top 55` removes the Safari title bar / traffic lights (the one
  "crop" that's safe — it's browser chrome, not text). Compare the demo
  images in `debug/` (`crop_demo_top*.png`) and tune the number.
- The server automatically removes the window shadow/rounded corners via the
  alpha channel, applies a gamma contrast curve (default 2.0), and resamples
  with bilinear interpolation for clean text.

Verify it's alive:

```bash
curl http://127.0.0.1:8765/status
```

### Permissions (System Settings → Privacy & Security), once

- **Screen Recording** for your terminal app — window capture needs it
  (both the in-process Quartz path and the `screencapture` fallback).
  Without it, captures come back empty or title-bar-only.
- **Accessibility** for your terminal app — synthetic arrow keys need it.
- **Automation** for your terminal app → Safari/Chrome — used by `--tab`
  and by the last-book memory (it reads the mirrored window's URL via
  AppleScript; without it there's simply nothing to resume).

### One-time: stable signing identity (recommended)

macOS keys those privacy grants to the app's *code hash*. An ad-hoc
signature gets a fresh hash on every rebuild, so macOS quietly stops
honoring the grants — you'd re-grant Screen Recording & co after each
rebuild. Signing with a stable self-signed identity keeps the hash — and
your grants — across rebuilds.

Keychain Access → Certificate Assistant → **Create a Certificate…**:
Name `yb-mirror dev`, Identity Type *Self-Signed Root*, Certificate Type
*Code Signing*. `make-app.sh` picks it up automatically and falls back to
ad-hoc (with a warning) when it's absent.

### Under the hood: the C launcher and the signing dance

- **The launcher is C, not a script**, because macOS refuses to
  LaunchServices-open an app bundle whose executable is a script (`open`
  fails with `-10669` regardless of signing). `mac/launcher.c` is a tiny
  compiled shim that redirects stdout/stderr to
  `/tmp/yb-mirror-menubar.log` (so tracebacks survive), `cd`s to the repo,
  and `exec`s `uv run mac/menubar.py` — the whole app still runs inside
  this project's uv venv, never system python. The repo path is baked in at
  build time; move the repo, rebuild with `make-app.sh`.
- **The bundle is always signed** because modern macOS wants bundles signed
  (ad-hoc is the floor). The subtle part is *which* signature — see the
  stable identity above. A notarized, Developer-ID-signed build would cost
  $99/yr and still prompt for the same three privacy grants, so for this
  audience the stable local identity is the right stop.
- **The three grants are per-machine by design** — no package can ship with
  them pre-granted. Every user grants them once, in System Settings, on
  their own Mac.

### Options

```
--app NAME            app to mirror (default Safari)
--title SUBSTR        only consider windows whose title contains SUBSTR
--tab SUBSTR          bring the app forward before capturing (single-tab use)
--autosize            resize the window to the optimal mirror size at startup
--url URL             open this URL at startup (fresh sessions)
--no-resume           don't reopen the last remembered page on a cold start
--no-stay-awake       don't hold a caffeinate no-idle-sleep assertion
--display-idle N      secs of Kindle quiet before the screen may sleep
                      again (default 1800; 0 = never hold it awake)
--crop-top N          shave N window points off the top (Safari tab bar ~55)
--crop-bottom N       shave N points off the bottom
--crop-left N         shave N points off the left
--crop-right N        shave N points off the right
--no-shadow-crop      keep the window's shadow/rounded corners
--no-aspect-crop      don't center-crop to 0.75 as a fallback (may look stretched)
--contrast GAMMA      grayscale gamma, higher = darker text (default 2.0, 1.0 = off)
--port N              HTTP port (default 8765)
```

## Kindle side (KOReader plugin — primary)

> **If you run yb-reader, you don't need this section.** The reader ships
> its own Rust port of this plugin (same `/mnt/us/extensions/mirror/`
> config and log paths) — start it from the reader's menu instead. The
> KOReader plugin below is the standalone path for non-yb-reader Kindles.

1. Copy `kindle/koreader-plugin/mirror.koplugin/` into
   `/mnt/us/koreader/plugins/` over USB (only needed when it changes).
2. Eject the Kindle, restart KOReader, then:
   **menu → Tools → Screen mirror (Mac)**.

No configuration: on first start (or whenever the remembered address stops
working — DHCP lease changed, laptop moved networks) the plugin broadcasts a
UDP probe on port 8766, the Mac answers, and the address is remembered in
`/mnt/us/extensions/mirror/mirror.conf`. `SERVER=http://<mac-ip>:8765` in
that file just pins the address to skip the ~1 s discovery; delete the file
to re-discover from scratch. The Mac's firewall must allow **UDP 8766** in
addition to TCP 8765 for discovery (a pinned `SERVER=` works without it).

`REFRESH_EVERY=N` in the same file tunes the anti-ghosting safety net: a
full (flashing) refresh every N frames, default 60, `0` = never (rely on
corner-tap cleaning). The plugin re-reads the file at every start, so no
re-copy needed to change it.

### Controls

- **tap left third** = previous page · **tap elsewhere** = next page
  (swipe left/right works too)
- **tap top-right corner** = screen clean: re-fetch the frame with a full
  (flashing) refresh to wipe ghosting. Normal turns never flash.
- **two-finger tap** = frontlight dialog, without leaving the mirror
- **swipe down (or up)** = exit back to KOReader (works even mid-sync)

## Sending a file to the Kindle (send.py)

USB is only needed when the *plugin* itself changes. For the everyday
"put this book on the Kindle", the direction flips: the Mac briefly
serves one file, and the Kindle fetches it.

```bash
uv run mac/send.py ~/Downloads/some-book.epub
```

The server prints its address, answers the same UDP discovery as the
mirror server, and **exits by itself** once the file has been received —
or after 10 idle minutes (`--wait`). On the Kindle: KOReader →
**Tools → Fetch book from Mac**. The file streams into `/documents`
under its own name (an existing file is overwritten) and the file
manager refreshes by itself.

Delivery is *ack*-based, not write-based: a fetch only counts when the
Kindle confirms the full body arrived intact (`GET /ack`), because a
whole small file can sit in kernel socket buffers while the reader dies
— counting the last `write()` would let the server quit with the file
never delivered. A Wi-Fi drop mid-download is absorbed the usual way:
the plugin retries the same server, send.py re-serves from scratch. The
fetch menu finds send.py at the saved mirror address (port 8767), via
discovery, or at whatever port discovery reported (a `--port` run).

## Kindle side (python client — alternative)

`kindle/mirror.py` + the KUAL extension in `kindle/extensions/mirror/` write
raw frames straight to `/dev/fb0` via evdev taps. No python3 package is
readily available for modern jailbroken Kindles, which is why the KOReader
plugin is the primary path. `kindle/probe.sh` reports fb geometry etc.

## Troubleshooting

**"Mirror: Mac not found — is the server running?"**
- The server isn't reachable and UDP discovery found nothing. Check the
  server is up (`curl http://127.0.0.1:8765/status`), that both devices are
  on the same Wi-Fi, and that the Mac firewall allows incoming TCP 8765 and
  UDP 8766. The plugin retries on the next tap; it re-discovers
  automatically, so a changed IP is not something to fix by hand anymore.
  Details land in `/mnt/us/extensions/mirror/plugin.log`.

**The mirror shows the wrong window/tab**
- The server logs every candidate window at startup (`candidate windows: …`)
  and picks the largest. Use `--title "some text from the window title"` to
  force the right one, or `--tab` to bring the browser forward first.

**Tapping crashes KOReader**
- The plugin on the Kindle is outdated. Re-copy `main.lua` and restart
  KOReader (older versions had a gesture-handling bug).

**Text is cropped on the sides**
- That happens only when the window aspect isn't 0.75 (the fallback
  center-crop cuts width). Use `--autosize` so the window itself is the
  right shape and nothing gets cropped. If you still see it, the window was
  resized manually after autosize.

**Text looks too tall / too wide**
- The window aspect doesn't match 0.75 (stretched to fill). Run with
  `--autosize`. Avoid `--no-aspect-crop` unless you accept stretching.

**Text is blurry / letters look dirty**
- The window is being upscaled — make it larger (`--autosize`). The old
  nearest-neighbour resizer caused jaggies; current frames are bilinear.

**Everything is gray / washed out**
- Increase `--contrast` (default 2.0). Real pages have no true black; the
  gamma curve maps gray text closer to black.

**Black borders / shadow around the image**
- Automatic shadow crop is on; it can be disabled with `--no-shadow-crop`.
  Black borders usually mean an older server build — restart with the current
  `mac/server.py`.

**The server "died" / is silent**
- The server only logs when it serves a frame. If the Kindle isn't
  requesting, the log is quiet — check with
  `lsof -iTCP:8765 -sTCP:LISTEN`. Starting a second instance fails with
  "address already in use" plus a hint about `lsof` — kill the old one or
  use `--port`.

**The mirror died ~10 minutes into Kindle-only reading** *(older builds)*
- Window capture fails on a sleeping display (`no window` errors), and
  page-turn keys posted to Safari don't count as display activity — so the
  screen's own idle timer used to kill mid-book sessions. The server now
  holds the display awake while the Kindle is in contact and wakes it
  (`caffeinate -u`) on the first contact after a sleep; `--display-idle`
  tunes how long the screen stays lit after the Kindle goes quiet.

**curl connects but waits forever / Kindle says "Mac not found"**
- The server process was demoted by macOS into background scheduling
  (typically: left running from a detached/hidden context). It still holds
  the port, so connects succeed and nothing answers. Run it in your own
  Terminal window (the built-in caffeinate keeps it foreground-honest);
  `ps -o state= -p <pid>` showing `SN` is the signature.

**"Fetch book from Mac" says Mac not found**
- send.py isn't running (start it with the file as its argument). If the
  mirror server is running at the same time, that's fine — discovery
  finds the Mac, and the fetch talks to send.py on port 8767 next door.
  The fetch sequence lands in `/mnt/us/extensions/mirror/plugin.log`.

**"client … dropped the connection" lines in the server log**
- Routine: the Kindle's power-saving radio resets the TCP connection every
  ~20–30 s. The protocol absorbs it — the plugin retries the turn with the
  same id, the server answers "duplicate: frame only, no keypress", and no
  page is skipped. It's logged as one quiet line, not a traceback.

**A loading screen was shown between chapters**
- Old bug, fixed: the server now waits for the frame to *settle* and for
  content that doesn't encode like a loader (~10 KB) before answering a
  turn. If a loader ever slips through again, it means the reader's loader
  got bigger — the threshold is `LOADER_RAW_FRACTION` in `server.py`.

**Page turns feel slow**
- Read the budget off the two logs before guessing. The server logs
  `turn: site+capture=… changed=… settled=… encode=…` per turn, and the
  plugin appends one line per request to
  `/mnt/us/extensions/mirror/plugin.log`. `site+capture` is the reader
  site's own re-render (plus ~15 ms capture per poll; chapter boundaries
  add the loader wait); the plugin's `POST /next … ms` line minus the
  server total is Wi-Fi + Kindle decode; the rest is the e-ink panel. If
  the site is still loading when the server's 3 s wait ends, the plugin
  keeps polling (up to 8 s) rather than re-pressing the key, so slow pages
  don't skip. Normal turns never flash the screen; the anti-ghosting flash
  is every `REFRESH_EVERY` frames (60 by default) or a top-right corner
  tap.

## Endpoints

`GET /frame[.png]?w&h[&depth&stride&bpp]` raw-fb or PNG frame, loader-waited
and settled (w/h/bpp are remembered, and any frame-bearing request re-sets
them) · `POST /next /prev[?wait=1[&id=N]]` arrow keys — posted straight to
the app's process, so the reader window keeps working while the Mac's focus
is elsewhere; `id` is an idempotency key (a retried turn with the same id
answers with the current frame instead of pressing the key again — that's
what makes radio-drop retries safe); the wait reply is the new PNG plus
`X-Seq`/`X-Changed`/`X-Settled` headers (`X-Dup` on deduplicated retries) ·
`GET /ping` keep-alive heartbeat · `POST /tap {x,y}` coordinate-mapped mouse
click (for menus; unused by the plugin) · `GET /status` · `POST /rewin` ·
`POST /autosize`. UDP discovery on `<port>+1` answers
`ybmirror <tcp-port>` to broadcast probes. Separately, `mac/send.py`
speaks `GET /book` (raw bytes + `X-Filename`, percent-encoded) ·
`GET /ack` (delivery confirmation — its exit signal) · `GET /status` ·
`GET /ping`, self-exiting after the ack or `--wait`.

## Known limits

- Keep the mirrored window unminimized and on the current Space; if frames
  come back stale, that's usually why.
- No reflow — text is pixels; the window is auto-sized for crisp output.
  On a 1440×900-class display the autosized window tops out at 600×856 pt,
  so the capture is mildly (~3%) upscaled to the Kindle's 1236×1648 — a
  bigger display buys crisper fonts.
- A closed/reopened window is re-found automatically on the next capture;
  a *minimized* one can't be captured (unminimize first).
- Occluded-window capture works via window ID, but keep the window visible.
- Avoid opening Kindle menus while mirroring; the fullscreen view is modal.
- The python client (`mirror.py`) is a legacy path (no /dev/fb0 niceties
  from the plugin: no discovery, no idempotent turns); evdev axis issues
  are addressed with `SWAP_AXES` per `probe.out`.

## Why this shape

The remote books reader is a client-only JS app (see project notes); the
Kindle's browser renders it blank. Mirroring sidesteps all of it: no
automation, no undocumented APIs — the Mac side is just you, in a normal
browser, with a very patient external monitor attached.
