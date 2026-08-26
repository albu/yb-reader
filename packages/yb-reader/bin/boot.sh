#!/bin/sh
# Takeover boot: launched by /etc/upstart/yb-reader.conf whenever the
# framework job ends up stopped (including its DONT_START_FRAMEWORK early
# exit). The flag file is the single enable bit: no flag -> this is a
# no-op and stock mode is untouched (shutdown ghost-starts land here too).
#
# Responsibilities, in order:
#   1. ssh lifeline (KOReader's dropbear) - started unconditionally so
#      recovery never depends on a framework-dependent launcher
#   2. USB plug lifecycle - USB is FULLY STOCK (plug = drive, always;
#      never touched from our side). The export unmounts /mnt/us — the
#      reader binary, books and logs live there — so a plug kills the
#      reader BY DESIGN, and this script must never respawn against an
#      exported disk (the 2026-08-23 respawn storm gave up a blank
#      device). Both ends of a plug are managed: boot-while-plugged
#      waits for the cable, and an export death waits out the plug
#      before handing upstart a clean respawn.
#   3. boot audit - account for how the last session died (an unclean
#      SoC death counts a decaying strike; 4 recent strikes hand the
#      device to stock)
#   4. crash counter - 3 consecutive fast failures also hand the device
#      back to stock (which then has Amazon's own escalation net
#      underneath: 3 restarts -> 2 reboots -> customer-service halt)
#   5. freeze any GUI actors that came up anyway (start.sh semantics)
#   6. run the reader under the hang watchdog; exit 42 means "user asked
#      for stock, restart it"
#
# Two failure ledgers, deliberately split:
#   fails   (shell, this file) - the binary won't run at all. Cleared
#           after 60s of runtime because a binary that dies instantly is
#           a different disease from one that misbehaves later.
#   strikes (Rust: bootaudit/watchdog) - runtime misbehavior: hangs the
#           watchdog killed and SoC deaths the audit found. Decayed by
#           a 1h window, NOT by runtime: a hang that develops after a
#           minute would wipe its own evidence under the 60s rule and
#           reboot-loop forever. Both ladders land on the same
#           fallback_to_stock().
ROOT=/mnt/us/extensions/reader
DIR=$ROOT/bin
FLAG=/mnt/us/DONT_START_FRAMEWORK
STATE=/var/local/yb-reader
LOG=/var/local/yb-boot.log
HEARTBEAT=/tmp/yb-heartbeat

[ -e "$FLAG" ] || exit 0

mkdir -p "$STATE"
echo "---- $(date) boot.sh start (pid $$, ppid $PPID) ----" >> "$LOG"
trap 'sig=$?; echo "$(date) boot.sh EXIT (pid $$) rc=$sig" >> "$LOG"' EXIT
trap 'echo "$(date) boot.sh TRAP: SIGTERM received (pid $$)" >> "$LOG"; exit 143' TERM
trap 'echo "$(date) boot.sh TRAP: SIGINT received (pid $$)" >> "$LOG"; exit 130' INT
trap 'echo "$(date) boot.sh TRAP: SIGHUP received (pid $$)" >> "$LOG"; exit 129' HUP
trap 'echo "$(date) boot.sh TRAP: SIGQUIT received (pid $$)" >> "$LOG"; exit 131' QUIT

# USB power present (the wired charger input; sysinfo::vbus reads the
# same node). Shell-side, cheap, no dependencies.
vbus() {
    [ "$(cat /sys/class/power_supply/bd71827_ac/online 2>/dev/null)" = "1" ]
}

