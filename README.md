# mctl

Mission control display for the ISS, Hubble, JWST and Roman, styled after a flight control console. Design and
requirements: [`DESIGN.md`](DESIGN.md). Decisions: [`docs/adr/`](docs/adr/). Status: M0-M4 done (core and sources, MASTER/TRAJ with store-backed trends, live telemetry, C&W,
recording, observer sites and passes), plus JWST and Roman at Sun-Earth L2 from Horizons, STScI observing
schedules (OPS page) and telemetry visuals; next is M5. See "What is real" below.

## Running

One-time install of the launcher (symlinks into `~/.local/bin`, which must be on `PATH`):

```
ln -sf "$PWD/scripts/mctl-launch" ~/.local/bin/mctl
ln -sf "$PWD/scripts/mctl-launch" ~/.local/bin/mctl-gfx
```

After that, from any directory:

```
mctl-gfx                         # the console (same as `mctl`); rebuilds first if any source changed
mctl-gfx --vehicle jwst --theme retrowave --fullscreen   # iss | hubble | jwst | roman (or V in the console)
mctl-gfx --offline               # cached data only, no network
mctl check                       # validate config, fetch every source once, report
mctl shot --out-dir /tmp/shots --pages all   # headless PNGs, one per theme and page (page or page:sub)
mctl --vehicle jwst shot --out-dir /tmp/shots --pages master,ops --settle-s 60   # --settle-s lets history charts fill
mctl isslive --duration-s 30     # print live ISS telemetry (the GNC/EPS/TCS/COMM pages show the same feed)
mctl export telemetry --from 2026-10-23T00:00:00 --out t.csv   # state|telemetry|elements|events|cw_log
mctl export --out ./dump         # every table, one CSV each
tools/smoke.sh                   # render every page headless and check the images
mctl --no-record                 # do not record this run
```

The launcher runs `cargo build --release` (a no-op in ~0.3 s when nothing changed) and refuses to run a
stale binary if the build fails. `MCTL_NO_BUILD=1` skips the build.

Keys (all single-character keys are remappable under `[keys]` in the config): `F1`-`F11` pages (clickable in the footer) (MASTER TRAJ GNC EPS TCS/ECLSS COMM PASS PROX VIEW SYS OPS), `Tab`/`Shift-Tab` sub-pages,
`T` theme, `V` vehicle, `Space` pause/resume the simulation clock, `[` `]` halve/double the warp (up to
10000x either way), `0` back to LIVE, `A` acknowledge the master caution/warning, `:` command line
(`warp`, `seek`, `step`, `page`, `theme`, `vehicle`, `fx`, `ack`, ...; `Tab` completes), `?` help.
`W` cycles the trends window on TRAJ elements and trends. Orbit map only: drag with the left button inside the map to rotate, wheel to zoom there; arrows and
`+`/`-` also work. `X` GCRF/ITRF, `Z` altitude exaggeration, `F` inertial/Earth-fixed, `C` focus,
`1`-`4` effects, `E` all effects, `Q` quit.

## Configuration and data

Defaults are embedded (`config/default.toml`); a file at `~/.config/mctl/config.toml` overrides any key,
and invalid values fail loudly. Downloaded data is cached in `~/.cache/mctl`, logs go to
`~/.local/state/mctl/mctl.log`, and the recording is `~/.local/share/mctl/mctl.db` (SQLite, WAL;
retention 30 days / 2 GB, set under `[store]`). C&W rules are `[[rule]]` entries; the ISS subsystem
ones are illustrative, not flight rules. Only real time is recorded, never SIM states.

## What is real

Every number on the MASTER page comes from a source: CelesTrak GP (SGP4) and the NASA ISS OEM through an
arbiter, IERS EOP for UT1/polar motion, and an analytic Sun. A quantity with no source reads `NO DATA`.
JWST and Roman come from JPL Horizons (Goddard flight-dynamics trajectories, geocentric ICRF); Hubble uses
CelesTrak GP with Horizons as a second opinion (Horizons' Hubble table is itself built from TLEs, so it is a
cross-check, not a better source). The OPS page shows the JWST weekly visit plan and the Hubble daily
programme list from STScI; both are plans, not engineering telemetry, which neither observatory publishes.
ISS telemetry (137 ISSLIVE items) feeds GNC/EPS/TCS/COMM, with recent-history sparklines and charts; units the live values disprove are corrected
and documented in `docs/step3/attitude-verification.md`. Panels for later milestones say which milestone
supplies them. Passes are predicted for each observer site (OEM then GP/SGP4); pass times match an independent
implementation to milliseconds (`docs/step4/pass-verification.md`) but have not been compared with a
published predictor. SUP-GP (ISS-E) is implemented but off by default (`docs/step4/supgp-evaluation.md`). The observer site is the configured one, else GeoClue, else (optional, off by
default) an IP lookup, else the NASA HQ fallback; `:site save` keeps one. Not yet: planner and replay
(M5), visiting vehicles and proximity operations (M6).

## Layout

`crates/`: `mctl-core` (time, state, config, snapshot, bus, arbiter), `mctl-astro` (SGP4, OEM, frames,
Sun, elements), `mctl-sources` (HTTP cache, supervised fetch threads, ISSLIVE client), `mctl-sim` (the
50 Hz engine), `mctl-gui` (renderer and pages). `spikes/` holds the throwaway M0 experiments.
