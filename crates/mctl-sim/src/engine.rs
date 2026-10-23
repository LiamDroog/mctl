//! The propagation engine: consumes source events, runs the arbiter every tick, and publishes an
//! immutable [`World`] snapshot (DESIGN.md §6.2, §3.2). It runs on its own thread at `engine.tick_hz`;
//! the UI only ever reads the latest snapshot.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, SyncSender, TryRecvError, channel, sync_channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mctl_astro::frames::{Orientation, Reduction};
use mctl_astro::kepler;
use mctl_astro::oem::Oem;
use mctl_astro::sgp4model::GpModel;
use mctl_astro::sun::{illumination, sun_gcrf};
use mctl_astro::trajectory::{Trajectory, TrajectoryError};
use mctl_core::arbiter::{Candidate, select};
use mctl_core::bus::Bus;
use mctl_core::command::Command;
use mctl_core::config::{Config, VehicleDef};
use mctl_core::cw::{Condition, Latch, Metric, RuleDef, Sample as CwSample, Tracker};
use mctl_core::state::{Elements, Geodetic, Quality, SourceKind, State};
use mctl_core::time::{EopTable, Utc};
use mctl_core::world::{
    ClockView, CwRuleView, CwView, EopView, Health, LogEntry, Residual, Solution, SourceStatus,
    SourceView, TelemetryItem, TelemetryView, Track, TrackPoint, VehicleView, World,
};
use mctl_sources::tasks::{DataInfo, Event, Payload, SourceKey, SourceSet, TelemetryMeta};

use crate::clock::Clock;

/// Period assumed for track sizing when elements cannot be computed (LEO), seconds.
const FALLBACK_PERIOD_S: f64 = 5_600.0;
/// Event-log entries kept in each snapshot.
const LOG_CAPACITY: usize = 200;
/// After the first snapshot, conditions only list (they do not latch the master annunciator) for
/// this long, so the normal start-up (sources still loading, EOP not yet read) is not an alarm.
const CW_STARTUP_GRACE_S: f64 = 10.0;
/// Track is rebuilt at least this often, seconds.
const TRACK_REBUILD_S: f64 = 1.0;

struct Loaded<T> {
    data: Arc<T>,
    fetched_at: Utc,
    from_cache: bool,
}

struct VehicleModel {
    def: VehicleDef,
    gp: Option<Loaded<GpModel>>,
    oem: Option<Loaded<Oem>>,
    gp_health: Health,
    oem_health: Health,
    track: Option<(Utc, SourceKind, Arc<Track>)>,
}

/// All engine state, separate from threading so it can be tested with fixtures.
pub struct Model {
    cfg: Config,
    vehicles: Vec<VehicleModel>,
    eop: Option<Loaded<EopTable>>,
    track_revision: u64,
    seq: u64,
    log: VecDeque<LogEntry>,
    telemetry_meta: Vec<TelemetryMeta>,
    telemetry: HashMap<String, (Option<String>, Utc)>,
    telemetry_health: Option<Health>,
    cw_rules: Vec<(RuleDef, Tracker)>,
    latch: Latch,
    prev_conditions: HashSet<String>,
    first_wall_s: Option<f64>,
    prev: HashMap<String, Observed>,
    prev_eop_loaded: Option<bool>,
}

/// What the previous snapshot said about a vehicle, for transition logging.
#[derive(Default)]
struct Observed {
    active: Option<SourceKind>,
    sources: Vec<(SourceKind, Health, SourceStatus)>,
}

/// The value a rule watches right now, and its data quality.
fn rule_sample(
    def: &RuleDef,
    vehicles: &[VehicleView],
    telemetry: Option<&TelemetryView>,
) -> (CwSample, Option<f64>) {
    let by_id = |id: &str| vehicles.iter().find(|v| v.id == id);
    match &def.metric {
        Metric::Item(id) => match telemetry.and_then(|t| t.get(id)) {
            None => (CwSample::Missing, None),
            Some(i) if i.value.is_none() => (CwSample::Missing, None),
            Some(i) if i.age_s > def.stale_after_s => (CwSample::Stale, i.value),
            Some(i) => (CwSample::Value(i.value.unwrap_or(f64::NAN)), i.value),
        },
        Metric::GpAgeHours(v) => {
            let age = by_id(v)
                .and_then(|v| v.sources.iter().find(|s| s.kind == SourceKind::Gp))
                .and_then(|s| s.age_s);
            age.map_or((CwSample::Missing, None), |a| {
                (CwSample::Value(a.abs() / 3600.0), Some(a.abs() / 3600.0))
            })
        }
        Metric::ResidualKm(v) => {
            let worst = by_id(v).and_then(|v| {
                v.sources
                    .iter()
                    .filter_map(|s| s.residual)
                    .map(|r| r.norm_km)
                    .reduce(f64::max)
            });
            worst.map_or((CwSample::Missing, None), |r| (CwSample::Value(r), Some(r)))
        }
        Metric::TelemetryFeedDown => match telemetry {
            None => (CwSample::Missing, None),
            Some(t) => {
                let down = if t.health == Health::Ok { 0.0 } else { 1.0 };
                (CwSample::Value(down), Some(down))
            }
        },
    }
}

fn group_totals(meta: &[TelemetryMeta]) -> Vec<(String, usize)> {
    let mut out: Vec<(String, usize)> = Vec::new();
    for m in meta {
        match out.iter_mut().find(|(g, _)| *g == m.group) {
            Some((_, n)) => *n += 1,
            None => out.push((m.group.clone(), 1)),
        }
    }
    out
}

fn status_label(s: &SourceStatus) -> String {
    match s {
        SourceStatus::Active => "ACTIVE".into(),
        SourceStatus::Standby => "STANDBY".into(),
        SourceStatus::Stale => "STALE".into(),
        SourceStatus::OutOfSpan => "OUT OF SPAN".into(),
        SourceStatus::NoData => "NO DATA".into(),
        SourceStatus::Failed(w) => format!("FAILED ({w})"),
    }
}

