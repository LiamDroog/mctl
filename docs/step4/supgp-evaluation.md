# CelesTrak supplemental GP (ISS-E): M1 evaluation (the design document §3.1)

Checked 2026-10-23 with `cargo test -p mctl-astro supgp_vs_oem -- --ignored --nocapture` against the cached
NASA OEM (created 2026-10-21 16:27).

Each ISS-E segment is a fit forward from its own epoch, so the segment answering a query is the latest one whose
epoch is not after it. Even so, every segment sits a constant along-track offset from the OEM, growing with the
segment epoch:

| segment epoch | along-track offset at epoch | at +5.9 h |
|---|---|---|
| 2026-10-21 12:02 | -29.2 km | -31.6 km |
| 2026-10-23 00:02 | -46.5 km | -49.2 km |
| 2026-10-24 12:02 | -66.9 km | -71.0 km |
| 2026-10-26 00:02 | -92.1 km | -97.0 km |
| 2026-10-27 12:02 | -121.7 km | -125.9 km |
| 2026-10-29 00:06 | -153.9 km | -159.3 km |

Radial differences stay under 2 km. At the time of the check standard GP (epoch 21 h old) differed from the OEM by
1.9 km. The OEM and standard GP agree with each other, so ISS-E is the outlier; the pattern (a smooth, accelerating
along-track divergence) looks like a different drag or maneuver assumption in the prediction, but I could not
determine which source is closer to the truth without an independent measurement.

**Decision:** the source is implemented (`supgp` in a vehicle's `sources`) but is not in the ISS default list. In the
default list it would be preferred over standard GP when the OEM is unavailable, and it would raise a permanent
residual warning. Re-evaluate when a reboost or a fresh OEM changes the picture.
