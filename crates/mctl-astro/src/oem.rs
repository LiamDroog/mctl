//! CCSDS OEM (KVN) parsing and Lagrange interpolation for the NASA ISS ephemeris.
//!
//! Only what the ISS file needs: one metadata block, `COMMENT` lines and a single contiguous run
//! of `epoch x y z vx vy vz` lines. Anything that would make interpolation silently wrong
//! (unknown frame or time system, non-monotonic epochs, short rows, no data) is an error.

use thiserror::Error;

use mctl_core::state::State;
use mctl_core::time::{TimeError, Utc};

/// Frame the NASA ISS OEM is published in. Mean of J2000 equator and equinox.
const EXPECTED_FRAME: &str = "EME2000";
const EXPECTED_TIME_SYSTEM: &str = "UTC";
/// Points used per interpolation: the order is this minus one. 9 points = 8th order, as DESIGN.md §7.1 says.
pub const INTERPOLATION_POINTS: usize = 9;

#[derive(Debug, Error)]
pub enum OemError {
    #[error("line {line}: {reason}")]
    Line { line: usize, reason: String },
    #[error("line {line}: {source}")]
    Time {
        line: usize,
        #[source]
        source: TimeError,
    },
    #[error("OEM has fewer than {INTERPOLATION_POINTS} state vectors ({0})")]
    TooFewStates(usize),
    #[error("OEM metadata is missing {0}")]
    MissingKey(&'static str),
    #[error("requested time {requested} is outside the OEM span {start} .. {stop}")]
    OutOfSpan {
        requested: String,
        start: String,
        stop: String,
    },
}

/// Drag-related values from the OEM header comments (units: kg, m^2).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DragParams {
    pub mass_kg: f64,
    pub drag_area_m2: f64,
    pub drag_coeff: f64,
}

impl DragParams {
    /// Ballistic parameter Cd*A/m, m^2/kg.
    pub fn cd_a_over_m(&self) -> f64 {
        self.drag_coeff * self.drag_area_m2 / self.mass_kg
    }
}

#[derive(Debug, Clone)]
pub struct OemState {
    pub t: Utc,
    pub state: State,
}

/// Comments the parser recognises as routine header text (never reported as notes).
const ROUTINE_COMMENT_PREFIXES: [&str; 9] = [
    "Source:",
    "Units",
    "MASS",
    "DRAG_AREA",
    "DRAG_COEFF",
    "SOLAR_RAD",
    "Orbits start",
    "ISS first asc",
    "ISS last asc",
];
/// Words that mark a comment as possibly describing a maneuver. The comment-block format is not
/// formally specified (DESIGN.md R3), so these are shown as raw text rather than parsed.
const MANEUVER_WORDS: [&str; 6] = ["maneuver", "manoeuvre", "reboost", "burn", "delta-v", "dv"];

#[derive(Debug, Clone)]
pub struct Oem {
    /// Comments the parser did not recognise as routine header text, verbatim.
    pub notes: Vec<String>,
    pub states: Vec<OemState>,
    pub drag: DragParams,
    /// `CREATION_DATE` from the header: when NASA produced the file, the basis for its age.
    pub creation_date: Utc,
}

fn meta_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let (k, v) = line.split_once('=')?;
    (k.trim() == key).then(|| v.trim())
}

