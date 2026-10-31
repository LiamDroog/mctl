# Spike A — `gfx`: GPU console renderer

Code: `spikes/gfx/`. Run `cargo run --release -p gfx` (keys: `T` theme, `1`–`4` scanlines/bloom/persistence/CRT, `E` all, `Q` quit).
Headless: `gfx shot --out-dir DIR`, `gfx bench --breakdown --out FILE.json`.
Raw data: `gfx-frametimes-*.json` here. Screenshots: `shots/`.

## Hardware and drivers (this machine)

wgpu 30.0.1 enumerates three adapters. Intel Haswell has no usable Vulkan, so the two real GPUs are on different backends:

| Adapter | Backend | Driver | Notes |
|---|---|---|---|
| Quadro K5100M (Kepler, discrete) | Vulkan | NVK (Mesa 26.0.8-1ubuntu0.3) | Picked by default (`HighPerformance`). Open-source driver; nouveau Kepler reclocking is limited, so the GPU probably runs at low clocks. **Clock state unverified.** |
| Intel HD Graphics 4600 (Haswell GT2) | **OpenGL 4.6** | Mesa 26.0.8 (`--low-power`) | `MESA-INTEL: warning: Haswell Vulkan support is incomplete`. wgpu does not offer Vulkan on it. |
| llvmpipe | Vulkan, CPU | Mesa | Refused by the spike (CPU adapters are rejected). |

Session: KDE Plasma, Wayland (Xwayland on `:0`), panel 1920×1080 @ 60 Hz.

## Frame times (GPU timestamp query, whole frame: globe pass → egui pass → post chain), ms

Measured headless with the full pipeline rendering into an offscreen texture of the stated size, `--release`. "All on" forces scanlines, bloom, persistence and curvature on in every theme. Median / p95; 400 frames after 100 warm-up (NVK); 300 after 600 warm-up (Intel).

| GPU | Size | All effects on | All effects off |
|---|---|---|---|
| Quadro K5100M / NVK | 1920×1080 | 7.80 / 7.82 | 4.15 / 4.17 |
| Quadro K5100M / NVK | 2560×1440 | 13.88 / 13.91 | 7.45 / 7.47 |
| HD 4600 / GL | 1920×1080 | 7.07 / 13.06 | 3.30 / 6.75 |
| HD 4600 / GL | 2560×1440 | 8.50 / 11.54 | 4.56 / 7.63 |

All four themes measure within 1% of each other on NVK (full matrix in the JSON). Budget for 60 fps is 16.7 ms.

**Intel numbers are noisy and not monotonic in resolution** (1440p all-on measured *below* 1080p all-on; p95 up to 13 ms, max 24 ms). Cause is probably iGPU frequency scaling under a bursty load, which I did not confirm. Treat the Intel medians as a rough indication only.

Per-effect cost on NVK (Phosphor, delta over all-off, 1080p / 1440p): bloom +1.2 / +2.1 ms, persistence +2.1 / +3.7 ms, scanlines and curvature ≈ 0 (they are arithmetic inside the always-present final pass).
Persistence originally used an `Rgba16Float` history and cost +4.2 / +7.4 ms; moving to `Rgba8Unorm` with a one-LSB decay floor (so ghosts still reach zero) halved it.

**Live window** (fullscreen, 1920×1080, Fifo, 3594 frames ≈ 60 s, both GPUs): median interval 16.69 ms (NVK) / 16.68 ms (Intel), p95 17.8 ms, max 36 ms (NVK) / 49 ms (Intel). About 1% of frames after warm-up missed a refresh (35 / 3444 NVK, 28 / 3444 Intel); no sustained misses. **Cause of the isolated slow frames is not investigated** (compositor, driver or app).
`--present immediate` still paced at 16.7 ms under KWin/Wayland, so uncapped frame time cannot be measured through a window here; the headless benchmark is the real measurement.

## What was built and verified

- winit 0.30 + wgpu 30 + egui/egui-winit/egui-wgpu 0.37, **no eframe**. egui renders into an offscreen `Rgba8Unorm` scene texture; the post chain then runs over the whole frame. Verified by screenshots of all four themes and by one live capture of the window (the capture itself was deleted, see below).
- Fixed character grid (`grid.rs`): 16×32 cells for the Unscii themes, 18×36 for JetBrains Mono. Cell width is checked against the real font advance at startup and a mismatch is an error.
- Globe in its own pass: analytic ray-cast sphere, Lambert day/night with soft terminator, lat/lon graticule (15°), coastlines from a rasterised Natural Earth mask, atmosphere glow, Blue Marble texture, rotation from the Earth rotation angle. Dashed orbit ribbon depth-tested against the sphere (hidden behind Earth); hollow vehicle marker plus `OCCULTED` label when behind Earth.
- Post chain with independent toggles: scanlines, bloom (quarter-res, 9-tap separable), phosphor persistence (frame max-blend with decay), barrel curvature + vignette. Disabled effects skip their passes.
- Four TOML themes, validated on load; `--themes-dir` replaces the embedded set.
- GPU timestamps via a timestamp query set; wall-clock frame interval on screen.
- 22 unit tests (themes, coastline parsing/rasterisation, grid, orbit maths, layout at tiny sizes).

## Not verified

- **`T` and the 1–4/E keys were not exercised interactively.** No key-injection tool is available here (`/dev/uinput` is root-only). `Console::cycle_theme` and the toggles are the same code the headless runs use, but the winit key handler itself is untested.
- Only the window's initial frame was observed live. Resize handling (`Resized` → `surface.configure`, targets recreated by size) is untested.
- X11 not tested (Wayland only).
- NFR-2 itself (Xe-class iGPU at 1440p) cannot be tested on this machine. The nearest data point is HD 4600 on GL, which is older and a different backend.
- Retrowave's optional horizon-grid backdrop is not implemented.
- A live-desktop screenshot was taken with `spectacle -f` and **deleted immediately** because it captured unrelated windows (browser, terminals). It is not in the repo.

## Findings that affect the design document

1. **VT323 cannot be the CRT font**: it has no box-drawing glyphs. Unscii-16 (public domain) is used instead (see `assets/LICENSES.md`).
2. **Intel Haswell (and likely other pre-Gen9 iGPUs) get OpenGL, not Vulkan, from wgpu.** The GL backend works, including timestamp queries. NFR-6/NFR-2 should say which backends are supported.
3. **Persistence is the expensive effect** (R5 is real). Options if Xe-class hardware is tight: fuse persistence into the final pass with a second render target, or run bloom/persistence at reduced resolution.
4. **HiDPI**: the grid is pixel-addressed (theme cell sizes are physical pixels) and egui is run at 1 point = 1 pixel regardless of the display scale factor. A scaling policy for the grid is needed before M2.
5. egui 0.37 panics if a `TexturesDelta` is dropped unapplied; the render API takes it by `&mut` and clears it.
6. wgpu 30 and glam 0.34 have breaking API changes from earlier versions (e.g. glam removed `Mat4::perspective_rh`/`look_at_rh`; use `glam::camera::rh`). Pin versions as NFR-9 says.
