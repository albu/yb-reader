# yb-reader

A minimal Rust reader for a jailbroken Kindle Paperwhite 5 (FW 5.19.x,
**32-bit ARM kernel** — `uname: armv7l`, kernel 4.9.77-lab126).
One static binary, one job: **read**.

- **Screen Mirror (Mac)** — the exact `yb-mirror` protocol, ported 1:1 from
  `mirror.koplugin`. Your existing `mac/server.py` is untouched and the two
  can even run side-by-side during migration.
- **Receive over Wi-Fi** — the Kindle *is* the server: a QR code on screen
  points any phone/laptop browser at a drag-drop page; books stream straight
  to `documents/` (atomically, never RAM-buffered). The lab126 default-DROP
  firewall is opened for the listener's lifetime and closed on exit.
- **Library** — local EPUB/PDF/MOBI/FB2/TXT/CBZ reading via MuPDF, reflowed to
  the panel width. Per-book positions/settings persisted on `/mnt/us`.
- **Reading tools** — TOC navigation, live-preview page scrubber, footnotes as
  a bottom sheet, split-column/landscape modes, contrast curves, night mode.
- **Vocabulary** — Word Wise–style inline translations (57k+ word Russian
  dictionary) and an SM-2 flashcard deck fed from looked-up words.
- **Frontlight** — `/dev/frontlight` ioctls (white + amber), two-finger tap from
  anywhere.

No LuaJIT, no KOReader, no 37 plugins. A few MB instead of a hundred.

## Layout

```
yb-reader/
  ybdev/     device layer: e-ink panel (MTK ioctls), frontlight, evdev input,
             mirror PNG decoding, mirror.conf, plugin log — no heavy deps
  reader/    the app: mirror protocol + Wi-Fi receive server, UI, MuPDF reader
  probe/     on-device introspection tool (fb geometry, input, frontlight)
  kual/      KUAL extension (kept for reference; the library scriptlet in
             documents/ is the real launcher on this unit — no KUAL here)
  deploy.sh  build + deploy: SSH fast loop (default), `usb`, or `probe`
```

## Prerequisites (macOS)

```sh
rustup target add arm-unknown-linux-musleabihf
brew install zig cargo-zigbuild lld llvm
```

`make setup` does exactly this.

**The target is `arm-unknown-linux-musleabihf`, not aarch64.** The MT8168's
cores boot a 32-bit kernel on this unit; KOReader ships armhf, and an aarch64
binary will never exec (it fails ENOEXEC and the shell reports a nonsense
"syntax error"). The probe's `uname` output settles it.

## Build & deploy (over SSH — the fast loop)

The Kindle runs a KOReader SSH server (`ssh kindle`; enable it in
KOReader → Network → SSH server, and put the current IP in `~/.ssh/config`).
No USB, no eject — one command does the whole loop:

```sh
./deploy.sh        # or: make deploy
```

