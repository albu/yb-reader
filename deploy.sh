#!/bin/sh
# Build for the Kindle and deploy.
#   ./deploy.sh          -> yb-reader over SSH (the fast loop; default)
#   ./deploy.sh usb      -> yb-reader to a USB-mounted /Volumes/Kindle
#   ./deploy.sh probe    -> yb-probe (USB)
# The LD/AR envs are the same ones the Makefile exports (see README).
# After every binary copy we verify the sha256 on-device: a truncated copy
# execs as ENOEXEC and fails like a shell-script syntax error.
set -e
ROOT="$(cd "$(dirname "$0")" && pwd)"
TARGET=arm-unknown-linux-musleabihf
HOST=kindle
export LD="${YB_LD:-/opt/homebrew/opt/lld@21/bin/ld.lld -m armelf_linux_eabi}"
export AR="${YB_AR:-/opt/homebrew/opt/llvm@22/bin/llvm-ar}"

cd "$ROOT"

verify_bin() {
    src="$1"
    dst="$2"
    s=$(shasum -a 256 "$src" | awk '{print $1}')
    d=$(shasum -a 256 "$dst" | awk '{print $1}')
    if [ "$s" != "$d" ]; then
        echo "ERROR: hash mismatch after copy: $dst" >&2
        echo "  expected $s" >&2
        echo "  got      $d" >&2
        exit 1
    fi
}

