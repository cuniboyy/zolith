#!/bin/sh
# Installs Zolith for the current user (no root needed): the program, its icon and a
# launcher entry. Run it from the folder that contains `zolith`, or from a source checkout
# after `cargo build --release`.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
bin=$here/zolith
[ -x "$bin" ] || bin=$here/target/release/zolith
[ -x "$bin" ] || { echo "zolith binary not found: build it first with 'cargo build --release'" >&2; exit 1; }
icon=$here/zolith.png
[ -f "$icon" ] || icon=$here/assets/zolith.png
desktop=$here/zolith.desktop
[ -f "$desktop" ] || desktop=$here/packaging/zolith.desktop

prefix=${PREFIX:-$HOME/.local}
install -Dm755 "$bin" "$prefix/bin/zolith"
install -Dm644 "$icon" "$prefix/share/icons/hicolor/512x512/apps/zolith.png"
mkdir -p "$prefix/share/applications"
sed "s|@EXEC@|$prefix/bin/zolith|" "$desktop" > "$prefix/share/applications/zolith.desktop"
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$prefix/share/applications" 2>/dev/null || true

echo "Installed Zolith to $prefix/bin/zolith"
echo "Find it in your app launcher, or run: zolith"
