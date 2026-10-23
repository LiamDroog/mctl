//! Vallado SGP4 verification vectors (AIAA 2006-6753, Dec 2015 reference output).
//!
//! `SGP4-VER.TLE` lists test satellites with `startmfe stopmfe deltamin` appended to line 2.
//! `tmatverDec2015.out` holds one block per case, in file order, headed `<satnum> xx`, with rows
//! `tsince r(3) v(3) ...`. A satnum can repeat (20413 appears twice), so blocks are matched to
//! cases by position, not by number.

use thiserror::Error;

use crate::geom::State;

/// Standard TLE line length; anything beyond is the verification schedule.
const TLE_LINE_LEN: usize = 69;

#[derive(Debug, Error)]
pub enum ValladoError {
    #[error("TLE file: {0}")]
    Tle(String),
    #[error("reference file: {0}")]
    Reference(String),
    #[error("{tles} TLE cases but {blocks} reference blocks")]
    CountMismatch { tles: usize, blocks: usize },
    #[error("case {index}: TLE satnum {tle} but reference block satnum {block}")]
    SatnumMismatch { index: usize, tle: u32, block: u32 },
}

/// TLE line checksum: digits sum, `-` counts as 1, modulo 10, over the first 68 characters.
pub fn tle_checksum(line: &str) -> u32 {
    line.chars()
        .take(68)
        .map(|c| c.to_digit(10).unwrap_or(u32::from(c == '-')))
        .sum::<u32>()
        % 10
}

/// Make a verification TLE acceptable to strict parsers without changing its orbital content:
/// a blank ephemeris-type field becomes `0`, and both line checksums are recomputed.
///
/// Some cases in the Vallado file carry deliberately wrong checksums (they probe SGP4 error
/// codes, not the parser) and one has a blank ephemeris-type field, as old catalogue TLEs do.
pub fn repair_tle(line1: &str, line2: &str) -> (String, String) {
    let mut l1: Vec<char> = line1.chars().collect();
    if l1.get(62) == Some(&' ') {
        l1[62] = '0';
    }
    let fix = |chars: &[char]| {
        let body: String = chars.iter().take(68).collect();
        format!("{body}{}", tle_checksum(&body))
    };
    let l2: Vec<char> = line2.chars().collect();
    (fix(&l1), fix(&l2))
}

#[derive(Debug, Clone)]
pub struct RefRow {
    pub tsince_min: f64,
    pub state: State,
}

#[derive(Debug, Clone)]
pub struct Case {
    pub satnum: u32,
    pub line1: String,
    pub line2: String,
    pub start_min: f64,
    pub stop_min: f64,
    pub step_min: f64,
    pub reference: Vec<RefRow>,
}

impl Case {
    /// Times the verification schedule asks for, minutes since epoch.
    pub fn scheduled_times(&self) -> Vec<f64> {
        let mut out = Vec::new();
        let mut k = 0.0;
        while self.start_min + k * self.step_min <= self.stop_min + 1e-9 {
            out.push(self.start_min + k * self.step_min);
            k += 1.0;
        }
        out
    }
}

/// One parsed TLE pair with its verification schedule `[start, stop, step]` in minutes.
type TleEntry = (u32, String, String, [f64; 3]);

fn parse_tles(text: &str) -> Result<Vec<TleEntry>, ValladoError> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .collect();
    if !lines.len().is_multiple_of(2) {
        return Err(ValladoError::Tle("odd number of TLE lines".into()));
    }
    lines
        .chunks(2)
        .enumerate()
        .map(|(i, p)| {
            let (l1, l2) = (p[0], p[1]);
            if !l1.starts_with("1 ")
                || !l2.starts_with("2 ")
                || l1.len() < TLE_LINE_LEN
                || l2.len() < TLE_LINE_LEN
            {
                return Err(ValladoError::Tle(format!("case {i}: malformed TLE pair")));
            }
            let satnum = l1[2..7]
                .trim()
                .parse()
                .map_err(|_| ValladoError::Tle(format!("case {i}: bad satnum")))?;
            let extra: Vec<f64> = l2[TLE_LINE_LEN..]
                .split_whitespace()
                .map(|x| {
                    x.parse()
                        .map_err(|_| ValladoError::Tle(format!("case {i}: bad schedule {x:?}")))
                })
                .collect::<Result<_, _>>()?;
            let [a, b, c] = extra[..] else {
                return Err(ValladoError::Tle(format!(
                    "case {i}: expected 3 schedule values, found {}",
                    extra.len()
                )));
            };
            if c <= 0.0 {
                return Err(ValladoError::Tle(format!("case {i}: non-positive step")));
            }
            Ok((
                satnum,
                l1[..TLE_LINE_LEN].to_owned(),
                l2[..TLE_LINE_LEN].to_owned(),
                [a, b, c],
            ))
        })
        .collect()
}

fn parse_reference(text: &str) -> Result<Vec<(u32, Vec<RefRow>)>, ValladoError> {
    let mut blocks: Vec<(u32, Vec<RefRow>)> = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let p: Vec<&str> = line.split_whitespace().collect();
        if p.len() == 2 && p[1] == "xx" {
            blocks.push((
                p[0].parse()
                    .map_err(|_| ValladoError::Reference(format!("line {}: bad satnum", n + 1)))?,
                Vec::new(),
            ));
        } else if p.len() >= 7 {
            let block = blocks.last_mut().ok_or_else(|| {
                ValladoError::Reference(format!("line {}: data before first header", n + 1))
            })?;
            let v: Vec<f64> = p[..7]
                .iter()
                .map(|x| {
                    x.parse().map_err(|_| {
                        ValladoError::Reference(format!("line {}: not a number {x:?}", n + 1))
                    })
                })
                .collect::<Result<_, _>>()?;
            block.1.push(RefRow {
                tsince_min: v[0],
                state: State::new([v[1], v[2], v[3]], [v[4], v[5], v[6]]),
            });
        }
    }
    Ok(blocks)
}

