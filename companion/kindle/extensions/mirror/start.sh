#!/bin/sh
# Launch the mirror client in the background.
DIR="$(cd "$(dirname "$0")" && pwd)"
CONF="$DIR/mirror.conf"
LOG="$DIR/mirror.log"
[ -f "$CONF" ] || { echo "no mirror.conf" >> "$LOG"; exit 1; }
. "$CONF"

# python3: KUAL Python-3 extension, system, or system alternate
PY="$(command -v python3)"
[ -z "$PY" ] && PY="$(ls /mnt/us/python3/bin/python3 \
                            /mnt/us/system/python3/bin/python3 2>/dev/null | head -1)"
[ -z "$PY" ] && { echo "no python3 — install the KUAL Python 3 extension" >> "$LOG"; exit 1; }

# best-effort: wake wifi and keep the screensaver from stealing the fb
[ -x /usr/bin/lipc-set ] && {
    lipc-set-prop com.lab126.wifid enable 1 2>/dev/null
    lipc-set-prop com.lab126.powerd preventScreenSaver 1 2>/dev/null
}

pkill -f mirror.py 2>/dev/null
sleep 1
cd "$DIR/.." 2>/dev/null || cd "$DIR"
nohup "$PY" "$DIR/../mirror.py" "$SERVER" "$SETTLE" >> "$LOG" 2>&1 &
echo "started: $PY mirror.py $SERVER $SETTLE" >> "$LOG"