fn health_label(h: &Health) -> String {
    match h {
        Health::Starting => "STARTING".into(),
        Health::Ok => "FEED OK".into(),
        Health::Failing(w) => format!("FEED FAILING ({w})"),
        Health::Offline => "OFFLINE".into(),
    }
}

impl Model {
    pub fn new(cfg: Config) -> Self {
        let vehicles = cfg
            .vehicles
            .iter()
            .map(|d| VehicleModel {
                def: d.clone(),
                gp: None,
                oem: None,
                gp_health: Health::Starting,
                oem_health: Health::Starting,
                track: None,
            })
            .collect();
        let cw_rules = cfg
            .rules
            .iter()
            .map(|r| (r.clone(), Tracker::default()))
            .collect();
        Self {
            cfg,
            vehicles,
            eop: None,
            track_revision: 0,
            seq: 0,
            log: VecDeque::new(),
            telemetry_meta: Vec::new(),
            telemetry: HashMap::new(),
            telemetry_health: None,
            cw_rules,
            latch: Latch::default(),
            prev_conditions: HashSet::new(),
            first_wall_s: None,
            prev: HashMap::new(),
            prev_eop_loaded: None,
        }
    }

    /// Record an event in the log (wall-clock timestamped).
    pub fn note(&mut self, at: Utc, text: impl Into<String>) {
        if self.log.len() == LOG_CAPACITY {
            self.log.pop_front();
        }
        self.log.push_back(LogEntry {
            at,
            text: text.into(),
        });
    }

    /// Log every change since the previous snapshot: active source, feed health, source status, EOP.
    fn observe(&mut self, vehicles: &[VehicleView], eop: &EopView, wall: Utc) {
        let loaded = matches!(eop, EopView::Loaded { .. });
        if self.prev_eop_loaded.is_some_and(|p| p != loaded) {
            self.note(
                wall,
                if loaded {
                    "EOP loaded"
                } else {
                    "EOP unavailable: UT1-UTC assumed 0"
                },
            );
        }
        self.prev_eop_loaded = Some(loaded);
        for v in vehicles {
            let now = Observed {
                active: v.solution.as_ref().map(|s| s.source),
                sources: v
                    .sources
                    .iter()
                    .map(|s| (s.kind, s.health.clone(), s.status.clone()))
                    .collect(),
            };
            let prev = self.prev.remove(&v.id).unwrap_or_default();
            let mut lines = Vec::new();
            if prev.active != now.active {
                lines.push(match (prev.active, now.active) {
                    (None, Some(k)) => format!("{}: solution from {}", v.name, k.label()),
                    (Some(a), Some(b)) => {
                        format!("{}: active source {} -> {}", v.name, a.label(), b.label())
                    }
                    (Some(a), None) => format!("{}: lost solution (was {})", v.name, a.label()),
                    (None, None) => unreachable!("unchanged"),
                });
            }
            for (kind, health, status) in &now.sources {
                let before = prev.sources.iter().find(|(k, _, _)| k == kind);
                if before.is_none_or(|(_, h, _)| h != health)
                    && !(before.is_none() && *health == Health::Starting)
                {
                    lines.push(format!(
                        "{} {}: {}",
                        v.name,
                        kind.label(),
                        health_label(health)
                    ));
                }
                let was = before.map(|(_, _, s)| s);
                // Active/standby flips are already reported as active-source changes.
                let routine =
                    |s: &SourceStatus| matches!(s, SourceStatus::Active | SourceStatus::Standby);
                if was.is_none_or(|w| w != status)
                    && !(was.is_none() && *status == SourceStatus::NoData)
                    && !(was.is_some_and(routine) && routine(status))
                {
                    lines.push(format!(
                        "{} {}: {}",
                        v.name,
                        kind.label(),
                        status_label(status)
                    ));
                }
            }
            for l in lines {
                self.note(wall, l);
            }
            self.prev.insert(v.id.clone(), now);
        }
    }

    /// Acknowledge the master caution and warning latches.
    pub fn ack(&mut self, wall: Utc) {
        let was = (self.latch.caution, self.latch.warning);
        self.latch.ack(wall);
        self.note(
            wall,
            format!(
                "C&W acknowledged (latched: caution {}, warning {})",
                if was.0 { "yes" } else { "no" },
                if was.1 { "yes" } else { "no" }
            ),
        );
    }

    /// Evaluate every rule and condition for this snapshot; log transitions and update the latch.
    fn cw_step(
        &mut self,
        vehicles: &[VehicleView],
        eop: &EopView,
        telemetry: Option<&TelemetryView>,
        wall: Utc,
    ) -> CwView {
        let now_s = wall.0;
        let mut views = Vec::new();
        let mut events = Vec::new();
        for (def, tracker) in &mut self.cw_rules {
            let (sample, value) = rule_sample(def, vehicles, telemetry);
            if let Some(tr) = tracker.update(def, now_s, sample) {
                events.push((def.clone(), tr, value));
            }
            views.push(CwRuleView {
                id: def.id.clone(),
                title: def.title.clone(),
                state: tracker.state(),
                value,
                units: def.units.clone(),
                caution: def.caution,
                warning: def.warning,
                basis: def.basis.clone(),
            });
        }
        for (def, tr, value) in events {
            let v = value.map_or(String::new(), |v| format!(" ({v:.2} {})", def.units));
            self.note(
                wall,
                format!(
                    "C&W {}: {} -> {}{v}",
                    def.id,
                    tr.from.label(),
                    tr.to.label()
                ),
            );
            let rose = tr
                .to
                .severity()
                .filter(|s| tr.from.severity().is_none_or(|f| *s > f));
            if let Some(s) = rose {
                self.latch.raise(s);
            }
        }
        let mut conditions: Vec<Condition> =
            mctl_core::cw::eop_condition(eop).into_iter().collect();
        for v in vehicles {
            conditions.extend(mctl_core::cw::vehicle_conditions(v, &self.cfg.limits));
        }
        let first = *self.first_wall_s.get_or_insert(now_s);
        if now_s - first >= CW_STARTUP_GRACE_S {
            let keys: HashSet<String> = conditions.iter().map(Condition::key).collect();
            for c in conditions
                .iter()
                .filter(|c| !self.prev_conditions.contains(&c.key()))
            {
                self.latch.raise(c.severity);
            }
            self.prev_conditions = keys;
        }
        CwView {
            rules: views,
            conditions,
            latch: self.latch,
        }
    }

