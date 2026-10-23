//! Commands the console sends to the engine (DESIGN.md §6.2: GUI -> core via a command channel; the
//! core never calls into the GUI), and the parsing of the time arguments they take.

use thiserror::Error;

use crate::time::{TimeError, Utc};

/// Largest simulation rate magnitude (DESIGN.md FR-20: -10000x .. +10000x).
pub const MAX_RATE: f64 = 10_000.0;
/// Smallest rate magnitude accepted; slower than this is indistinguishable from a pause.
pub const MIN_RATE: f64 = 0.01;

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Enter (or stay in) SIM mode at this rate; negative runs backwards.
    SetRate(f64),
    Pause,
    Resume,
    /// Jump to an absolute epoch, entering SIM mode (paused state is kept).
    Seek(Utc),
    /// Jump relative to the current clock time, seconds.
    SeekRelative(f64),
    /// Advance by this many seconds and stay paused (entering SIM mode if live).
    Step(f64),
    /// Return to wall-clock time at 1x.
    Live,
    /// Acknowledge the master caution and warning latches (not a clock command).
    Ack,
}

#[derive(Debug, Error, PartialEq)]
pub enum CommandError {
    #[error("{0:?} is not a number")]
    NotANumber(String),
    #[error("unknown time unit in {0:?}; use s, m, h or d")]
    BadUnit(String),
    #[error("rate {0} is outside +-{MAX_RATE} (and not smaller than {MIN_RATE})")]
    RateRange(f64),
    #[error("{0}")]
    Time(#[from] TimeError),
    #[error("expected a time such as +2h, -30m, 90s, now, or 2026-10-25T12:00:00")]
    Empty,
}

/// Parse a duration like `+2h`, `-30m`, `90s`, `1.5d` into seconds. A bare number is seconds.
pub fn parse_offset(s: &str) -> Result<f64, CommandError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(CommandError::Empty);
    }
    let (num, mult) = match s.chars().last() {
        Some('s') => (&s[..s.len() - 1], 1.0),
        Some('m') => (&s[..s.len() - 1], 60.0),
        Some('h') => (&s[..s.len() - 1], 3600.0),
        Some('d') => (&s[..s.len() - 1], 86_400.0),
        Some(c) if c.is_ascii_digit() || c == '.' => (s, 1.0),
        _ => return Err(CommandError::BadUnit(s.to_owned())),
    };
    let v: f64 = num
        .parse()
        .map_err(|_| CommandError::NotANumber(num.to_owned()))?;
    if !v.is_finite() {
        return Err(CommandError::NotANumber(num.to_owned()));
    }
    Ok(v * mult)
}

/// Validate a simulation rate.
pub fn check_rate(rate: f64) -> Result<f64, CommandError> {
    if rate.is_finite() && (MIN_RATE..=MAX_RATE).contains(&rate.abs()) {
        Ok(rate)
    } else {
        Err(CommandError::RateRange(rate))
    }
}

/// `:seek` argument: `now`, a signed offset from the current clock time, or an absolute UTC epoch.
pub fn parse_seek(arg: &str) -> Result<Command, CommandError> {
    let arg = arg.trim();
    if arg.eq_ignore_ascii_case("now") {
        return Ok(Command::Live);
    }
    if arg.starts_with('+') || arg.starts_with('-') {
        return Ok(Command::SeekRelative(parse_offset(arg)?));
    }
    let iso = arg.strip_suffix('Z').unwrap_or(arg);
    Ok(Command::Seek(Utc::parse(iso)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_parse_with_units_and_signs() {
        assert_eq!(parse_offset("+2h"), Ok(7200.0));
        assert_eq!(parse_offset("-30m"), Ok(-1800.0));
        assert_eq!(parse_offset("90s"), Ok(90.0));
        assert_eq!(parse_offset("1.5d"), Ok(129_600.0));
        assert_eq!(parse_offset("45"), Ok(45.0), "a bare number is seconds");
        assert!(matches!(parse_offset("2x"), Err(CommandError::BadUnit(_))));
        assert!(matches!(parse_offset("abc"), Err(CommandError::BadUnit(_))));
        assert!(matches!(
            parse_offset("hh"),
            Err(CommandError::NotANumber(_))
        ));
        assert_eq!(parse_offset(""), Err(CommandError::Empty));
        assert!(parse_offset("infh").is_err() && parse_offset("nanm").is_err());
    }

    #[test]
    fn rates_are_range_checked_in_both_directions() {
        assert_eq!(check_rate(60.0), Ok(60.0));
        assert_eq!(check_rate(-10_000.0), Ok(-10_000.0));
        for bad in [0.0, 0.001, 10_001.0, -50_000.0, f64::NAN, f64::INFINITY] {
            assert!(check_rate(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn seek_accepts_now_offsets_and_absolute_epochs() {
        assert_eq!(parse_seek("now"), Ok(Command::Live));
        assert_eq!(parse_seek("+2h"), Ok(Command::SeekRelative(7200.0)));
        assert_eq!(parse_seek("-1d"), Ok(Command::SeekRelative(-86_400.0)));
        let abs = Utc::parse("2026-10-25T12:00:00").unwrap();
        assert_eq!(parse_seek("2026-10-25T12:00:00"), Ok(Command::Seek(abs)));
        assert_eq!(
            parse_seek("2026-10-25T12:00:00Z"),
            Ok(Command::Seek(abs)),
            "trailing Z accepted"
        );
        assert!(parse_seek("tomorrow").is_err());
        assert!(parse_seek("2026-13-01T00:00:00").is_err());
        assert!(parse_seek("").is_err());
    }
}