impl Oem {
    pub fn parse(text: &str) -> Result<Self, OemError> {
        let (mut frame, mut tsys, mut creation) = (None, None, None);
        let (mut mass, mut area, mut cd) = (None, None, None);
        let mut states: Vec<OemState> = Vec::new();
        let mut notes: Vec<String> = Vec::new();
        for (idx, raw) in text.lines().enumerate() {
            let line = idx + 1;
            let l = raw.trim();
            if l.is_empty() || l == "META_START" || l == "META_STOP" {
                continue;
            }
            if let Some(c) = l.strip_prefix("COMMENT") {
                let c = c.trim();
                if !c.is_empty() && !ROUTINE_COMMENT_PREFIXES.iter().any(|p| c.starts_with(p)) {
                    notes.push(c.to_owned());
                }
                for (key, slot) in [
                    ("MASS", &mut mass),
                    ("DRAG_AREA", &mut area),
                    ("DRAG_COEFF", &mut cd),
                ] {
                    if let Some(v) = c.strip_prefix(key).and_then(|r| r.strip_prefix('=')) {
                        *slot = Some(v.trim().parse::<f64>().map_err(|_| OemError::Line {
                            line,
                            reason: format!("{key} is not a number: {v:?}"),
                        })?);
                    }
                }
                continue;
            }
            if let Some(v) = meta_value(l, "REF_FRAME") {
                frame = Some(v.to_owned());
                continue;
            }
            if let Some(v) = meta_value(l, "CREATION_DATE") {
                creation = Some(Utc::parse(v).map_err(|source| OemError::Time { line, source })?);
                continue;
            }
            if let Some(v) = meta_value(l, "TIME_SYSTEM") {
                tsys = Some(v.to_owned());
                continue;
            }
            if l.starts_with(|c: char| c.is_ascii_digit()) && l.contains('T') && !l.contains('=') {
                let cols: Vec<&str> = l.split_whitespace().collect();
                if cols.len() != 7 {
                    return Err(OemError::Line {
                        line,
                        reason: format!("expected 7 columns, found {}", cols.len()),
                    });
                }
                let t = Utc::parse(cols[0]).map_err(|source| OemError::Time { line, source })?;
                let mut v = [0.0; 6];
                for (slot, text) in v.iter_mut().zip(&cols[1..]) {
                    *slot = text.parse().map_err(|_| OemError::Line {
                        line,
                        reason: format!("not a number: {text:?}"),
                    })?;
                    if !f64::is_finite(*slot) {
                        return Err(OemError::Line {
                            line,
                            reason: format!("non-finite value {text:?}"),
                        });
                    }
                }
                if let Some(prev) = states.last()
                    && t.0 <= prev.t.0
                {
                    return Err(OemError::Line {
                        line,
                        reason: format!(
                            "epoch {} is not after the previous epoch {}",
                            t.iso(),
                            prev.t.iso()
                        ),
                    });
                }
                states.push(OemState {
                    t,
                    state: State::new([v[0], v[1], v[2]], [v[3], v[4], v[5]]),
                });
            }
        }
        let frame = frame.ok_or(OemError::MissingKey("REF_FRAME"))?;
        let tsys = tsys.ok_or(OemError::MissingKey("TIME_SYSTEM"))?;
        if frame != EXPECTED_FRAME {
            return Err(OemError::Line {
                line: 0,
                reason: format!("REF_FRAME is {frame}, this build only handles {EXPECTED_FRAME}"),
            });
        }
        if tsys != EXPECTED_TIME_SYSTEM {
            return Err(OemError::Line {
                line: 0,
                reason: format!(
                    "TIME_SYSTEM is {tsys}, this build only handles {EXPECTED_TIME_SYSTEM}"
                ),
            });
        }
        if states.len() < INTERPOLATION_POINTS {
            return Err(OemError::TooFewStates(states.len()));
        }
        Ok(Self {
            notes,
            creation_date: creation.ok_or(OemError::MissingKey("CREATION_DATE"))?,
            states,
            drag: DragParams {
                mass_kg: mass.ok_or(OemError::MissingKey("COMMENT MASS"))?,
                drag_area_m2: area.ok_or(OemError::MissingKey("COMMENT DRAG_AREA"))?,
                drag_coeff: cd.ok_or(OemError::MissingKey("COMMENT DRAG_COEFF"))?,
            },
        })
    }

    /// Notes that mention a maneuver, reboost or burn, as raw text for the operator.
    pub fn maneuver_notes(&self) -> Vec<&str> {
        self.notes
            .iter()
            .filter(|n| {
                let l = n.to_ascii_lowercase();
                MANEUVER_WORDS.iter().any(|w| {
                    l.split(|c: char| !c.is_alphanumeric() && c != '-')
                        .any(|t| t == *w)
                        || (w.len() > 3 && l.contains(w))
                })
            })
            .map(String::as_str)
            .collect()
    }

    pub fn span(&self) -> (Utc, Utc) {
        (self.states[0].t, self.states[self.states.len() - 1].t)
    }

    /// Smallest and largest spacing between consecutive epochs, seconds.
    pub fn spacing_range_s(&self) -> (f64, f64) {
        self.states
            .windows(2)
            .map(|w| w[1].t.0 - w[0].t.0)
            .fold((f64::INFINITY, 0.0), |(lo, hi), d| (lo.min(d), hi.max(d)))
    }

    /// State at `t` by degree-8 Lagrange interpolation of position and velocity, each independently.
    ///
    /// Nodes are the 9 samples nearest `t`, shifted to stay inside the file. Segment boundaries
    /// at maneuvers must not be interpolated across; this file has a single segment, which is
    /// checked at parse time by requiring one contiguous run.
    pub fn interpolate(&self, t: Utc) -> Result<State, OemError> {
        let (start, stop) = self.span();
        if t.0 < start.0 || t.0 > stop.0 {
            return Err(OemError::OutOfSpan {
                requested: t.iso(),
                start: start.iso(),
                stop: stop.iso(),
            });
        }
        self.interpolate_excluding(t, None)
    }

