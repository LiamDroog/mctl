//! Time: UTC instants, the leap-second table, and Earth-orientation (UT1-UTC, polar motion) data.
//!
//! Known simplification versus DESIGN.md §6.3: instants are POSIX-style UTC seconds in an `f64`, not
//! TAI as `i64` plus a fraction. That is exact (to ~0.2 µs) for every window that does not contain a
//! leap second and for the present day; a leap second itself cannot be represented. Moving to TAI is
//! planned before the sim clock can scrub across historical leap seconds.

use thiserror::Error;

/// Seconds per day.
pub const SECONDS_PER_DAY: f64 = 86_400.0;
/// MJD of the Unix epoch, 1970-01-01T00:00:00 UTC.
const MJD_UNIX_EPOCH: f64 = 40_587.0;
/// JD of MJD 0.
pub const JD_MJD_OFFSET: f64 = 2_400_000.5;
/// TT - TAI, seconds (exact by definition).
pub const TT_MINUS_TAI_S: f64 = 32.184;

#[derive(Debug, Error, PartialEq)]
pub enum TimeError {
    #[error("cannot parse timestamp {0:?}: expected YYYY-MM-DDThh:mm:ss[.fff]")]
    Parse(String),
    #[error("invalid calendar date {0}-{1}-{2}")]
    BadDate(i32, u32, u32),
    #[error("no leap-second data before 1972-01-01")]
    BeforeUtc,
    #[error("system clock is before 1970")]
    ClockBeforeEpoch,
}

/// UTC instant as seconds since 1970-01-01T00:00:00 UTC, leap seconds not counted.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, serde::Serialize, serde::Deserialize)]
pub struct Utc(pub f64);

/// Days since 1970-01-01 of a proleptic Gregorian date (Hinnant's algorithm).
pub fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = i64::from(y) - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = i64::from((m + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Inverse of [`days_from_civil`].
pub fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    ((y + i64::from(m <= 2)) as i32, m, d)
}

impl Utc {
    pub fn from_ymd_hms(
        y: i32,
        mo: u32,
        d: u32,
        h: u32,
        mi: u32,
        s: f64,
    ) -> Result<Self, TimeError> {
        if !(1..=12).contains(&mo) || d == 0 || d > 31 {
            return Err(TimeError::BadDate(y, mo, d));
        }
        let days = days_from_civil(y, mo, d);
        if civil_from_days(days) != (y, mo, d) {
            return Err(TimeError::BadDate(y, mo, d));
        }
        Ok(Self(
            days as f64 * SECONDS_PER_DAY + f64::from(h) * 3600.0 + f64::from(mi) * 60.0 + s,
        ))
    }

    /// The system clock. Fails only if the clock is set before 1970.
    pub fn now() -> Result<Self, TimeError> {
        let d = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| TimeError::ClockBeforeEpoch)?;
        Ok(Self(d.as_secs_f64()))
    }

    /// Parse `YYYY-MM-DDThh:mm:ss[.fff]`, as used by OMM and OEM.
    pub fn parse(s: &str) -> Result<Self, TimeError> {
        let bad = || TimeError::Parse(s.to_owned());
        let (date, time) = s.split_once('T').ok_or_else(bad)?;
        let mut dp = date.split('-');
        let (y, mo, d) = (dp.next(), dp.next(), dp.next());
        let mut tp = time.split(':');
        let (h, mi, sec) = (tp.next(), tp.next(), tp.next());
        let (Some(y), Some(mo), Some(d), Some(h), Some(mi), Some(sec)) = (y, mo, d, h, mi, sec)
        else {
            return Err(bad());
        };
        if dp.next().is_some() || tp.next().is_some() {
            return Err(bad());
        }
        Self::from_ymd_hms(
            y.parse().map_err(|_| bad())?,
            mo.parse().map_err(|_| bad())?,
            d.parse().map_err(|_| bad())?,
            h.parse().map_err(|_| bad())?,
            mi.parse().map_err(|_| bad())?,
            sec.parse().map_err(|_| bad())?,
        )
    }

    /// (year, month, day, hour, minute, second) of this instant.
    pub fn to_ymd_hms(self) -> (i32, u32, u32, u32, u32, f64) {
        let days = (self.0 / SECONDS_PER_DAY).floor();
        let rem = self.0 - days * SECONDS_PER_DAY;
        let (y, m, d) = civil_from_days(days as i64);
        let h = (rem / 3600.0).floor();
        let mi = ((rem - h * 3600.0) / 60.0).floor();
        (y, m, d, h as u32, mi as u32, rem - h * 3600.0 - mi * 60.0)
    }

    pub fn plus(self, seconds: f64) -> Self {
        Self(self.0 + seconds)
    }

    pub fn minus(self, other: Self) -> f64 {
        self.0 - other.0
    }

    pub fn mjd_utc(self) -> f64 {
        self.0 / SECONDS_PER_DAY + MJD_UNIX_EPOCH
    }

    pub fn jd_utc(self) -> f64 {
        self.mjd_utc() + JD_MJD_OFFSET
    }

    pub fn iso(self) -> String {
        let (y, mo, d, h, mi, s) = self.to_ymd_hms();
        format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:06.3}")
    }

    /// GMT as `DDD:HH:MM:SS.ss` (day of year), DESIGN.md §6.3.
    pub fn gmt_doy(self) -> String {
        let (y, mo, d, h, mi, s) = self.to_ymd_hms();
        let doy = days_from_civil(y, mo, d) - days_from_civil(y, 1, 1) + 1;
        format!("{doy:03}:{h:02}:{mi:02}:{s:05.2}")
    }
}

