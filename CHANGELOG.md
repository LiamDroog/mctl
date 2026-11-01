# Changelog

All notable changes are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/), and
the project uses [semantic versioning](https://semver.org/) once it reaches 1.0; until then, minor versions may
change behaviour.

## [Unreleased]

### Added

- `--kiosk-order random` shows every scene once per cycle in a new random order each cycle, and
  `--kiosk-order vehicle` shows one vehicle at a time, all its scenes before the next, in a random vehicle order
  reshuffled each cycle. `--kiosk-vehicles iss,jwst` limits the display to chosen vehicles.
- Four more vehicles: the Tiangong space station (Earth orbit, from CelesTrak elements), and Voyager 1, Voyager 2
  and New Horizons, which are on escape trajectories and come from JPL Horizons. They get a solar-system plot
  (path among the planets' orbits, from ecliptic north), distance from the Sun, light time to Earth, heliocentric
  speed and ecliptic direction, and line art of each spacecraft. A new `regime = "deep"` selects these pages.
- Three themes: Cosmonaut (red and amber, Soviet control room), Teletext (seven saturated colours on black) and
  Nostromo (cold teal with an orange alarm). They follow Retrowave in the `T` cycle, and the Pi display's `T` cycle
  has them too.
- `--kiosk-theme-cycle` shows each kiosk scene in a different random theme. `install-display.sh --args` sets the
  arguments the service runs with.

### Changed

- Much less CPU. The window sleeps between frames instead of spinning on the compositor (a desktop window used a whole
  core; it now uses about a fifth of one). On a Raspberry Pi 3B+ the kiosk ticks the engine at 5 Hz instead of 50 and
  rebuilds a vehicle's drawn track every 10 s instead of every second (`[engine] track_rebuild_s`), which took the engine
  from a quarter of a core to a few percent, and on a software graphics adapter it draws at 10 frames a second
  instead of 20. `--max-fps` overrides the cap, and so does the new `[display] max_fps` setting in the
  configuration (0 keeps the default).
- `--offline` now fetches nothing for the sky pictures: they come from the cache at any age, and a picture that is not
  cached is reported unavailable instead of being requested.
- Under WSL the event loop stays on `Poll` (it only sleeps between frames on other systems), as it did before the
  sleeping change; `MCTL_EVENT_LOOP=poll` or `wait` chooses either way.
- The frame timings kept for the `bench` report are bounded, so a display left running for weeks no longer grows.
- `--pi` is now `--kiosk` (and `--pi-style` is `--kiosk-style`); the old names still work as aliases.
- The kiosk display scales its fonts to fill the screen, keeping the layout designed for the Pi's 640x480, instead
  of staying at that size on a larger screen.
- The desktop console shrinks its font, down to half size, to keep the full 160 x 45 panel layout on windows
  smaller than 1920 x 1080 (a 1080p monitor's window is a title bar short of it), instead of clipping panels.
  The Pi display is unchanged.
- The kiosk's dense scenes no longer cut text off at 80 columns: long lines wrap, rows give up padding and unit
  explanations before values, box titles drop their asides, and plot labels (halo orbit, ecliptic north, event
  timeline, instrument share) avoid each other. JWST and Hubble OPS have narrow layouts. A test fails if a dense
  page clips text it did not truncate on purpose.
- The survey pictures fall back to the MAST name resolver and the STScI copy of the DSS2 survey (greyscale GIF)
  when the CDS services cannot be reached, and a host that fails to connect is not retried for ten minutes.
  `[sources.sky]` gains `mast_name_url`, `dss_url` and `dss_survey`.
- Rows of numbers line up in narrow panels (state vectors, L2 and deep-space state, sources), and the NEXT PASS
  panel is two aligned columns.
- On the kiosk, a status line too long for the screen scrolls like a news ticker instead of being cut off.
- The survey pictures wait up to `[sources.sky] timeout_s` (default 90 s, was shorter) for the sky services, with a
  separate 15 s connect timeout and one retry on a timeout.

## [0.1.1] - 2026-11-01

### Changed

- Updated rusqlite to 0.41.1.

### Fixed

- The window no longer needs `--allow-software` on a machine with only a software Vulkan driver (WSL, a VM): it
  runs, with a warning. `shot` and `bench` still refuse, since their numbers would mislead.
- Under WSL the window opens through X11 (WSLg's XWayland) when an X display exists, because WSLg's Wayland
  connection was dropped ("Broken pipe") as soon as the window was resized. Setting `WINIT_UNIX_BACKEND`
  overrides this.
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