    /// Tell the model which telemetry items exist (their order is the display order).
    pub fn set_telemetry(&mut self, meta: Vec<TelemetryMeta>) {
        if !meta.is_empty() && self.telemetry_health.is_none() {
            self.telemetry_health = Some(Health::Starting);
        }
        self.telemetry_meta = meta;
    }

    pub fn apply(&mut self, event: Event) {
        match event {
            Event::Telemetry { id, text, rx } => {
                self.telemetry.insert(id, (text, rx));
            }
            Event::TelemetryHealth(h) => self.telemetry_health = Some(h),
            Event::Data {
                key,
                payload,
                info:
                    DataInfo {
                        from_cache,
                        fetched_at,
                    },
            } => match (key, payload) {
                (SourceKey::Gp(id), Payload::Gp(m)) => {
                    if let Some(v) = self.vehicles.iter_mut().find(|v| v.def.id == id) {
                        v.gp = Some(Loaded {
                            data: Arc::new(m),
                            fetched_at,
                            from_cache,
                        });
                        v.track = None;
                    }
                }
                (SourceKey::Oem(id), Payload::Oem(o)) => {
                    if let Some(v) = self.vehicles.iter_mut().find(|v| v.def.id == id) {
                        v.oem = Some(Loaded {
                            data: Arc::new(*o),
                            fetched_at,
                            from_cache,
                        });
                        v.track = None;
                    }
                }
                (SourceKey::Eop, Payload::Eop(t)) => {
                    self.eop = Some(Loaded {
                        data: Arc::new(t),
                        fetched_at,
                        from_cache,
                    })
                }
                (key, _) => tracing::error!(
                    ?key,
                    "source delivered a payload of the wrong kind; ignored"
                ),
            },
            Event::Health { key, health } => match key {
                SourceKey::Gp(id) => {
                    if let Some(v) = self.vehicles.iter_mut().find(|v| v.def.id == id) {
                        v.gp_health = health;
                    }
                }
                SourceKey::Oem(id) => {
                    if let Some(v) = self.vehicles.iter_mut().find(|v| v.def.id == id) {
                        v.oem_health = health;
                    }
                }
                SourceKey::Eop => {}
            },
        }
    }

    fn orientation(&self, t: Utc) -> (Orientation, EopView) {
        match &self.eop {
            None => (
                Orientation::ASSUMED_ZERO,
                EopView::Assumed {
                    reason: "no EOP data loaded; UT1-UTC and polar motion assumed 0".into(),
                },
            ),
            Some(l) => match l.data.at(t.mjd_utc()) {
                Ok(r) => (
                    Orientation {
                        dut1_s: r.dut1,
                        xp_as: r.xp,
                        yp_as: r.yp,
                        assumed: false,
                    },
                    EopView::Loaded {
                        covers_to: Utc((l.data.coverage().1 - 40_587.0) * 86_400.0),
                        fetched_at: Some(l.fetched_at),
                        from_cache: l.from_cache,
                    },
                ),
                Err(e) => (
                    Orientation::ASSUMED_ZERO,
                    EopView::Assumed {
                        reason: format!("{e}; assumed 0"),
                    },
                ),
            },
        }
    }

    /// Snapshot for time `t` with the clock live (used by tests and the LIVE path).
    pub fn world(&mut self, t: Utc) -> World {
        self.world_at(t, ClockView::LIVE, t)
    }

    /// Build the snapshot for clock time `t`; `wall` stamps log entries.
    pub fn world_at(&mut self, t: Utc, clock: ClockView, wall: Utc) -> World {
        let (orientation, eop_view) = self.orientation(t);
        let stale = (
            self.cfg.sources.celestrak_gp.stale_after_s,
            self.cfg.sources.nasa_oem.stale_after_s,
        );
        let limits = self.cfg.limits.clone();
        let (past_rev, future_rev, samples) = (
            self.cfg.engine.track_past_rev,
            self.cfg.engine.track_future_rev,
            self.cfg.engine.track_samples,
        );
        let eop = self.eop.as_ref().map(|l| l.data.clone());
        let mut revision = self.track_revision;
        let vehicles = self
            .vehicles
            .iter_mut()
            .map(|v| {
                vehicle_view(
                    v,
                    t,
                    orientation,
                    stale,
                    &limits,
                    (past_rev, future_rev, samples),
                    eop.as_deref(),
                    &mut revision,
                    clock.is_live(),
                )
            })
            .collect::<Vec<_>>();
        self.track_revision = revision;
        self.seq += 1;
        self.observe(&vehicles, &eop_view, wall);
        let telemetry = self.telemetry_health.clone().map(|health| TelemetryView {
            health,
            subscribed: self.telemetry_meta.len(),
            group_totals: group_totals(&self.telemetry_meta),
            items: self
                .telemetry_meta
                .iter()
                .filter_map(|m| {
                    let (text, rx) = self.telemetry.get(&m.id)?;
                    Some(TelemetryItem {
                        id: m.id.clone(),
                        group: m.group.clone(),
                        description: m.description.clone(),
                        label: m.label.clone(),
                        units: m.units.clone(),
                        value: text.as_deref().and_then(|t| t.trim().parse().ok()),
                        text: text.clone(),
                        rx: *rx,
                        age_s: wall.minus(*rx),
                    })
                })
                .collect(),
        });
        let cw = self.cw_step(&vehicles, &eop_view, telemetry.as_ref(), wall);
        World {
            telemetry,
            cw,
            t,
            clock,
            log: self.log.iter().cloned().collect(),
            seq: self.seq,
            eop: eop_view,
            vehicles,
        }
    }
}