/// First UTC date (y, m, d) from which TAI-UTC takes the given value (IERS Bulletin C history).
/// The latest entry is 2017-01-01; nothing newer is known to this table, so it must be checked
/// against Bulletin C before relying on it for dates much later than the present.
const LEAP_TABLE: &[((i32, u32, u32), f64)] = &[
    ((1972, 1, 1), 10.0),
    ((1972, 7, 1), 11.0),
    ((1973, 1, 1), 12.0),
    ((1974, 1, 1), 13.0),
    ((1975, 1, 1), 14.0),
    ((1976, 1, 1), 15.0),
    ((1977, 1, 1), 16.0),
    ((1978, 1, 1), 17.0),
    ((1979, 1, 1), 18.0),
    ((1980, 1, 1), 19.0),
    ((1981, 7, 1), 20.0),
    ((1982, 7, 1), 21.0),
    ((1983, 7, 1), 22.0),
    ((1985, 7, 1), 23.0),
    ((1988, 1, 1), 24.0),
    ((1990, 1, 1), 25.0),
    ((1991, 1, 1), 26.0),
    ((1992, 7, 1), 27.0),
    ((1993, 7, 1), 28.0),
    ((1994, 7, 1), 29.0),
    ((1996, 1, 1), 30.0),
    ((1997, 7, 1), 31.0),
    ((1999, 1, 1), 32.0),
    ((2006, 1, 1), 33.0),
    ((2009, 1, 1), 34.0),
    ((2012, 7, 1), 35.0),
    ((2015, 7, 1), 36.0),
    ((2017, 1, 1), 37.0),
];

/// TAI - UTC, seconds.
pub fn tai_minus_utc(t: Utc) -> Result<f64, TimeError> {
    let mut found = None;
    for &((y, m, d), dt) in LEAP_TABLE {
        let start = Utc::from_ymd_hms(y, m, d, 0, 0, 0.0).expect("table dates are valid");
        if t.0 >= start.0 {
            found = Some(dt);
        }
    }
    found.ok_or(TimeError::BeforeUtc)
}

#[derive(Debug, Error)]
pub enum EopError {
    #[error("EOP file: {0}")]
    Parse(String),
    #[error("no EOP data for MJD {mjd:.1}: table covers {first:.1} .. {last:.1}")]
    Coverage { mjd: f64, first: f64, last: f64 },
}

#[derive(Debug, Clone, Copy)]
pub struct EopRow {
    pub mjd: f64,
    /// Polar motion, arcseconds.
    pub xp: f64,
    pub yp: f64,
    /// UT1 - UTC, seconds.
    pub dut1: f64,
}

/// IERS `finals2000A` (Bulletin A) daily values.
#[derive(Debug, Clone)]
pub struct EopTable {
    rows: Vec<EopRow>,
}

