//! Caution and warning: rule definitions, the per-rule state machine, and the
//! master-annunciator latch. Pure logic; the engine feeds it values and publishes the result.

use std::str::FromStr;

use serde::Deserialize;

use crate::config::Limits;
use crate::state::Quality;
use crate::time::{Utc, fmt_dur};
use crate::world::{EopView, Health, SourceStatus, VehicleView};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Stale,
    Caution,
    Warning,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Self::Stale => "STALE",
            Self::Caution => "CAUT",
            Self::Warning => "WARN",
        }
    }
}

/// Displayed state of a rule (FR-41).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CwState {
    Nom,
    Caution,
    Warning,
    /// The value exists but is older than the rule allows.
    Stale,
    /// The value does not exist.
    NoData,
}

impl CwState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Nom => "NOM",
            Self::Caution => "CAUT",
            Self::Warning => "WARN",
            Self::Stale => "STALE",
            Self::NoData => "NO DATA",
        }
    }

    /// Severity for annunciation; NOM and NO DATA do not annunciate.
    pub fn severity(self) -> Option<Severity> {
        match self {
            Self::Warning => Some(Severity::Warning),
            Self::Caution => Some(Severity::Caution),
            Self::Stale => Some(Severity::Stale),
            Self::Nom | Self::NoData => None,
        }
    }
}

/// A value a rule watches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Metric {
    /// A telemetry item's numeric value.
    Item(String),
    /// Age of the vehicle's GP element set at the clock time, hours.
    GpAgeHours(String),
    /// Largest inter-source difference for the vehicle, km.
    ResidualKm(String),
    /// 1 when the telemetry feed is not delivering, else 0.
    TelemetryFeedDown,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error(
    "metric {0:?} is not item:<id>, gp_age_h:<vehicle>, residual_km:<vehicle> or telemetry_feed_down"
)]
pub struct MetricParseError(String);

