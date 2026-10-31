# ADR 0002: Lightstreamer client for ISSLIVE — in-house minimal TLCP over a rustls WebSocket

- **Status:** Proposed (M0 spike, 2026-10-23); the owner confirms or overrides.
- **Decides:** the design document §6.5 "Lightstreamer TLCP" and risks R1, R2, R4.
- **Evidence:** `spikes/isslive` (`isslive`), config in `spikes/isslive/isslive.toml`, run logs in `docs/spikes/isslive-*.log`.

## Context

ISSLIVE is the public Lightstreamer feed that ISS Mimic consumes. The project licence is undecided, so no GPL dependency is allowed, and the default build must not need OpenSSL (NFR-1, "use rustls").

Everything below about the feed's address and vocabulary comes from ISS Mimic's own source (MIT, `github.com/ISS-Mimic/Mimic`, active: last push 2026-10-15):

| Fact | Value | Source |
|---|---|---|
| Server | `https://push.lightstreamer.com` (WebSocket `wss://…/lightstreamer`) | `Pi/iss_telemetry.py` |
| Adapter set | `ISSLIVE` | same |
| Mode / fields | `MERGE`; `TimeStamp`, `Value` (the TIME item adds `Status.Class`, `Status.Indicator`) | same |
| Item IDs | ~380, listed in `Pi/telemetry_ids.py` | same |
| Item meanings | spreadsheet `Telemetry/ISS_Public_Telemetry.xlsx` (338 rows: ID, category, description, names, units) | same |

## Options

| Option | Licence | Result |
|---|---|---|
| `lightstreamer-client` 0.1.12 | **GPL-3.0-only** | Rejected by the brief. |
| `lightstreamer-rs` 1.0.1 | **MIT**, but see below | Checked, does not fit. |
| In-house TLCP subset | ours | **Chosen.** |

`lightstreamer-rs` detail. Versions up to 0.3.3 were **GPL-3.0-only**; 1.0.0 (2026-07-22) relicensed to MIT as a documented clean-room rewrite from the TLCP 2.5.0 spec (its CHANGELOG and `docs/adr/0001-mit-relicensing-clean-room.md`; I could not verify the contributor history behind that claim). 1.0.1 has no GPL dependency. It fails the other constraint: `native-tls` and `tokio-native-tls` are **hard dependencies with no feature to avoid them**. I built it in a scratch project: **`openssl-sys` fails ("Could not find directory of OpenSSL installation") on this machine** because OpenSSL development files are not installed, which is exactly the NFR-1 violation (a build-time and runtime dependency on OpenSSL). I did not run it against the feed: making it build would mean installing `libssl-dev`, which contradicts the rule I was working to, so its runtime behaviour is untested.

## Decision

Implement the minimal TLCP 2.5.0 subset in-house, as `isslive/src/tlcp.rs` (pure protocol: request encoding, notification parsing, update decoding; no I/O, ready to move into `mctl-sources`) plus `session.rs` (tokio, `tokio-tungstenite`, `rustls` with the `ring` provider and bundled web-PKI roots). The linked binary depends only on libc, libm and libgcc_s.

The subset: `create_session` (adapter set + the generic client id), `control` `LS_op=add` for `MERGE` subscriptions with snapshot, `LS_op=destroy`, and parsing of `CONOK`, `CONERR`, `REQOK`, `REQERR`, `ERROR`, `SUBOK`, `U`, `PROBE`, `LOOP`, `END` and the informational tags (`SERVNAME`, `CLIENTIP`, `CONS`, `CONF`, `SYNC`, `PROG`, `NOOP`, `EOS`, `CS`). The `U` value list is decoded per the spec: empty token = unchanged, `#` = null, `$` = empty string, `^N` = N unchanged fields; `^P`/`^T` diffs are rejected loudly (never seen from this server). Protocol facts were taken from the public TLCP specification, not from memory; the bundled protocol notes of `lightstreamer-rs` were used as a readable copy of it (MIT, no code reused).

## Evidence

- **Unit tests (12: 9 protocol, 3 config):** the spec's worked update examples (snapshot with null and empty, unchanged fields, `^4`, `^7`), rejection leaves state untouched, percent-decoding after splitting, every request shape, malformed notifications are errors and never panics.
- **Against a live ticking feed:** Lightstreamer's public `DEMO` adapter (`QUOTE_ADAPTER`) updates about once a second. My client decoded it correctly, including partial updates where only `last_price`, `pct_change`, `bid`, `ask` changed and `stock_name` carried over. This separates "client bug" from "feed state" (see below).
- **Against ISSLIVE:** `CONOK` after 1.0–1.2 s in the saved runs (3.0 s on the very first connection), both `SUBOK`s 0.34–0.44 s later, then a full snapshot, `keepalive_ms=5000`, server sends `PROBE` every 5 s when idle and `SYNC` roughly every 13 s.
- **Failure paths exercised live:** wrong adapter set → `CONERR 2 Requested Adapter Set not available`; unresolvable host; HTTP 404 on a wrong path; each exits non-zero with a clear message.

### Finding: the server silently accepts unknown items

Subscribing to a made-up item name returns `SUBOK` for the full item count and then simply never sends data for it. A typo in the item list would therefore give no error at all. The client now requires a snapshot for every item within `setup_timeout_s`, and fails naming the missing items.

### Finding: connection health does not imply fresh data