struct Sample {
    kind: SourceKind,
    has_data: bool,
    health: Health,
    data_epoch: Option<Utc>,
    fetched_at: Option<Utc>,
    from_cache: bool,
    stale_after_s: f64,
    validity: Option<(Utc, Utc)>,
    state: Option<Result<State, TrajectoryError>>,
}

fn sample<T: Trajectory>(
    kind: SourceKind,
    l: &Option<Loaded<T>>,
    health: &Health,
    epoch: impl Fn(&T) -> Utc,
    stale_after_s: f64,
    t: Utc,
) -> Sample {
    match l {
        None => Sample {
            kind,
            has_data: false,
            health: health.clone(),
            data_epoch: None,
            fetched_at: None,
            from_cache: false,
            stale_after_s,
            validity: None,
            state: None,
        },
        Some(l) => Sample {
            kind,
            has_data: true,
            health: health.clone(),
            data_epoch: Some(epoch(&l.data)),
            fetched_at: Some(l.fetched_at),
            from_cache: l.from_cache,
            stale_after_s,
            validity: l.data.validity(),
            state: Some(l.data.state_gcrf(t)),
        },
    }
}

fn rtn_residual(reference: &State, other: &State) -> ([f64; 3], f64) {
    let rhat = reference.r.normalize();
    let nhat = reference.r.cross(&reference.v).normalize();
    let that = nhat.cross(&rhat);
    let d = other.r - reference.r;
    ([d.dot(&rhat), d.dot(&that), d.dot(&nhat)], d.norm())
}

#[allow(clippy::too_many_arguments)]
fn vehicle_view(
    v: &mut VehicleModel,
    t: Utc,
    orientation: Orientation,
    stale: (f64, f64),
    limits: &mctl_core::config::Limits,
    track_cfg: (f64, f64, usize),
    eop: Option<&EopTable>,
    revision: &mut u64,
    live: bool,
) -> VehicleView {
    // Samples in the vehicle's configured priority order.
    let samples: Vec<Sample> = v
        .def
        .sources
        .iter()
        .map(|k| match k {
            SourceKind::Gp => sample(*k, &v.gp, &v.gp_health, |g| g.epoch, stale.0, t),
            SourceKind::Oem => sample(*k, &v.oem, &v.oem_health, |o| o.creation_date, stale.1, t),
        })
        .collect();
    let age = |s: &Sample| s.data_epoch.map(|e| t.minus(e));
    // Element sets degrade in both directions from their epoch, so scrubbing into the past counts too.
    let is_stale = |s: &Sample| age(s).is_some_and(|a| a.abs() > s.stale_after_s);
    let ok = |s: &Sample| matches!(s.state, Some(Ok(_)));
    let in_span = |s: &Sample| !matches!(s.state, Some(Err(TrajectoryError::OutOfSpan)));
    // First pass: healthy and not stale. Second pass: accept stale data rather than show nothing, labelled STALE.
    let strict: Vec<Candidate> = samples
        .iter()
        .map(|s| Candidate {
            healthy: ok(s) && !is_stale(s),
            valid: in_span(s),
        })
        .collect();
    let (active, quality) = match select(&strict) {
        // Away from the present the state is a simulation of a prediction, not a prediction of now (FR-6).
        Some(i) => (Some(i), if live { Quality::Pred } else { Quality::Sim }),
        None => {
            let relaxed: Vec<Candidate> = samples
                .iter()
                .map(|s| Candidate {
                    healthy: ok(s),
                    valid: in_span(s),
                })
                .collect();
            (select(&relaxed), Quality::Stale)
        }
    };
    let active_state = active.and_then(|i| {
        samples[i]
            .state
            .as_ref()
            .and_then(|r| r.as_ref().ok().copied())
    });

    let sources: Vec<SourceView> = samples
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let status = if Some(i) == active {
                SourceStatus::Active
            } else if !s.has_data {
                SourceStatus::NoData
            } else {
                match &s.state {
                    Some(Err(TrajectoryError::OutOfSpan)) => SourceStatus::OutOfSpan,
                    Some(Err(e)) => SourceStatus::Failed(e.to_string()),
                    _ if is_stale(s) => SourceStatus::Stale,
                    _ => SourceStatus::Standby,
                }
            };
            let residual = match (&active_state, &s.state) {
                (Some(a), Some(Ok(o))) if Some(i) != active => {
                    let (rtn, norm) = rtn_residual(a, o);
                    Some(Residual {
                        vs: s.kind,
                        rtn_km: rtn,
                        norm_km: norm,
                        dv_mm_s: (o.v - a.v).norm() * 1.0e6,
                    })
                }
                _ => None,
            };
            SourceView {
                kind: s.kind,
                health: s.health.clone(),
                status,
                data_epoch: s.data_epoch,
                fetched_at: s.fetched_at,
                from_cache: s.from_cache,
                age_s: age(s),
                validity: s.validity,
                residual,
            }
        })
        .collect();
    let _ = limits; // thresholds are applied by the display, which owns the CAUT/WARN styling

    let solution = active_state.map(|state| {
        solution(
            active.map(|i| samples[i].kind).expect("active"),
            quality,
            state,
            t,
            orientation,
        )
    });
    let track = match (&solution, active) {
        (Some(sol), Some(i)) => {
            let kind = samples[i].kind;
            let due = v
                .track
                .as_ref()
                .is_none_or(|(built, k, _)| *k != kind || t.minus(*built).abs() >= TRACK_REBUILD_S);
            if due {
                *revision += 1;
                let traj: Option<&dyn Trajectory> = match kind {
                    SourceKind::Gp => v.gp.as_ref().map(|l| &*l.data as &dyn Trajectory),
                    SourceKind::Oem => v.oem.as_ref().map(|l| &*l.data as &dyn Trajectory),
                };
                if let Some(traj) = traj {
                    v.track = Some((
                        t,
                        kind,
                        Arc::new(build_track(
                            traj,
                            kind,
                            t,
                            sol.elements.period_s,
                            track_cfg,
                            eop,
                            *revision,
                        )),
                    ));
                }
            }
            v.track.as_ref().map(|(_, _, tr)| tr.clone())
        }
        _ => {
            v.track = None;
            None
        }
    };
    VehicleView {
        id: v.def.id.clone(),
        name: v.def.name.clone(),
        norad: v.def.norad,
        telemetry: v.def.telemetry.clone(),
        met_epoch: v.def.met_epoch.as_deref().and_then(|m| Utc::parse(m).ok()),
        sources,
        solution,
        track,
    }
}

