#!/bin/sh
# YB Reader launcher.
#
# Pauses the framework UI and hides the status bar (pillow), so the stock
# UI stops drawing over us and stops eating touch input. Everything is
# restored when the reader exits.
# Resolves its own directory, so it works from any launcher (scriptlet,
# kpm launch, ssh).
#
# 2026-08-17 post-mortem: do NOT get clever here. Stopping kppmainapp for
# +66 MB of reading RAM poisons appmgrd's history (its periodic
# savecontext timeout-check navigates "back" onto KPP, whose registry
# entry is executable=NONE → "Application error" dialog on every exit)
# and one variant left cvm dead mid-restore. Memory was never the
# constraint (73 MB peak reading vs ~150 MB available). Keep this file
# minimal: freeze, run, unfreeze, repaint.
DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$DIR"

# Single instance, atomically: the framework re-opens this script as the
# "current book" when a previous instance exits, so a second start.sh can
# arrive at the same moment as another launch. A pgrep check races in
# exactly that window (both launchers see "none" before either has
# exec'd reader); mkdir is atomic, so only one wins. Without this, two
# readers both read the touchscreen and every tap fires twice.
LOCK=/tmp/yb-reader.lock
if ! mkdir -m 777 "$LOCK" 2>/dev/null; then
    if pgrep -x reader >/dev/null 2>&1; then
        exit 0
    fi
    # No reader but a lock: leftovers from a crash — reclaim it.
    rm -rf "$LOCK" 2>/dev/null
    mkdir -m 777 "$LOCK" 2>/dev/null || exit 0
fi

cleanup() {
    # Unfreeze UI actors and restore pillow/home booklet on any exit/signal
    killall -CONT webreader kfxreader kfxview awesome cvm 2>/dev/null
    lipc-set-prop com.lab126.pillow disableEnablePillow enable 2>/dev/null
    lipc-set-prop com.lab126.appmgrd start app://com.lab126.booklet.home 2>/dev/null
    modprobe usb_f_mass_storage 2>/dev/null || true
    modprobe g_mass_storage 2>/dev/null || true
    rm -rf "$LOCK" 2>/dev/null
}
trap cleanup EXIT INT TERM HUP

# An orphan takeover flag (armed without the upstart job installed — the
# pre-gate state, or a non-root install) can only strand the device on the
# next reboot: the framework stops and nothing starts the reader. This
# session is proof the device boots stock, so drop it.
if [ -e /mnt/us/DONT_START_FRAMEWORK ] && [ ! -e /etc/upstart/yb-reader.conf ]; then
    rm -f /mnt/us/DONT_START_FRAMEWORK
fi

lipc-set-prop com.lab126.pillow disableEnablePillow disable 2>/dev/null
# Freeze the on-screen UI (awesome, the WM) and the Java framework core
# (cvm) — both draw over us and answer taps if left running.
killall -STOP awesome cvm 2>/dev/null
# The framework's book opener also draws over us when the library item is
# launched (and it reacts to taps); freeze it too, restore on exit.
killall -STOP webreader kfxreader 2>/dev/null

# Put CPU governor in ondemand mode so frequencies downclock when idle
for gov in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do
    [ -f "$gov" ] && echo ondemand > "$gov" 2>/dev/null || true
done

./reader
rc=$?

# Takeover handoff: a session launched by start.sh while takeover mode is
# armed (DONT_START_FRAMEWORK present — e.g. the deploy's fallback path)
# must still be able to return to the stock UI. boot.sh owns this handoff
# when the upstart job runs the reader; mirror it here so "Exit to Kindle"
# works from either launcher instead of stranding the device frozen.
if [ "$rc" -eq 42 ] && [ -e /mnt/us/DONT_START_FRAMEWORK ]; then
    rm -f /mnt/us/DONT_START_FRAMEWORK
    killall -CONT awesome webreader kfxreader kfxview KPPMainApp pillowd \
        kb scanner-main JunoStatusBarDr 2>/dev/null
    initctl start framework 2>/dev/null || true
fi
exit $rc
