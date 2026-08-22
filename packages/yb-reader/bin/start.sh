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
trap 'rm -rf "$LOCK" 2>/dev/null' EXIT INT TERM


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

# Wake everything BEFORE the lipc calls: lipc is a conversation, and a
# frozen cvm cannot answer — appmgrd times the call out and draws the
# "Application error" dialog.
killall -CONT webreader kfxreader 2>/dev/null
killall -CONT awesome cvm 2>/dev/null
lipc-set-prop com.lab126.pillow disableEnablePillow enable 2>/dev/null
# The app held an EVIOCGRAB on the touchscreen, so the framework wakes
# with an EMPTY event queue and no reason to repaint — our last screen
# stays on the framebuffer until the next tap. Starting the home booklet
# forces the redraw (a harmless re-launch if home is already current).
lipc-set-prop com.lab126.appmgrd start app://com.lab126.booklet.home 2>/dev/null
exit $rc
