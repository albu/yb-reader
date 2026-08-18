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
DIR=/mnt/us/extensions/reader/bin
FLAG=/mnt/us/DONT_START_FRAMEWORK
STATE=/var/local/yb-reader
LOG=/var/local/yb-boot.log

[ -e "$FLAG" ] || exit 0

mkdir -p "$STATE"
echo "---- $(date) boot.sh start (pid $$) ----" >> "$LOG"

# 1. ssh lifeline. Our only hard recovery rung short of a serial cable;
#    nothing on the device reliably starts this in takeover mode. Runs
#    the bundled copy (koreader's binary pinned by sha on this unit);
#    the host key lives in $STATE so the identity survives launcher
#    changes (koreader's build resolves keys relative to its cwd).
if ! grep -qx dropbear /proc/[0-9]*/comm 2>/dev/null; then
    DB="$DIR/dropbear"
    [ -x "$DB" ] || DB=/mnt/us/koreader/dropbear
    KEY="$STATE/hostkey"
    [ -f "$KEY" ] || \
        cp /mnt/us/koreader/settings/SSH/dropbear_rsa_host_key "$KEY" 2>/dev/null
    "$DB" -E -R -p 2222 -P /tmp/dropbear_koreader.pid -r "$KEY" \
        >> "$LOG" 2>&1 &
fi

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

# 4. Run the reader. TERM (job stop / shutdown) is forwarded so the
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
