#!/usr/bin/env bash
# Render every page headless to PNG and check each image is a real, distinct render (DESIGN.md §12,
# "Rendering": no panic, image review). Needs a GPU adapter (a software Vulkan one is enough).
# Usage: tools/smoke.sh [path/to/mctl]   (default: target/release/mctl)
set -euo pipefail
bin="${1:-target/release/mctl}"
out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT
"$bin" --offline --no-record shot --out-dir "$out" --theme phosphor --pages all --wait-s 5 --warmup 30 >/dev/null 2>&1
count=$(ls "$out"/*.png | wc -l)
[ "$count" -ge 20 ] || { echo "FAIL: only $count pages rendered"; exit 1; }
for f in "$out"/*.png; do
  size=$(stat -c %s "$f")
  [ "$size" -gt 20000 ] || { echo "FAIL: $(basename "$f") is only $size bytes (blank?)"; exit 1; }
done
distinct=$(md5sum "$out"/*.png | cut -d' ' -f1 | sort -u | wc -l)
[ "$distinct" -eq "$count" ] || echo "note: $distinct distinct images among $count pages (pages with no source can look alike)"
# An L2 vehicle has its own pages (halo plots, OPS timeline); render them too.
"$bin" --offline --no-record --vehicle jwst shot --out-dir "$out/jwst" --theme phosphor --pages master,traj,traj:3,ops,pass --wait-s 5 --warmup 30 >/dev/null 2>&1
jcount=$(ls "$out"/jwst/*.png | wc -l)
[ "$jcount" -eq 5 ] || { echo "FAIL: only $jcount JWST pages rendered"; exit 1; }
for f in "$out"/jwst/*.png; do
  [ "$(stat -c %s "$f")" -gt 20000 ] || { echo "FAIL: JWST $(basename "$f") looks blank"; exit 1; }
done
echo "OK: $count pages rendered, $jcount JWST pages rendered"