# SSH fast loop. Scp'ing straight over the running binary fails ETXTBSY
# ("text file busy"), so we stage as reader.new, hash-verify the staged
# copy, then mv it into place (atomic inode swap — the live binary is only
# ever replaced by a verified copy). Killing the reader afterwards is safe:
# start.sh owns the framework freeze and restores cvm/pillow when it exits.
ssh_deploy() {
    # Concurrent deploy.sh runs (double-invoke, editor auto-rerun) must
    # never interleave the build/scp/swap: serialize with a host lock.
    # mkdir is atomic, so a second instance aborts cleanly instead of
    # racing the mv and killall below; the EXIT trap releases it on any
    # exit (including set -e aborts and signals).
    LOCK_DIR=/tmp/yb-reader-deploy.lock
    if ! mkdir "$LOCK_DIR" 2>/dev/null; then
        echo "ERROR: another deploy is running ($LOCK_DIR exists) — aborting" >&2
        echo "       (if a previous deploy was killed, rm -rf $LOCK_DIR)" >&2
        exit 1
    fi
    trap 'rm -rf "$LOCK_DIR" 2>/dev/null' EXIT INT TERM HUP

    cargo zigbuild --target "$TARGET" --release
    BIN="$ROOT/target/$TARGET/release/yb-reader"
    DST=/mnt/us/extensions/reader/bin/reader
    STAGE_DST="$DST.new.$$"
    # The device suspends on an input-idle timer even mid-conversation;
    # a bare ssh then hangs to its 60s+ timeout and (set -e) can eat the
    # rest of the deploy silently. Every remote call gets a timeout.
    SSHC="ssh -o ConnectTimeout=10"

    # Sweep stage files left by earlier failed or interrupted deploys so
    # they cannot accumulate on the device across retries.
    $SSHC "$HOST" "rm -f /mnt/us/extensions/reader/bin/reader.new.* /mnt/us/extensions/reader/bin/start.sh.new.* /mnt/us/extensions/reader/bin/boot.sh.new.*" || true

    # Sync launcher scripts so fixes to start.sh/boot.sh land alongside the
    # binary. Staged and renamed, never written in place: a live POSIX sh
    # reads start.sh lazily, so truncating it mid-run can skip commands.
    scp -o ConnectTimeout=10 -q "$ROOT/packages/yb-reader/bin/start.sh" "$HOST:/mnt/us/extensions/reader/bin/start.sh.new.$$" || true
    scp -o ConnectTimeout=10 -q "$ROOT/packages/yb-reader/bin/boot.sh" "$HOST:/mnt/us/extensions/reader/bin/boot.sh.new.$$" || true

    # Companion source zip for the receive page (built with `make dist`).
    # Sits next to the books; invisible to the library listing, downloadable
    # through the Companion card's /api/file link.
    if [ -f "$ROOT/companion/dist/yb-mirror.zip" ]; then
        scp -o ConnectTimeout=10 -q "$ROOT/companion/dist/yb-mirror.zip" "$HOST:/mnt/us/documents/yb-mirror.zip" || true
    fi

    # Builtin WordNet dictionary (English glosses) — built locally by
    # tools/build_wordnet, never committed (third-party data). Copy only
    # when present, so a fresh clone without the source still deploys.
    # Hash-gated: the 17 MB scp is ~7 s of the loop (ssh to the device
    # runs ~2.3 MB/s), and the blob changes only on a WordNet rebuild —
    # a device-side sha256sum round-trip costs ~1.3 s, so routine deploys
    # skip the copy entirely.
    if [ -f "$ROOT/resources/wordnet.ybdict" ]; then
        local_sha=$(shasum -a 256 "$ROOT/resources/wordnet.ybdict" | awk '{print $1}')
        remote_sha=$($SSHC "$HOST" "sha256sum /mnt/us/extensions/reader/data/wordnet.ybdict 2>/dev/null" | awk '{print $1}' || true)
        if [ "$local_sha" != "$remote_sha" ]; then
            $SSHC "$HOST" "mkdir -p /mnt/us/extensions/reader/data"
            scp -o ConnectTimeout=10 -q "$ROOT/resources/wordnet.ybdict" "$HOST:/mnt/us/extensions/reader/data/wordnet.ybdict" || true
        else
            echo "wordnet.ybdict unchanged — skipping scp"
        fi
    else
        echo "WARNING: resources/wordnet.ybdict missing — builtin WordNet will not be deployed."
        echo "         Build it with: python3 tools/build_wordnet/build_wordnet.py <WordNet-3.0>/dict resources/wordnet.ybdict"
    fi

    scp -o ConnectTimeout=10 -q "$BIN" "$HOST:$STAGE_DST"
    s=$(shasum -a 256 "$BIN" | awk '{print $1}')
    d=$($SSHC "$HOST" "sha256sum $STAGE_DST" | awk '{print $1}')
    if [ "$s" != "$d" ]; then
        echo "ERROR: hash mismatch after scp: $HOST:$STAGE_DST" >&2
        echo "  expected $s" >&2
        echo "  got      $d" >&2
        ssh "$HOST" "rm -f $STAGE_DST" || true
        exit 1
    fi

    # TERM first so the in-app guard restores frontlight/wifi/firewall on
    # the way out; -9 one second later for anything that ignored it.
    # Reset the crash counter (a deploy is not a crash) and relaunch the
    # way this session was launched: via the takeover job when the job is
    # running, via start.sh for a stock session. The yb-reader JOB state
    # is the truth, not the flag — the curtain card can arm the flag from
    # a stock session, and boot.sh there would race start.sh's unfreeze
    # of cvm (and a second reader).
    #
    # The reset MUST run before the kills below: a killed boot.sh never
    # removes its `running` marker, and the next boot's audit would read
    # the deploy as an unclean shutdown (a strike it did not earn). The
    # whole safety-ledger state is swept for the same reason.
    $SSHC "$HOST" "mv -f /mnt/us/extensions/reader/bin/start.sh.new.$$ /mnt/us/extensions/reader/bin/start.sh 2>/dev/null; mv -f /mnt/us/extensions/reader/bin/boot.sh.new.$$ /mnt/us/extensions/reader/bin/boot.sh 2>/dev/null; mv -f $STAGE_DST $DST && chmod +x $DST /mnt/us/extensions/reader/bin/start.sh /mnt/us/extensions/reader/bin/boot.sh && { cp -f $DST /mnt/us/kmc/kpm/packages/yb-reader/bin/reader 2>/dev/null || true; }; rm -f /var/local/yb-reader/fails /var/local/yb-reader/running /var/local/yb-reader/sleeping /var/local/yb-reader/strikes && touch /var/local/yb-reader/last && pkill -9 -x boot.sh 2>/dev/null || true; killall -9 reader 2>/dev/null || true; rm -rf /tmp/yb-reader.lock /tmp/yb-heartbeat 2>/dev/null; sleep 1; if test -e /mnt/us/DONT_START_FRAMEWORK; then initctl restart yb-reader </dev/null >/dev/null 2>&1 || initctl start yb-reader </dev/null >/dev/null 2>&1; else nohup /mnt/us/extensions/reader/bin/start.sh </dev/null >/dev/null 2>&1 & fi"
    # Post-verification: the deploy is not done when the bytes land, it is
    # done when the new binary is the one running (rc=0 TERM exits are
    # "normal" to the job, so nothing else guarantees the relaunch).
    ok=""
    for i in 1 2 3 4 5 6; do
        if $SSHC "$HOST" "pidof reader >/dev/null && ! ls -l /proc/\$(pidof reader | awk '{print \$1}')/exe 2>/dev/null | grep -q deleted" 2>/dev/null; then
            ok=1; break
        fi
        sleep 3
    done
    if [ -n "$ok" ]; then
        echo "SSH deploy -> $HOST:$DST (+ kpm package copy, reader running new build)"
    else
        echo "WARNING: binary deployed but reader not running — check:" >&2
        $SSHC "$HOST" "initctl status yb-reader; tail -3 /var/local/yb-boot.log" >&2 || true
        exit 1
    fi
}

