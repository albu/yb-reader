#!/bin/sh
# Set SERVER in mirror.conf (the yb-mirror config template) to this Mac's
# current Wi-Fi IP and, if the Kindle is mounted, copy the file to the
# device's /mnt/us/extensions/mirror/mirror.conf too.
set -e
IP="$(ipconfig getifaddr en0 2>/dev/null || ipconfig getifaddr en1 2>/dev/null)"
if [ -z "$IP" ]; then
    echo "error: no IP on en0/en1 — are you on Wi-Fi?" >&2
    exit 1
fi
ROOT="$(cd "$(dirname "$0")" && pwd)"
CONF="$ROOT/mirror.conf"
if grep -q '^SERVER=' "$CONF" 2>/dev/null; then
    sed -i '' "s#^SERVER=.*#SERVER=http://$IP:8765#" "$CONF"
else
    printf '\nSERVER=http://%s:8765\n' "$IP" >> "$CONF"
fi
echo "mirror.conf now: SERVER=http://$IP:8765"
if [ -d /Volumes/Kindle/extensions/mirror ]; then
    cp "$CONF" /Volumes/Kindle/extensions/mirror/mirror.conf
    echo "copied to /Volumes/Kindle/extensions/mirror/mirror.conf"
else
    echo "Kindle not mounted — copy later: cp $CONF /Volumes/Kindle/extensions/mirror/mirror.conf"
fi
