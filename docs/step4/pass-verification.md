# Pass prediction cross-check

Checked 2026-10-23, re-run 2026-10-29 at the NASA HQ site. `cargo test -p mctl-astro print_fixture_passes -- --ignored --nocapture` prints
the passes mctl finds for the ISS OMM fixture over NASA HQ (38.883 N, 77.0163 W, 10 m), mask 10 deg,
first 24 h. `tools/xcheck_passes.py` (needs `pip install sgp4`) predicts the same passes with the
python `sgp4` package, a plain GMST rotation and its own topocentric geometry.

| pass | AOS diff | TCA diff | LOS diff | max-elevation diff |
|---|---|---|---|---|
| 1 (11.7 deg) | 0.00 s | -0.10 s | 0.00 s | 0.000 deg |
| 2 (69.1 deg) | 0.00 s | +0.10 s | 0.00 s | 0.000 deg |
| 3 (15.2 deg) | 0.00 s | -0.20 s | 0.00 s | 0.000 deg |
| 4 (17.9 deg) | 0.00 s | +0.15 s | 0.00 s | 0.000 deg |
| 5 (82.7 deg) | 0.00 s | -0.37 s | 0.00 s | 0.009 deg |

AOS and LOS agree to a few milliseconds. TCA differs by under half a second because elevation is flat at
culmination and the check samples it at 0.5 s; the 0.009 deg difference is on the near-zenith pass 5, where
elevation changes fastest. This validates the frame reduction, WGS-84 geometry and root finding against an
independent implementation using the **same element set**. It does not compare against a published pass
predictor (Spot the Station, Heavens-Above), which would also test the element set's age and source.

## Extended to a week (38 passes)

Re-run over seven days (`xcheck_passes.py` and `print_fixture_passes` both set to 7 d): mctl and the
independent implementation each find 38 passes. Largest differences: AOS 0.003 s, LOS 0.003 s, TCA 0.46 s,
max elevation 0.013 deg. DESIGN.md §12 asks for 20 passes within 10 s and 0.5 deg against NASA Spot the
Station or an independent tool; this meets the numbers against an independent *implementation*, but
not against Spot the Station, which also depends on the element set it uses.
