# Pass prediction cross-check

Checked 2026-10-23. `cargo test -p mctl-astro print_fixture_passes -- --ignored --nocapture` prints
the passes mctl finds for the ISS OMM fixture over a site at 51.05 N, 114.07 W, 1045 m, mask 10 deg,
first 24 h. `tools/xcheck_passes.py` (needs `pip install sgp4`) predicts the same passes with the
python `sgp4` package, a plain GMST rotation and its own topocentric geometry.

| pass | AOS diff | TCA diff | LOS diff | max-elevation diff |
|---|---|---|---|---|
| 1 (34.9 deg) | 0.00 s | +0.31 s | 0.00 s | 0.000 deg |
| 2 (88.1 deg) | 0.00 s | -0.41 s | 0.00 s | 0.044 deg |
| 3 (89.0 deg) | 0.00 s | +0.04 s | 0.00 s | 0.001 deg |
| 4 (34.1 deg) | 0.00 s | +0.35 s | 0.00 s | 0.000 deg |

AOS and LOS agree to a few milliseconds. TCA differs by under half a second because elevation is flat at
culmination and the check samples it at 0.5 s; the 0.044 deg difference is on a near-zenith pass where
elevation changes fastest. This validates the frame reduction, WGS-84 geometry and root finding against an
independent implementation using the **same element set**. It does not compare against a published pass
predictor (Spot the Station, Heavens-Above), which would also test the element set's age and source.

## Extended to a week (33 passes)

Re-run over seven days (`xcheck_passes.py` and `print_fixture_passes` both set to 7 d): mctl and the
independent implementation each find 33 passes. Largest differences: AOS 0.003 s, LOS 0.003 s, TCA 0.49 s,
max elevation 0.056 deg. DESIGN.md §12 asks for 20 passes within 10 s and 0.5 deg against NASA Spot the
Station or an independent tool; this meets the numbers against an independent *implementation*, but
not against Spot the Station, which also depends on the element set it uses.
