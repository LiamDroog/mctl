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
