#!/bin/sh
# YB Reader KPM install hook. Runs from the package folder while unpacked,
# so relative paths point at the freshly extracted package files.
set -e

# 1. The binary
mkdir -p /mnt/us/extensions/reader/bin
cp ./bin/reader /mnt/us/extensions/reader/bin/reader
chmod +x /mnt/us/extensions/reader/bin/reader
cp ./bin/start.sh /mnt/us/extensions/reader/bin/start.sh
chmod +x /mnt/us/extensions/reader/bin/start.sh

# 2. The library scriptlet ("start like a book", like KOReader.sh)
cp ./scriptlets/YBReader.sh /mnt/us/documents/YBReader.sh
chmod +x /mnt/us/documents/YBReader.sh
