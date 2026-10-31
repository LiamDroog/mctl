#!/usr/bin/env bash
# Regenerate THIRD-PARTY-NOTICES.md: every library's licence text (cargo-about) plus the embedded assets.
# Needs `cargo install cargo-about --locked --features cli`. Run after changing dependencies.
set -euo pipefail
cd "$(dirname "$0")/.."
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT
cargo about generate --locked --fail about.hbs > "$tmp"
{
    cat "$tmp"
    cat <<'ASSETS'
## Embedded assets

| Asset | Source | Licence |
|---|---|---|
| Earth day texture: Blue Marble Next Generation with topography and bathymetry, 2004 (R. Stöckli) | NASA Earth Observatory, https://visibleearth.nasa.gov | NASA media usage guidelines: not subject to copyright in the United States for this use; no endorsement is implied |
| Earth night lights: Black Marble 2012 (Suomi NPP VIIRS) | NASA Earth Observatory / NOAA NGDC, https://visibleearth.nasa.gov | NASA media usage guidelines, as above |
| Coastlines: Natural Earth 110m and 50m | https://www.naturalearthdata.com | Public domain |
| Font: Unscii-16 | http://viznut.fi/unscii/ | Public domain, per its author |
| Font: JetBrains Mono | https://github.com/JetBrains/JetBrainsMono | SIL Open Font License 1.1 (below) |

The same information, with how each asset was checked, is in `assets/LICENSES.md`.

### SIL Open Font License 1.1 (JetBrains Mono)

```text
ASSETS
    cat assets/fonts/OFL-JetBrainsMono.txt
    printf '```\n'
} > THIRD-PARTY-NOTICES.md
echo "wrote THIRD-PARTY-NOTICES.md ($(wc -c < THIRD-PARTY-NOTICES.md) bytes)"
