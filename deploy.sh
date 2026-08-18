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
    cargo zigbuild --target "$TARGET" --release
    BIN="$ROOT/target/$TARGET/release/yb-reader"
    DST=/mnt/us/extensions/reader/bin/reader

    scp -q "$BIN" "$HOST:$DST.new"
    s=$(shasum -a 256 "$BIN" | awk '{print $1}')
    d=$(ssh "$HOST" "sha256sum $DST.new" | awk '{print $1}')
    if [ "$s" != "$d" ]; then
        echo "ERROR: hash mismatch after scp: $HOST:$DST.new" >&2
        echo "  expected $s" >&2
        echo "  got      $d" >&2
        ssh "$HOST" "rm -f $DST.new" || true
        exit 1
    fi

    # TERM first so the in-app guard restores frontlight/wifi/firewall on
    # the way out; -9 one second later for anything that ignored it.
    # Reset the crash counter (a deploy is not a crash) and relaunch:
    # via the takeover job when the framework is disabled (start.sh's
    # lipc calls need a live cvm), via start.sh otherwise.
    ssh "$HOST" "mv -f $DST.new $DST && chmod +x $DST && { cp -f $DST /mnt/us/kmc/kpm/packages/yb-reader/bin/reader 2>/dev/null || true; }; rm -f /var/local/yb-reader/fails; killall reader 2>/dev/null || true; sleep 1; killall -9 reader 2>/dev/null || true"
    sleep 1
    ssh "$HOST" "if [ -e /mnt/us/DONT_START_FRAMEWORK ]; then initctl restart yb-reader </dev/null >/dev/null 2>&1; else nohup /mnt/us/extensions/reader/bin/start.sh </dev/null >/dev/null 2>&1 & fi"
    echo "SSH deploy -> $HOST:$DST (+ kpm package copy, reader relaunched)"
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
    cp "$BIN" "$EXT/bin/reader"
    chmod +x "$EXT/bin/reader" "$EXT/bin/start.sh"
    verify_bin "$BIN" "$EXT/bin/reader"
    cp "$PKG/scriptlets/YBReader.sh" /Volumes/Kindle/documents/YBReader.sh
    chmod +x /Volumes/Kindle/documents/YBReader.sh
    echo "Direct install -> extensions/reader + documents/YBReader.sh"
    ls -lh "$EXT/bin/reader"
    exit 0
fi

if [ "$1" != "" ] && [ "$1" != "ssh" ]; then
    echo "usage: $0 [ssh|usb|probe]" >&2
    exit 2
fi

ssh_deploy
