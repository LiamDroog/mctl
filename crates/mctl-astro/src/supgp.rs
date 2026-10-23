//! CelesTrak supplemental GP (DESIGN.md §3.1): operator-derived element segments fitted forward from
//! the current epoch, e.g. `ISS-E`. Each segment is only good near its own epoch, so the segment with
//! the nearest epoch answers each query and the span of the whole set is its first and last epoch
//! padded by half a segment spacing.

use mctl_core::state::State;
use mctl_core::time::Utc;

use crate::sgp4model::{GpError, GpModel};
use crate::trajectory::{Trajectory, TrajectoryError};

#[derive(Debug, Clone)]
pub struct SupGp {
    segments: Vec<GpModel>,
    /// Half the median spacing between segment epochs, seconds: how far a segment is trusted.
    half_spacing_s: f64,
    pub norad: u32,
}

impl SupGp {
    pub fn parse(json: &str) -> Result<Self, GpError> {
        let mut segments = GpModel::all_from_omm_json(json)?;
        segments.sort_by(|a, b| a.epoch.0.total_cmp(&b.epoch.0));
        let mut gaps: Vec<f64> = segments
            .windows(2)
            .map(|w| w[1].epoch.minus(w[0].epoch))
            .collect();
        gaps.sort_by(f64::total_cmp);
        // A single segment has no spacing to learn from; trust it for the usual 6 h cadence.
        let half = gaps.get(gaps.len() / 2).map_or(3.0 * 3600.0, |g| g / 2.0);
        let norad = segments[0].norad;
        Ok(Self {
            segments,
            half_spacing_s: half,
            norad,
        })
    }

    pub fn len(&self) -> usize {
        self.segments.len()
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// Epoch of the segment nearest `t`.
    pub fn nearest(&self, t: Utc) -> &GpModel {
        self.segments
            .iter()
            .min_by(|a, b| (a.epoch.0 - t.0).abs().total_cmp(&(b.epoch.0 - t.0).abs()))
            .expect("parse guarantees at least one segment")
    }

    /// Epoch of the newest segment, used as the element-set age for staleness.
    pub fn newest_epoch(&self) -> Utc {
        self.segments[self.segments.len() - 1].epoch
    }
}

impl Trajectory for SupGp {
    fn state_gcrf(&self, t: Utc) -> Result<State, TrajectoryError> {
        match self.validity() {
            Some((a, b)) if t.0 >= a.0 && t.0 <= b.0 => self.nearest(t).state_gcrf(t),
            _ => Err(TrajectoryError::OutOfSpan),
        }
    }

    fn validity(&self) -> Option<(Utc, Utc)> {
        Some((
            self.segments[0].epoch.plus(-self.half_spacing_s),
            self.newest_epoch().plus(self.half_spacing_s),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three 6-hourly segments cut from the real ISS-E file, with distinct epochs.
    fn three_segments() -> String {
        let base: serde_json::Value =
            serde_json::from_str(include_str!("../fixtures/iss-omm.json")).unwrap();
        let rec = base[0].clone();
        let mut out = vec![];
        for (i, h) in ["06", "12", "18"].iter().enumerate() {
            let mut r = rec.clone();
            r["EPOCH"] = format!("2026-10-23T{h}:00:00.000000").into();
            r["OBJECT_NAME"] = format!("ISS [Segment 0{}]", i + 1).into();
            out.push(r);
        }
        serde_json::to_string(&out).unwrap()
    }

    #[test]
    fn the_segment_nearest_in_time_answers_and_the_span_is_padded_by_half_a_spacing() {
        let s = SupGp::parse(&three_segments()).unwrap();
        assert_eq!(s.len(), 3);
        assert_eq!(
            s.nearest(Utc::parse("2026-10-23T10:00:00").unwrap()).name,
            "ISS [Segment 02]"
        );
        assert_eq!(
            s.nearest(Utc::parse("2026-10-23T08:59:00").unwrap()).name,
            "ISS [Segment 01]"
        );
        let (a, b) = s.validity().unwrap();
        assert_eq!(a, Utc::parse("2026-10-23T03:00:00").unwrap());
        assert_eq!(b, Utc::parse("2026-10-23T21:00:00").unwrap());
        assert!(
            s.state_gcrf(Utc::parse("2026-10-23T12:00:00").unwrap())
                .is_ok()
        );
        assert_eq!(
            s.state_gcrf(Utc::parse("2026-10-23T22:00:00").unwrap()),
            Err(TrajectoryError::OutOfSpan)
        );
        assert_eq!(
            s.state_gcrf(Utc::parse("2026-10-22T00:00:00").unwrap()),
            Err(TrajectoryError::OutOfSpan)
        );
    }

    #[test]
    fn segments_sort_by_epoch_whatever_the_file_order() {
        let mut v: Vec<serde_json::Value> = serde_json::from_str(&three_segments()).unwrap();
        v.reverse();
        let s = SupGp::parse(&serde_json::to_string(&v).unwrap()).unwrap();
        assert_eq!(s.newest_epoch(), Utc::parse("2026-10-23T18:00:00").unwrap());
    }

    #[test]
    fn an_empty_or_non_json_response_is_an_error() {
        assert!(SupGp::parse("[]").is_err());
        assert!(SupGp::parse("No SupGP data found").is_err());
    }
}