fn solution(
    source: SourceKind,
    quality: Quality,
    state_gcrf: State,
    t: Utc,
    orientation: Orientation,
) -> Solution {
    let red = Reduction::at(t, orientation).expect("time scales are available for current dates");
    let state_itrf = red.gcrf_to_itrf(&state_gcrf);
    let sun = sun_gcrf(t, orientation.dut1_s).expect("sun");
    let m = red.ecef_from_gcrf();
    let elements = kepler::elements(&state_gcrf).unwrap_or(Elements {
        sma_km: f64::NAN,
        ecc: f64::NAN,
        inc_deg: f64::NAN,
        raan_deg: f64::NAN,
        argp_deg: f64::NAN,
        true_anom_deg: f64::NAN,
        mean_anom_deg: f64::NAN,
        period_s: FALLBACK_PERIOD_S,
        apo_alt_km: f64::NAN,
        peri_alt_km: f64::NAN,
    });
    Solution {
        source,
        quality,
        state_gcrf,
        state_itrf,
        geodetic: kepler::geodetic(&state_itrf.r),
        elements,
        sun_gcrf_km: [sun.x, sun.y, sun.z],
        beta_deg: kepler::beta_deg(&state_gcrf, &sun).unwrap_or(f64::NAN),
        illumination: illumination(&state_gcrf.r, &sun),
        gmst_rad: red.gmst_rad,
        ecef_from_gcrf: [
            [m[(0, 0)], m[(0, 1)], m[(0, 2)]],
            [m[(1, 0)], m[(1, 1)], m[(1, 2)]],
            [m[(2, 0)], m[(2, 1)], m[(2, 2)]],
        ],
    }
}

fn build_track(
    traj: &dyn Trajectory,
    kind: SourceKind,
    now: Utc,
    period_s: f64,
    (past_rev, future_rev, samples): (f64, f64, usize),
    eop: Option<&EopTable>,
    revision: u64,
) -> Track {
    let period = if period_s.is_finite() && period_s > 600.0 {
        period_s
    } else {
        FALLBACK_PERIOD_S
    };
    let (past, future) = (past_rev * period, future_rev * period);
    let dt = (past + future) / (samples - 1) as f64;
    let mut points = Vec::with_capacity(samples);
    for k in 0..samples {
        let tk = now.plus(-past + k as f64 * dt);
        // Samples a source cannot produce (outside an OEM span, decayed) are skipped, not invented.
        let Ok(s) = traj.state_gcrf(tk) else { continue };
        let orient =
            eop.and_then(|e| e.at(tk.mjd_utc()).ok())
                .map_or(Orientation::ASSUMED_ZERO, |r| Orientation {
                    dut1_s: r.dut1,
                    xp_as: r.xp,
                    yp_as: r.yp,
                    assumed: false,
                });
        let (Ok(red), Ok(sun)) = (Reduction::at(tk, orient), sun_gcrf(tk, orient.dut1_s)) else {
            continue;
        };
        let itrf = red.gcrf_to_itrf(&s);
        let g: Geodetic = kepler::geodetic(&itrf.r);
        points.push(TrackPoint {
            t: tk,
            r_gcrf_km: [s.r.x, s.r.y, s.r.z],
            lat_deg: g.lat_deg,
            lon_deg: g.lon_deg,
            alt_km: g.alt_km,
            illumination: illumination(&s.r, &sun) as f32,
        });
    }
    let now_index = points.partition_point(|p| p.t.0 < now.0);
    Track {
        revision,
        source: kind,
        points,
        now_index,
    }
}

/// A running engine. Dropping it stops the thread.
pub struct Engine {
    pub bus: Arc<Bus>,
    /// Commands for the simulation clock (the GUI never touches engine state directly).
    pub commands: Sender<Command>,
    _stop: SyncSender<()>,
    _thread: JoinHandle<()>,
}

/// Apply every queued command to the clock, logging the outcome.
fn drain_commands(rx: &Receiver<Command>, clock: &mut Clock, model: &mut Model, wall_utc: Utc) {
    while let Ok(cmd) = rx.try_recv() {
        if cmd == Command::Ack {
            model.ack(wall_utc);
            continue;
        }
        match clock.apply(&cmd, Instant::now(), wall_utc) {
            Ok(text) => model.note(wall_utc, text),
            Err(why) => model.note(wall_utc, format!("command rejected: {why}")),
        }
    }
}

/// Start the engine thread. It owns `sources` and publishes to the returned bus.
pub fn start(cfg: Config, sources: SourceSet) -> Engine {
    let bus = Bus::new(World::empty(Utc::now().unwrap_or(Utc(0.0))));
    let (stop_tx, stop_rx) = sync_channel::<()>(1);
    let (cmd_tx, cmd_rx) = channel::<Command>();
    let (b, tick) = (
        bus.clone(),
        Duration::from_secs_f64(1.0 / cfg.engine.tick_hz),
    );
    let thread = std::thread::Builder::new()
        .name("engine".into())
        .spawn(move || {
            // Capture the whole set: closures capture disjoint fields, and dropping the set's stop
            // channels would tell every source thread to exit.
            let sources = sources;
            let mut model = Model::new(cfg);
            model.set_telemetry(sources.telemetry.clone());
            let mut clock = Clock::live(Instant::now());
            let mut next = Instant::now();
            loop {
                while let Ok(e) = sources.events.try_recv() {
                    model.apply(e);
                }
                match Utc::now() {
                    Ok(wall_utc) => {
                        drain_commands(&cmd_rx, &mut clock, &mut model, wall_utc);
                        if let Some(msg) = clock.enforce_bounds(Instant::now(), wall_utc) {
                            model.note(wall_utc, msg);
                        }
                        let t = clock.now(Instant::now(), wall_utc);
                        b.publish(model.world_at(t, clock.view(), wall_utc));
                    }
                    Err(e) => tracing::error!(error = %e, "cannot read the clock"),
                }
                next += tick;
                let now = Instant::now();
                if next > now {
                    if !matches!(
                        stop_rx.recv_timeout(next - now),
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                    ) {
                        return;
                    }
                } else {
                    // Fell behind (a slow tick): resynchronise rather than burst to catch up.
                    next = now;
                    if !matches!(stop_rx.try_recv(), Err(TryRecvError::Empty)) {
                        return;
                    }
                }
            }
        })
        .expect("spawn engine thread");
    Engine {
        bus,
        commands: cmd_tx,
        _stop: stop_tx,
        _thread: thread,
    }
}