impl EopTable {
    pub fn parse(text: &str) -> Result<Self, EopError> {
        let mut rows = Vec::new();
        for (n, l) in text
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty())
        {
            let field = |a: usize, b: usize| l.get(a..b).map(str::trim).filter(|s| !s.is_empty());
            let num = |a: usize, b: usize, what: &str| -> Result<Option<f64>, EopError> {
                field(a, b)
                    .map(|s| {
                        s.parse::<f64>().map_err(|_| {
                            EopError::Parse(format!("line {}: bad {what} {s:?}", n + 1))
                        })
                    })
                    .transpose()
            };
            let (Some(mjd), Some(xp), Some(yp), Some(dut1)) = (
                num(7, 15, "MJD")?,
                num(18, 27, "x")?,
                num(37, 46, "y")?,
                num(58, 68, "UT1-UTC")?,
            ) else {
                // Rows past the end of predictions can be blank in these columns; they carry no usable value.
                continue;
            };
            if let Some(prev) = rows.last().map(|r: &EopRow| r.mjd)
                && mjd <= prev
            {
                return Err(EopError::Parse(format!(
                    "line {}: MJD {mjd} not after {prev}",
                    n + 1
                )));
            }
            rows.push(EopRow { mjd, xp, yp, dut1 });
        }
        if rows.len() < 2 {
            return Err(EopError::Parse("fewer than two usable rows".into()));
        }
        Ok(Self { rows })
    }

    /// MJD range covered.
    pub fn coverage(&self) -> (f64, f64) {
        (self.rows[0].mjd, self.rows[self.rows.len() - 1].mjd)
    }

    /// Linearly interpolated values. Outside the table is an error: no silent zeros.
    ///
    /// UT1-UTC steps by one second at a leap second; interpolating across one would be wrong, so a
    /// jump of more than 0.5 s between neighbours is rejected.
    pub fn at(&self, mjd_utc: f64) -> Result<EopRow, EopError> {
        let (first, last) = self.coverage();
        if mjd_utc < first || mjd_utc > last {
            return Err(EopError::Coverage {
                mjd: mjd_utc,
                first,
                last,
            });
        }
        let i = self
            .rows
            .partition_point(|r| r.mjd <= mjd_utc)
            .saturating_sub(1)
            .min(self.rows.len() - 2);
        let (a, b) = (self.rows[i], self.rows[i + 1]);
        if (b.dut1 - a.dut1).abs() > 0.5 {
            return Err(EopError::Parse(format!(
                "UT1-UTC jumps by {:.3} s near MJD {:.0} (leap second); not interpolated",
                b.dut1 - a.dut1,
                a.mjd
            )));
        }
        let w = (mjd_utc - a.mjd) / (b.mjd - a.mjd);
        Ok(EopRow {
            mjd: mjd_utc,
            xp: a.xp + w * (b.xp - a.xp),
            yp: a.yp + w * (b.yp - a.yp),
            dut1: a.dut1 + w * (b.dut1 - a.dut1),
        })
    }
}

/// `1d02h`, `15h22m`, `4m10s`, `37s`; negative or non-finite values render as `--`.
pub fn fmt_dur(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "--".into();
    }
    let s = secs as u64;
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m{:02}s", s / 60, s % 60),
        3600..=86_399 => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
        _ => format!("{}d{:02}h", s / 86_400, (s % 86_400) / 3600),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_roundtrip_over_many_years() {
        for days in (-30_000..60_000).step_by(37) {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
    }

    #[test]
    fn parse_format_and_gmt() {
        let t = Utc::parse("2026-10-21T12:00:00.000").unwrap();
        assert_eq!(t.iso(), "2026-10-21T12:00:00.000");
        assert!((t.mjd_utc() - 61_334.5).abs() < 1e-9);
        assert_eq!(t.gmt_doy(), "294:12:00:00.00");
        assert_eq!(
            Utc::parse("2024-12-31T00:00:00").unwrap().gmt_doy(),
            "366:00:00:00.00",
            "leap year"
        );
        for bad in [
            "2026-10-21 12:00:00",
            "2026-13-21T12:00:00",
            "2026-02-30T12:00:00",
            "2026-10-21T12:00",
            "2026-10-21T12:00:00:00",
        ] {
            assert!(Utc::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn leap_seconds_around_the_2016_insertion() {
        let before = Utc::from_ymd_hms(2016, 12, 31, 23, 59, 59.0).unwrap();
        let after = Utc::from_ymd_hms(2017, 1, 1, 0, 0, 0.0).unwrap();
        assert_eq!(tai_minus_utc(before).unwrap(), 36.0);
        assert_eq!(tai_minus_utc(after).unwrap(), 37.0);
        assert!(tai_minus_utc(Utc::from_ymd_hms(1971, 12, 31, 0, 0, 0.0).unwrap()).is_err());
    }

    const ROW: &str = "261022 61335.00 I  0.149159 0.000041  0.317685 0.000041  I-0.0428601 0.0000100  0.0000 0.1916  P    -0.766    0.199    -0.720    0.300   .143000   .137000   .8075000   -18.637    -3.667\n261023 61336.00 P  0.148194 0.000600  0.317462 0.000400  P-0.0444801 0.0000100\n";

    #[test]
    fn eop_parses_interpolates_and_rejects_outside() {
        let e = EopTable::parse(ROW).unwrap();
        let r = e.at(61_335.0).unwrap();
        assert!(
            (r.dut1 + 0.0428601).abs() < 1e-9
                && (r.xp - 0.149159).abs() < 1e-9
                && (r.yp - 0.317685).abs() < 1e-9
        );
        assert!((e.at(61_335.5).unwrap().dut1 - (-0.0436701)).abs() < 1e-9);
        assert!(matches!(e.at(10_000.0), Err(EopError::Coverage { .. })));
        assert!(matches!(e.at(99_999.0), Err(EopError::Coverage { .. })));
        assert!(EopTable::parse("").is_err());
    }

    #[test]
    fn eop_refuses_to_interpolate_across_a_leap_second() {
        let t = "170101 57754.00 I  0.100000 0.000001  0.300000 0.000001  I 0.4000000 0.0000100\n170102 57755.00 I  0.100000 0.000001  0.300000 0.000001  I-0.5990000 0.0000100\n";
        assert!(EopTable::parse(t).unwrap().at(57_754.5).is_err());
    }
}
