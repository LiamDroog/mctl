# Architecture

mctl is a Cargo workspace. The rule that shapes it: **the user interface never touches a data source or a
propagator.** Sources produce events, one engine thread turns them into an immutable snapshot, and the GUI
only reads that snapshot and sends commands back.

```
 source threads                      engine thread (50 Hz; 5 on the kiosk)              GUI (winit + wgpu + egui)
 ──────────────                      ─────────────────────              ─────────────────────────
 CelesTrak GP / NASA OEM   ─┐
 IERS earth orientation     │ Event   ┌──────────────────┐  World      ┌──────────────────────┐
 JPL Horizons               ├───────▶ │ arbiter, clock,  │ ──────────▶ │ pages, widgets, globe │
 ISSLIVE telemetry          │ (mpsc)  │ propagation, C&W │ (arc-swap)  │ read-only             │
 STScI schedules, pictures  │         │ passes, planner  │             └──────────┬───────────┘
 observer location         ─┘         └────────▲─────────┘                        │ Command
                                               └──────────────────────────────────┘ (mpsc)
```

## Crates

| Crate | Responsibility |
|---|---|
| `mctl-core` | The vocabulary shared by everything: UTC time and earth-orientation data, states, configuration, the `World` snapshot and its bus, the source arbiter, caution-and-warning rules, commands, plans. No UI, no network, no async runtime, so it is unit-testable alone. |
| `mctl-astro` | Pure astrodynamics: SGP4 from OMM, NASA OEM interpolation, TEME/GCRF/ITRF and the local frames (RTN, VNB, LVLH), Sun and Moon, osculating elements, orbit events by root finding, observer geometry and pass prediction, the Sun-Earth L2 frame, Horizons parsing, and the numerical propagator (DOP853 with J2-J4, third bodies, drag and radiation pressure). |
| `mctl-sources` | Everything that talks to the outside: the on-disk HTTP cache with ETag revalidation, supervised fetch threads with backoff and jitter, the Lightstreamer client for ISSLIVE, STScI schedule parsers, sky-survey pictures, GeoClue and IP location. |
| `mctl-sim` | The engine thread and its workers (pass prediction, trends from the recording, the maneuver planner, proximity operations), and the simulation clock with its LIVE, SIM and REPLAY modes. |
| `mctl-store` | The SQLite recording of states, telemetry, element sets, events and the C&W log, with retention and CSV export. |
| `mctl-gui` | The console: a fixed character grid with pixel-drawn widgets over it, every page and sub-page, the themes, the globe pass (GPU), the line art, the window view, and the Raspberry Pi kiosk display. |
| `mctl-tui` | Placeholder for a future text-mode front end. |
| `mctl` (root) | The command-line program: argument parsing, wiring, and the `check`, `shot`, `bench`, `export`, `isslive` and `about` subcommands. |

## Data flow and honesty

Each source thread owns its connection, retries with backoff, and reports its health as an event; a failing
source never blocks the interface (the UI thread does no I/O). The **arbiter** picks, per vehicle, the first
source in the configured priority order that has data covering the requested time, and keeps the others
running as cross-checks so their disagreement can be shown. The result is a `World`: an immutable snapshot
published through `arc-swap`, so the renderer takes a consistent view without locks.

A quantity with no source is shown as `NO DATA`, never as a plausible number. Values that are estimated,
planned, TLE-derived or from a survey cutout carry a short flag (`[EST]`, `[PLAN]`, `[TLE]`, `[SURVEY]`).
Telemetry whose stream clock has stopped (as the ISSLIVE feed does at loss of signal) is marked stale from
that clock, not from the wall clock.

## Time and frames

Instants are POSIX-style UTC seconds in an `f64`; leap seconds come from an embedded table and UT1-UTC and
polar motion from IERS data, assumed zero (and flagged) when unavailable. Positions are carried in GCRF and
rotated to ITRF for geodetic output. The simulation clock can run live, at any rate from -10000x to +10000x,
or replay the recording; only real time is ever recorded.

## The console

The text model is a fixed grid of cells (`grid.rs`); pages write text and queue widgets (bars, sparklines,
dials, plots, pictures, line art) into rectangles of cells, and the renderer paints the widgets over those
cells. The globe, ground track and sky plot are drawn by GPU or egui painters at the pixel rectangle their
page reports. Themes are TOML (palette, font, limit-state encoding, CRT effects) validated on load. The CRT
effects (scanlines, bloom, persistence, curvature) are a post-processing pass that can be toggled per effect.

The same page code serves the Raspberry Pi display (`kiosk.rs`): a smaller grid, a different font, flat
rendering without the effects, and a cycle of scenes drawn from the same pages.

## Testing

Unit tests live beside the code and use recorded fixtures instead of the network (about 370 tests). The
parsers are checked against captured real responses, the propagators against the NASA ISS ephemeris, and pass
times against an independent implementation (`tools/xcheck_passes.py`). `tools/smoke.sh` renders every page
headless and checks the images. GPU rendering needs an adapter; Mesa's software Vulkan driver is enough.
