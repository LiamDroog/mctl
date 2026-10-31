# Spike B fixtures

Committed so `astro eval` is reproducible offline. Refresh with `astro fetch` (network). Fetched 2026-10-23.

| File | Source | Licence / terms |
|---|---|---|
| `iss-omm.json` | `https://celestrak.org/NORAD/elements/gp.php?CATNR=25544&FORMAT=JSON` (CelesTrak GP). `FORMAT=JSON` is explicit because the default is CSV. | CelesTrak publishes GP data freely; its usage policy applies. |
| `ISS.OEM_J2K_EPH.txt` | `https://nasa-public-data.s3.amazonaws.com/iss-coords/current/ISS_OEM/ISS.OEM_J2K_EPH.txt` (NASA JSC/FOD/TOPO) | NASA public data. 717 KB, a snapshot that will be superseded; keep it only as a test fixture. |
| `finals2000A.all` | `https://maia.usno.navy.mil/ser7/finals2000A.all` (IERS Rapid Service/Prediction Centre, USNO), **trimmed to MJD >= 60000** (2023-02-25 onward, 330 KB of 3.7 MB) | Public IERS data. The leap-second tests before 2023 therefore see no EOP, by construction. |
| `sun-horizons-geometric.json`, `sun-horizons-apparent.json` | JPL Horizons API (`COMMAND=10`, `CENTER=500@399`, `REF_SYSTEM=ICRF`, `VEC_CORR=NONE` / `LT+S`), 13 epochs from JD 2461336.5 weekly | JPL Horizons output is public. |
| `SGP4-VER.TLE`, `tmatverDec2015.out` | `https://celestrak.org/publications/AIAA/2006-6753/AIAA-2006-6753.zip` (`sgp4/mat/`), Vallado, Crawford, Hujsak, Kelso, "Revisiting Spacetrack Report #3" | Test vectors from the AIAA paper's supplementary code. **No formal redistribution licence was found for these two files**; they are used here as test data. Confirm before shipping them anywhere beyond this repository. |

Known defects in the reference output (handled in `src/vallado.rs`, with tests): one stray off-schedule row in the Matlab block of case 20413 (its long run), and a stale single row for case 33334.
