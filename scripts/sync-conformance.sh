#!/bin/sh
# Copy the conformance cases from a checkout of Jargon (the Crystal
# implementation, which records them with `just conformance`) into
# tests/conformance, noting the checkout's commit in tests/conformance/COMMIT.
#
#   scripts/sync-conformance.sh path/to/jargon
set -eu
src=${1:?usage: scripts/sync-conformance.sh path/to/jargon}
dest="$(cd "$(dirname "$0")/.." && pwd)/tests/conformance"
rm -rf "$dest"
mkdir -p "$dest"
cp "$src"/conformance/*.json "$dest"/
git -C "$src" log -1 --format="%H %cs %s" > "$dest/COMMIT"
[ -z "$(git -C "$src" status --porcelain -- conformance)" ] || echo "(cases from the working tree, not yet committed)" >> "$dest/COMMIT"
echo "synced: $(head -1 "$dest/COMMIT")"
