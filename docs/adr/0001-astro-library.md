# ADR 0001: Astrodynamics library — `satkit`, `brahe`, or in-house on `sgp4` + `nalgebra`

- **Status:** Proposed (M0 spike, 2026-10-23). The owner confirms or overrides; nothing downstream depends on it before M1.
- **Decides:** DESIGN.md §6.5 "Frames, numerical propagation, force models" and the SGP4 row.
- **Evidence:** `spikes/astro` (`astro eval`), raw results in `docs/spikes/astro-results.json`, fixtures in `spikes/astro/fixtures/` (fetched 2026-10-23).

## Context

mctl needs SGP4 from OMM JSON, TEME↔GCRF↔ITRF, Sun/Moon, UT1/leap-second handling, OEM interpolation, and (M5) a numerical propagator with J2–J4 + drag for the maneuver planner (DESIGN.md §4.2, §7). NFR-1 wants one executable whose only runtime dependencies are the GPU stack and libc, with assets embedded. The project licence is undecided, so every candidate must be permissive.

## Options

| | Licence | Latest release | History | Maintainers | Linear algebra |
|---|---|---|---|---|---|
| **satkit 0.24.1** | MIT OR Apache-2.0 | 2026-10-01 | 62 releases in 31 months; five in the 11 days to 2026-10-01 | one dominant author (491 commits; next contributor 4); 10 contributors; 1 open issue | `numeris` (not nalgebra) |
| **brahe 1.8.0** | MIT | 2026-10-16 | 31 releases in 33 months; MSRV 1.95 | one dominant author (1,671 commits); 9 contributors incl. 2 bots; 22 open issues | `nalgebra` |
| **in-house** = `sgp4 2.4.0` (MIT, 2026-02-23, 26 releases since 2020, 414k downloads) + `nalgebra 0.35` (Apache-2.0) | permissive | | | `sgp4`: one main author, 10 contributors | `nalgebra` |

All are permissive and compatible with any licence the project picks. Brahe's SGP4 *is* the `sgp4` crate, called with its default configuration.

## Evidence

Everything below was produced by running code (`astro eval`, release build). The "in-house" row is the cheapest honest version of each capability, so it shows the *floor* of what writing it ourselves buys.

### 1. SGP4 from the current ISS OMM (CelesTrak `FORMAT=JSON`, epoch 2026-10-22T23:36:04), 24 h at 60 s

| Candidate | Max position difference from in-house, 24 h |
|---|---|
| in-house (`sgp4`, AFSPC mode) | 0 (reference) |
| satkit | **0.29 µm** |
| brahe | **67.9 m** (velocity 58.5 mm/s) |

Cause, read from the `sgp4` source: `Constants::from_elements` uses **WGS-84** constants and IAU sidereal time. CelesTrak/Space-Track elements are fit with the AFSPC convention (WGS-72), exposed as `from_elements_afspc_compatibility_mode`. Brahe wraps the default and offers no switch.

### 2. SGP4 vs the NASA OEM over the same 24 h (OEM interpolated, SGP4 rotated TEME→GCRF by each candidate; RTN residuals)

OEM hold-out test (interpolate every other sample from its neighbours): **4.09 m / 33.8 mm/s** max, against the 10 m target in DESIGN.md §12. Sample spacing is 90–240 s (not uniform: the last rows are 11:54:30, 11:58:30, 12:00:00), so interpolation must use arbitrary nodes.

| Candidate | RMS 3-D | Max 3-D | Max R / T / N | At 24 h (R/T/N) |
|---|---|---|---|---|
| in-house | 1,198 m | 2,349 m | 242 / 2,348 / 400 m | −171 / 1,888 / −285 m |
| satkit | 1,199 m | 2,350 m | 242 / 2,349 / 399 m | −171 / 1,889 / −286 m |
| brahe | 1,220 m | 2,402 m | 250 / 2,400 / 396 m | −173 / 1,896 / −295 m |

Along-track dominates, as DESIGN.md §7.2 expects for a 16-hour-old element set. The WGS-72 candidates are 53 m closer to the OEM than brahe, which is weak independent support for AFSPC being the right convention (the 2.3 km prediction error dwarfs it).

### 3. Vallado SGP4 verification vectors (33 cases, Dec 2015 reference, AFSPC/WGS-72)

Tolerance used (my choice; the reference prints 8 decimals): 1e-5 km position, 1e-8 km/s velocity.

