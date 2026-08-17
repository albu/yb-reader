#!/bin/sh
# KPM launch hook: started via /var/local/kmc/bin/kpm launch yb-reader.
# The framework pause/restore lives in start.sh, shared with the scriptlet.
exec /mnt/us/extensions/reader/bin/start.sh

