#!/bin/bash
# Builds dist/Cumulus-x86_64.AppImage. Run on the OLDEST Linux you want to
# support (releases are built on Ubuntu 22.04, glibc 2.35), with the build
# dependencies installed (see README → Build).
#
# The AppImage holds only Cumulus itself; GTK 3 and WebKitGTK 4.1 come from
# the user's system (AppRun checks for them). Bundling those breaks: the
# bundled GLib can't load the system's newer GIO modules (no HTTPS) and
# WebKit's helper processes aren't relocatable.
set -euo pipefail
cd "$(dirname "$0")/../.."

# appimagetool checks the .desktop file with this (package: desktop-file-utils).
command -v desktop-file-validate >/dev/null || { echo "Install desktop-file-utils first." >&2; exit 1; }

TOOL_VERSION=1.9.0
TOOL="$HOME/.cache/cumulus/appimagetool-$TOOL_VERSION-x86_64.AppImage"
if [ ! -x "$TOOL" ]; then
    mkdir -p "$(dirname "$TOOL")"
    curl -fsSL -o "$TOOL" "https://github.com/AppImage/appimagetool/releases/download/$TOOL_VERSION/appimagetool-x86_64.AppImage"
    chmod +x "$TOOL"
fi

# Public build: no local update path, user folder kept out of the binary.
CUMULUS_PUBLIC_BUILD=1 CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=$HOME=~" cargo build --release

APPDIR=$(mktemp -d)/Cumulus.AppDir
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/share/icons/hicolor/256x256/apps" "$APPDIR/usr/share/applications"
cp target/release/cumulus "$APPDIR/usr/bin/cumulus"
cp packaging/linux/AppRun "$APPDIR/AppRun"
chmod +x "$APPDIR/AppRun" "$APPDIR/usr/bin/cumulus"
cp packaging/linux/cumulus.desktop "$APPDIR/cumulus.desktop"
cp packaging/linux/cumulus.desktop "$APPDIR/usr/share/applications/"
cp assets/icon.png "$APPDIR/cumulus.png"
cp assets/icon.png "$APPDIR/usr/share/icons/hicolor/256x256/apps/cumulus.png"
ln -s cumulus.png "$APPDIR/.DirIcon"

mkdir -p dist
# WSL and some containers have no FUSE; let the tool unpack itself.
APPIMAGE_EXTRACT_AND_RUN=1 ARCH=x86_64 "$TOOL" --no-appstream "$APPDIR" dist/Cumulus-x86_64.AppImage
ls -la dist/Cumulus-x86_64.AppImage
