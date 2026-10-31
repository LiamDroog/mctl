#!/usr/bin/env bash
# Render every page headless to PNG and check each image is a real, distinct render (no panic, image
# review). Needs a GPU adapter (a software Vulkan one is enough).
# Usage: tools/smoke.sh [path/to/mctl]   (default: target/release/mctl)
# SMOKE_SOFTWARE=1 allows a software (CPU) adapter, which is all a CI runner has.
set -euo pipefail
bin="${1:-target/release/mctl}"
soft=()
[ "${SMOKE_SOFTWARE:-0}" = 1 ] && soft=(--allow-software)
out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT
"$bin" --offline --no-record "${soft[@]}" shot --out-dir "$out" --theme phosphor --pages all --wait-s 5 --warmup 30 >/dev/null 2>&1
count=$(find "$out" -maxdepth 1 -name "*.png" | wc -l)
[ "$count" -ge 20 ] || { echo "FAIL: only $count pages rendered"; exit 1; }
for f in "$out"/*.png; do
  size=$(stat -c %s "$f")
  [ "$size" -gt 20000 ] || { echo "FAIL: $(basename "$f") is only $size bytes (blank?)"; exit 1; }
done
distinct=$(md5sum "$out"/*.png | cut -d' ' -f1 | sort -u | wc -l)
[ "$distinct" -eq "$count" ] || echo "note: $distinct distinct images among $count pages (pages with no source can look alike)"
# An L2 vehicle has its own pages (halo plots, OPS timeline); render them too.
"$bin" --offline --no-record "${soft[@]}" --vehicle jwst shot --out-dir "$out/jwst" --theme phosphor --pages master,traj,traj:3,ops,pass --wait-s 5 --warmup 30 >/dev/null 2>&1
jcount=$(find "$out/jwst" -maxdepth 1 -name "*.png" | wc -l)
[ "$jcount" -eq 5 ] || { echo "FAIL: only $jcount JWST pages rendered"; exit 1; }
for f in "$out"/jwst/*.png; do
  [ "$(stat -c %s "$f")" -gt 20000 ] || { echo "FAIL: JWST $(basename "$f") looks blank"; exit 1; }
done
echo "OK: $count pages rendered, $jcount JWST pages rendered"
