#!/bin/sh
# Report everything mirror.py needs to know about this Kindle.
# Run on-device (via KUAL or kterm/SSH) and send the output back.

echo "=== framebuffer ==="
for fb in /dev/fb*; do
    n=$(basename "$fb")
    echo "--- $fb"
    for f in virtual_size stride bits_per_pixel rotate; do
        p="/sys/class/graphics/$n/$f"
        [ -f "$p" ] && echo "  $f: $(cat "$p")"
    done
done

echo "=== input devices ==="
cat /proc/bus/input/devices 2>/dev/null | grep -A4 '^N: ' | sed 's/^/  /'

echo "=== raw sample from each touchscreen (3s) ==="
for dev in /dev/input/event*; do
    name=$(grep -B2 "$(basename "$dev")" /proc/bus/input/devices 2>/dev/null | grep '^N:' | head -1)
    echo "--- $dev  ($name)"
    timeout 3 hexdump -C "$dev" 2>/dev/null | head -8
done

echo "=== python3 ==="
command -v python3 && python3 --version
ls /mnt/us/python3/bin/python3 /mnt/us/system/python3/bin/python3 2>/dev/null

echo "=== curl/wget ==="
command -v curl; command -v wget

echo "=== fbink binary (optional) ==="
ls /mnt/us/koreader/fbink /mnt/us/system/bin/fbink /mnt/us/extensions/*/fbink 2>/dev/null
find /mnt/us -maxdepth 3 -name "fbink*" -type f 2>/dev/null | head -5

echo "=== wifi state ==="
[ -x /usr/bin/lipc-hash ] && lipc-get-prop com.lab126.wifid cm_state 2>/dev/null
echo "ip: $(ifconfig wlan0 2>/dev/null | grep 'inet addr' | sed 's/.*inet addr:\([0-9.]*\).*/\1/')"
