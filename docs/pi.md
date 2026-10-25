# The Raspberry Pi / Commodore 1702 display

`mctl --pi` is a second front end for the same engine, made to be looked at rather than operated: big line
art, a few large numbers, and a cycle through the vehicles. It is for a small composite CRT such as a
Commodore 1702.

What it changes:

- **Low resolution.** 640x480 by default (`--size WxH` overrides), a 12 x 24 px cell, so 53 x 20 character
  cells. Text keeps one row and two columns clear of the edge, because a CRT hides the border (overscan).
- **No effects.** Every theme has scanlines, bloom, persistence and curvature off, and keys `1`-`4` and `E` do
  nothing: the tube provides them. `--themes-dir` is ignored.
- **Themes.** `T` cycles CRT (cyan, the default), Phosphor (green), Retro (amber) and Retrowave; `--theme NAME`
  starts on one. They are the console's themes with the display's font and no effects.
- **Cycling.** Each vehicle gets a run of scenes of `--dwell` seconds each (default 20; console pages stay up
  1.5 times as long), and one piece of gallery art (Falcon 9, the Shuttle, the Earth with an orbit) follows
  each vehicle's art scene for 60 % of that time.
- **Full screen, no cursor.** `--pi` implies `--fullscreen`, unless you give `--size` or `--windowed`, which
  keep it in a window of that size (the cursor is still hidden over it).

`--pi-style` picks the scenes: `mixed` (default), `pretty` (pictures only) or `dense` (console pages only).

A vehicle's scenes are for whichever of these it has data for. **Pictures** use the console's pixel panels
in big frames at 53 x 20 cells:

| Scene | From the console |
|---|---|
| line art | SYS / VEHICLE: turning over a starfield and the Earth's limb, three readouts a side, two status lines |
| ORBIT MAP | TRAJ orbit map: the 3D Earth with city lights, turning by itself; HALO ORBIT for JWST and Roman |
| GROUND TRACK | TRAJ ground track |
| SKY PLOT | PASS sky plot |
| R-BAR / V-BAR | PROX, when a visitor is not docked |
| pictures | OPS survey pictures of the current and next target, when they have arrived |

**Console pages** are the console's own pages in the small 8 x 16 font, 80 x 30 cells (76 x 28 inside the
overscan margin), so they are mission-control dense:

| Scene | From the console |
|---|---|
| MASTER | the map, ground track, state vector, event timeline and eclipse ring (the halo-orbit MASTER at L2) |
| ORBIT MAP | TRAJ sub-page 1: the map with state vector, elements and sources |
| PASS | the sky plot and the next pass |
| PROX | R-bar/V-bar and the relative state |
| OPS | timeline, visit table, pictures; the Hubble report |
| EVENT LOG | SYS event log, for a vehicle with telemetry (the ISS) |

Docked Dragons show only their art, since their maps would repeat the station's. The telemetry pages (GNC,
EPS, TCS/ECLSS, COMM) and the tables are not reproduced: they are no use to look at. Whether the 8 x 16 text
is legible on a 1702 is untested; if it is not, use `--pi-style pretty`.

Keys: `V` or Right next scene, Left previous, Space hold or resume the cycle, `Q` or Esc quit.

## Setting up a Pi

Not verified on a Pi: this was built and looked at on a desktop with `mctl --pi shot`. Treat the following as
a starting point.

1. A Pi 4 or 5 on Raspberry Pi OS (64-bit) with the Vulkan driver (`mesa-vulkan-drivers`, which provides `v3dv`).
   wgpu needs Vulkan; a Pi 3 or earlier has none.
2. Build on the Pi (`cargo build --release -p mctl`); there is no cross-compile setup. The default build needs
   only the GPU stack and libc.
3. Composite output. In `/boot/firmware/config.txt` enable the composite connector and pick the standard,
   for example `dtoverlay=vc4-kms-v3d,composite` with `video=Composite-1:720x480@60ie` on the kernel command
   line (NTSC; use `720x576@50ie` for PAL), and `enable_tvout=1` on a Pi 4. The 1702 is a 4:3 tube, so 640x480
   keeps square pixels. If the picture does not fill the tube, change `--size`, not the theme.
4. A compositor is needed for the window: `cage` (a single-application Wayland kiosk compositor) is the
   simplest: `cage -- mctl --pi`.

The network is the only other need: with no network, `mctl --pi --offline` runs from the cache.