| Candidate | Within tolerance | Worst error | Notes |
|---|---|---|---|
| in-house | **33/33** | 0.15 mm | 4 TLEs needed repair (3 deliberately bad checksums, 1 blank ephemeris-type field); **returns a state inside the Earth instead of an error on 3 decay cases** (radius 5,297–6,354 km) |
| satkit | **33/33** | 4.1 mm | detects every error; accepts all TLEs unmodified |
| brahe | **1/33** | 13,070 km (deep space); typically 0.07 km | the WGS-84 default; cannot be switched |

Two reference-file defects were found and handled by stated rules (`vallado.rs`): one stray off-schedule row in the Matlab output of case 20413 (absent from the Fortran output), and one stale "t=0" state for case 33334 that repeats the previous case (the libraries refusing that TLE is correct).

### 4. Frames (candidate TEME→GCRF and GCRF→ITRF against satkit's, max over the 24 h)

| Candidate | TEME→GCRF | GCRF→ITRF |
|---|---|---|
| brahe | 0.14 mm | 0.31 mm |
| in-house, no polar motion (DESIGN.md §6.4) | 1.37 m | 12.8 m |
| in-house, with polar motion | 1.37 m | 1.37 m |

Satkit and brahe agree to a third of a millimetre: two independent full IAU 2006/2000A implementations. The in-house chain (IAU-76 precession, 10 of 106 IAU-80 nutation terms, no frame bias) is 1.37 m off; ignoring polar motion adds 11.5 m, matching the design's "~10 m". DESIGN.md §12's "< 1 m" frames target is not met by the truncated in-house chain; the residual is truncation plus the omitted frame bias (about 0.8 m by my estimate, not isolated), and it is irrelevant next to SGP4's own ~1 km.

### 5. Sun against JPL Horizons (13 weekly epochs, geocentric ICRF)

Horizons gives *geometric* positions by default, while analytic series model the *apparent* direction (light-time plus aberration, about 20.5″). I first compared against geometric and saw a constant ~21″ for satkit; fetching Horizons with `VEC_CORR='LT+S'` confirmed that as a definition mismatch, so both are reported.

| Candidate | vs geometric (max/mean) | vs apparent (max/mean) |
|---|---|---|
| satkit (`lpephem`) | 21.9″ / 21.0″ | **1.3″ / 0.6″** |
| in-house (Vallado series) | 36.4″ / 22.7″ | 15.8″ / 6.0″ |
| brahe (`sun_position`) | 326.5″ / 314.9″ | **305.9″ / 294.1″** |

DESIGN.md FR-13 needs arcminute level: satkit and in-house pass; brahe's analytic Sun (5′) does not (`sun_position` is documented as low-precision analytical; brahe also has an ephemeris-based path through its SPICE module, not evaluated).

### 6. Numerical propagation, 24 h from an OEM state (2026-10-22T12:00), hourly, residual vs OEM; Cd·A/m = 5.30e-3 m²/kg from the OEM header

| Force model | in-house | satkit | brahe |
|---|---|---|---|
| J2–J4 only, no drag (max 3-D) | 7,685 m | 7,687 m | 7,688 m |
| + drag, library's own density | 3,409 m (my assumed exponential) | 3,165 m (NRLMSISE-00, nominal weather) | 3,585 m (Harris-Priester) |
| + drag, one fitted density multiplier | 1,997 m (×2.21) | 1,970 m (×1.95) | 1,958 m (×2.34) |
| 20×20 gravity + Sun + Moon + drag, library density | unsupported | **805 m** | not run |
| same, one fitted multiplier | unsupported | **90.6 m** (×0.855) | not run |
| Runtime for the 24 h run | 5 ms | 28 ms | ~1,000 ms |

The three independent J2–J4 results agree within 3 m, which validates the in-house integrator and zonal-gravity code (also unit-tested: J2 nodal regression within 5% of theory, and the acceleration is checked against a finite-difference of the potential). **J2–J4 + drag alone cannot reach DESIGN.md §12's "< 1 km per 24 h": the floor is ~2 km.** Only the richer force model does (805 m untuned).

### 7. Time scales and EOP

| | in-house | satkit | brahe |
|---|---|---|---|
| TAI−UTC across the 2016-12-31 leap second | 36 → 37 s | 36 → 37 s | 36 → 37 s |
| UT1−UTC at an EOP row / mid-row (file: −0.0428601 s) | −0.0428601 / −0.0436701 s | agrees within 2e-7 s | agrees within 2e-7 s |
| Before the EOP table (fixture table starts 2023-02-25) | **error** | **0.0 s, log warning only** | **holds the first row (−0.015 s), silently** |
| Beyond the EOP table (2040) | **error** | holds last row (−0.208 s), log warning | holds last row (−0.208 s), silently |
| Before 1972 | error | 9.34 s (pre-1972 UTC formula) | 9.34 s |

