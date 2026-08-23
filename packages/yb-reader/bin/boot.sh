#!/bin/sh
# Takeover boot: launched by /etc/upstart/yb-reader.conf whenever the
# framework job ends up stopped (including its DONT_START_FRAMEWORK early
# exit). The flag file is the single enable bit: no flag -> this is a
# no-op and stock mode is untouched (shutdown ghost-starts land here too).
#
# Responsibilities, in order:
#   1. ssh lifeline (KOReader's dropbear) - started unconditionally so
#      recovery never depends on a framework-dependent launcher
#   2. crash counter - 3 consecutive failures hand the device back to
#      the stock framework (which then has Amazon's own escalation net
#      underneath: 3 restarts -> 2 reboots -> customer-service halt)
#   3. freeze any GUI actors that came up anyway (start.sh semantics)
#   4. run the reader; exit 42 means "user asked for stock, restart it"
ROOT=/mnt/us/extensions/reader
DIR=$ROOT/bin
FLAG=/mnt/us/DONT_START_FRAMEWORK
STATE=/var/local/yb-reader
LOG=/var/local/yb-boot.log

[ -e "$FLAG" ] || exit 0

mkdir -p "$STATE"
echo "---- $(date) boot.sh start (pid $$) ----" >> "$LOG"

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
# DROP, so a freshly-booted dropbear listens into a black hole until
# something adds the allow rule — the curtain's SSH card does exactly
# this (ybdev::ssh::rules); after a reboot nothing did, and ssh "didn't
# wake up" (2026-08-23). Same specs, idempotent via -C.
IPT=/usr/sbin/iptables
"$IPT" -C INPUT -p tcp --dport 2222 -m conntrack --ctstate NEW,ESTABLISHED -j ACCEPT 2>/dev/null || \
    "$IPT" -I INPUT 1 -p tcp --dport 2222 -m conntrack --ctstate NEW,ESTABLISHED -j ACCEPT
"$IPT" -C OUTPUT -p tcp --sport 2222 -m conntrack --ctstate ESTABLISHED -j ACCEPT 2>/dev/null || \
    "$IPT" -A OUTPUT -p tcp --sport 2222 -m conntrack --ctstate ESTABLISHED -j ACCEPT

# 2. Crash counter. A replaced binary earns a fresh count (deploys must
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
    rm -f "$FLAG" "$STATE/fails"
    # The reader's charge-only USB mode removes the mass-storage kernel
    # modules; a SIGKILLed reader never restores them itself, and stock
    # mode without drive mode looks broken. Give them back here.
    modprobe usb_f_mass_storage 2>/dev/null || true
    modprobe g_mass_storage 2>/dev/null || true
    # The stock GUI cannot come back onto a frozen WM (start.sh's own
    # post-mortem: a frozen UI cannot answer lipc and draws nothing).
    killall -CONT awesome webreader kfxreader kfxview KPPMainApp pillowd \
        kb scanner-main JunoStatusBarDr 2>/dev/null
    initctl start framework >> "$LOG" 2>&1 || true
    exit 1
fi
echo "$fails" > "$STATE/fails"
(sleep 60; killall -0 reader >/dev/null 2>&1 && rm -f "$STATE/fails") &

# 3. Freeze GUI actors that ignore the framework's absence. Xorg is
#    deliberately left running (start.sh never froze it either); an idle
#    X server holds nobody's touchscreen.
killall -STOP awesome webreader kfxreader kfxview KPPMainApp pillowd \
    kb scanner-main JunoStatusBarDr 2>/dev/null

# 4. Put CPU governor in ondemand mode so frequencies downclock when idle
for gov in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do
    [ -f "$gov" ] && echo ondemand > "$gov" 2>/dev/null || true
done

# 5. Run the reader. TERM (job stop / shutdown) is forwarded so the
#    in-app guard can restore frontlight/wifi/firewall on the way out.
"$DIR/reader" &
rpid=$!
trap 'kill -TERM "$rpid" 2>/dev/null' TERM INT
wait "$rpid"
rc=$?
trap - TERM INT

echo "$(date) reader exited rc=$rc" >> "$LOG"
if [ "$rc" -eq 0 ] || [ "$rc" -eq 42 ]; then
    rm -f "$STATE/fails"
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