# Stock drive mode actually engaged: a host has the gadget configured.
# NOT /proc-modules presence — the module stays resident for life on
# some boots, and matching on it alone classified every non-0/42 death
# as an export death (each deploy's killall took the plug branch and
# cleared the fails ledger — found live 2026-08-25).
drive_mode() {
    grep -qx configured /sys/class/udc/*/state 2>/dev/null
}

# Wait out the cable. 5 s polls — a plug session is minutes-to-hours,
# and the SoC idles between polls (nothing else runs while we wait).
wait_unplug() {
    echo "$(date) usb: plugged — waiting for unplug ($1)" >> "$LOG"
    while vbus; do
        sleep 5
    done
    echo "$(date) usb: unplugged" >> "$LOG"
}

# After an unplug, volumd re-serves the fuse mount itself; give it half
# a minute before declaring the disk lost. No mount -> no binary -> the
# reader can never run again from here; stock is the only rung left.
wait_us_mounted() {
    i=0
    while [ "$i" -lt 6 ]; do
        grep -q ' /mnt/us ' /proc/mounts 2>/dev/null && return 0
        sleep 5
        i=$((i + 1))
    done
    grep -q ' /mnt/us ' /proc/mounts 2>/dev/null
}

# Give the device back to the stock framework: an unfrozen WM and
# Amazon's escalation underneath (USB needs nothing — it is stock).
fallback_to_stock() {
    echo "$(date) FALLBACK: returning to stock" >> "$LOG"
    # strikes too: the episode that earned them is over, and a user
    # re-arming takeover within the decay window must not bounce off an
    # instant re-fallback on their very next boot.
    rm -f "$FLAG" "$STATE/fails" "$STATE/strikes" "$STATE/running" \
        "$STATE/sleeping"
    # The stock GUI cannot come back onto a frozen WM (start.sh's own
    # post-mortem: a frozen UI cannot answer lipc and draws nothing).
    killall -CONT awesome webreader kfxreader kfxview KPPMainApp pillowd \
        kb scanner-main JunoStatusBarDr 2>/dev/null
    initctl start framework >> "$LOG" 2>&1 || true
}

# 1. ssh lifeline. Our only hard recovery rung short of a serial cable;
#    nothing on the device reliably starts this in takeover mode. The
#    patched dropbear resolves settings/SSH/ (authorized_keys AND host
#    keys) relative to its CWD — koreader's plugin runs it the same way,
#    from its own tree. Our tree mirrors the layout. NO -r pinning: a
#    single RSA -r path kills the ed25519 handshake (banner, then death
#    at first KEX — found on device 2026-08-19; koreader never ships an
#    RSA host key at all).
if ! grep -qx dropbear /proc/[0-9]*/comm 2>/dev/null; then
    if [ -x "$DIR/dropbear" ]; then
        (cd "$ROOT" && "$DIR/dropbear" -E -R -s -p 2222 \
            -P /tmp/dropbear_koreader.pid >> "$LOG" 2>&1 &)
    else
        (cd /mnt/us/koreader && ./dropbear -E -R -s -p 2222 \
            -P /tmp/dropbear_koreader.pid >> "$LOG" 2>&1 &)
    fi
fi

# 1b. ...and let it be reached. The stock firewall's INPUT policy is
#     DROP, so a freshly-booted dropbear listens into a black hole until
#     something adds the allow rule — the curtain's SSH card does exactly
#     this (ybdev::ssh::rules); after a reboot nothing did, and ssh "didn't
#     wake up" (2026-08-23). Same specs, idempotent via -C.
IPT=/usr/sbin/iptables
"$IPT" -C INPUT -p tcp --dport 2222 -m conntrack --ctstate NEW,ESTABLISHED -j ACCEPT 2>/dev/null || \
    "$IPT" -I INPUT 1 -p tcp --dport 2222 -m conntrack --ctstate NEW,ESTABLISHED -j ACCEPT
"$IPT" -C OUTPUT -p tcp --sport 2222 -m conntrack --ctstate ESTABLISHED -j ACCEPT 2>/dev/null || \
    "$IPT" -A OUTPUT -p tcp --sport 2222 -m conntrack --ctstate ESTABLISHED -j ACCEPT

# 1c. Booting while plugged (reboot on the cable, wake-from-dead-battery
#     on the cable): the reader cannot run against an exported /mnt/us,
#     so wait for the cable before anything else touches the disk. Stock
#     parks on a drive screen in the same situation; the ssh lifeline
#     above is already up either way.
if vbus; then
    wait_unplug "boot with usb power"
    wait_us_mounted || { fallback_to_stock; exit 1; }
fi

# 2. Boot audit: classify how the previous session ended. Only SoC-level
#    death (power-hold reboot, battery pull, panic) leaves the `running`
#    marker behind — this script removes it after every exit it observes.
#    rc 5 = strikes say stock; 0 = proceed. A crashed audit (any other
#    rc, including not-runnable 126/127) degrades to "proceed": the
#    fails ledger below still guards the won't-run cases.
"$DIR/reader" bootaudit >> "$LOG" 2>&1
case $? in
    5) fallback_to_stock; exit 1 ;;
esac

# 3. Crash counter. A replaced binary earns a fresh count (deploys must
#    not trip the fallback); only the same binary failing repeatedly
#    does. Cleared again after 60s of genuine runtime below.
if [ "$DIR/reader" -nt "$STATE/last" ]; then
    echo 0 > "$STATE/fails"
fi
touch "$STATE/last"
fails=$(cat "$STATE/fails" 2>/dev/null || echo 0)
fails=$((fails + 1))
if [ "$fails" -gt 3 ]; then
    echo "$(date) FALLBACK: $((fails - 1)) fast failures, returning to stock" >> "$LOG"
    fallback_to_stock
    exit 1
fi
echo "$fails" > "$STATE/fails"

# 4. Freeze GUI actors that ignore the framework's absence. Xorg is
# deliberately left running (start.sh never froze it either); an idle
# X server holds nobody's touchscreen.
killall -STOP awesome webreader kfxreader kfxview KPPMainApp pillowd \
    kb scanner-main JunoStatusBarDr 2>/dev/null