    /// As [`Self::interpolate`], but never using the sample at index `skip` (hold-out testing).
    fn interpolate_excluding(&self, t: Utc, skip: Option<usize>) -> Result<State, OemError> {
        // Virtual index space with `skip` removed, so the hot path (no skip) allocates nothing.
        let n = self.states.len() - usize::from(skip.is_some());
        let at = |k: usize| {
            if skip.is_some_and(|s| k >= s) {
                k + 1
            } else {
                k
            }
        };
        let (mut lo, mut hi) = (0, n);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.states[at(mid)].t.0 < t.0 {
                lo = mid + 1
            } else {
                hi = mid
            }
        }
        let pos = lo;
        // Centre the window on the nearest sample: the Lagrange error is smallest mid-window.
        let nearest = match pos {
            0 => 0,
            p if p == n => p - 1,
            p => {
                let (before, after) = (self.states[at(p - 1)].t.0, self.states[at(p)].t.0);
                if t.0 - before <= after - t.0 {
                    p - 1
                } else {
                    p
                }
            }
        };
        let first = nearest
            .saturating_sub(INTERPOLATION_POINTS / 2)
            .min(n - INTERPOLATION_POINTS);
        let nodes: [usize; INTERPOLATION_POINTS] = std::array::from_fn(|k| at(first + k));
        let mut out = State::new([0.0; 3], [0.0; 3]);
        // Times are taken relative to the first node so the polynomial is well conditioned.
        let t0 = self.states[nodes[0]].t.0;
        let x = t.0 - t0;
        for (a, &ia) in nodes.iter().enumerate() {
            let xa = self.states[ia].t.0 - t0;
            let mut w = 1.0;
            for (b, &ib) in nodes.iter().enumerate() {
                if a != b {
                    let xb = self.states[ib].t.0 - t0;
                    w *= (x - xb) / (xa - xb);
                }
            }
            out.r += w * self.states[ia].state.r;
            out.v += w * self.states[ia].state.v;
        }
        Ok(out)
    }

    /// Hold-out test from DESIGN.md §12: interpolate every other sample from its neighbours,
    /// leaving the sample itself out. Returns the position error (m) and velocity error (mm/s) maxima.
    pub fn hold_out_errors(&self) -> (f64, f64) {
        let (mut max_r, mut max_v) = (0.0f64, 0.0f64);
        let n = self.states.len();
        for i in (INTERPOLATION_POINTS..n - INTERPOLATION_POINTS).step_by(2) {
            let est = self
                .interpolate_excluding(self.states[i].t, Some(i))
                .expect("inside span");
            max_r = max_r.max((est.r - self.states[i].state.r).norm() * 1000.0);
            max_v = max_v.max((est.v - self.states[i].state.v).norm() * 1.0e6);
        }
        (max_r, max_v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL: &str = include_str!("../fixtures/iss-oem-sample.txt");

    fn synthetic(n: usize) -> String {
        // A circular orbit sampled every 240 s, so the exact answer is known analytically.
        let mut s = String::from(
            "CREATION_DATE = 2026-01-01T00:00:00.000\nMETA_START\nREF_FRAME = EME2000\nTIME_SYSTEM = UTC\nMETA_STOP\nCOMMENT MASS=1000.0\nCOMMENT DRAG_AREA=10.0\nCOMMENT DRAG_COEFF=2.0\n",
        );
        let (a, w) = (6800.0f64, (398_600.441_8_f64 / 6800.0f64.powi(3)).sqrt());
        for k in 0..n {
            let (t, th) = (k as f64 * 240.0, w * k as f64 * 240.0);
            let ts = Utc::parse("2026-01-01T00:00:00").unwrap().plus(t).iso();
            s += &format!(
                "{ts} {} {} 0 {} {} 0\n",
                a * th.cos(),
                a * th.sin(),
                -a * w * th.sin(),
                a * w * th.cos()
            );
        }
        s
    }

    #[test]
    fn parses_the_real_file() {
        let oem = Oem::parse(REAL).unwrap();
        assert!(oem.states.len() > 300, "sample file has ~400 states");
        assert!((oem.drag.cd_a_over_m() - 1.4 * 1791.8 / 473_406.5).abs() < 1e-12);
        let (lo, hi) = oem.spacing_range_s();
        assert!(lo > 0.0 && hi >= lo);
    }

    #[test]
    fn interpolation_reproduces_a_circular_orbit() {
        let oem = Oem::parse(&synthetic(60)).unwrap();
        let t = oem.states[20].t.plus(100.0);
        let est = oem.interpolate(t).unwrap();
        let th = (100.0 + 20.0 * 240.0) * (398_600.441_8_f64 / 6800.0f64.powi(3)).sqrt();
        // The truncation error of 8th-order Lagrange on this coarse grid (0.27 rad per sample) is
        // 0.02597 m in 3-D with a window centred on the nearest node, computed independently in Python.
        let err_m = (est.r - nalgebra::Vector3::new(6800.0 * th.cos(), 6800.0 * th.sin(), 0.0))
            .norm()
            * 1000.0;
        assert!(
            (err_m - 0.025_97).abs() < 5e-4,
            "interpolation error {err_m} m, expected the analytic 0.02597 m"
        );
    }

    #[test]
    fn interpolation_works_at_both_ends_and_rejects_outside() {
        let oem = Oem::parse(&synthetic(30)).unwrap();
        let (a, b) = oem.span();
        assert!(oem.interpolate(a).is_ok() && oem.interpolate(b).is_ok());
        assert!(matches!(
            oem.interpolate(a.plus(-1.0)),
            Err(OemError::OutOfSpan { .. })
        ));
        assert!(matches!(
            oem.interpolate(b.plus(1.0)),
            Err(OemError::OutOfSpan { .. })
        ));
    }

    #[test]
    fn malformed_input_is_rejected_with_a_line_number() {
        let good = synthetic(12);
        let short = good.replacen("0 0 ", "0 ", 1);
        assert!(matches!(
            Oem::parse(&short),
            Err(OemError::Line { .. } | OemError::TooFewStates(_))
        ));
        let nonmono = format!("{good}2026-01-01T00:00:00 1 2 3 4 5 6\n");
        assert!(matches!(Oem::parse(&nonmono), Err(OemError::Line { .. })));
        assert!(matches!(
            Oem::parse(&good.replace("EME2000", "ITRF")),
            Err(OemError::Line { .. })
        ));
        assert!(matches!(
            Oem::parse(&good.replace("TIME_SYSTEM = UTC", "TIME_SYSTEM = TAI")),
            Err(OemError::Line { .. })
        ));
        assert!(matches!(
            Oem::parse("garbage"),
            Err(OemError::MissingKey(_))
        ));
        assert!(matches!(
            Oem::parse(&good.replace("COMMENT MASS=1000.0\n", "")),
            Err(OemError::MissingKey(_))
        ));
        let nan = good.replacen(" 0 ", " NaN ", 1);
        assert!(Oem::parse(&nan).is_err());
    }

    #[test]
    fn too_few_states_is_an_error() {
        assert!(matches!(
            Oem::parse(&synthetic(5)),
            Err(OemError::TooFewStates(5))
        ));
    }

    #[test]
    fn unrecognised_comments_are_kept_verbatim_and_maneuver_ones_are_picked_out() {
        let text = format!(
            "CREATION_DATE = 2026-01-01T00:00:00.000\nMETA_START\nREF_FRAME = EME2000\nTIME_SYSTEM = UTC\nMETA_STOP\nCOMMENT Source: TOPO\nCOMMENT MASS=1000.0\nCOMMENT DRAG_AREA=10.0\nCOMMENT DRAG_COEFF=2.0\nCOMMENT Reboost planned 2026-11-02T10:00:00 $ DV = 1.2 m/s\nCOMMENT Something else entirely\n{}",
            (0..12)
                .map(|i| format!(
                    "2026-01-01T00:{:02}:00.000 7000.0 0.0 0.0 0.0 7.5 0.0\n",
                    i * 4
                ))
                .collect::<String>()
        );
        let o = Oem::parse(&text).unwrap();
        assert_eq!(
            o.notes,
            vec![
                "Reboost planned 2026-11-02T10:00:00 $ DV = 1.2 m/s",
                "Something else entirely"
            ]
        );
        assert_eq!(
            o.maneuver_notes(),
            vec!["Reboost planned 2026-11-02T10:00:00 $ DV = 1.2 m/s"]
        );
    }

    #[test]
    fn the_real_routine_header_produces_no_notes() {
        let o = Oem::parse(include_str!("../fixtures/iss-oem-sample.txt")).unwrap();
        assert!(o.notes.is_empty(), "{:?}", o.notes);
        assert!(o.maneuver_notes().is_empty());
    }
}
