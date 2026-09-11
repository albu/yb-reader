#!/bin/bash
# Build (or rebuild) the yb-mirror menu-bar app in ~/Applications.
# The launcher is compiled from mac/launcher.c — a script executable in
# CFBundleExecutable gets refused by this macOS's LaunchServices (-10669)
# even ad-hoc signed; a Mach-O launcher opens fine. It only cd's to the
# repo and execs `uv run mac/menubar.py`, so everything runs in the
# project's venv — never system python.
set -e
REPO="$(cd "$(dirname "$0")/.." && pwd)"
APP="$HOME/Applications/yb-mirror.app"

# First build only: fetch the headless browser (~150 MB, cached in
# ~/Library/Caches/ms-playwright — `uv sync` installs the playwright wheel
# but never downloads browsers). Idempotent: no-ops when already present.
cd "$REPO"
uv run playwright install chromium

mkdir -p "$APP/Contents/MacOS"
clang -DREPO_PATH="\"$REPO\"" -o "$APP/Contents/MacOS/yb-mirror" \
    "$REPO/mac/launcher.c"

# Info.plist must exist (and be final) BEFORE codesign: the signature
# seals the bundle, and a plist written after signing invalidates it.
cat > "$APP/Contents/Info.plist" <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>              <string>yb-mirror</string>
    <key>CFBundleDisplayName</key>       <string>yb-mirror</string>
    <key>CFBundleIdentifier</key>        <string>local.yb-mirror</string>
    <key>CFBundleExecutable</key>        <string>yb-mirror</string>
    <key>CFBundlePackageType</key>       <string>APPL</string>
    <key>CFBundleShortVersionString</key><string>0.1.0</string>
    <key>LSUIElement</key>               <true/>
</dict>
</plist>
EOF

# Ad-hoc signing is the floor macOS wants for bundles. Signature stability
# used to matter because privacy grants keyed to the code hash; the headless
# server needs no grants, so there is nothing to preserve across rebuilds.
codesign --force -s - "$APP" >/dev/null 2>&1 || true

echo "built $APP (baked to $REPO — rebuild via mac/make-app.sh if the repo moves)"