#[cfg(test)]
mod tests {
    use mctl_core::world::SourceStatus;

    use super::*;

    const OMM: &str = include_str!("../../mctl-astro/fixtures/iss-omm.json");
    const OEM: &str = include_str!("../../mctl-astro/fixtures/iss-oem-sample.txt");

    fn model() -> Model {
        Model::new(Config::from_layers(mctl_core::config::DEFAULT_CONFIG, None).unwrap())
    }

    fn feed(m: &mut Model, gp: bool, oem: bool) {
        if gp {
            m.apply(Event::Data {
                key: SourceKey::Gp("iss".into()),
                payload: Payload::Gp(GpModel::from_omm_json(OMM).unwrap()),
                info: DataInfo {
                    from_cache: false,
                    fetched_at: Utc(0.0),
                },
            });
        }
        if oem {
            m.apply(Event::Data {
                key: SourceKey::Oem("iss".into()),
                payload: Payload::Oem(Box::new(Oem::parse(OEM).unwrap())),
                info: DataInfo {
                    from_cache: true,
                    fetched_at: Utc(0.0),
                },
            });
        }
    }

    /// Inside the sample OEM's span, shortly before the GP epoch.
    fn t_in_oem() -> Utc {
        Utc::parse("2026-10-22T10:00:00").unwrap()
    }

    #[test]
    fn with_no_data_there_is_no_solution_and_no_invented_values() {
        let mut m = model();
        let w = m.world(t_in_oem());
        let iss = &w.vehicles[0];
        assert!(iss.solution.is_none() && iss.track.is_none());
        assert!(iss.sources.iter().all(|s| s.status == SourceStatus::NoData));
        assert!(
            matches!(w.eop, EopView::Assumed { .. }),
            "EOP absent is reported, not hidden"
        );
    }

    #[test]
    fn oem_is_active_when_valid_and_gp_is_standby_with_a_residual() {
        let mut m = model();
        feed(&mut m, true, true);
        // Stale limits are relative to data age; put `t` close to the OEM creation date (2026-10-21T16:27).
        let t = Utc::parse("2026-10-22T10:00:00").unwrap();
        let w = m.world(t);
        let v = &w.vehicles[0];
        let sol = v.solution.as_ref().unwrap();
        assert_eq!(sol.source, SourceKind::Oem);
        assert_eq!(sol.quality, Quality::Pred);
        assert_eq!(v.sources[0].status, SourceStatus::Active);
        assert_eq!(v.sources[1].status, SourceStatus::Standby);
        let r = v.sources[1].residual.unwrap();
        assert!(
            r.norm_km > 0.0 && r.norm_km < 20.0,
            "GP vs OEM residual {} km should be a few km",
            r.norm_km
        );
        assert!(sol.geodetic.alt_km > 380.0 && sol.geodetic.alt_km < 450.0);
        assert!((sol.elements.inc_deg - 51.6).abs() < 0.2);
        assert!((0.0..=1.0).contains(&sol.illumination));
        let tr = v.track.as_ref().unwrap();
        assert_eq!(tr.source, SourceKind::Oem);
        assert!(tr.points.len() > 300 && tr.now_index > 0 && tr.now_index < tr.points.len());
    }

    #[test]
    fn outside_the_oem_span_the_arbiter_falls_back_to_gp() {
        let mut m = model();
        feed(&mut m, true, true);
        let t = Utc::parse("2026-10-25T00:00:00").unwrap(); // after the (sample) OEM ends
        let w = m.world(t);
        let v = &w.vehicles[0];
        assert_eq!(v.solution.as_ref().unwrap().source, SourceKind::Gp);
        assert_eq!(v.sources[0].status, SourceStatus::OutOfSpan);
        assert_eq!(
            v.sources[0].residual, None,
            "an out-of-span source has no state to compare"
        );
    }

    #[test]
    fn stale_data_is_still_shown_but_labelled_stale() {
        let mut m = model();
        feed(&mut m, true, false);
        // 10 days after the GP epoch exceeds the 3-day stale limit but SGP4 still propagates.
        let t = GpModel::from_omm_json(OMM)
            .unwrap()
            .epoch
            .plus(10.0 * 86_400.0);
        let w = m.world(t);
        let v = &w.vehicles[0];
        assert_eq!(v.solution.as_ref().unwrap().quality, Quality::Stale);
        assert_eq!(
            v.sources[0].status,
            SourceStatus::NoData,
            "OEM was never loaded"
        );
        assert_eq!(v.sources[1].status, SourceStatus::Active);
    }

