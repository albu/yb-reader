# yb-reader

A minimal Rust reader for a jailbroken Kindle Paperwhite 5 (FW 5.19.x,
**32-bit ARM kernel** — `uname: armv7l`, kernel 4.9.77-lab126).
One static binary, one job: **read**.

- **Screen Mirror (Mac)** — the `yb-mirror` protocol, ported from
  `mirror.koplugin` and since extended: page turns go through `/key`
  (selectable presets), and a **Control mode** (two-finger tap) turns the
  Kindle into a touchpad — taps click at their mirrored coordinates
  (crop-accurate), swipes scroll the window.
- **Receive over Wi-Fi** — the Kindle *is* the server: a QR code on screen
  points any phone/laptop browser at a drag-drop page; books stream straight
  to `documents/` (atomically, never RAM-buffered). The lab126 default-DROP
  firewall is opened for the listener's lifetime and closed on exit.
- **Library** — local EPUB/PDF/MOBI/FB2/TXT/CBZ reading via MuPDF, reflowed to
  the panel width. Per-book positions/settings persisted on `/mnt/us`.
- **Reading tools** — TOC navigation, live-preview page scrubber, footnotes as
  a bottom sheet, split-column/landscape modes, contrast curves, night mode.
- **Typography** — four embedded OFL typefaces (Literata, PT Serif, Bitter,
  PT Sans), a Quick Settings → Typography page (family, alignment,
  hyphenation, indent, paragraph spacing), a unified justification width
  model, hyphenation hygiene, and a verification gallery that scores every
  typesetting change. See [Typography & the reading engine](#typography--the-reading-engine).
- **Vocabulary** — Word Wise–style inline translations (57k+ word Russian
  dictionary) and an SM-2 flashcard deck fed from looked-up words.
- **Frontlight** — `/dev/frontlight` ioctls (white + amber), two-finger tap from
  anywhere.
- **Takeover mode** — the reader *is* the OS: the framework never boots, no
  JVM, no restart races, straight into the library at power-on. See
  [Takeover mode](#takeover-mode--the-reader-as-the-os).

No LuaJIT, no KOReader, no 37 plugins — not even for ssh (the dropbear is
ours now). A few MB instead of a hundred.

## Layout

```
yb-reader/
  ybdev/     device layer: e-ink panel (MTK ioctls), frontlight, evdev input,
             mirror PNG decoding, ssh (dropbear), plugin log — no heavy deps
  reader/    the app: mirror protocol + Wi-Fi receive server, UI, MuPDF reader
  probe/     on-device introspection tool (fb geometry, input, frontlight)
  kual/      KUAL extension (kept for reference; the library scriptlet in
             documents/ is the real launcher on this unit — no KUAL here)
  companion/ macOS mirror companion (source). `make dist` builds the small
             source zip the receive page offers in its "Companion (macOS)"
             card — unzip on a Mac, `uv sync` + `bash mac/make-app.sh`, and
             the menu-bar app is built locally (no Gatekeeper, no signing)
  packages/  KPM package incl. bin/{start,boot}.sh + upstart/yb-reader.conf
             (the takeover pieces) + bin/dropbear (bundled ssh server)
  deploy.sh  build + deploy: SSH fast loop (default), `usb`, or `probe`
```

## Typography & the reading engine

The reflowable engine (yread) is a from-scratch typesetter: XHTML/FB2 →
Block/Run tree → line breaking with hypher hyphenation → rustybuzz shaping →
swash rasterization onto the e-ink framebuffer.

**Typefaces** — four embedded OFL-1.1 families (license texts live next to
them as `resources/fonts/OFL-*.txt`):

| Family | Character | Notes |
|---|---|---|
| Literata (default) | book serif | Google Books' reading face, full-size faces |
| PT Serif | news serif | native Cyrillic — the Russian-book workhorse |
| Bitter | slab serif | designed for screens / e-paper |
| PT Sans | humanist sans | the clean / accessible option |

The non-Literata faces are subsetted (Latin + Cyrillic + punctuation,
`pyftsubset`) to ~35–190 KB/face; Bitter is instanced from its variable font
to static TTFs (variable fonts can quirk on e-ink firmware). All four carry
real Cyrillic — the Russian Word Wise / book corpus is the stated
requirement. Code and fallback stay Noto Sans.

**Settings** — Quick Settings (swipe up bottom-left, or swipe down below the
top edge) carries the per-session knobs: font size, margins, line spacing,
contrast, night mode. Its `TYPOGRAPHY ›` button opens the page for the
set-once style: font family, alignment, hyphenation, first-line indent,
paragraph spacing — plus engine-level word/letter spacing. Every change
repaginates live and persists per book (positions.txt), and the page
snapshot cache keys on a **layout fingerprint** over every layout-affecting
field, so a stale-styled pixel is structurally impossible.

**The width model** — justification (leading offset + per-gap stretch) is
solved once at line-build time and stored on the line; the rasterizer and
word-rect hit-testing both consume the stored values, so the two width
models that once drifted apart (and broke dictionary taps) cannot exist
anymore. Per-space TeX-style stretch/shrink tolerances are stored for
future optimal (Knuth-Plass) breaking.

**Correctness work** — non-breaking spaces (NBSP) never split "10 km";
soft hyphens break with a real hyphen at line ends and stay invisible
mid-line; hyphenation is hygienic (max 2 consecutive hyphenated line ends,
min 3-char prefix / 2-char suffix, no break next to an existing dash);
justified lines land exactly on both margins.

**Verification gate** — the breaker is pinned by property tests (extent,
justify-fill, byte tiling, hyphen rules, cache-key invariants) and an
opt-in real-book gate:

```sh
YB_TEST_EPUB=/path/to/book.epub cargo test -p yread --test real_book_test
```

`cargo run -p rendertest -- <book.epub> <chapter> gallery` renders a
settings matrix to PNGs with per-config stats (extent violations, rivers,
hyphen ladders) — the before/after scoreboard for any typesetting change.

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

## Takeover mode — the reader as the OS

Stock mode launches the reader "like a book" under the framework
(start.sh SIGSTOPs cvm and restores it on exit). Takeover mode removes
the framework from boot entirely — no JVM, no restart races, straight
into the library at power-on. Three pieces:

| Piece | Path | Notes |
|---|---|---|
| enable flag | `/mnt/us/DONT_START_FRAMEWORK` | Amazon's own `framework.conf` pre-start check. Touch to enable, remove to disable. USB-visible partition — no ssh needed to flip it. |
| upstart job | `/etc/upstart/yb-reader.conf` | `start on stopped framework`; no-op without the flag (shutdown ghost-starts land there harmlessly). Copy from `packages/yb-reader/upstart/` (rootfs remount-rw for the copy), then `initctl reload`. |
| boot script | `/mnt/us/extensions/reader/bin/boot.sh` | ssh lifeline, boot audit, crash counter, GUI freeze, reader + hang watchdog. Deploy atomically (stage + verify + `mv`): a live shell mid-`wait` reads its remaining lines from a rewritten file. |

Verified surviving without cvm: **powerd** (owns `/sys/power/state`, t1/t2
idle timers, battery monitor), **wifid** + wpa_supplicant + udhcpc
(auto-join works on a cold framework-free boot), **volumd/fsp** (`/mnt/us`),
**dropbear** (started by boot.sh — recovery never depends on the framework).
Never starts / gets SIGSTOPped: Xorg, awesome, lxinit, pillow, kb,
KPPMainApp, webreader, kfxreader (keyed on `framework_ready` /
`started lab126_gui`, or frozen — start.sh's proven semantics).

**Fallback ladder** — nothing below rung 5 needs more than the power button:

1. **Hang watchdog** (in-session): the reader's loop touches
   `/tmp/yb-heartbeat` (≤1 utimensat per 5 s, tmpfs); a second copy of
   the binary (`reader --watchdog <pid>`, one 30 s pass, zero cost
   while suspended) kills it when the heartbeat is 3 min stale with a
   healthy self-gap (a large gap means the SoC slept — suspend looks
   exactly like a hang to a naive observer). upstart respawns a fresh
   instance; a transient hang self-heals with no user-visible symptom.
2. **Boot audit** (power-hold reboot): only SoC-level death leaves the
   `running` marker behind (boot.sh removes it after every exit it
   observes). `reader bootaudit` classifies the previous end: died
   *suspended* (overnight battery) → nothing; died *awake* → append a
   decaying strike. 4 strikes within 1 h → stock fallback (the strike
   ledger exists because the crash counter's 60 s self-clear would wipe
   a slow-developing hang's evidence every cycle — the infinite
   hang→reboot→hang loop). USB needs no arming at any rung: it is fully
   stock, so **plug is always the recovery session** — the drive mounts
   for any computer, in any state, no network needed.
3. boot.sh crash counter: 3 consecutive fast failures of the *same*
   binary (a replaced binary resets the count — deploys don't count;
   deploy.sh clears it too) → CONT the frozen GUI, remove the flag,
   `initctl start framework`. Tested live with `kill -9`: stock GUI back
   in under 30 s.
4. Amazon's own net under that: 3 framework restarts → 2 reboots →
   airplane-mode retry → halt with a customer-service page.
5. Long-press power = clean shutdown cascade (`stopping lab126_gui` →
   job stop → TERM → reader guard restores frontlight/wifi/firewall;
   observed as reader rc=143) — and with rung 2, even the *forced*
   power-hold variant lands in an accounted, recoverable state.
6. ssh over Wi-Fi, then remove the flag. Serial getty on `ttymxc0` is
   the absolute floor (never needed so far).

**Exit to stock**: the Exit row (relabelled "Exit to Kindle") and the
home vertical swipes confirm, then the reader exits 42 → boot.sh removes
the flag, CONTs the frozen GUI, and starts the framework. Reboot brings
takeover back — the flag survives use; only exit-42 or the crash
fallback remove it. The **System screen** (home row, gear icon) holds
the BOOT MODE card (flips the next-boot target) and the Reboot card
(power-cycles in whichever mode is armed; plain `reboot` — the same init
cascade as a long-press power, so the reader's signal guard still
restores frontlight/wifi/firewall). No computer, no ssh, no button
gymnastics in either direction.

Deploy in takeover mode works unchanged — `./deploy.sh` resets the
counter, kills the reader, and relaunches via `initctl restart
yb-reader` when the flag is present (start.sh's lipc calls need a live
cvm, so it must not be used there). Every build shows its git sha
top-right on the home tab (`vXXXX`, `*` = dirty tree) — the on-device
answer to "did the deploy land?". Five quick taps on that stamp are the
passphrase to a hidden easter egg: three snakes rise from the bottom of
the screen and trace a YB logo around the Continue card, animated on the
fast A2 waveform — 2 gray levels, no flash, the panel's idea of video.
It ends with a full refresh, so no ghosting survives it.

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
| Mirror | tap left third | previous page (turn-key preset) |
| Mirror | tap elsewhere | next page (turn-key preset) |
| Mirror | tap top-right corner | screen clean (full flashing refresh) |
| Mirror | two-finger tap | toggle Control mode ↔ Read mode |
| Mirror | vertical swipe (read mode) | exit to launcher |
| Mirror (control) | tap | click at that point in the window (`/tap`, crop-accurate) |
| Mirror (control) | swipe | scroll the window (`/scroll`), natural-scroll direction |
| Mirror (control) | app-level edge gestures | disabled — every swipe goes to the page (exit via two-finger tap → Read mode) |
| Reader | tap left third / swipe east | previous page |
| Reader | tap elsewhere / swipe west | next page |
| Reader | tap top-left or bottom-right corner | back to library |
| Reader | tap top-right corner | clean refresh (full flash) |
| Reader | tap top strip / two-finger tap | curtain (frontlight & controls) |
| Reader | tap bottom footer strip | page scrubber (live preview, ±steps, TOC + highlights) |
| Reader | tap top-right bookmark | toggle selection mode (long-press selects instead of dictionary) |
| Reader | hold word + drag (selection mode) | highlight span → saved with persistent underline |
| Scrubber | 🖍 Highlights | highlights list: tap = jump back, hold = delete |
| Reader | swipe down (below the top edge) / swipe up bottom-left | reader settings (size, margins, spacing, contrast, night; `TYPOGRAPHY ›` page for type style) |
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
| Home | tap build stamp (top-right) 5× quickly | YB-snake easter egg (A2 animation) |
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
  rewrites it), `REFRESH_EVERY=N` (full refresh every N frames; 0 = never),
  `TURN_KEYS=arrows|space|pages` (read-mode page turns):
  - `arrows` (default) — ←/→, for readers whose JS pages on arrow keys
    (the classic behavior, verified on books.example.com);
  - `space` — Space / Shift+Space, only where the site binds Space itself
    (a pid-posted Space never triggers Safari's native space-scroll);
  - `pages` — PageDown / PageUp, native full-page keys delivered even to a
    background Safari, no site JS required.
- Log: `/mnt/us/extensions/mirror/plugin.log` (same format). Override with
  `yb-reader --log /tmp/x.log` for testing.

## Known limitations

- No suspend/resume handling yet: the app holds `preventScreenSaver` while
  mirroring/reading, but if the device does sleep (cover, power button) the
  app doesn't yet re-establish Wi-Fi / refresh the panel on wake. In
  takeover mode this is worse: an open book keeps the device awake
  *forever* (nobody else will suspend it), and 20+ min idle on the library
  screen showed no autonomous suspend either — powerd's idle timers appear
  to need arming by someone. Idle policy is the reader's job now.
- USB cable in takeover mode: **file access, always** — USB is fully
  stock (we never touch the mass-storage modules), so volumd
  auto-configures `g_mass_storage` on plug (framework-free) and the
  userstore (flag, boot.sh, binary) is editable from any computer. The
  cost is deliberate: the export unmounts /mnt/us under the running
  reader, so **the reader dies on plug by design** — boot.sh waits out
  the cable and upstart respawns a fresh instance on unplug (the app
  also self-exits on resume if the mount changed under it, so a
  plug-while-suspended can't corrupt positions through stale handles).
  A wall charger may or may not trip the same path — it depends on
  whether volumd engages drive mode on vbus alone or only when a USB
  host actually enumerates (untested on hardware; the plug test will
  settle it). If it does, nightly charging means a dead reader until
  morning's unplug — the price of an absolute recovery floor.
  (`ENABLE_USBNET`/usbnetd is a dead path on this FW — the job exists,
  the binary doesn't.)
- Touch device is discovered from `/proc/bus/input/devices`
  (`ABS_MT_POSITION_X`); if discovery fails it falls back to
  `/dev/input/touch`. The `probe` output will confirm the real path.
- The binary statically links **MuPDF** (AGPL-3.0, © Artifex Software).
  Personal use on your own device is unrestricted, but *distributing* the
  built binary carries AGPL obligations (license alongside the binary,
  notice, and an offer of the complete corresponding source) — see
  `LICENSE` and `NOTICE`.

## Roadmap

- Resume hook (one work item, three symptoms): on wake, re-apply the
  frontlight (powerd restores *its* level over ours), repaint, and
  re-establish Wi-Fi. The elapsed-suspend detection already exists
  (SystemTime accounting).
- Idle policy in takeover mode: release `preventScreenSaver` during plain
  reading; hold awake while USB VBUS is present (~100 s window today).
- Suspend/resume hooks (lipc event → re-enable Wi-Fi, full refresh).

## License

AGPL-3.0 — see [LICENSE](LICENSE) and [NOTICE](NOTICE). The fonts are
SIL Open Font License 1.1 (license texts in `resources/fonts/OFL-*.txt`);
the statically-linked MuPDF is AGPL-3.0 (© Artifex Software, source at
[mupdf.com](https://mupdf.com)).
