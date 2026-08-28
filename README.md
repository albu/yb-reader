# yb-reader

A from-scratch Rust reader for a jailbroken Kindle Paperwhite 5 (FW 5.19.x,
**32-bit ARM kernel** — `uname: armv7l`, kernel 4.9.77-lab126). It replaces
KOReader on the device: one static binary that boots straight into your
library, typesets books natively on the e-ink panel, and — when a Mac is on
the same Wi-Fi — turns the Kindle into a monitor for anything the Mac can
show. The Kindle half lives here; the Mac half (mirror server, menu-bar
app, AI stream) is [`companion/`](companion/README.md).

## What it does

- **Screen Mirror (Mac)** — the Kindle shows a live grayscale copy of one
  Mac window (the `yb-mirror` protocol, ported from `mirror.koplugin` and
  since extended): taps become real ←/→ key presses, page-turn keys are
  selectable presets (`arrows` / `space` / `pages`), and a **Control mode**
  (toggled in the quick-settings sheet) turns the Kindle into a touchpad —
  taps click at their mirrored coordinates (crop-accurate), swipes scroll
  the window. Pairing is PIN-verified and trust is token-based, so it
  works across Wi-Fi networks with zero configuration.
- **AI Stream** — the same screen shows your live AI coding session
  (Antigravity, Claude Code, or a terminal pipe) rendered as paginated
  e-ink pages: headings, nested lists, code blocks, tables, alerts.
- **Receive over Wi-Fi** — the Kindle *is* the server: a QR code on screen
  points any phone/laptop browser at a drag-drop page; books stream
  straight to `documents/` (atomically, never RAM-buffered). The lab126
  default-DROP firewall is opened for the listener's lifetime and closed on
  exit.
- **Library** — local EPUB/PDF/FB2/TXT/CBZ reading — EPUB/FB2/TXT through
  the built-in typesetter reflowed to the panel width, PDF/CBZ through
  MuPDF with crop/split tools. MOBI is deliberately not supported
  (convert to EPUB). Per-book positions and settings persist on `/mnt/us`.
- **Reading tools** — TOC navigation, a live-preview page scrubber,
  footnotes as a bottom sheet, split-column/landscape modes, contrast
  curves, night mode.
