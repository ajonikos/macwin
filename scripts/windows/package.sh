#!/usr/bin/env bash
# Builds target/macwin-<version>-windows-x64.zip: macwin.exe (command line),
# macwin-tray.exe (notification area), install.cmd / install.ps1,
# uninstall.ps1, README.txt and the license notices.
#
# From a Mac or Linux this cross-builds with mingw-w64 (brew install
# mingw-w64); on Windows set MACWIN_WINDOWS_TARGET=x86_64-pc-windows-msvc.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
TARGET="${MACWIN_WINDOWS_TARGET:-x86_64-pc-windows-gnu}"

# rust-toolchain.toml pins 1.96.0; use `stable` when it is that version and
# the pinned toolchain is not installed (never auto-install).
if [[ -z "${RUSTUP_TOOLCHAIN:-}" ]] && command -v rustup >/dev/null; then
    if ! rustup toolchain list | grep -q '^1\.96\.0'; then
        if rustup run stable rustc --version 2>/dev/null | grep -q '^rustc 1\.96\.0 '; then
            export RUSTUP_TOOLCHAIN=stable
        fi
    fi
fi

VERSION="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)"
cargo build --release --locked -p macwin-agent --target "$TARGET"

NAME="macwin-$VERSION-windows-x64"
ZIP="$ROOT/target/$NAME.zip"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
mkdir "$STAGE/$NAME"
cp "target/$TARGET/release/macwin.exe" "target/$TARGET/release/macwin-tray.exe" \
    scripts/windows/install.cmd scripts/windows/install.ps1 scripts/windows/uninstall.ps1 \
    scripts/windows/README.txt THIRD_PARTY_NOTICES "$STAGE/$NAME/"
rm -f "$ZIP"
if command -v zip >/dev/null; then
    (cd "$STAGE" && zip -qr "$ZIP" "$NAME")
else
    (cd "$STAGE" && python3 -m zipfile -c "$ZIP" "$NAME")
fi
echo "built $ZIP"
