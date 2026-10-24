//! JPL Horizons vector tables (`EPHEM_TYPE=VECTORS`, `CSV_FORMAT=YES`) as a tabulated trajectory.
//!
//! The request must ask for geocentric (`500@399`) ICRF (`REF_PLANE=FRAME`) vectors in km and km/s
//! with `TIME_TYPE=UT`. Every one of those is checked in the reply header: a table in another frame,
//! centre or unit would otherwise interpolate happily and be wrong by thousands of kilometres.
//! ICRF is used as GCRF (they differ by milliarcseconds, far below the data's accuracy).

use thiserror::Error;

use mctl_core::state::State;
use mctl_core::time::Utc;

use crate::oem::{DragParams, INTERPOLATION_POINTS, Oem, OemState};

/// Julian date of the POSIX epoch.
const JD_POSIX_EPOCH: f64 = 2_440_587.5;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum HorizonsError {
    #[error("Horizons replied with an error or no table: {0}")]
    NoTable(String),
    #[error("reply header does not say {0:?}; refusing to read the table")]
    Header(&'static str),
    #[error("row {row}: {reason}")]
    Row { row: usize, reason: String },
    #[error("table has {0} rows; need at least {INTERPOLATION_POINTS}")]
    TooFew(usize),
}

/// Header lines that must be present for the table to mean what this parser assumes.
const REQUIRED_HEADER: [(&str, &str); 5] = [
    (
        "Center body name: Earth (399)",
        "Center body name: Earth (399)",
    ),
    ("Output units    : KM-S", "Output units: KM-S"),
    ("Reference frame : ICRF", "Reference frame: ICRF"),
    (
        "Output type     : GEOMETRIC cartesian states",
        "geometric cartesian states",
    ),
    ("(UT )", "UT time tags"),
];

/// Parse a Horizons reply into a tabulated ephemeris. `fetched_at` stamps the data's age, since the
/// reply carries no creation time of its own.
pub fn parse(text: &str, fetched_at: Utc) -> Result<Oem, HorizonsError> {
    let (Some(soe), Some(eoe)) = (text.find("$$SOE"), text.find("$$EOE")) else {
        let first = text
            .lines()
            .find(|l| !l.trim().is_empty() && !l.starts_with('*'))
            .unwrap_or("empty reply");
        return Err(HorizonsError::NoTable(first.trim().to_owned()));
    };
    let header = &text[..soe];
    for (needle, shown) in REQUIRED_HEADER {
        if !header.contains(needle) {
            return Err(HorizonsError::Header(shown));
        }
    }
    let mut states: Vec<OemState> = Vec::new();
    let rows = text[soe + 5..eoe].lines().filter(|l| !l.trim().is_empty());
    for (i, line) in rows.enumerate() {
        let row = i + 1;
        let bad = |reason: String| HorizonsError::Row { row, reason };
        let cols: Vec<&str> = line.split(',').map(str::trim).collect();
        // JD, calendar date, X, Y, Z, VX, VY, VZ and a trailing comma.
        if cols.len() < 8 {
            return Err(bad(format!("{} columns, need 8", cols.len())));
        }
        let num = |k: usize| {
            cols[k]
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite())
                .ok_or_else(|| bad(format!("column {} is {:?}, not a number", k + 1, cols[k])))
        };
        // The JD column carries ~86 us of precision; tables are on whole seconds, so round to ms.
        let t = Utc(((num(0)? - JD_POSIX_EPOCH) * 86_400_000.0).round() / 1000.0);
        if let Some(prev) = states.last()
            && t.0 <= prev.t.0
        {
            return Err(bad("time does not increase".into()));
        }
        states.push(OemState {
            t,
            state: State::new([num(2)?, num(3)?, num(4)?], [num(5)?, num(6)?, num(7)?]),
        });
    }
    if states.len() < INTERPOLATION_POINTS {
        return Err(HorizonsError::TooFew(states.len()));
    }
    Ok(Oem {
        notes: Vec::new(),
        states,
        // Not used: nothing here propagates with drag.
        drag: DragParams {
            mass_kg: f64::NAN,
            drag_area_m2: f64::NAN,
            drag_coeff: f64::NAN,
        },
        creation_date: fetched_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trajectory::Trajectory;

    const SAMPLE: &str = include_str!("../fixtures/horizons-jwst-sample.txt");

    #[test]
    fn parses_a_real_reply() {
        let o = parse(SAMPLE, Utc(1.0)).unwrap();
        assert_eq!(o.states.len(), 13);
        // 2026-Oct-21 00:00 UT, JD 2461334.5.
        let first = o.states[0].t;
        assert!((first.0 - (2_461_334.5 - JD_POSIX_EPOCH) * 86_400.0).abs() < 1e-3);
        assert_eq!(o.states[1].t.0 - first.0, 3600.0);
        let r = o.states[0].state.r;
        assert!((r.x - 7.934_751_860_640_186e5).abs() < 1e-6);
        // About 1.4 million km from Earth: the L2 distance.
        assert!((1.3e6..1.7e6).contains(&r.norm()), "{}", r.norm());
        assert_eq!(o.creation_date, Utc(1.0));
    }

    #[test]
    fn interpolates_between_rows_and_refuses_outside() {
        let o = parse(SAMPLE, Utc(0.0)).unwrap();
        let mid = Utc(o.states[6].t.0 + 1800.0);
        let s = o.state_gcrf(mid).unwrap();
        let (a, b) = (o.states[6].state.r, o.states[7].state.r);
        // Halfway between two hourly rows of a slow orbit is close to their mean.
        assert!(
            (s.r - (a + b) * 0.5).norm() < 1.0,
            "{}",
            (s.r - (a + b) * 0.5).norm()
        );
        assert!(o.state_gcrf(Utc(o.states[0].t.0 - 10.0)).is_err());
    }

    #[test]
    fn rejects_a_table_in_another_frame_or_centre() {
        let ecl = SAMPLE.replace(
            "Reference frame : ICRF",
            "Reference frame : Ecliptic of J2000.0",
        );
        assert_eq!(
            parse(&ecl, Utc(0.0)).unwrap_err(),
            HorizonsError::Header("Reference frame: ICRF")
        );
        let sun = SAMPLE.replace("Earth (399)", "Sun (10)");
        assert!(matches!(
            parse(&sun, Utc(0.0)),
            Err(HorizonsError::Header(_))
        ));
        let tdb = SAMPLE.replace("(UT )", "(TDB)");
        assert!(matches!(
            parse(&tdb, Utc(0.0)),
            Err(HorizonsError::Header(_))
        ));
    }

    #[test]
    fn an_error_reply_reports_horizons_own_words() {
        let e = parse(
            "No ephemeris for target \"-9999\" prior to A.D. 1600\n",
            Utc(0.0),
        )
        .unwrap_err();
        assert_eq!(
            e,
            HorizonsError::NoTable("No ephemeris for target \"-9999\" prior to A.D. 1600".into())
        );
    }

    #[test]
    fn bad_rows_fail_loudly() {
        let bad = SAMPLE.replacen("7.934751860640186E+05", "nope", 1);
        assert!(matches!(
            parse(&bad, Utc(0.0)),
            Err(HorizonsError::Row { row: 1, .. })
        ));
    }
}