# 5. Put CPU governor in ondemand mode so frequencies downclock when idle
for gov in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do
    [ -f "$gov" ] && echo ondemand > "$gov" 2>/dev/null || true
done

# 6. Run the reader under the hang watchdog. TERM (job stop / shutdown)
# is forwarded so the in-app guard can restore frontlight/wifi/firewall
# on the way out. The heartbeat is swept first so a stale file from a
# previous boot can never satisfy this session's watchdog.
rm -f "$HEARTBEAT"
"$DIR/reader" &
rpid=$!
"$DIR/reader" --watchdog "$rpid" >> "$LOG" 2>&1 &
# Runtime proof earns the fails reset: by THIS pid, never by name — the
# hang watchdog runs the same comm as the reader, and the old
# `killall -0 reader` let a healthy watchdog vouch for a dead reader.
(sleep 60; kill -0 "$rpid" 2>/dev/null && rm -f "$STATE/fails") &
trap 'echo "$(date) boot.sh: forwarding TERM to reader (rpid $rpid)" >> "$LOG"; kill -TERM "$rpid" 2>/dev/null' TERM
trap 'echo "$(date) boot.sh: forwarding INT to reader (rpid $rpid)" >> "$LOG"; kill -INT "$rpid" 2>/dev/null' INT
wait "$rpid"
rc=$?
trap 'echo "$(date) boot.sh TRAP: post-wait SIGTERM received (pid $$)" >> "$LOG"; exit 143' TERM
trap 'echo "$(date) boot.sh TRAP: post-wait SIGINT received (pid $$)" >> "$LOG"; exit 130' INT

echo "$(date) boot.sh [step 1]: reader exited rc=$rc" >> "$LOG"
# Observed exit: the boot audit's `running` marker has served its turn.
# Removed BEFORE the plug wait below — a power-hold during that wait must
# find no marker and classify as clean, not strike an already-accounted
# death a second time.
rm -f "$STATE/running"
echo "$(date) boot.sh [step 2]: removed running marker; vbus=$(cat /sys/class/power_supply/bd71827_ac/online 2>/dev/null) udc=$(cat /sys/class/udc/*/state 2>/dev/null)" >> "$LOG"
# Export death (the common one: plug pulled the disk from under the
# reader) OR the graceful bow-out (rc 43 — the reader saw USB power,
# painted its farewell screen and exited so the stock drive-mode dance
# gets a free disk; same park applies). Expected by design — don't let
# either near the failure ledgers. Park until the cable is out and the
# disk is back, then exit 1 (NOT 0: 0 is in the job's "normal exit"
# list and would not respawn) so upstart starts exactly one fresh
# instance, post-unplug. The vbus||drive_mode test is deliberately
# coarse: a GENUINE crash while on a wall charger parks until unplug
# too — conservative, self-correcting, and the price of never
# respawning against a possibly-exported disk.
if [ "$rc" -ne 0 ] && [ "$rc" -ne 42 ] && { vbus || drive_mode; }; then
    echo "$(date) boot.sh [step 3]: usb export/bow-out branch taken (rc=$rc)" >> "$LOG"
    wait_unplug "export death"
    echo "$(date) boot.sh [step 4]: wait_unplug finished, checking if /mnt/us is mounted" >> "$LOG"
    if ! wait_us_mounted; then
        echo "$(date) boot.sh [step 5]: /mnt/us did not come back — falling back" >> "$LOG"
        fallback_to_stock
        exit 1
    fi
    echo "$(date) boot.sh [step 6]: /mnt/us confirmed mounted, clearing fails and exiting 1 for clean upstart respawn" >> "$LOG"
    # Expected death — clear fails too, not just strikes: plug cycles
    # faster than the 60 s runtime proof would otherwise accumulate
    # toward the won't-run fallback and hand a healthy device to stock
    # mid-session. Each repeat needs a physical plug, so real won't-run
    # diseases still climb their ladder.
    rm -f "$STATE/fails"
    exit 1
fi
echo "$(date) boot.sh [step 7]: non-usb exit path (rc=$rc)" >> "$LOG"
if [ "$rc" -eq 0 ] || [ "$rc" -eq 42 ]; then
    rm -f "$STATE/fails" "$STATE/strikes"
fi
if [ "$rc" -eq 42 ]; then
    echo "$(date) user requested stock; removing flag and starting framework" >> "$LOG"
    rm -f "$FLAG"
    killall -CONT awesome webreader kfxreader kfxview KPPMainApp pillowd \
        kb scanner-main JunoStatusBarDr 2>/dev/null
    initctl start framework >> "$LOG" 2>&1 || true
    exit 0
fi
exit "$rc"
