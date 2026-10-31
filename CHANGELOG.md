# Changelog

All notable changes are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/), and
the project uses [semantic versioning](https://semver.org/) once it reaches 1.0; until then, minor versions may
change behaviour.

## [Unreleased]

### Fixed

- The window no longer needs `--allow-software` on a machine with only a software Vulkan driver (WSL, a VM): it
  runs, with a warning. `shot` and `bench` still refuse, since their numbers would mislead.
- Under WSL, `--fullscreen` opens a maximized window, because WSLg's compositor dropped a fullscreen client
  ("Broken pipe" in the Wayland event loop). Not yet confirmed on a WSL machine.
- The window title is `mctl`, not `mctl gfx spike`.

## [0.1.0] - 2026-10-31

First public release. In the box:

- A mission-control console for the ISS, Hubble, JWST, Roman and the Crew and Cargo Dragons: orbit map,
  ground track, sky plot and pass predictions, elements and trends, a maneuver planner, a window view of the
  Earth from the vehicle, proximity operations, and JWST and Hubble observing schedules with survey pictures.
- Live ISS telemetry (GNC, EPS, TCS/ECLSS, COMM) with a caution-and-warning rule engine.
- Recording to SQLite, export to CSV, and replay of the recording.
- Four themes with optional CRT effects, and a low-resolution display mode (`--pi`) for a Raspberry Pi driving
  a composite CRT.
- `mctl about`, with the licence, credits and third-party notices, and matching ABOUT and CREDITS pages.
- `packaging/bootstrap.sh`: checks a machine for a compiler, libraries, a Vulkan driver and Rust, installs what is
  missing (apt, dnf, pacman, and rustup), then builds and installs mctl.
