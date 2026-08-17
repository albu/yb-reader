#!/bin/sh
# YB Reader KPM uninstall hook. KPM removes the package files themselves;
# we only clean up what install.sh placed outside the package folder.
rm -f /mnt/us/documents/YBReader.sh