I connected at 15:13 UTC on 2026-10-23. The session was healthy (`PROBE` every 5 s) but delivered only the snapshot for the next ~7.5 minutes (two captures, 150 s and 300 s), with `TIME_000001` at `Status.Class=1`, `Status.Indicator=M`. The source timestamps decoded to **02:18:29 UTC, about 12.9 hours earlier**. In Mimic's client only class `24` means connected (`TimeListener`), and Mimic's own "Signal Error" state exists for this. When I checked again at 16:06 UTC the feed was live (`Status.Class=24`, data age ~4 s, 1,458 updates in 45 s). I do not know when it resumed or why it stalled. mctl must judge freshness from `TIME_000001` (status class and timestamp advance) and the timestamps of regularly-updating items, never from the socket (the design document §1.3, R1).

### Observed update rates (live, 180 s, 2026-10-23 16:07 UTC; `docs/spikes/isslive-run-3-live-180s.log`)

| Item(s) | Updates in 180 s | Rate | Median age at receipt |
|---|---|---|---|
| `TIME_000001` | 3,588 | **19.9 Hz** | 3.9 s |
| `USLAB000019`–`27` (attitude quaternion 1–3, errors, rates) | 180–181 each | **1.00 Hz** | 3.0–3.6 s |
| `USLAB000018` (quaternion 0) | 170 (140 value changes) | 0.94 Hz | 3.2 s |
| `S0000003`, `S0000004` (SARJ angles) | 180 each | 1.00 Hz | 3.4 s |
| `P4000007` (BGA 2A angle) | 48 | 0.27 Hz (mean 3.7 s apart) | 3.8 s |
| `USLAB000013`, `17`, `81`, `28`–`31` (attitude source, control frame, maneuver flag, **commanded quaternion**) | 1 each (snapshot only) | change rarely | 17–18 min |

"Age" is receive time minus the item's own `TimeStamp`, which is fractional hours since 00:00 UTC on 31 December of the previous year (as Mimic computes it). It includes any offset of this machine's clock from UTC, which I did not check, so treat 3–4 s as an upper bound on latency. **For rarely-changing items the timestamp is the time of the last change, so a 17-minute age is normal and says nothing about staleness.** C&W staleness rules need a per-item policy ("fresh if the feed is live and the item's own cadence is long"), not one age limit.

### What the attitude items appear to represent

Documented by Mimic's spreadsheet (its text, quoted in `isslive.toml`): `USLAB000018`–`21` "US Current Local Vertical Local Horizontal (LVLH) Attitude Quaternion Component 0–3"; `USLAB000028`–`31` "US Commanded Attitude Quaternion Component 0–3"; `USLAB000022`–`24` attitude roll/pitch/yaw error; `USLAB000025`–`27` inertial attitude rate X/Y/Z; `USLAB000013` attitude source; `USLAB000017` attitude control reference frame; `USLAB000081` attitude maneuver in progress.

| Question | Status | Evidence |
|---|---|---|
| Items 18–21 are a unit quaternion | **verified** | |q| over 708 complete samples: 0.999999 … 1.000001 |
| Component 0 is the scalar part | **inferred, strongly** | q0 ≈ 0.9972, others ≈ 0.007, −0.066, −0.034. If the scalar were last, q3 = −0.034 would mean a ~176° attitude, which the ISS is not flying |
| The attitude is small relative to the LVLH reference (~8.5° total rotation) | **verified** | from the same values |
| Attitude errors are in **degrees** (spreadsheet says RAD) | **inferred** | roll 0.166 and pitch 0.288 match 2·(q_cur − q_cmd) = 0.158° and 0.293°; yaw 0.032 vs 0.019 matches less well |
| Inertial rates are in **deg/s** (spreadsheet says RAD/S) | **inferred** | Y = −0.0639 matches the ISS orbital rate 360°/92.9 min = 0.0647 °/s; the same number in rad/s would be 3.7 °/s |
| Which LVLH axes, and whether the quaternion rotates LVLH→body or body→LVLH | **unverified** | the spreadsheet does not say. Settling it needs the OEM-derived LVLH at the receive time and a known attitude mode (R4) |
| The meaning of `USLAB000017 = 0` and `USLAB000013 = 2` | **unverified** | enumerations not documented |
| `Status.Indicator` values | **unverified** | seen `M` while stalled and empty while live |

## Consequences

- R2 is closed for now: no GPL dependency, no OpenSSL. If the project later picks a GPL-compatible licence, `lightstreamer-client` stays unattractive anyway (partial implementation); `lightstreamer-rs` could be revisited if it gains a rustls feature.
- Maintenance cost: `tlcp.rs` is 509 lines including its 9 tests, `session.rs` 362. It covers only what ISSLIVE needs; reconnection and rebind are **not implemented** (WebSocket never sent `LOOP`, and any failure ends the session with an error). M3 must add reconnect with backoff and re-subscription, and the staleness policy above.
- R4 stays open: do not enable the ADI until the quaternion convention is verified against a known attitude (the design document already requires this).
- Design-document changes proposed: §3.1 (ISSLIVE row: item IDs and field names confirmed; units column of the public spreadsheet is unreliable; freeze behaviour), §14 R1 (a healthy connection can carry 13+ hours of stale data), R4 (what is now verified).

## Not verified

- Update rates were measured over one 180 s window at one time of day; the stall episode was observed twice but its cause and duration are unknown.
- The `lightstreamer-rs` runtime behaviour (only its build failure is verified).
- TLS root handling beyond the bundled web-PKI roots; TLCP features I did not need (rebind, recovery, `LOOP`, diff formats, message sending, other modes).
- Whether the item list is stable: IDs were taken from Mimic's current `main`.
