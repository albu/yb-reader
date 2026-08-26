#!/bin/sh
DIR="$(cd "$(dirname "$0")" && pwd)"
pkill -f mirror.py 2>/dev/null
[ -x /usr/bin/lipc-set ] && \
    lipc-set-prop com.lab126.powerd preventScreenSaver 0 2>/dev/null
echo "stopped" >> "$DIR/mirror.log"
