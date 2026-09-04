#!/bin/sh
# Build companion/dist/yb-mirror.zip — the source distribution offered by
# the Kindle's receive page ("Companion (macOS)" card).
#
# Deliberately a *source* zip, not an app or DMG:
#   - ~1 MB, so it downloads fast from the Kindle over Wi-Fi;
#   - the user builds the menu-bar app on their own Mac (`uv sync` +
#     `mac/make-app.sh`), which means no quarantine attribute, no
#     Gatekeeper warnings, no signing/notarization at all.
# Never ships .venv, __pycache__, .DS_Store, debug/, or dist/ itself.
set -e
ROOT="$(cd "$(dirname "$0")" && pwd)"
OUT="$ROOT/dist/yb-mirror.zip"
STAGE="$ROOT/dist/stage"

cd "$ROOT"
rm -rf "$STAGE"
mkdir -p "$STAGE/yb-mirror"
cp -R README.md pyproject.toml uv.lock sync-ip.sh mac "$STAGE/yb-mirror/"
cp "$ROOT/mirror.conf.example" "$STAGE/yb-mirror/mirror.conf"

cd "$STAGE"
rm -f "$OUT"
zip -q -r -X "$OUT" yb-mirror \
    -x '*/__pycache__/*' -x '*.pyc' -x '*/.DS_Store' -x '*/debug/*' \
    -x '*/dist/*'
cd "$ROOT"
rm -rf "$STAGE"

ls -lh "$OUT"
echo "built $OUT — deploy carries it to the Kindle automatically"