- **Typography** — four embedded OFL typefaces (Literata, PT Serif, Bitter,
  PT Sans — all with native Cyrillic), a Quick Settings → Typography page
  (family, alignment, hyphenation, indent, paragraph spacing), a unified
  justification width model, hyphenation hygiene, and a verification
  gallery that scores every typesetting change. See
  [Typography](#typography--the-reading-engine).
- **Vocabulary** — Word Wise–style inline translations (45k+ English words
  with English + Russian glosses) and an SM-2 flashcard deck fed from
  looked-up words.
- **Frontlight** — `/dev/frontlight` ioctls (white + amber warmth) via the
  curtain, from anywhere in the reader.
- **Takeover mode** — the reader *is* the OS: the framework never boots,
  no JVM, no restart races, straight into the library at power-on. See
  [Takeover mode](#takeover-mode--the-reader-as-the-os).

No LuaJIT, no KOReader, no 37 plugins — not even for ssh (the dropbear is
ours now). A few MB instead of a hundred.

## Quick start (build & deploy)

Prerequisites (macOS): the `arm-unknown-linux-musleabihf` Rust target, plus
`zig`, `cargo-zigbuild`, `lld`, `llvm` — `make setup` installs all of it.

```sh
make setup
make deploy        # or: ./deploy.sh
```

The device runs its own SSH — the patched dropbear from
`packages/yb-reader/bin/dropbear`, started by `boot.sh` on port 2222 (the
"ssh lifeline"; no KOReader needed). Put the Kindle's IP in `~/.ssh/config`
and `ssh kindle` just works:

```sh
Host kindle
    HostName <kindle-ip>
    Port 2222
    User root
```

`make deploy` cross-compiles the armhf release, stages the binary as
`reader.new` (scp'ing straight over the running binary fails ETXTBSY —
"text file busy"), verifies the sha256 **before** swapping it in (a
truncated copy execs as ENOEXEC), `mv`s it into the launch path atomically,
syncs the KPM package copy, and kills the running reader so the next launch
picks up the new build. Then tap **"YB Reader"** in the library, exactly
like you start KOReader — the launch mechanism on this unit is a scriptlet
in `/mnt/us/documents/` (`YBReader.sh`, same trick as `KOReader.sh`), not
KUAL.

**The launch-path gotcha (this bit us):** the library tap runs
`/mnt/us/extensions/reader/bin/start.sh` → `./reader`. If you scp a new
binary anywhere else, the tap still runs the old one — the symptom is
"nothing changed". `./deploy.sh` updates the launch-path copy and the KPM
copy together; keep them in sync yourself for ad-hoc manual scps.

**Your SSH key is not committed** —
`packages/yb-reader/settings/SSH/authorized_keys` is per-device config (the
patched dropbear reads it at runtime), so it's gitignored. Put your own
public key there or the device won't authenticate you:

```sh
mkdir -p packages/yb-reader/settings/SSH
cat ~/.ssh/id_ed25519.pub > packages/yb-reader/settings/SSH/authorized_keys
```

When the Kindle is USB-mounted (`/Volumes/Kindle`) and SSH is off,
`./deploy.sh usb` (or `make deploy-usb`) does the same install over USB.

### Where things live on the device

| Path | What |
|---|---|
| `/mnt/us/documents/YBReader.sh` | the library item ("start like a book") |
| `/mnt/us/extensions/reader/bin/start.sh` | launcher: pauses cvm, hides pillow, runs reader, restores on exit |
| `/mnt/us/extensions/reader/bin/reader` | the binary (the launch path) |
| `/mnt/us/kmc/kpm/packages/yb-reader/` | the KPM package (install/upgrade via KPM) |
| `/mnt/us/extensions/mirror/plugin.log` | the mirror/AI-stream log (the protocol's long-standing path) |
| `/var/local/yb-reader/` | the trust store — `devices.json` (paired Macs) + `kindle_id.json` (this Kindle's identity), 0600, never on the USB-visible partition |

### Running headless & reading logs

```sh
ssh kindle /mnt/us/extensions/reader/bin/start.sh    # takes over the screen; stdout/stderr stream back
ssh kindle 'tail -f /mnt/us/extensions/mirror/plugin.log'
```

## Layout

```
yb-reader/
  ybdev/     device layer: e-ink panel (MTK ioctls), frontlight, evdev input,
             mirror PNG decoding, ssh (dropbear), plugin log — no heavy deps
  reader/    the app: mirror protocol + Wi-Fi receive server, UI, MuPDF reader,
             AI stream screen
  yread/     the typesetting engine (XHTML/FB2 → blocks → shaped raster)
  yui/       the e-ink UI toolkit (screens, gestures, painter)
  probe/     on-device introspection tool (fb geometry, input, frontlight)
  kual/      KUAL extension (kept for reference; the library scriptlet in
             documents/ is the real launcher on this unit — no KUAL here)
  companion/ macOS mirror + AI-stream companion (source). `make dist` builds
             the small source zip the receive page offers in its "Companion
             (macOS)" card — unzip on a Mac, `uv sync` +
             `bash mac/make-app.sh`, and the menu-bar app is built locally
             (no Gatekeeper, no signing)
  packages/  KPM package incl. bin/{start,boot}.sh + upstart/yb-reader.conf
             (the takeover pieces) + bin/dropbear (bundled ssh server)
  deploy.sh  build + deploy: SSH fast loop (default), `usb`, or `probe`
```

## Typography & the reading engine

`yread` is a from-scratch typesetter: XHTML/FB2 → Block/Run tree → line
breaking with hypher hyphenation → rustybuzz shaping → swash rasterization
onto the e-ink framebuffer.

**Typefaces** — four embedded OFL-1.1 families (license texts live next to
them as `resources/fonts/OFL-*.txt`):

| Family | Character | Notes |
|---|---|---|
| Literata (default) | book serif | Google Books' reading face |
| PT Serif | news serif | native Cyrillic — the Russian-book workhorse |
| Bitter | slab serif | designed for screens / e-paper |
| PT Sans | humanist sans | the clean / accessible option |

The non-Literata faces are subsetted (Latin + Cyrillic + punctuation) to
~35–190 KB/face; Bitter is instanced from its variable font to static TTFs.
All four carry real Cyrillic — the Russian book corpus is the stated
requirement. Code and fallback stay Noto Sans.

**Settings** — Quick Settings (swipe up bottom-left, or swipe down below
the top edge) carries the per-session knobs: font size, margins, line
spacing, contrast, night mode. Its `TYPOGRAPHY ›` button opens the page for
the set-once style: family, alignment, hyphenation, first-line indent,
paragraph spacing — plus engine-level word/letter spacing. Every change
repaginates live and persists per book, and the page snapshot cache keys on
a **layout fingerprint** over every layout-affecting field.

**Correctness details** — justification (leading offset + per-gap stretch)
is solved once at line-build time and stored on the line, so the rasterizer
and word-rect hit-testing can never drift apart (that drift once broke
dictionary taps). Non-breaking spaces never split "10 km"; soft hyphens
break with a real hyphen at line ends and stay invisible mid-line;
hyphenation is hygienic (max 2 consecutive hyphenated line ends, min 3-char
prefix / 2-char suffix); justified lines land exactly on both margins.

**Verification gate** — the breaker is pinned by property tests, plus an
opt-in real-book gate:

```sh
YB_TEST_EPUB=/path/to/book.epub cargo test -p yread --test real_book_test
```

`cargo run -p rendertest -- <book.epub> <chapter> gallery` renders a
settings matrix to PNGs with per-config stats (extent violations, rivers,
hyphen ladders) — the before/after scoreboard for any typesetting change.

## Mirror & companion

The mirror screen (started from the reader's menu) talks to the Mac half in
[`companion/`](companion/README.md) — the README there covers the Mac
server, the menu-bar app, and the AI stream end to end. The essentials:

- **Discovery is automatic.** The reader broadcasts a UDP probe; the Mac
  answers, and the address is pinned to `mirror.conf` only after a real
  frame exchange. If your router drops broadcast between clients, the
  reader falls back to a unicast sweep of the local /24 — and if even that
  is blocked, pin `SERVER=` manually.
- **Pairing is PIN-verified and self-healing.** Open the Kindle's receive
  page from the Mac (menu bar → *Open Kindle Web Manager…*), enter the PIN,
  check *link mirror*. The PIN-minted token lands on both sides; discovery
  replies carry `mac=HMAC-SHA256(token, nonce)` so trust is token
  possession, not IP+id — a Mac that changes IP is trusted automatically,
  and a host that merely echoes an overheard id gets nothing.
- **Once paired, both Mac servers require `X-YB-Secret`** on Kindle-facing
  requests (401 otherwise). Deleting `~/.yb-mirror-devices.json` on the Mac
  unpairs.

See also [Configuration](#configuration) for the reader-side knobs.

## Takeover mode — the reader as the OS

Stock mode launches the reader "like a book" under the framework (start.sh
SIGSTOPs cvm and restores it on exit). Takeover mode removes the framework
from boot entirely — no JVM, no restart races, straight into the library at
power-on. Three pieces:

| Piece | Path | Notes |
|---|---|---|
| enable flag | `/mnt/us/DONT_START_FRAMEWORK` | Amazon's own `framework.conf` pre-start check. Touch to enable, remove to disable. USB-visible — no ssh needed to flip it. |
| upstart job | `/etc/upstart/yb-reader.conf` | `start on stopped framework`; no-op without the flag. Copy from `packages/yb-reader/upstart/` (rootfs remount-rw for the copy), then `initctl reload`. |
| boot script | `/mnt/us/extensions/reader/bin/boot.sh` | ssh lifeline, boot audit, crash counter, GUI freeze, reader + hang watchdog. Deploy atomically (stage + verify + `mv`). |

Verified surviving without cvm: powerd, wifid + wpa_supplicant + udhcpc
(auto-join on a cold boot), volumd/fsp (`/mnt/us`), dropbear (started by
boot.sh). Never starts / gets SIGSTOPped: Xorg, awesome, lxinit, pillow,
kb, KPPMainApp, webreader, kfxreader.

**Fallback ladder** — nothing below rung 5 needs more than the power button:

1. **Hang watchdog** (in-session): the reader touches `/tmp/yb-heartbeat`;
   a second copy (`reader --watchdog <pid>`) kills it when the heartbeat is
   stale with a healthy self-gap (a large gap means the SoC slept — suspend
   looks like a hang to a naive observer). upstart respawns a fresh
   instance.
2. **Boot audit** (power-hold reboot): only SoC-level death leaves the
   `running` marker behind; `reader bootaudit` classifies the previous end
   (suspended = nothing; awake = a decaying strike; 4 strikes within 1 h →
   stock fallback). USB is fully stock at every rung — plug is always the
   recovery session.
3. **Crash counter**: 3 consecutive fast failures of the *same* binary →
   CONT the frozen GUI, remove the flag, `initctl start framework`.
4. **Amazon's own net**: 3 framework restarts → 2 reboots → airplane-mode
   retry → halt with a customer-service page.
5. **Long-press power** = clean shutdown cascade, observed as reader
   rc=143.
6. **ssh over Wi-Fi**, then remove the flag. Serial getty on `ttymxc0` is
   the absolute floor.

**Exit to stock**: the Exit row (relabelled "Exit to Kindle") or a home
vertical swipe confirms, the reader exits 42, boot.sh removes the flag,
CONTs the frozen GUI, and starts the framework. The **System screen** (home
row, gear icon) holds the BOOT MODE card (flips the next-boot target) and
the Reboot card. Every build shows its git sha top-right on the home tab
(`vXXXX`, `*` = dirty tree) — the on-device answer to "did the deploy
land?".

## Configuration

One file, the same one the mirror protocol has always used:

- `/mnt/us/extensions/mirror/mirror.conf` — `SERVER=` (pin the Mac;
  discovery rewrites it after a real exchange), `REFRESH_EVERY=N` (full
  refresh every N frames; 0 = never), `TURN_KEYS=arrows|space|pages`
  (read-mode page turns), `SECRET=` (optional static shared secret, sent
  as `X-YB-Secret`):
  - `arrows` (default) — ←/→, for readers whose JS pages on arrow keys
    (the classic behavior, verified on books.example.com);
  - `space` — Space / Shift+Space, only where the site binds Space itself;
  - `pages` — PageDown / PageUp, native full-page keys delivered even to a
    background Safari, no site JS required.
- Log: `/mnt/us/extensions/mirror/plugin.log`. Override with
  `yb-reader --log /tmp/x.log` for testing.

**Trust & pairing** state lives in `/var/local/yb-reader/devices.json` on
the device and `~/.yb-mirror-devices.json` on the Mac — the full flow is in
[`companion/README.md`](companion/README.md).

## Controls

| Screen | Gesture | Action |
|---|---|---|
| Mirror | tap left third | previous page (turn-key preset) |
| Mirror | tap elsewhere | next page (turn-key preset) |
| Mirror | tap top-right corner / two-finger tap | screen clean (full flashing refresh) |
| Mirror | swipe up bottom-left | quick-settings sheet (Control mode toggle, turn-key preset) |
| Mirror | vertical swipe (read mode) | exit to launcher |
| Mirror (control) | tap | click at that point in the window (`/tap`, crop-accurate) |
| Mirror (control) | swipe | scroll the window (`/scroll`), natural-scroll direction |
| Mirror (control) | app-level edge gestures | disabled — every swipe goes to the page (exit via the sheet's Control toggle) |
| Reader | tap left third / swipe east | previous page |
| Reader | tap elsewhere / swipe west | next page |
| Reader | tap top-left or bottom-right corner | back to library |
| Reader | tap top-right corner / two-finger tap | clean refresh (full flash) |
| Reader | tap bottom footer strip | page scrubber (live preview, ±steps, TOC + highlights) |
| Reader | tap top-right bookmark | toggle selection mode (long-press selects instead of dictionary) |
| Reader | hold word + drag (selection mode) | highlight span → saved with persistent underline |
| Scrubber | 🖍 Highlights | highlights list: tap = jump back, hold = delete |
| Reader | swipe down (below the top edge) / swipe up bottom-left | reader settings (size, margins, spacing, contrast, night; `TYPOGRAPHY ›` page for type style) |
| Reader | swipe down from the top edge | curtain |
| Reader | swipe up, bottom-center | table of contents |
| Reader | swipe up, bottom-right | back to library |
| Reader | long-press a word | dictionary / translation dialog |
| Reader | long-press a footnote or link | footnote bottom sheet (jump to note) |
| Library | tap row | open book |
| Library | tap footer band (or header) | cycle sort: title / recent / reading |
| Library | long-press row | delete book (confirm dialog) |
| Library | swipe up/down | scroll / back |
| Launcher | tap row | run |
| Launcher | vertical swipe | exit app |

## On-device testing (probe)

```sh
make probe
scp target/arm-unknown-linux-musleabihf/release/yb-probe kindle:/mnt/us/yb-probe
ssh kindle /mnt/us/yb-probe
```

It reports fb geometry/stride/bpp, the discovered touch device, frontlight
max/current, `/proc/bus/input/devices`, FW version, and the documents
folder — how we confirm the panel assumptions (1236×1648, Y8,
`/dev/frontlight`) on a specific unit.

## Packaging (KPM)

`packages/yb-reader/` is a proper KPM package (id `yb-reader`, platform
`kindlehf`): `install.sh` copies the binary and drops the library scriptlet,
`launch.sh` runs it, `uninstall.sh` removes the scriptlet. The scriptlet
notes `# DontUseFBInk` — its stdout/stderr normally goes to FBInk, which
would fight our framebuffer drawing. `make deploy` stages the built binary
into the package, copies it to the device's KPM directory, *and* does the
direct install so the library item works immediately.

## Cross-compiling — the scars, documented

The two non-obvious pieces are **MuPDF's C build** and **Homebrew tool
paths**:

1. **mupdf features.** The `mupdf` crate's default features pull in
   `system-fonts` (→ fontconfig, which refuses cross-compiles) plus
   JS/tesseract/XPS bloat. We use
   `default-features = false, features = ["epub", "cbz", "img"]`.
2. **`ld -r -b binary`.** MuPDF's Makefile embeds its base-14 fonts with
   `ld -r -b binary -z noexecstack`. Apple's `ld` rejects `-z` and can't
   emit 32-bit ARM ELF — but an environment `LD` flows straight through:
   `export LD="/opt/homebrew/opt/lld@21/bin/ld.lld -m armelf_linux_eabi"`.
3. **`ar` drops ELF objects.** Apple's `/usr/bin/ar` silently produces an
   *empty* archive from ARM ELF objects → link errors like `undefined
   symbol: fz_new_context_imp`. Use `llvm-ar`:
   `export AR=/opt/homebrew/opt/llvm@22/bin/llvm-ar`.

Both exports are baked into the `Makefile` and `deploy.sh`. If the link
fails with MuPDF core symbols undefined, the cached mupdf-sys build dir is
stale — delete `target/.../build/mupdf-sys-*` and rebuild (cargo does not
re-run the build script on env changes alone). Homebrew keg paths change
with versions; override with `make LLD=... AR=...` or `YB_LD`/`YB_AR`.

### Hardware truths from the first probe (PW5, FW 5.19.2)

- **Panel**: sysfs reports `1248x3296` — padded width (1248 = stride for a
  1236 px panel) and doubled height (2×1648, the MTK driver's virtual
  buffer). The real panel is 1236×1648; `Panel::open` reads
  `FBIOGET_VSCREENINFO` for true xres/yres and draws only the first yres
  rows.
- **Touch**: event0 is the `bd71828-pwrkey`; the panel is event1 `pt_mt`
  (high-word-first ABS_MT_SLOT/POSITION_X/POSITION_Y/TRACKING_ID/PRESSURE).
  `input::discover` parses the hex masks and scores devices by MT axes.
- **Frontlight**: 0–2047 (current 1210), no amber channels on this unit.
- **32-bit `input_event`**: 16 bytes on armv7 (32-bit timeval), not 24; the
  reader uses `size_of::<InputEvent>()`, never a hardcoded count.

### Regression note: the 4-bit PNG bug

`decode_png_gray` (in `ybdev/src/img.rs`) crashed on the first 4-bit frame
the mirror requests: the png crate returns sub-byte depths *packed*, so a
4-bit frame is `w*h/2` bytes and the grayscale branch indexed past the end.
The decoder now uses `Transformations::EXPAND`, with unit tests that
round-trip a genuine nibble-packed 4-bit gray PNG — the same wire format
`server.py`'s `png_gray(bits=4)` emits.

## Known limitations

- **Suspend policy is deliberate**: plain reading holds no awake assertion,
  so an idle device sleeps on the stock ~10-min input-idle timer (the
  Kindle behavior); live sessions (mirror streaming, receive server) and
  USB power hold it awake. On wake, one resume hook repaints, re-applies
  our frontlight levels (powerd restores its own over ours), and heals
  Wi-Fi for screens that want it. The one hardware unknown left is whether
  a wall charger trips volumd's drive-mode path on VBUS alone.
- **USB cable in takeover mode**: **file access, always** — USB is fully
  stock, so the userstore is editable from any computer. The cost is
  deliberate: the export unmounts `/mnt/us` under the running reader, so
  the reader bows out gracefully on plug (positions flushed) and boot.sh
  waits out the cable; upstart respawns on unplug. A wall charger may or
  may not trip the same path (untested on hardware).
- Touch device is discovered from `/proc/bus/input/devices`
  (`ABS_MT_POSITION_X`); if discovery fails it falls back to
  `/dev/input/touch`. The `probe` output will confirm the real path.
- The binary statically links **MuPDF** (AGPL-3.0, © Artifex Software).
  Personal use on your own device is unrestricted, but *distributing* the
  built binary carries AGPL obligations (license alongside the binary,
  notice, and an offer of the complete corresponding source) — see
  `LICENSE` and `NOTICE`.

## License

AGPL-3.0 — see [LICENSE](LICENSE) and [NOTICE](NOTICE). The fonts are
SIL Open Font License 1.1 (license texts in `resources/fonts/OFL-*.txt`);
the statically-linked MuPDF is AGPL-3.0 (© Artifex Software, source at
[mupdf.com](https://mupdf.com)).
