#!/usr/bin/env bash
# Install VHS Deck for the current user: binary, icons and application-menu entry.
# Run from the unpacked release folder (or the repo root after `cargo build --release`).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
bin="$here/vhs-deck"; [ -x "$bin" ] || bin="$here/../native/target/release/vhs-deck"
assets="$here/assets"; [ -d "$assets" ] || assets="$here/../assets"
[ -x "$bin" ] || { echo "vhs-deck binary not found (build it first)"; exit 1; }
prefix="${PREFIX:-$HOME/.local}"
install -Dm755 "$bin" "$prefix/bin/vhs-deck"
install -Dm644 "$assets/icon.png"     "$prefix/share/icons/hicolor/256x256/apps/vhs-deck.png"
install -Dm644 "$assets/icon-128.png" "$prefix/share/icons/hicolor/128x128/apps/vhs-deck.png"
install -Dm644 "$assets/icon.svg"     "$prefix/share/icons/hicolor/scalable/apps/vhs-deck.svg"
tmp="$(mktemp)"; sed "s|^Exec=.*|Exec=$prefix/bin/vhs-deck %f|" "$assets/vhs-deck.desktop" > "$tmp"
install -Dm644 "$tmp" "$prefix/share/applications/vhs-deck.desktop"; rm -f "$tmp"
command -v update-desktop-database >/dev/null && update-desktop-database "$prefix/share/applications" || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -t "$prefix/share/icons/hicolor" || true
echo "Installed to $prefix. Find \"VHS Deck\" in your application menu, or run: $prefix/bin/vhs-deck"
