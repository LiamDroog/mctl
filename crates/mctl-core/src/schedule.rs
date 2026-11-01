//! Public observing schedules of the space telescopes (JWST weekly visit lists, Hubble daily reports).
//!
//! These say what the observatory is doing, not how it is: no engineering telemetry is published for
//! either. JWST times are plans; executed observations can differ (unplanned events, targets of
//! opportunity).

use crate::time::Utc;

/// One JWST visit and the parallel observations attached to it.
#[derive(Debug, Clone, PartialEq)]
pub struct Visit {
    /// `program:observation:visit`.
    pub id: String,
    pub start: Utc,
    pub duration_s: f64,
    /// e.g. `PRIME TARGETED FIXED`, `FULL BIASED MOMENTUM UNLOAD`.
    pub kind: String,
    pub pcs_mode: String,
    /// Prime instrument and mode; empty for a maintenance activity.
    pub instrument: String,
    pub target: String,
    pub category: String,
    /// Instruments running in parallel, as published.
    pub parallels: Vec<String>,
}

impl Visit {
    pub fn end(&self) -> Utc {
        self.start.plus(self.duration_s)
    }

    /// A momentum unload or other activity without a science instrument.
    pub fn is_maintenance(&self) -> bool {
        self.instrument.is_empty()
    }

    /// The instrument family, e.g. `NIRSpec` from `NIRSpec MultiObject Spectroscopy`.
    pub fn instrument_family(&self) -> &str {
        self.instrument
            .split_whitespace()
            .next()
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct JwstSchedule {
    /// Visits in start-time order.
    pub visits: Vec<Visit>,
    /// Source files read, newest first.
    pub files: Vec<String>,
    pub fetched_at: Utc,
}

impl JwstSchedule {
    /// The visit in progress at `t`, if any.
    pub fn current(&self, t: Utc) -> Option<&Visit> {
        self.visits
            .iter()
            .find(|v| v.start.0 <= t.0 && t.0 < v.end().0)
    }

    /// Visits starting after `t`.
    pub fn upcoming(&self, t: Utc) -> impl Iterator<Item = &Visit> {
        self.visits.iter().filter(move |v| v.start.0 > t.0)
    }

    /// Hours of prime observing per instrument family inside `[from, to]` (visits are clipped to the
    /// window; maintenance activities are counted as `MAINT`). Largest first.
    pub fn hours_by_instrument(&self, from: Utc, to: Utc) -> Vec<(String, f64)> {
        let mut out: Vec<(String, f64)> = Vec::new();
        for v in &self.visits {
            let (a, b) = (v.start.0.max(from.0), v.end().0.min(to.0));
            if b <= a {
                continue;
            }
            let family = if v.is_maintenance() {
                "MAINT".to_owned()
            } else {
                v.instrument_family().to_owned()
            };
            match out.iter_mut().find(|(f, _)| *f == family) {
                Some((_, h)) => *h += (b - a) / 3600.0,
                None => out.push((family, (b - a) / 3600.0)),
            }
        }
        out.sort_by(|a, b| b.1.total_cmp(&a.1));
        out
    }

    /// Span of time the schedule covers.
    pub fn span(&self) -> Option<(Utc, Utc)> {
        let first = self.visits.first()?.start;
        let last = self
            .visits
            .iter()
            .map(|v| v.end().0)
            .fold(f64::MIN, f64::max);
        Some((first, Utc(last)))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct HstProgram {
    pub id: String,
    pub investigator: String,
    pub title: String,
}

/// One Hubble daily status report.
#[derive(Debug, Clone, PartialEq)]
pub struct HstReport {
    pub number: u32,
    /// The period the report covers, as published (local-time wording).
    pub period: String,
    pub science: Vec<HstProgram>,
    pub calibration: Vec<HstProgram>,
    pub other: Vec<HstProgram>,
    pub url: String,
    pub fetched_at: Utc,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Ops {
    Jwst(std::sync::Arc<JwstSchedule>),
    Hst(std::sync::Arc<HstReport>),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn visit(id: &str, start: f64, dur: f64, instrument: &str) -> Visit {
        Visit {
            id: id.into(),
            start: Utc(start),
            duration_s: dur,
            kind: "PRIME TARGETED FIXED".into(),
            pcs_mode: String::new(),
            instrument: instrument.into(),
            target: "T".into(),
            category: String::new(),
            parallels: vec![],
        }
    }

    fn sched() -> JwstSchedule {
        JwstSchedule {
            visits: vec![
                visit("a", 0.0, 3600.0, "NIRSpec MultiObject Spectroscopy"),
                visit("b", 3600.0, 7200.0, "NIRCam Imaging"),
                visit("c", 10_800.0, 1800.0, "NIRSpec Fixed Slit"),
                visit("m", 12_600.0, 600.0, ""),
            ],
            files: vec![],
            fetched_at: Utc(0.0),
        }
    }

    #[test]
    fn current_and_upcoming_visits() {
        let s = sched();
        assert_eq!(s.current(Utc(4000.0)).unwrap().id, "b");
        assert!(s.current(Utc(-1.0)).is_none());
        // A visit ends exactly when the next begins; at that instant the next one is current.
        assert_eq!(s.current(Utc(3600.0)).unwrap().id, "b");
        let up: Vec<&str> = s.upcoming(Utc(4000.0)).map(|v| v.id.as_str()).collect();
        assert_eq!(up, ["c", "m"]);
        assert_eq!(s.span(), Some((Utc(0.0), Utc(13_200.0))));
    }

    #[test]
    fn hours_per_instrument_are_clipped_to_the_window_and_sorted() {
        let h = sched().hours_by_instrument(Utc(1800.0), Utc(12_600.0));
        // NIRCam 2 h; NIRSpec 0.5 h (clipped) + 0.5 h; the maintenance visit starts at the window end.
        assert_eq!(h[0].0, "NIRCam");
        assert!((h[0].1 - 2.0).abs() < 1e-9);
        assert_eq!(h[1].0, "NIRSpec");
        assert!((h[1].1 - 1.0).abs() < 1e-9);
        assert_eq!(h.len(), 2);
        let all = sched().hours_by_instrument(Utc(0.0), Utc(20_000.0));
        assert!(all.iter().any(|(f, _)| f == "MAINT"));
    }
}

/// A request for a picture of what an observatory is pointed at.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FieldRequest {
    /// A JWST target by name, as the visit list gives it.
    Target(String),
    /// The first positioned target of a Hubble program (number and title for the label).
    HstProgram(String),
}

impl FieldRequest {
    pub fn key(&self) -> String {
        match self {
            Self::Target(n) => format!("target:{n}"),
            Self::HstProgram(id) => format!("hst:{id}"),
        }
    }
}

/// Where the request stands.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldState {
    Loading,
    /// Nothing to show, and why (name not resolvable, program has only solar-system targets, ...).
    Unavailable(String),
    Ready {
        /// Resolved target name and position, ICRS degrees.
        target: String,
        ra_deg: f64,
        dec_deg: f64,
        fov_deg: f64,
        /// How the position was found, e.g. `SIMBAD via Sesame`.
        origin: String,
        survey: String,
        /// The cutout as sent by its service: a JPEG, or a GIF from the fallback survey.
        jpeg: std::sync::Arc<Vec<u8>>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldView {
    pub key: String,
    /// What the operator sees as the caption's first line (the visit's instrument, or the program).
    pub label: String,
    pub state: FieldState,
}