pub fn load(tle_text: &str, reference_text: &str) -> Result<Vec<Case>, ValladoError> {
    let tles = parse_tles(tle_text)?;
    let blocks = parse_reference(reference_text)?;
    if tles.len() != blocks.len() {
        return Err(ValladoError::CountMismatch {
            tles: tles.len(),
            blocks: blocks.len(),
        });
    }
    let mut previous_last: Option<RefRow> = None;
    let mut cases = Vec::new();
    for (index, ((satnum, line1, line2, [start, stop, step]), (block_sat, rows))) in
        tles.into_iter().zip(blocks).enumerate()
    {
        if satnum != block_sat {
            return Err(ValladoError::SatnumMismatch {
                index,
                tle: satnum,
                block: block_sat,
            });
        }
        let raw_last = rows.last().cloned();
        let mut case = Case {
            satnum,
            line1,
            line2,
            start_min: start,
            stop_min: stop,
            step_min: step,
            reference: Vec::new(),
        };
        let scheduled = case.scheduled_times();
        // Reference cleaning, each rule justified by the data itself:
        // 1. A row off the case's schedule (other than t=0) is not part of the verification set. The
        //    Matlab output has one such stray row (t=120 min, case 20413's long run) that is absent from
        //    the Fortran output of the same cases.
        // 2. A block whose only row repeats the previous block's last state is stale output from a failed
        //    initialisation (case 33334); the reference is "error", not that state.
        let on_schedule = |t: f64| t == 0.0 || scheduled.iter().any(|s| (s - t).abs() < 1e-6);
        let mut kept: Vec<RefRow> = rows
            .into_iter()
            .filter(|r| on_schedule(r.tsince_min))
            .collect();
        if let (Some(prev), [only]) = (&previous_last, kept.as_slice())
            && only.state == prev.state
        {
            kept.clear();
        }
        previous_last = raw_last;
        case.reference = kept;
        cases.push(case);
    }
    Ok(cases)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TLE: &str = include_str!("../fixtures/SGP4-VER.TLE");
    const OUT: &str = include_str!("../fixtures/tmatverDec2015.out");

    #[test]
    fn real_files_pair_up() {
        let cases = load(TLE, OUT).unwrap();
        assert_eq!(cases.len(), 33);
        let empty: Vec<u32> = cases
            .iter()
            .filter(|c| c.reference.is_empty())
            .map(|c| c.satnum)
            .collect();
        assert_eq!(
            empty,
            [33334],
            "only the stale-output case has no valid reference rows"
        );
        assert_eq!(
            cases.iter().filter(|c| c.satnum == 20413).count(),
            2,
            "satnum repeats, so matching must be positional"
        );
        assert_eq!(cases[0].reference[0].tsince_min, 0.0);
    }

    #[test]
    fn mismatches_are_errors() {
        assert!(matches!(
            load(TLE, &OUT.replacen("5 xx", "6 xx", 1)),
            Err(ValladoError::SatnumMismatch { .. })
        ));
        let truncated: String = OUT.lines().take(100).map(|l| format!("{l}\n")).collect();
        assert!(matches!(
            load(TLE, &truncated),
            Err(ValladoError::CountMismatch { .. })
        ));
        assert!(load("1 2", OUT).is_err());
        assert!(load(TLE, "1.0 2.0 3.0 4.0 5.0 6.0 7.0").is_err());
    }

    #[test]
    fn checksum_of_a_known_tle_line() {
        // ISS TLE from the sgp4 crate documentation; its published line checksums are 2 and 2.
        let l1 = "1 25544U 98067A   20194.88612269 -.00002218  00000-0 -31515-4 0  9992";
        let l2 = "2 25544  51.6461 221.2784 0001413  89.1723 280.4612 15.49507896236008";
        assert_eq!(tle_checksum(l1), 2);
        assert_eq!(tle_checksum(l2), 8);
        let (r1, r2) = repair_tle(l1, l2);
        assert_eq!(
            (r1.as_str(), r2.as_str()),
            (l1, l2),
            "a valid TLE is unchanged"
        );
    }

    #[test]
    fn repair_fixes_blank_ephemeris_type_and_checksum() {
        let c = load(TLE, OUT).unwrap();
        let blank = c.iter().find(|c| c.satnum == 11801).unwrap();
        assert_eq!(blank.line1.chars().nth(62), Some(' '));
        let (l1, _) = repair_tle(&blank.line1, &blank.line2);
        assert_eq!(l1.chars().nth(62), Some('0'));
        assert_eq!(l1.len(), 69);
    }

    #[test]
    fn stray_off_schedule_row_is_dropped_but_t0_is_kept() {
        let cases = load(TLE, OUT).unwrap();
        let long = cases.iter().filter(|c| c.satnum == 20413).nth(1).unwrap();
        assert!(
            long.reference
                .iter()
                .all(|r| r.tsince_min == 0.0 || r.tsince_min >= 1_844_000.0),
            "t=120 stray row removed"
        );
        assert_eq!(long.reference.len(), 70);
        assert_eq!(long.reference[0].tsince_min, 0.0);
    }

    #[test]
    fn schedule_matches_the_tle_extras() {
        let c = &load(TLE, OUT).unwrap()[0];
        let t = c.scheduled_times();
        assert_eq!(
            (t[0], *t.last().unwrap(), t[1] - t[0]),
            (0.0, 4320.0, 360.0)
        );
    }
}
