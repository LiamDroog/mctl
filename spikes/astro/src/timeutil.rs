//! A minimal UTC time type shared by every candidate in the evaluation.
//!
//! Seconds are POSIX-style (leap seconds are not representable), which is exact for any window
//! that does not contain a leap second. The leap-second test uses each library's own time type.

use thiserror::Error;

/// Seconds per day.
pub const SECONDS_PER_DAY: f64 = 86_400.0;
/// MJD of the Unix epoch, 1970-01-01T00:00:00 UTC.
const MJD_UNIX_EPOCH: f64 = 40_587.0;
/// JD of MJD 0.
pub const JD_MJD_OFFSET: f64 = 2_400_000.5;

#[derive(Debug, Error, PartialEq)]
pub enum TimeError {
    #[error("cannot parse timestamp {0:?}: expected YYYY-MM-DDThh:mm:ss[.fff]")]
    Parse(String),
    #[error("invalid calendar date {0}-{1}-{2}")]
    BadDate(i32, u32, u32),
}

/// UTC instant as seconds since 1970-01-01T00:00:00 UTC, leap seconds not counted.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
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

    pub fn iso(self) -> String {
        let (y, mo, d, h, mi, s) = self.to_ymd_hms();
        format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:06.3}")
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
    fn parse_and_format() {
        let t = Utc::parse("2026-10-21T12:00:00.000").unwrap();
        assert_eq!(t.iso(), "2026-10-21T12:00:00.000");
        assert!((t.mjd_utc() - 61_334.5).abs() < 1e-9, "{}", t.mjd_utc());
        assert!(Utc::parse("2026-10-21 12:00:00").is_err());
        assert!(Utc::parse("2026-13-21T12:00:00").is_err());
        assert!(Utc::parse("2026-02-30T12:00:00").is_err());
        assert!(Utc::parse("2026-10-21T12:00").is_err());
        assert!(Utc::parse("2026-10-21T12:00:00:00").is_err());
    }
}