    #[test]
    fn eop_changes_the_itrf_position_and_is_reported() {
        let mut with = model();
        feed(&mut with, true, false);
        let text = include_str!("../../../spikes/astro/fixtures/finals2000A.all");
        with.apply(Event::Data {
            key: SourceKey::Eop,
            payload: Payload::Eop(EopTable::parse(text).unwrap()),
            info: DataInfo {
                from_cache: false,
                fetched_at: Utc(0.0),
            },
        });
        let mut without = model();
        feed(&mut without, true, false);
        let t = GpModel::from_omm_json(OMM).unwrap().epoch.plus(3600.0);
        let (a, b) = (with.world(t), without.world(t));
        assert!(
            matches!(a.eop, EopView::Loaded { .. }) && matches!(b.eop, EopView::Assumed { .. })
        );
        let (sa, sb) = (
            a.vehicles[0].solution.as_ref().unwrap(),
            b.vehicles[0].solution.as_ref().unwrap(),
        );
        let d = (sa.state_itrf.r - sb.state_itrf.r).norm() * 1000.0;
        // UT1-UTC of -0.04 s is ~30 m of along-track Earth rotation at the ISS, plus ~10 m polar motion.
        assert!(d > 5.0 && d < 100.0, "EOP effect on ITRF position {d} m");
        assert!(
            (sa.state_gcrf.r - sb.state_gcrf.r).norm() < 1e-9,
            "EOP must not move the inertial state"
        );
    }

    #[test]
    fn ground_track_longitudes_and_latitudes_are_in_range_and_continuous() {
        let mut m = model();
        feed(&mut m, true, false);
        let w = m.world(GpModel::from_omm_json(OMM).unwrap().epoch);
        let tr = w.vehicles[0].track.as_ref().unwrap();
        assert!(
            tr.points
                .iter()
                .all(|p| p.lat_deg.abs() <= 52.0 && p.lon_deg.abs() <= 180.0)
        );
        let max_lat = tr
            .points
            .iter()
            .map(|p| p.lat_deg.abs())
            .fold(0.0, f64::max);
        assert!(max_lat > 50.0, "ISS reaches ~51.6 deg latitude");
    }

