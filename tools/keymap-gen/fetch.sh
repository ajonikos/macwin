#!/usr/bin/env bash
# One-time fetch of the Chromium keycode table used by keymap-gen, plus the
# Chromium LICENSE it is distributed under. Run manually by a maintainer, never
# by the build; the results in data/ are checked in. macOS tools (base64 -D,
# shasum).
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
data="$here/data"
base="https://chromium.googlesource.com/chromium/src/+/main"
inc_url="$base/ui/events/keycodes/dom/dom_code_data.inc?format=TEXT"
license_url="$base/LICENSE?format=TEXT"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# gitiles serves raw file contents base64-encoded with ?format=TEXT.
curl -fsSL "$inc_url" | base64 -D > "$tmp/dom_code_data.inc"
curl -fsSL "$license_url" | base64 -D > "$tmp/LICENSE.chromium"

mkdir -p "$data"
mv "$tmp/dom_code_data.inc" "$data/dom_code_data.inc"
mv "$tmp/LICENSE.chromium" "$data/LICENSE.chromium"

inc_sha="$(shasum -a 256 "$data/dom_code_data.inc" | cut -d ' ' -f 1)"
license_sha="$(shasum -a 256 "$data/LICENSE.chromium" | cut -d ' ' -f 1)"

cat > "$data/SOURCE.txt" <<EOF
Fetched by tools/keymap-gen/fetch.sh on $(date -u +%Y-%m-%dT%H:%M:%SZ) (UTC).
Both files are stored unmodified (base64-decoded gitiles ?format=TEXT output).

dom_code_data.inc
  url:    $inc_url
  sha256: $inc_sha

LICENSE.chromium
  url:    $license_url
  sha256: $license_sha
EOF

echo "dom_code_data.inc sha256 $inc_sha"
echo "LICENSE.chromium  sha256 $license_sha"
