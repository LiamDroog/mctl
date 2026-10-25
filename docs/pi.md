# The Raspberry Pi / Commodore 1702 display

`mctl --pi` is a second front end for the same engine, made to be looked at rather than operated: big line
art, a few large numbers, and a cycle through the vehicles. It is for a small composite CRT such as a
Commodore 1702.

What it changes:

- **Low resolution.** 640x480 by default (`--size WxH` overrides), a 12 x 24 px cell, so 53 x 20 character
  cells. Text keeps one row and two columns clear of the edge, because a CRT hides the border (overscan).
- **No effects.** The `CRT` theme has scanlines, bloom, persistence and curvature all off, and keys `1`-`4`
  and `E` do nothing: the tube provides them. The theme is not in the `T` cycle and `--themes-dir` is ignored.
- **Cycling.** Each vehicle is shown for `--dwell` seconds (default 20), followed by one piece of gallery art
  (Soyuz, Falcon 9, Cygnus, the Shuttle, the Earth with an orbit, Voyager) for 60 % of that time.
- **Full screen, no cursor.** `--pi` implies `--fullscreen`.

Each vehicle scene shows its line art turning over a starfield and the Earth's limb (or a small distant Earth
at L2 for JWST and Roman), with three readouts down each side and two status lines under it: the next pass,
the current observation, the day's Hubble programs, or the docked state, whichever the vehicle has.

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