    #[test]
    fn engine_thread_publishes_at_the_tick_rate_and_stops_on_drop() {
        let cfg = Config::from_layers(mctl_core::config::DEFAULT_CONFIG, None).unwrap();
        let dir = std::env::temp_dir().join(format!("mctl-engine-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let set = mctl_sources::tasks::start(&cfg, dir.clone(), true).unwrap();
        let eng = start(cfg, set);
        let s0 = eng.bus.latest().seq;
        std::thread::sleep(Duration::from_millis(500));
        let advanced = eng.bus.latest().seq - s0;
        assert!(
            (15..=30).contains(&advanced),
            "expected ~25 snapshots in 0.5 s at 50 Hz, got {advanced}"
        );
        drop(eng);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn away_from_live_the_quality_label_is_sim_and_staleness_counts_both_directions() {
        let mut m = model();
        feed(&mut m, true, true);
        let t = Utc::parse("2026-10-22T10:00:00").unwrap();
        let sim = ClockView {
            mode: mctl_core::world::ClockMode::Sim,
            rate: 60.0,
            paused: false,
        };
        let w = m.world_at(t, sim, t);
        assert_eq!(
            w.vehicles[0].solution.as_ref().unwrap().quality,
            Quality::Sim
        );
        assert_eq!(w.clock, sim);
        // Ten days before the GP epoch is as stale as ten days after (the OEM is out of span there).
        let early = GpModel::from_omm_json(OMM)
            .unwrap()
            .epoch
            .plus(-10.0 * 86_400.0);
        let mut m2 = model();
        feed(&mut m2, true, false);
        assert_eq!(
            m2.world_at(early, sim, early).vehicles[0].sources[1].status,
            SourceStatus::Active,
            "relaxed pass still accepts it"
        );
        assert_eq!(
            m2.world_at(early, sim, early).vehicles[0]
                .solution
                .as_ref()
                .unwrap()
                .quality,
            Quality::Stale
        );
    }

    #[test]
    fn the_event_log_records_transitions_once_not_every_tick() {
        let mut m = model();
        let t = t_in_oem();
        m.world(t);
        m.world(t);
        assert!(m.world(t).log.is_empty(), "steady state logs nothing");
        feed(&mut m, true, true);
        let w = m.world(t);
        let texts: Vec<&str> = w.log.iter().map(|e| e.text.as_str()).collect();
        assert!(
            texts.iter().any(|x| x.contains("ISS: solution from OEM")),
            "{texts:?}"
        );
        let n = w.log.len();
        assert_eq!(
            m.world(t).log.len(),
            n,
            "no repeat entries while nothing changes"
        );
        m.apply(Event::Health {
            key: SourceKey::Oem("iss".into()),
            health: Health::Failing("HTTP 503".into()),
        });
        let w = m.world(t);
        assert!(
            w.log
                .last()
                .unwrap()
                .text
                .contains("OEM: FEED FAILING (HTTP 503)"),
            "{:?}",
            w.log.last()
        );
        // Scrubbing out of the OEM span flips the active source and is logged.
        let later = Utc::parse("2026-10-25T00:00:00").unwrap();
        let w = m.world(later);
        assert!(
            w.log
                .iter()
                .any(|e| e.text.contains("active source OEM -> GP/SGP4")),
            "{:?}",
            w.log
        );
    }

    #[test]
    fn the_log_is_bounded() {
        let mut m = model();
        for i in 0..(LOG_CAPACITY + 50) {
            m.note(Utc(i as f64), format!("e{i}"));
        }
        let w = m.world(t_in_oem());
        assert_eq!(w.log.len(), LOG_CAPACITY);
        assert_eq!(w.log[0].text, "e50", "oldest entries are dropped first");
    }

    #[test]
    fn commands_reach_the_clock_through_the_engine_thread() {
        let cfg = Config::from_layers(mctl_core::config::DEFAULT_CONFIG, None).unwrap();
        let dir = std::env::temp_dir().join(format!("mctl-engine-cmd-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let set = mctl_sources::tasks::start(&cfg, dir.clone(), true).unwrap();
        let eng = start(cfg, set);
        let target = Utc::parse("2026-10-25T00:00:00").unwrap();
        eng.commands.send(Command::Seek(target)).unwrap();
        eng.commands.send(Command::SetRate(1000.0)).unwrap();
        eng.commands.send(Command::SetRate(1.0e9)).unwrap(); // rejected
        std::thread::sleep(Duration::from_millis(300));
        let w = eng.bus.latest();
        assert!(
            !w.clock.is_live() && w.clock.rate == 1000.0,
            "{:?}",
            w.clock
        );
        assert!(
            w.t.0 > target.0 && w.t.0 < target.0 + 600.0,
            "sim time {} should have advanced from the target",
            w.t.iso()
        );
        assert!(
            w.log.iter().any(|e| e.text.contains("command rejected")),
            "{:?}",
            w.log
        );
        eng.commands.send(Command::Live).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(eng.bus.latest().clock.is_live());
        drop(eng);
        std::fs::remove_dir_all(dir).ok();
    }

    fn meta(id: &str) -> TelemetryMeta {
        TelemetryMeta {
            id: id.into(),
            group: "eclss".into(),
            description: id.into(),
            label: id.into(),
            units: "mmHg".into(),
        }
    }

    #[test]
    fn a_telemetry_rule_escalates_after_persistence_latches_and_clears_on_ack() {
        let mut m = model();
        m.set_telemetry(vec![meta("USLAB000058"), meta("USLAB000055")]);
        let t0 = Utc::parse("2026-10-23T12:00:00").unwrap();
        let live = ClockView::LIVE;
        let send = |m: &mut Model, id: &str, v: &str, at: Utc| {
            m.apply(Event::Telemetry {
                id: id.into(),
                text: Some(v.into()),
                rx: at,
            })
        };
        send(&mut m, "USLAB000058", "749", t0);
        let w = m.world_at(t0, live, t0);
        let rule = |w: &World| {
            w.cw.rules
                .iter()
                .find(|r| r.id == "iss-cabin-pressure")
                .unwrap()
                .state
        };
        assert_eq!(rule(&w), mctl_core::cw::CwState::Nom);
        // Pressure drops below the warning threshold: nothing is declared until it has held 10 s.
        send(&mut m, "USLAB000058", "640", t0.plus(1.0));
        assert_eq!(
            rule(&m.world_at(t0.plus(1.0), live, t0.plus(1.0))),
            mctl_core::cw::CwState::Nom
        );
        send(&mut m, "USLAB000058", "640", t0.plus(12.0));
        let w = m.world_at(t0.plus(12.0), live, t0.plus(12.0));
        assert_eq!(rule(&w), mctl_core::cw::CwState::Warning);
        assert!(w.cw.latch.warning && w.cw.latch.caution);
        assert_eq!(
            w.cw.worst_now(Some("iss")),
            Some(mctl_core::cw::Severity::Warning)
        );
        assert!(
            w.log
                .iter()
                .any(|e| e.text.contains("C&W iss-cabin-pressure: NOM -> WARN")),
            "{:?}",
            w.log
        );
        // Recovery clears the live state but the latch stays until acknowledged.
        send(&mut m, "USLAB000058", "749", t0.plus(20.0));
        let w = m.world_at(t0.plus(20.0), live, t0.plus(20.0));
        assert_eq!(rule(&w), mctl_core::cw::CwState::Nom);
        assert!(w.cw.latch.warning, "latched until acknowledged");
        m.ack(t0.plus(21.0));
        let w = m.world_at(t0.plus(21.0), live, t0.plus(21.0));
        assert!(!w.cw.latch.warning && !w.cw.latch.caution);
        assert!(w.log.iter().any(|e| e.text.contains("C&W acknowledged")));
    }

    #[test]
    fn rules_on_missing_or_old_telemetry_are_no_data_or_stale_never_nominal() {
        let mut m = model();
        m.set_telemetry(vec![meta("USLAB000058")]);
        let t0 = Utc::parse("2026-10-23T12:00:00").unwrap();
        let rule = |w: &World| {
            w.cw.rules
                .iter()
                .find(|r| r.id == "iss-cabin-pressure")
                .unwrap()
                .state
        };
        assert_eq!(
            rule(&m.world_at(t0, ClockView::LIVE, t0)),
            mctl_core::cw::CwState::NoData
        );
        m.apply(Event::Telemetry {
            id: "USLAB000058".into(),
            text: Some("749".into()),
            rx: t0,
        });
        assert_eq!(
            rule(&m.world_at(t0, ClockView::LIVE, t0)),
            mctl_core::cw::CwState::Nom
        );
        assert_eq!(
            rule(&m.world_at(t0.plus(300.0), ClockView::LIVE, t0.plus(300.0))),
            mctl_core::cw::CwState::Stale,
            "5 minutes old"
        );
    }

    #[test]
    fn a_new_source_condition_latches_a_caution_but_start_up_does_not() {
        let mut m = model();
        feed(&mut m, true, true);
        let t = t_in_oem();
        let w = m.world_at(t, ClockView::LIVE, t);
        assert!(
            !w.cw.latch.caution,
            "inside the start-up grace nothing latches"
        );
        // Past the grace, standing conditions that were already there (no EOP in this model) latch once.
        let later = t.plus(CW_STARTUP_GRACE_S + 1.0);
        let w = m.world_at(t, ClockView::LIVE, later);
        assert!(
            w.cw.conditions
                .iter()
                .any(|c| c.text.contains("EOP ASSUMED"))
        );
        assert!(w.cw.latch.caution);
        m.ack(later);
        let w = m.world_at(t, ClockView::LIVE, later.plus(1.0));
        assert!(
            !w.cw.latch.caution,
            "acknowledged, and nothing new has appeared"
        );
        m.apply(Event::Health {
            key: SourceKey::Oem("iss".into()),
            health: Health::Failing("HTTP 503".into()),
        });
        let w = m.world_at(t, ClockView::LIVE, later.plus(2.0));
        assert!(w.cw.latch.caution);
        assert!(
            w.cw.conditions
                .iter()
                .any(|c| c.text.contains("FETCH FAILING"))
        );
        m.ack(later.plus(3.5));
        assert!(
            !m.world_at(t, ClockView::LIVE, later.plus(4.0))
                .cw
                .latch
                .caution,
            "a persisting condition does not re-latch"
        );
    }
}