It cross-compiles the armhf release, stages the binary as `reader.new`
(scp'ing straight over the running binary fails ETXTBSY — "text file busy"),
verifies the sha256 **before** swapping it in (a truncated copy execs as
ENOEXEC), `mv`s it into the launch path atomically, syncs the KPM package
copy, and kills the running reader so the next launch picks up the new build
(safe: `start.sh` restores cvm/pillow when the reader exits).

Then **tap "YB Reader" in the library, exactly like you start KOReader**.
The launch mechanism on this unit is a scriptlet in `/mnt/us/documents/`
(`YBReader.sh`, same trick as `KOReader.sh`), not KUAL.

**The launch-path gotcha (this bit us):** the library tap runs
`/mnt/us/extensions/reader/bin/start.sh` → `./reader`. If you scp a new
binary anywhere else (e.g. `/mnt/us/yb/`) the tap still runs the old one —
the symptom is "nothing changed". `./deploy.sh` updates the launch-path
copy and the KPM copy together; for ad-hoc manual scps keep them in sync
yourself:

```sh
ssh kindle 'cp /mnt/us/extensions/reader/bin/reader \
    /mnt/us/kmc/kpm/packages/yb-reader/bin/reader'
```

If you changed the launcher scripts (start.sh / scriptlet / KPM hooks), push
those too:

```sh
scp packages/yb-reader/bin/start.sh kindle:/mnt/us/extensions/reader/bin/start.sh
scp packages/yb-reader/scriptlets/YBReader.sh kindle:/mnt/us/documents/YBReader.sh
scp packages/yb-reader/{launch.sh,install.sh,uninstall.sh,scriptlets/YBReader.sh,bin/start.sh} \
    kindle:/mnt/us/kmc/kpm/packages/yb-reader/
```

### Where things live on the device

| Path | What |
|---|---|
| `/mnt/us/documents/YBReader.sh` | the library item ("start like a book") |
| `/mnt/us/extensions/reader/bin/start.sh` | launcher: pauses cvm, hides pillow, runs reader, restores on exit |
| `/mnt/us/extensions/reader/bin/reader` | the binary (the launch path) |
| `/mnt/us/kmc/kpm/packages/yb-reader/` | the KPM package (install/upgrade via KPM) |
| `/mnt/us/extensions/mirror/plugin.log` | our log (same file the Lua plugin used) |

### Running headless & reading logs

```sh
ssh kindle /mnt/us/extensions/reader/bin/start.sh    # takes over the screen; stdout/stderr stream back
ssh kindle 'tail -f /mnt/us/extensions/mirror/plugin.log'
```

### USB fallback

When the Kindle is mounted at `/Volumes/Kindle` (and SSH is off),
`./deploy.sh usb` (or `make deploy-usb`) stages the KPM package, does the
direct install (binary + scriptlet), and hash-verifies every binary copy.
It requires eject cycles — prefer SSH.

## On-device testing (probe)

```sh
make probe
scp target/arm-unknown-linux-musleabihf/release/yb-probe kindle:/mnt/us/yb-probe
ssh kindle /mnt/us/yb-probe
```

Or run it from KOReader's **Tools → Run hardware probe** menu.
It reports fb geometry/stride/bpp, the discovered touch device, frontlight
max/current, `/proc/bus/input/devices`, FW version, and the documents folder.
This is how we confirm the panel assumptions (1236×1648, Y8, `/dev/frontlight`)
on a specific unit.

## Packaging (KPM)

The unit installs software via **KPM** (`kmc/kpm/packages/koreader` is the
live example). `packages/yb-reader/` is a proper KPM package:

- `manifest.json` — id `yb-reader`, platform `kindlehf` (armhf).
- `install.sh` — copies `bin/reader` to `/mnt/us/extensions/reader/bin/` and
  drops the scriptlet into `/mnt/us/documents/YBReader.sh`.
- `launch.sh` — runs the binary (what `kpm launch yb-reader` calls).
- `uninstall.sh` — removes the scriptlet (KPM deletes package files itself).
- `scriptlets/YBReader.sh` — the library item. Note `# DontUseFBInk`:
  scriptlet stdout/stderr normally goes to FBInk, which would fight our
  framebuffer drawing.

`make deploy` stages the built binary into the package, copies the package to
`/Volumes/Kindle/kmc/kpm/packages/yb-reader/`, *and* does the direct install
(binary + scriptlet) so the library item works immediately without a formal
KPM install. Packing a `.kpkg` for a repository (via `kpm-helper.py`) is a
later nicety, not needed for on-device use.

### The framework fight (why the first run misbehaved)

Launched "like a book", the stock framework is still alive underneath: it
keeps drawing its UI over ours ("letters jump") and keeps consuming touch
input (taps/swipes dead). The launcher
([packages/yb-reader/bin/start.sh](packages/yb-reader/bin/start.sh))
does exactly what KOReader's `koreader.sh` does: `killall -STOP cvm` and
`lipc-set-prop com.lab126.pillow disableEnablePillow disable` before the
binary, and restores both (`killall -CONT cvm`, pillow enable) on exit.
Exit is a vertical swipe on the launcher, the **Exit** menu item, or
`ssh kindle 'killall reader; killall -CONT cvm'`.

Other targets: `make check` (host type-check), `make probe`, `make deploy-usb`,
`make clean`.

## Cross-compiling — the scars, documented

The two non-obvious pieces are **MuPDF's C build** and **Homebrew tool paths**:

1. **mupdf features.** The `mupdf` crate's default features pull in
   `system-fonts` (→ fontconfig, which refuses cross-compiles) plus
   JS/tesseract/XPS bloat. We use
   `default-features = false, features = ["epub", "cbz", "img"]`
   (`reader/Cargo.toml`). No fontconfig.

2. **`ld -r -b binary`.** MuPDF's Makefile embeds its base-14 fonts by invoking
   `ld -r -b binary -z noexecstack`. Apple's `ld` rejects `-z` and can't emit
   32-bit ARM ELF. The Makefile only hardcodes `LD` for Darwin; with `OS` overridden
   by mupdf-sys, an **environment `LD` flows straight through**:

   ```sh
   export LD="/opt/homebrew/opt/lld@21/bin/ld.lld -m armelf_linux_eabi"
   ```

3. **`ar` drops ELF objects.** Apple's `/usr/bin/ar` silently produces an
   *empty* archive when given ARM ELF objects (`ranlib: warning: archive
   member ... not a mach-o file`) → link errors like `undefined symbol:
   fz_new_context_imp`. Use llvm-ar:

   ```sh
   export AR=/opt/homebrew/opt/llvm@22/bin/llvm-ar
   ```

   Symptom of forgetting this: `libmupdf.a` is ~96 bytes and the final link
   fails with MuPDF core symbols undefined. Fix: delete the cached mupdf-sys
   build dir (`target/arm-unknown-linux-musleabihf/release/build/mupdf-sys-*`)
   and rebuild — cargo does not re-run the build script on env changes alone.

Both exports are baked into the `Makefile` and `deploy.sh`. Homebrew keg paths
change with versions; override with `make LLD=... AR=...` or `YB_LD`/`YB_AR`.

### Hardware truths from the first probe (PW5, FW 5.19.2)

- **Panel**: the fb reports `1248x3296` via sysfs (`virtual_size`), but that
  is padded width (1248 = stride for a 1236 px panel) and *doubled* height
  (2×1648 — the MTK driver's virtual buffer). The real panel is 1236×1648 at
  the top of the fb. `Panel::open` reads `FBIOGET_VSCREENINFO` for the true
  xres/yres and maps the full virtual buffer, drawing only the first yres rows.
- **Touch**: event0 is the `bd71828-pwrkey` (EV=3, no ABS) — "first event
  device" picks the power button. The panel is event1 `pt_mt`
  (EV=f, ABS=e618000 0, which is high-word-first for
  ABS_MT_SLOT/POSITION_X/POSITION_Y/TRACKING_ID/PRESSURE). `input::discover`
  parses the hex masks and scores devices by MT axes.
- **Frontlight**: 0–2047 (current 1210), no amber channels.
- **32-bit `input_event`**: the struct is 16 bytes on armv7 (32-bit timeval),
  not 24; the reader uses `size_of::<InputEvent>()`, never a hardcoded count.

### Regression note: the 4-bit PNG bug

`decode_png_gray` (in `ybdev/src/img.rs`) crashed on the first 4-bit frame the
mirror requests: the png crate returns sub-byte depths *packed*, so a 4-bit
frame is `w*h/2` bytes and the grayscale branch indexed past the end. The
decoder now uses `Transformations::EXPAND` and there are unit tests
(`cargo test -p ybdev`) that round-trip a genuine nibble-packed 4-bit gray PNG
— the same wire format `server.py`'s `png_gray(bits=4)` emits.

## Controls

| Screen | Gesture | Action |
|---|---|---|
| Mirror | tap left third | previous page (`/prev`) |
| Mirror | tap elsewhere | next page (`/next`) |
| Mirror | tap top-right corner | screen clean (full flashing refresh) |
| Mirror | two-finger tap | frontlight dialog |
| Mirror | vertical swipe | exit to launcher |
| Reader | tap left third / swipe east | previous page |
| Reader | tap elsewhere / swipe west | next page |
| Reader | tap top-left or bottom-right corner | back to library |
| Reader | tap top-right corner | clean refresh (full flash) |
| Reader | tap top strip / two-finger tap | curtain (frontlight & controls) |
| Reader | tap bottom footer strip | page scrubber (live preview, ±steps, TOC + highlights) |
| Reader | tap top-right bookmark | toggle selection mode (long-press selects instead of dictionary) |
| Reader | hold word + drag (selection mode) | highlight span → saved with persistent underline |
| Scrubber | 🖍 Highlights | highlights list: tap = jump back, hold = delete |
| Reader | swipe down (below the top edge) | reader settings (font, margins, contrast, split) |
| Reader | swipe down from the top edge | curtain |
| Reader | swipe up, bottom-left | reader settings |
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

The mirror keeps the device awake via
`lipc-set-prop com.lab126.powerd preventScreenSaver 1` (same as KOReader's
KeepAlive) and waits for Wi-Fi (`com.lab126.wifid cmState == CONNECTED`) after
enabling it — both were ported from the KOReader paths so a cold-radio start
doesn't produce spurious "Mac not found".

## Configuration

Same files as the Lua plugin, same semantics:

- `/mnt/us/extensions/mirror/mirror.conf` — `SERVER=` (pin the Mac; discovery
  rewrites it), `REFRESH_EVERY=N` (full refresh every N frames; 0 = never).
- Log: `/mnt/us/extensions/mirror/plugin.log` (same format). Override with
  `yb-reader --log /tmp/x.log` for testing.

## Known limitations

- No suspend/resume handling yet: the app holds `preventScreenSaver` while
  mirroring/reading, but if the device does sleep (cover, power button) the
  app doesn't yet re-establish Wi-Fi / refresh the panel on wake.
- Touch device is discovered from `/proc/bus/input/devices`
  (`ABS_MT_POSITION_X`); if discovery fails it falls back to
  `/dev/input/touch`. The `probe` output will confirm the real path.
- MuPDF is AGPL-3.0 — fine for personal use; keep that in mind if this ever
  gets distributed.

## Roadmap

- Suspend/resume hooks (lipc event → re-enable Wi-Fi, full refresh).
- Optional auto-start at boot (upstart/init script) so the Kindle is
  single-purpose, as designed.
