//! CelesTrak supplemental GP: operator-derived element segments fitted forward from
//! the current epoch, e.g. `ISS-E`. A segment is fitted FORWARD from its epoch, so the segment that
//! answers a query is the latest one whose epoch is not after it (using the nearest epoch instead
//! picked a later segment extrapolated backwards and was ~57 km off the OEM). The set is valid from
//! the first epoch to the last epoch plus one segment spacing.

use mctl_core::state::State;
use mctl_core::time::Utc;

use crate::sgp4model::{GpError, GpModel};
use crate::trajectory::{Trajectory, TrajectoryError};

#[derive(Debug, Clone)]
pub struct SupGp {
    segments: Vec<GpModel>,
    /// Median spacing between segment epochs, seconds: how long the last segment is trusted.
    spacing_s: f64,
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
        let spacing = gaps.get(gaps.len() / 2).copied().unwrap_or(6.0 * 3600.0);
        let norad = segments[0].norad;
        Ok(Self {
            segments,
            spacing_s: spacing,
            norad,
        })
    }

    pub fn len(&self) -> usize {
        self.segments.len()
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// The segment that answers for `t`: the latest whose epoch is not after `t`, or the first one
    /// when `t` precedes them all (validity then decides whether that is acceptable).
    pub fn segment_for(&self, t: Utc) -> &GpModel {
        let i = self.segments.partition_point(|g| g.epoch.0 <= t.0);
        &self.segments[i.saturating_sub(1)]
    }

    /// Epoch of the newest segment, used as the element-set age for staleness.
    pub fn newest_epoch(&self) -> Utc {
        self.segments[self.segments.len() - 1].epoch
    }
}

impl Trajectory for SupGp {
    fn state_gcrf(&self, t: Utc) -> Result<State, TrajectoryError> {
        match self.validity() {
            Some((a, b)) if t.0 >= a.0 && t.0 <= b.0 => self.segment_for(t).state_gcrf(t),
            _ => Err(TrajectoryError::OutOfSpan),
        }
    }

    fn validity(&self) -> Option<(Utc, Utc)> {
        Some((
            self.segments[0].epoch,
            self.newest_epoch().plus(self.spacing_s),
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
    fn the_latest_segment_not_after_t_answers_and_the_span_ends_one_spacing_after_the_last() {
        let s = SupGp::parse(&three_segments()).unwrap();
        assert_eq!(s.len(), 3);
        assert_eq!(
            s.segment_for(Utc::parse("2026-10-23T11:59:00").unwrap())
                .name,
            "ISS [Segment 01]",
            "not yet the 12:00 segment"
        );
        assert_eq!(
            s.segment_for(Utc::parse("2026-10-23T12:00:00").unwrap())
                .name,
            "ISS [Segment 02]"
        );
        assert_eq!(
            s.segment_for(Utc::parse("2026-10-23T17:59:00").unwrap())
                .name,
            "ISS [Segment 02]"
        );
        let (a, b) = s.validity().unwrap();
        assert_eq!(a, Utc::parse("2026-10-23T06:00:00").unwrap());
        assert_eq!(b, Utc::parse("2026-10-24T00:00:00").unwrap());
        assert!(
            s.state_gcrf(Utc::parse("2026-10-23T12:00:00").unwrap())
                .is_ok()
        );
        assert_eq!(
            s.state_gcrf(Utc::parse("2026-10-24T01:00:00").unwrap()),
            Err(TrajectoryError::OutOfSpan)
        );
        assert_eq!(
            s.state_gcrf(Utc::parse("2026-10-23T05:00:00").unwrap()),
            Err(TrajectoryError::OutOfSpan),
            "before the first segment's epoch"
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

    /// Diagnostic against the cached real files: how far each segment is from the OEM, along-track,
    /// at and after its epoch. `cargo test -p mctl-astro supgp_vs_oem -- --ignored --nocapture`.
    #[test]
    #[ignore = "reads ~/.cache/mctl"]
    fn supgp_vs_oem() {
        use crate::local_frames::{LocalFrame, to_local};
        let home = std::env::var("HOME").unwrap();
        let sup = SupGp::parse(
            &std::fs::read_to_string(format!("{home}/.cache/mctl/supgp-25544.json")).unwrap(),
        )
        .unwrap();
        let oem = crate::oem::Oem::parse(
            &std::fs::read_to_string(format!("{home}/.cache/mctl/oem-iss.txt")).unwrap(),
        )
        .unwrap();
        let (a, b) = oem.span();
        for seg in sup
            .segments
            .iter()
            .filter(|g| g.epoch.0 > a.0 && g.epoch.0 < b.0 - 4.0 * 3600.0)
            .step_by(6)
        {
            let line: Vec<String> = [0.0, 3600.0, 3.0 * 3600.0, 5.9 * 3600.0]
                .iter()
                .map(|dt| {
                    let t = seg.epoch.plus(*dt);
                    let (o, g) = (oem.interpolate(t).unwrap(), seg.state_gcrf(t).unwrap());
                    let d = to_local(LocalFrame::Rtn, &o, &(g.r - o.r)).unwrap();
                    format!("+{:.1}h T={:+.1} R={:+.1}", dt / 3600.0, d.y, d.x)
                })
                .collect();
            println!("{} {}", seg.epoch.iso(), line.join(" | "));
        }
    }
}