impl FromStr for Metric {
    type Err = MetricParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || MetricParseError(s.to_owned());
        if s == "telemetry_feed_down" {
            return Ok(Self::TelemetryFeedDown);
        }
        let (kind, arg) = s.split_once(':').ok_or_else(bad)?;
        if arg.is_empty() {
            return Err(bad());
        }
        match kind {
            "item" => Ok(Self::Item(arg.into())),
            "gp_age_h" => Ok(Self::GpAgeHours(arg.into())),
            "residual_km" => Ok(Self::ResidualKm(arg.into())),
            _ => Err(bad()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// Alarm when the value rises to or above a threshold.
    Above,
    /// Alarm when the value falls to or below a threshold.
    Below,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleDef {
    pub id: String,
    pub title: String,
    #[serde(deserialize_with = "de_metric")]
    pub metric: Metric,
    pub direction: Direction,
    pub caution: Option<f64>,
    pub warning: Option<f64>,
    /// How far past a threshold the value must come back before the level clears.
    #[serde(default)]
    pub hysteresis: f64,
    /// How long a worse level must hold before it is declared, seconds.
    #[serde(default)]
    pub persist_s: f64,
    /// Telemetry/age data older than this is STALE rather than evaluated, seconds.
    #[serde(default = "default_stale_after")]
    pub stale_after_s: f64,
    pub units: String,
    /// Why these limits: "illustrative", "user", or a citation. Shown with the rule, never omitted.
    pub basis: String,
    /// The vehicle the rule is about, for rules whose metric does not name one (an `item:` rule is
    /// about whichever vehicle owns the feed). Rules about no vehicle in particular apply everywhere.
    #[serde(default)]
    pub vehicle: Option<String>,
}

fn default_stale_after() -> f64 {
    60.0
}

fn de_metric<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Metric, D::Error> {
    let s = String::deserialize(d)?;
    s.parse().map_err(serde::de::Error::custom)
}

impl RuleDef {
    /// The vehicle this rule is shown for: the explicit `vehicle`, else the one in the metric.
    pub fn scope(&self) -> Option<&str> {
        self.vehicle.as_deref().or(match &self.metric {
            Metric::GpAgeHours(v) | Metric::ResidualKm(v) => Some(v.as_str()),
            _ => None,
        })
    }

    pub fn validate(&self) -> Result<(), String> {
        let id = &self.id;
        if self.caution.is_none() && self.warning.is_none() {
            return Err(format!("rule {id}: needs a caution or a warning threshold"));
        }
        if let (Some(c), Some(w)) = (self.caution, self.warning) {
            let ordered = match self.direction {
                Direction::Above => w >= c,
                Direction::Below => w <= c,
            };
            if !ordered {
                return Err(format!(
                    "rule {id}: warning threshold {w} is not beyond caution {c} for {:?}",
                    self.direction
                ));
            }
        }
        let finite = [self.caution, self.warning]
            .into_iter()
            .flatten()
            .chain([self.hysteresis, self.persist_s, self.stale_after_s])
            .all(f64::is_finite);
        if !finite || self.hysteresis < 0.0 || self.persist_s < 0.0 || self.stale_after_s <= 0.0 {
            return Err(format!(
                "rule {id}: thresholds must be finite, hysteresis and persist_s >= 0, stale_after_s > 0"
            ));
        }
        if self.basis.trim().is_empty() {
            return Err(format!(
                "rule {id}: basis must say where the limits come from"
            ));
        }
        Ok(())
    }
}

/// What a rule's metric is right now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Sample {
    Missing,
    Stale,
    Value(f64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    Nom,
    Caution,
    Warning,
}

/// Per-rule state machine: thresholds with hysteresis, and escalation only after `persist_s`.
#[derive(Debug, Clone)]
pub struct Tracker {
    level: Level,
    pending: Option<(Level, f64)>,
    state: CwState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    pub from: CwState,
    pub to: CwState,
}

impl Default for Tracker {
    fn default() -> Self {
        Self {
            level: Level::Nom,
            pending: None,
            state: CwState::NoData,
        }
    }
}

impl Tracker {
    pub fn state(&self) -> CwState {
        self.state
    }

    fn engaged(def: &RuleDef, threshold: Option<f64>, was_engaged: bool, v: f64) -> bool {
        let Some(t) = threshold else { return false };
        match (def.direction, was_engaged) {
            (Direction::Above, false) => v >= t,
            (Direction::Above, true) => v >= t - def.hysteresis,
            (Direction::Below, false) => v <= t,
            (Direction::Below, true) => v <= t + def.hysteresis,
        }
    }

    /// Advance to wall time `now_s`. Returns the transition when the displayed state changed.
    pub fn update(&mut self, def: &RuleDef, now_s: f64, sample: Sample) -> Option<Transition> {
        let before = self.state;
        let v = match sample {
            Sample::Value(v) if v.is_finite() => v,
            Sample::Stale => {
                (self.level, self.pending, self.state) = (Level::Nom, None, CwState::Stale);
                return (before != self.state).then_some(Transition {
                    from: before,
                    to: self.state,
                });
            }
            _ => {
                (self.level, self.pending, self.state) = (Level::Nom, None, CwState::NoData);
                return (before != self.state).then_some(Transition {
                    from: before,
                    to: self.state,
                });
            }
        };
        let warn = Self::engaged(def, def.warning, self.level == Level::Warning, v);
        let caut = Self::engaged(def, def.caution, self.level >= Level::Caution, v);
        let raw = if warn {
            Level::Warning
        } else if caut {
            Level::Caution
        } else {
            Level::Nom
        };
        if raw > self.level {
            let since = match self.pending {
                Some((p, s)) if p == raw => s,
                _ => now_s,
            };
            self.pending = Some((raw, since));
            if now_s - since >= def.persist_s {
                self.level = raw;
                self.pending = None;
            }
        } else {
            // Improvement and steady state apply at once; only escalation waits.
            self.level = raw;
            self.pending = None;
        }
        self.state = match self.level {
            Level::Nom => CwState::Nom,
            Level::Caution => CwState::Caution,
            Level::Warning => CwState::Warning,
        };
        (before != self.state).then_some(Transition {
            from: before,
            to: self.state,
        })
    }
}

impl PartialOrd for Level {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Level {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        (*self as u8).cmp(&(*o as u8))
    }
}

/// Master caution/warning latch (FR-42): sets on any rise to that severity, clears only on acknowledge.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Latch {
    pub caution: bool,
    pub warning: bool,
    pub acked_at: Option<Utc>,
}

impl Latch {
    pub fn raise(&mut self, s: Severity) {
        match s {
            Severity::Warning => self.warning = true,
            Severity::Caution => self.caution = true,
            Severity::Stale => {}
        }
    }

    pub fn ack(&mut self, at: Utc) {
        *self = Self {
            caution: false,
            warning: false,
            acked_at: Some(at),
        };
    }
}

/// A standing condition derived from source health (not a threshold rule): feed failures, stale or
/// failed sources, disagreement between sources, assumed EOP.
#[derive(Debug, Clone, PartialEq)]
pub struct Condition {
    /// `None` for conditions that apply to every vehicle.
    pub vehicle: Option<String>,
    pub severity: Severity,
    pub text: String,
}

impl Condition {
    /// Identity for transition detection: the same condition keeps the same key while it persists.
    pub fn key(&self) -> String {
        format!(
            "{}|{}|{}",
            self.vehicle.as_deref().unwrap_or("*"),
            self.severity.label(),
            self.text
        )
    }
}

pub fn eop_condition(eop: &EopView) -> Option<Condition> {
    matches!(eop, EopView::Assumed { .. }).then(|| Condition {
        vehicle: None,
        severity: Severity::Caution,
        text: "EOP ASSUMED 0 (UT1 UP TO 0.9 S)".into(),
    })
}

/// Conditions for one vehicle from its source health, residuals and solution.
pub fn vehicle_conditions(v: &VehicleView, limits: &Limits) -> Vec<Condition> {
    let mut out = Vec::new();
    let mut add = |severity, text: String| {
        out.push(Condition {
            vehicle: Some(v.id.clone()),
            severity,
            text,
        })
    };
    if v.solution.is_none() {
        add(
            Severity::Warning,
            format!("{} NO TRAJECTORY SOLUTION", v.name),
        );
    }
    for s in &v.sources {
        match (&s.health, &s.status) {
            (_, SourceStatus::Failed(why)) => add(
                Severity::Warning,
                format!("{} {} FAILED: {}", v.name, s.kind.label(), why),
            ),
            (Health::Failing(why), _) => add(
                Severity::Caution,
                format!("{} {} FETCH FAILING: {}", v.name, s.kind.label(), why),
            ),
            (Health::Offline, _) => add(
                Severity::Stale,
                format!("{} {} OFFLINE, CACHE ONLY", v.name, s.kind.label()),
            ),
            _ => {}
        }
        if s.status == SourceStatus::Stale {
            add(
                Severity::Caution,
                format!(
                    "{} {} DATA STALE ({})",
                    v.name,
                    s.kind.label(),
                    s.age_s.map_or("--".into(), |a| fmt_dur(a.abs()))
                ),
            );
        }
        if let Some(r) = s.residual {
            let (a, b) = (
                v.solution.as_ref().map_or("?", |x| x.source.label()),
                s.kind.label(),
            );
            if r.norm_km >= limits.residual_warning_km {
                add(
                    Severity::Warning,
                    format!("{a} vs {b} DIFFER {:.1} KM", r.norm_km),
                );
            } else if r.norm_km >= limits.residual_caution_km {
                add(
                    Severity::Caution,
                    format!("{a} vs {b} DIFFER {:.1} KM", r.norm_km),
                );
            }
        }
    }
    if v.solution
        .as_ref()
        .is_some_and(|s| s.quality == Quality::Stale)
    {
        add(Severity::Caution, format!("{} SOLUTION IS STALE", v.name));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(dir: Direction, c: Option<f64>, w: Option<f64>, hyst: f64, persist: f64) -> RuleDef {
        RuleDef {
            id: "r".into(),
            title: "T".into(),
            metric: Metric::Item("X".into()),
            direction: dir,
            caution: c,
            warning: w,
            hysteresis: hyst,
            persist_s: persist,
            stale_after_s: 60.0,
            units: "u".into(),
            basis: "test".into(),
            vehicle: None,
        }
    }

    #[test]
    fn a_rules_scope_is_its_explicit_vehicle_else_the_one_in_its_metric() {
        let mut r = rule(Direction::Above, Some(1.0), None, 0.0, 0.0);
        assert_eq!(r.scope(), None, "an item rule names no vehicle by itself");
        r.vehicle = Some("iss".into());
        assert_eq!(r.scope(), Some("iss"));
        r.vehicle = None;
        r.metric = Metric::GpAgeHours("hubble".into());
        assert_eq!(r.scope(), Some("hubble"));
        r.metric = Metric::TelemetryFeedDown;
        assert_eq!(r.scope(), None);
    }

    fn run(t: &mut Tracker, d: &RuleDef, steps: &[(f64, f64)]) -> Vec<CwState> {
        steps
            .iter()
            .map(|&(now, v)| {
                t.update(d, now, Sample::Value(v));
                t.state()
            })
            .collect()
    }

    #[test]
    fn metrics_parse_and_reject_garbage() {
        assert_eq!(
            "item:USLAB000058".parse(),
            Ok(Metric::Item("USLAB000058".into()))
        );
        assert_eq!("gp_age_h:iss".parse(), Ok(Metric::GpAgeHours("iss".into())));
        assert_eq!(
            "residual_km:iss".parse(),
            Ok(Metric::ResidualKm("iss".into()))
        );
        assert_eq!("telemetry_feed_down".parse(), Ok(Metric::TelemetryFeedDown));
        for bad in ["", "item", "item:", "bogus:x", "gp_age_h"] {
            assert!(bad.parse::<Metric>().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn thresholds_grade_above_and_below() {
        let d = rule(Direction::Above, Some(5.0), Some(10.0), 0.0, 0.0);
        let mut t = Tracker::default();
        assert_eq!(
            run(
                &mut t,
                &d,
                &[(0.0, 1.0), (1.0, 5.0), (2.0, 12.0), (3.0, 1.0)]
            ),
            vec![
                CwState::Nom,
                CwState::Caution,
                CwState::Warning,
                CwState::Nom
            ]
        );
        let d = rule(Direction::Below, Some(100.0), Some(50.0), 0.0, 0.0);
        let mut t = Tracker::default();
        assert_eq!(
            run(&mut t, &d, &[(0.0, 200.0), (1.0, 100.0), (2.0, 49.0)]),
            vec![CwState::Nom, CwState::Caution, CwState::Warning]
        );
    }

    #[test]
    fn hysteresis_keeps_the_level_until_the_value_clears_by_the_margin() {
        let d = rule(Direction::Above, Some(10.0), None, 2.0, 0.0);
        let mut t = Tracker::default();
        let s = run(
            &mut t,
            &d,
            &[(0.0, 11.0), (1.0, 9.0), (2.0, 8.5), (3.0, 7.9)],
        );
        assert_eq!(
            s,
            vec![
                CwState::Caution,
                CwState::Caution,
                CwState::Caution,
                CwState::Nom
            ],
            "stays until below 10 - 2"
        );
        let d = rule(Direction::Below, Some(10.0), None, 2.0, 0.0);
        let mut t = Tracker::default();
        let s = run(&mut t, &d, &[(0.0, 9.0), (1.0, 11.0), (2.0, 12.1)]);
        assert_eq!(s, vec![CwState::Caution, CwState::Caution, CwState::Nom]);
    }

    #[test]
    fn escalation_waits_for_persistence_but_clearing_does_not() {
        let d = rule(Direction::Above, Some(10.0), None, 0.0, 5.0);
        let mut t = Tracker::default();
        let s = run(
            &mut t,
            &d,
            &[
                (0.0, 1.0),
                (1.0, 11.0),
                (4.0, 11.0),
                (6.0, 11.0),
                (7.0, 1.0),
            ],
        );
        assert_eq!(
            s,
            vec![
                CwState::Nom,
                CwState::Nom,
                CwState::Nom,
                CwState::Caution,
                CwState::Nom
            ],
            "declared 5 s after first exceedance, cleared at once"
        );
        // A blip that goes away restarts the timer.
        let mut t = Tracker::default();
        let s = run(
            &mut t,
            &d,
            &[
                (0.0, 11.0),
                (3.0, 1.0),
                (4.0, 11.0),
                (8.0, 11.0),
                (9.5, 11.0),
            ],
        );
        assert_eq!(
            s,
            vec![
                CwState::Nom,
                CwState::Nom,
                CwState::Nom,
                CwState::Nom,
                CwState::Caution
            ]
        );
    }

    #[test]
    fn missing_and_stale_data_are_their_own_states_not_nominal() {
        let d = rule(Direction::Above, Some(10.0), None, 0.0, 0.0);
        let mut t = Tracker::default();
        assert_eq!(t.state(), CwState::NoData);
        assert_eq!(
            t.update(&d, 0.0, Sample::Value(1.0)),
            Some(Transition {
                from: CwState::NoData,
                to: CwState::Nom
            })
        );
        assert_eq!(
            t.update(&d, 1.0, Sample::Stale),
            Some(Transition {
                from: CwState::Nom,
                to: CwState::Stale
            })
        );
        assert_eq!(
            t.update(&d, 2.0, Sample::Stale),
            None,
            "no repeat transition"
        );
        assert_eq!(
            t.update(&d, 3.0, Sample::Missing).unwrap().to,
            CwState::NoData
        );
        assert_eq!(
            t.update(&d, 4.0, Sample::Value(f64::NAN)),
            None,
            "NaN is no data, unchanged"
        );
        t.update(&d, 5.0, Sample::Value(11.0));
        t.update(&d, 6.0, Sample::Stale);
        t.update(&d, 7.0, Sample::Value(1.0));
        assert_eq!(
            t.state(),
            CwState::Nom,
            "an alarm does not survive a data gap"
        );
    }

    #[test]
    fn transitions_report_each_change_once() {
        let d = rule(Direction::Above, Some(5.0), Some(10.0), 0.0, 0.0);
        let mut t = Tracker::default();
        t.update(&d, 0.0, Sample::Value(1.0));
        assert_eq!(
            t.update(&d, 1.0, Sample::Value(6.0)),
            Some(Transition {
                from: CwState::Nom,
                to: CwState::Caution
            })
        );
        assert_eq!(t.update(&d, 2.0, Sample::Value(6.5)), None);
        assert_eq!(
            t.update(&d, 3.0, Sample::Value(11.0)),
            Some(Transition {
                from: CwState::Caution,
                to: CwState::Warning
            })
        );
    }

    #[test]
    fn latch_holds_until_acknowledged_and_ignores_stale() {
        let mut l = Latch::default();
        l.raise(Severity::Stale);
        assert!(!l.caution && !l.warning);
        l.raise(Severity::Caution);
        l.raise(Severity::Warning);
        assert!(l.caution && l.warning);
        l.ack(Utc(5.0));
        assert!(!l.caution && !l.warning);
        assert_eq!(l.acked_at, Some(Utc(5.0)));
    }

    #[test]
    fn validation_rejects_unusable_rules() {
        assert!(
            rule(Direction::Above, Some(1.0), Some(2.0), 0.0, 0.0)
                .validate()
                .is_ok()
        );
        assert!(
            rule(Direction::Above, None, None, 0.0, 0.0)
                .validate()
                .unwrap_err()
                .contains("threshold")
        );
        assert!(
            rule(Direction::Above, Some(5.0), Some(2.0), 0.0, 0.0)
                .validate()
                .unwrap_err()
                .contains("beyond caution")
        );
        assert!(
            rule(Direction::Below, Some(2.0), Some(5.0), 0.0, 0.0)
                .validate()
                .is_err()
        );
        assert!(
            rule(Direction::Above, Some(1.0), None, -1.0, 0.0)
                .validate()
                .is_err()
        );
        assert!(
            rule(Direction::Above, Some(f64::NAN), None, 0.0, 0.0)
                .validate()
                .is_err()
        );
        let mut r = rule(Direction::Above, Some(1.0), None, 0.0, 0.0);
        r.basis = " ".into();
        assert!(r.validate().unwrap_err().contains("basis"));
    }

    fn vehicle_view(status: SourceStatus, health: Health, residual_km: Option<f64>) -> VehicleView {
        use crate::state::SourceKind;
        use crate::world::{Residual, SourceView};
        VehicleView {
            id: "iss".into(),
            name: "ISS".into(),
            norad: 25544,
            telemetry: None,
            resolved_name: None,
            visits: None,
            visitors: vec![],
            regime: crate::config::Regime::Earth,
            schedule: None,
            deep: None,
            reference_burns: vec![],
            fields: vec![],
            ops: None,
            ops_health: None,
            met_epoch: None,
            sources: vec![SourceView {
                kind: SourceKind::Gp,
                health,
                status,
                data_epoch: None,
                fetched_at: None,
                from_cache: false,
                age_s: Some(3600.0),
                validity: None,
                state_gcrf: None,
                residual: residual_km.map(|k| Residual {
                    vs: SourceKind::Gp,
                    rtn_km: [0.0, k, 0.0],
                    norm_km: k,
                    dv_mm_s: 1.0,
                }),
            }],
            solution: Some(solution()),
            track: None,
            passes: vec![],
            beta_series: Default::default(),
        }
    }

    fn solution() -> crate::world::Solution {
        use crate::state::{Elements, Geodetic, SourceKind, State};
        crate::world::Solution {
            source: SourceKind::Oem,
            quality: Quality::Pred,
            state_gcrf: State::new([7000.0, 0.0, 0.0], [0.0, 7.5, 0.0]),
            state_itrf: State::new([7000.0, 0.0, 0.0], [0.0, 0.0, 0.0]),
            geodetic: Geodetic {
                lat_deg: 0.0,
                lon_deg: 0.0,
                alt_km: 400.0,
            },
            elements: Elements {
                sma_km: 6778.0,
                ecc: 0.001,
                inc_deg: 51.6,
                raan_deg: 10.0,
                argp_deg: 20.0,
                true_anom_deg: 30.0,
                mean_anom_deg: 30.0,
                period_s: 5560.0,
                apo_alt_km: 405.0,
                peri_alt_km: 398.0,
            },
            sun_gcrf_km: [1.5e8, 0.0, 0.0],
            beta_deg: 12.0,
            illumination: 1.0,
            gmst_rad: 0.0,
            ecef_from_gcrf: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        }
    }

    fn limits() -> Limits {
        Limits {
            residual_caution_km: 5.0,
            residual_warning_km: 20.0,
        }
    }

    #[test]
    fn residuals_are_graded_by_the_configured_limits() {
        let c = |km| {
            vehicle_conditions(
                &vehicle_view(SourceStatus::Standby, Health::Ok, Some(km)),
                &limits(),
            )
        };
        assert!(c(1.0).is_empty(), "small residual is nominal");
        assert_eq!(c(7.0)[0].severity, Severity::Caution);
        assert_eq!(c(25.0)[0].severity, Severity::Warning);
    }

    #[test]
    fn health_staleness_failures_and_missing_eop_are_reported() {
        let l = vehicle_conditions(
            &vehicle_view(SourceStatus::Stale, Health::Failing("503".into()), None),
            &limits(),
        );
        let text: Vec<&str> = l.iter().map(|c| c.text.as_str()).collect();
        assert!(text.iter().any(|t| t.contains("DATA STALE")), "{text:?}");
        assert!(
            text.iter().any(|t| t.contains("FETCH FAILING: 503")),
            "{text:?}"
        );
        let f = vehicle_conditions(
            &vehicle_view(SourceStatus::Failed("decayed".into()), Health::Ok, None),
            &limits(),
        );
        assert_eq!(f.iter().map(|c| c.severity).max(), Some(Severity::Warning));
        assert!(eop_condition(&EopView::Assumed { reason: "x".into() }).is_some());
        assert!(
            eop_condition(&EopView::Loaded {
                covers_to: Utc(2.0e9),
                fetched_at: None,
                from_cache: false
            })
            .is_none()
        );
        let off = vehicle_conditions(
            &vehicle_view(SourceStatus::Standby, Health::Offline, None),
            &limits(),
        );
        assert_eq!(off[0].severity, Severity::Stale);
    }
}
