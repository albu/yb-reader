#!/bin/sh
DIR="$(cd "$(dirname "$0")" && pwd)"
sh "$DIR/../../probe.sh" > "$DIR/probe.out" 2>&1
echo "probe written to $DIR/probe.out" >> "$DIR/mirror.log"
