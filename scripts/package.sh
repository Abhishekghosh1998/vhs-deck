#!/usr/bin/env bash
# Build a release tarball: dist/vhs-deck-<version>-linux-x86_64.tar.gz
set -euo pipefail
cd "$(dirname "$0")/.."
ver=$(grep -m1 '^version' native/vhs-deck/Cargo.toml | cut -d'"' -f2)
name="vhs-deck-$ver-linux-x86_64"
(cd native && cargo build --release -p vhs-deck --locked)
rm -rf "dist/$name"; mkdir -p "dist/$name"
cp native/target/release/vhs-deck "dist/$name/"
strip "dist/$name/vhs-deck"
cp LICENSE NOTICE.md README.md "dist/$name/"
tar -C dist -czf "dist/$name.tar.gz" "$name"
(cd dist && sha256sum "$name.tar.gz" > "$name.tar.gz.sha256")
echo "built dist/$name.tar.gz"
