#!/bin/sh
# Run the introspection probe and save its output to the user store so it
# can be read back over USB.
DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$DIR"
./bin/probe > /mnt/us/probe.out 2>&1

