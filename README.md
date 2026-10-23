# mctl

Mission control display for the ISS (and Hubble), styled after a flight control console. Design and
requirements: [`DESIGN.md`](DESIGN.md). Decisions: [`docs/adr/`](docs/adr/). Status: M1 + MASTER page
(step 1 of the action plan); see "What is real" below.

## Running

One-time install of the launcher (symlinks into `~/.local/bin`, which must be on `PATH`):

```
ln -sf "$PWD/scripts/mctl-launch" ~/.local/bin/mctl
ln -sf "$PWD/scripts/mctl-launch" ~/.local/bin/mctl-gfx
```

After that, from any directory:

```
mctl-gfx                         # the console (same as `mctl`); rebuilds first if any source changed
mctl-gfx --vehicle hubble --theme retrowave --fullscreen
mctl-gfx --offline               # cached data only, no network
mctl check                       # validate config, fetch every source once, report
mctl shot --out-dir /tmp/shots   # headless PNGs of the live page, one per theme
mctl isslive --duration-s 30     # print live ISS telemetry (UI pages arrive in M3)
```

The launcher runs `cargo build --release` (a no-op in ~0.3 s when nothing changed) and refuses to run a
stale binary if the build fails. `MCTL_NO_BUILD=1` skips the build.

Keys: `T` theme, `V` vehicle, `X` GCRF/ITRF, arrows or mouse drag orbit the camera, `+`/`-` or wheel
zoom, `Z` altitude exaggeration (x1/2/5/10), `F` inertial/Earth-fixed camera, `C` focus Earth/vehicle,
`1`-`4` scanlines/bloom/persistence/CRT, `E` all effects, `Q` quit.

## Configuration and data

Defaults are embedded (`config/default.toml`); a file at `~/.config/mctl/config.toml` overrides any key,
and invalid values fail loudly. Downloaded data is cached in `~/.cache/mctl`, logs go to
`~/.local/state/mctl/mctl.log`.

## What is real

Every number on the MASTER page comes from a source: CelesTrak GP (SGP4) and the NASA ISS OEM through an
arbiter, IERS EOP for UT1/polar motion, and an analytic Sun. A quantity with no source reads `NO DATA`.
Panels for later milestones say which milestone supplies them. See the step-1 report for the full list.

## Layout

`crates/`: `mctl-core` (time, state, config, snapshot, bus, arbiter), `mctl-astro` (SGP4, OEM, frames,
Sun, elements), `mctl-sources` (HTTP cache, supervised fetch threads, ISSLIVE client), `mctl-sim` (the
50 Hz engine), `mctl-gui` (renderer and pages). `spikes/` holds the throwaway M0 experiments.
