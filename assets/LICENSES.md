# Embedded asset licenses

Every asset embedded in the `mctl` binary, with its source and licence. The project itself is
GPL-3.0-or-later; none of the licences below conflict with that. Verified 2026-10-23.

| Asset | File | Source | Licence | Embeddable in a distributed binary? |
|---|---|---|---|---|
| JetBrains Mono Regular v2.x (crisp monospace; Modern theme) | `fonts/JetBrainsMono-Regular.ttf` | https://github.com/JetBrains/JetBrainsMono (`fonts/ttf/`, master as of 2026-10-23) | SIL OFL 1.1: `fonts/OFL-JetBrainsMono.txt`, https://openfontlicense.org | Yes. OFL permits bundling/embedding with software. Obligation: ship the copyright notice and licence text with the distribution. The font must not be sold on its own, and modified versions can't use the reserved name. We do not modify it. |
| Unscii-16 (CRT-style 8×16 bitmap look; retro themes) | `fonts/unscii-16.ttf` | http://viznut.fi/unscii/ (`unscii-16.ttf`) | Public domain, per the author (Viznut): "`unscii-16-full` falls under GPL because of how Unifont is licensed; the other variants are in the Public Domain." Quote taken from the page above. | Yes. **Use only `unscii-16`, never `unscii-16-full` (GPL).** No formal licence text exists; the licence is the author's statement. This is weaker than a written licence, so the statement is recorded here. |
| Blue Marble Next Generation, land + bathymetry + shaded topography, resampled from 5400×2700 to 4096×2048, Dec 2004 (Earth texture) | `earth/blue_marble_4096x2048.jpg` | https://eoimages.gsfc.nasa.gov/images/imagerecords/73000/73909/world.topo.bathy.200412.3x5400x2700.jpg (Visible Earth image 73909). Credit: NASA Earth Observatory / Reto Stöckli | NASA Media Usage Guidelines, https://www.nasa.gov/nasa-brand-center/images-and-media/ : "NASA content ... used in the rendition of 3-dimensional models, such as texture maps ... generally are not subject to copyright in the United States." | Yes, with the same caveats as below: a usage policy, not a formal licence; no implied endorsement, no NASA insignia. The image has shaded relief baked into the colours. 4096 px wide is the largest size every target GPU (including the Raspberry Pi 4/5) is guaranteed to take. |
| Black Marble 2012 (city lights at night), resampled from 3600×1800 to 2048×1024 (night side of the Earth) | `earth/black_marble_2048x1024.jpg` | https://eoimages.gsfc.nasa.gov/images/imagerecords/79000/79765/dnb_land_ocean_ice.2012.3600x1800.jpg (Visible Earth image 79765). Credit: NASA Earth Observatory / NOAA NGDC (Suomi NPP VIIRS) | NASA Media Usage Guidelines, as above. | Yes, with the same caveats. |
| Blue Marble Next Generation 2048×1024 (Jan 2004), used only by the M0 spike `spikes/gfx`, not by `mctl` | `earth/blue_marble_2048x1024.jpg` | https://eoimages.gsfc.nasa.gov/images/imagerecords/57000/57752/land_shallow_topo_2048.jpg (Visible Earth image 57752). Credit: NASA Earth Observatory / Reto Stöckli | NASA Media Usage Guidelines, as above. | Same caveats as the rows above. |
| Natural Earth 110m and 50m coastline v4.1.0 (shapefiles converted to `.bin` by `tools/shp_to_coast.py`) | `coastlines/ne_110m_coastline.bin`, `coastlines/ne_50m_coastline.bin` | https://naciscdn.org/naturalearth/110m/physical/ne_110m_coastline.zip and `.../50m/physical/ne_50m_coastline.zip`; terms: https://www.naturalearthdata.com/about/terms-of-use/ | Public domain: "You may use the maps in any manner, including modifying the content and design, electronic dissemination, and offset printing ... No permission is needed to use Natural Earth. Crediting the authors is unnecessary." | Yes. |

## Rejected

- **VT323** (OFL 1.1): licence is fine, but the font has **no box-drawing or block glyphs** (0 of 128 + 0 of 32 checked with fontTools), so it can't draw panel borders.
- **Unscii-16-full**: GPL via Unifont. Excluded by the licence constraint.

## Glyph coverage (checked with fontTools)

| Font | Box drawing U+2500–257F | Block elements U+2580–259F | Advance |
|---|---|---|---|
| JetBrains Mono | 128/128 | 32/32 | 600/1000 em |
| Unscii-16 | 128/128 | 32/32 | 32/64 em (8 px at 16 px) |
| VT323 | 0/128 | 0/32 | 400/1000 em |

## Dependency fonts

egui's `default-fonts` feature embeds additional fonts (Ubuntu-Light, Hack, NotoEmoji, emoji-icon-font) with their own licences.
The spikes build egui **without** that feature so that only the fonts above are in the binary.

## Embedded size

Measured 2026-10-23 on the `gfx` spike (release build, `strip`ped), same code built with and without the assets:

| | Bytes |
|---|---|
| Binary with assets | 17,357,856 |
| Binary without assets | 15,922,768 |
| **Delta** | **1,435,088 (≈ 1.4 MB)** |
| Raw asset bytes | 1,320,184 |

Raw breakdown: JetBrains Mono 270,224; Unscii-16 280,672; Blue Marble 2048×1024 JPEG 238,676; Natural Earth 110m `.bin` 41,564; 50m `.bin` 489,048.
Assets are embedded uncompressed with `include_bytes!`. The 15.9 MB baseline (wgpu, naga, egui, image) dominates; the assets are about 8% of the binary.
The spike embeds both coastline resolutions (110m for the ground-track panel, 50m for the globe mask). Using 110m everywhere would save 489 KB.