if [ "$1" = "probe" ]; then
    cargo zigbuild --target "$TARGET" --release -p yb-probe
    if [ ! -d /Volumes/Kindle ]; then
        echo "Kindle not mounted (no /Volumes/Kindle). Binary:"
        ls -lh "$ROOT/target/$TARGET/release/yb-probe"
        exit 0
    fi
    EXT=/Volumes/Kindle/extensions/probe
    mkdir -p "$EXT/bin"
    cp "$ROOT/kual/probe/config.sh" "$EXT/config.sh"
    cp "$ROOT/kual/probe/menu.json" "$EXT/menu.json"
    cp "$ROOT/kual/probe/bin/start.sh" "$EXT/bin/start.sh"
    cp "$ROOT/target/$TARGET/release/yb-probe" "$EXT/bin/probe"
    chmod +x "$EXT/bin/probe" "$EXT/bin/start.sh"
    verify_bin "$ROOT/target/$TARGET/release/yb-probe" "$EXT/bin/probe"
    echo "Probe deployed to /Volumes/Kindle/extensions/probe"
    ls -lh "$EXT/bin/probe"
    exit 0
fi

if [ "$1" = "usb" ]; then
    cargo zigbuild --target "$TARGET" --release

    if [ ! -d /Volumes/Kindle ]; then
        echo "Kindle not mounted (no /Volumes/Kindle). Binary:"
        ls -lh "$ROOT/target/$TARGET/release/yb-reader"
        exit 0
    fi

    BIN="$ROOT/target/$TARGET/release/yb-reader"
    PKG="$ROOT/packages/yb-reader"

    # Stage the KPM package with the freshly built binary + scriptlet.
    mkdir -p "$PKG/bin"
    cp "$BIN" "$PKG/bin/reader"
    chmod +x "$PKG/bin/reader"

    # 1. KPM package (proper install/upgrade path, like the koreader package).
    if [ -d /Volumes/Kindle/kmc/kpm/packages ]; then
        rm -rf /Volumes/Kindle/kmc/kpm/packages/yb-reader
        cp -R "$PKG" /Volumes/Kindle/kmc/kpm/packages/yb-reader
        # macOS cp -R leaves AppleDouble (._*) metadata and we don't want the
        # repo's .gitkeep in the installed package.
        find /Volumes/Kindle/kmc/kpm/packages/yb-reader -name '._*' -delete
        rm -f /Volumes/Kindle/kmc/kpm/packages/yb-reader/bin/.gitkeep
        verify_bin "$BIN" /Volumes/Kindle/kmc/kpm/packages/yb-reader/bin/reader
        echo "KPM package -> /Volumes/Kindle/kmc/kpm/packages/yb-reader"
    fi

    # 2. Direct install (works even before KPM knows the package): binary +
    #    library scriptlet, exactly the two things KOReader uses.
    EXT=/Volumes/Kindle/extensions/reader
    mkdir -p "$EXT/bin"
    cp "$ROOT/kual/reader/config.sh" "$EXT/config.sh"
    cp "$ROOT/kual/reader/menu.json" "$EXT/menu.json"
    cp "$PKG/bin/start.sh" "$EXT/bin/start.sh"
    cp "$PKG/bin/boot.sh" "$EXT/bin/boot.sh"
    # The patched dropbear resolves settings/SSH/ relative to the tree
    # root; authorized_keys ships, the host key does NOT (device-private;
    # dropbear -R generates one on first connect if absent).
    mkdir -p "$EXT/settings/SSH"
    cp "$PKG/settings/SSH/authorized_keys" "$EXT/settings/SSH/"
    cp "$BIN" "$EXT/bin/reader"
    chmod +x "$EXT/bin/reader" "$EXT/bin/start.sh" "$EXT/bin/boot.sh"
    verify_bin "$BIN" "$EXT/bin/reader"
    find "$EXT" -name '._*' -delete
    cp "$PKG/scriptlets/YBReader.sh" /Volumes/Kindle/documents/YBReader.sh
    chmod +x /Volumes/Kindle/documents/YBReader.sh
    if [ -f "$ROOT/companion/dist/yb-mirror.zip" ]; then
        cp "$ROOT/companion/dist/yb-mirror.zip" /Volumes/Kindle/documents/yb-mirror.zip
    fi
    if [ -f "$ROOT/resources/wordnet.ybdict" ]; then
        mkdir -p /Volumes/Kindle/extensions/reader/data
        cp "$ROOT/resources/wordnet.ybdict" /Volumes/Kindle/extensions/reader/data/wordnet.ybdict
    fi
    echo "Direct install -> extensions/reader + documents/YBReader.sh"
    ls -lh "$EXT/bin/reader"
    exit 0
fi

if [ "$1" != "" ] && [ "$1" != "ssh" ]; then
    echo "usage: $0 [ssh|usb|probe]" >&2
    exit 2
fi

ssh_deploy
