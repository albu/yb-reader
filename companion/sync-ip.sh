#!/bin/sh
# Set SERVER in kindle/extensions/mirror/mirror.conf to this Mac's current
# Wi-Fi IP and, if the Kindle is mounted, copy the file to the device too.
set -e
IP="$(ipconfig getifaddr en0 2>/dev/null || ipconfig getifaddr en1 2>/dev/null)"
if [ -z "$IP" ]; then
    echo "error: no IP on en0/en1 — are you on Wi-Fi?" >&2
    exit 1
fi
ROOT="$(cd "$(dirname "$0")" && pwd)"
CONF="$ROOT/kindle/extensions/mirror/mirror.conf"
sed -i '' "s#^SERVER=.*#SERVER=http://$IP:8765#" "$CONF"
echo "mirror.conf now: SERVER=http://$IP:8765"
if [ -d /Volumes/Kindle/extensions/mirror ]; then
    cp "$CONF" /Volumes/Kindle/extensions/mirror/mirror.conf
    echo "copied to /Volumes/Kindle/extensions/mirror/mirror.conf"
else
    echo "Kindle not mounted — copy later: cp $CONF /Volumes/Kindle/extensions/mirror/mirror.conf"
fi