Satkit's frame functions use zero EOP with a one-time warning when none is loaded, but its propagator refuses to build without one. Brahe accepted a zero-EOP provider (`StaticEOPProvider::from_zero()`) for a full propagation without any warning, and its `EOPExtrapolation::Error` mode panics rather than returning an error. DESIGN.md §6.3 says to flag missing UT1 rather than assume it; only the in-house path does that by construction.

### 8. Footprint and runtime requirements (stripped release binaries doing one SGP4 + frame + propagation)

| | Binary | Dynamic libraries | Runtime data |
|---|---|---|---|
| in-house | **0.59 MB** | libc, libm, libgcc_s | EOP + leap table, which §6.3 already requires us to manage |
| satkit | 5.8 MB | libc, libm, libgcc_s | EOP; **JPL DE440, 97.5 MB, required for *any* numerical propagation even with all Sun/Moon force terms off** (the force table is built from it unconditionally); space weather if NRLMSISE uses it |
| brahe | 10.4 MB | libc, libm, libgcc_s | data embedded (17 MB in the crate) |

All three respect "GPU stack and libc only". Satkit's DE440 requirement conflicts with NFR-1's "all assets embedded" and with NFR-4 on a first run without network.

## Decision

1. **M1–M4: build the core in-house** on `sgp4` (always `from_elements_afspc_compatibility_mode`) and `nalgebra`: SGP4/OMM, time scales and EOP, frames, RTN/LVLH/VNB, Sun/Moon, OEM interpolation, events and passes. It passed every check it was given, has no runtime data beyond what §6.3 already manages, adds 0.6 MB, and fails loudly where the libraries fall back silently.
2. **Reject brahe** for mctl: its SGP4 cannot be made conformant (1/33 on the verification vectors, 68 m per day on the ISS), its analytic Sun is 5′ off, its numerical propagator is 36–200× slower than the others, and it accepts zero EOP silently. Its frames are excellent but add nothing satkit does not.
3. **M5: use satkit for the numerical propagator and maneuver planner**, behind an `mctl-astro` trait so it can be swapped, gated on two conditions checked at the start of M5: (a) the 97.5 MB DE440 file is handled as an optional, cached, user-visible download (planner shows `NO DATA` until present; nothing else depends on it) or upstream removes the unconditional load; (b) the version is pinned exactly and re-evaluated, given pre-1.0 churn. The in-house J2–J4 propagator stays as a fallback and as an independent cross-check, because it agrees with satkit to 3 m on the same physics.

Reaching the 1 km target in-house would need a 20×20 geopotential (EGM coefficients embedded), analytic Sun/Moon third-body forces, and at least Harris-Priester drag. I did not build this, so its cost is an estimate, not a measurement.

## Consequences

- **Do now in the in-house path:** add the `sgp4` decay guard (radius below one Earth radius is an error, matching Vallado's error 6); fail on checksum/format errors for TLEs but note OMM JSON avoids them; add frame bias and the full nutation series (or IAU 2006 tables) plus EOP polar motion before the §12 "< 1 m" frame test; make the leap-second table updatable and detect staleness; never extrapolate or hold EOP silently.
- **DESIGN.md changes proposed** are in the M0 report (§6.5, §7.2, §12, NFR-1 wording, and the SGP4 constant choice).
- **Risk R-new:** satkit's single maintainer and release cadence. Mitigated by the trait, the exact pin, and the in-house fallback.
- **Cost:** the in-house frames and Moon series are real work (days, not hours); the spike's 400 lines show the shape of it.

## Not verified

- Event finding, pass prediction, eclipse and Moon accuracy in any library; satkit's maneuver API was not exercised. satkit's README claim of 2–13 cm agreement with GMAT was not checked.
- One ISS OMM, one OEM window and one numerical start epoch, on one machine. The OEM is itself a NASA prediction, not truth, so residuals include its own error.
- "AFSPC/WGS-72 is the right convention for CelesTrak elements" rests on the `sgp4` and Vallado documentation plus the weak 53 m OEM evidence above; I did not check against an independent truth ephemeris.
- brahe's high-fidelity propagation and its ephemeris-based Sun were not run.
- Timing is a single run, not a benchmark.

## Reproduce

```
cargo run --release -p astro -- fetch           # refresh fixtures (network)
SATKIT_DATA=<dir containing linux_p1550p2650.440> \
  cargo run --release -p astro -- eval --out docs/spikes/astro-results.json
```
