//! The propagation engine: consumes source events, runs the arbiter every tick, and publishes an
//! immutable [`World`] snapshot. It runs on its own thread at `engine.tick_hz`;
//! the UI only ever reads the latest snapshot.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, SyncSender, TryRecvError, channel, sync_channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mctl_astro::frames::{Orientation, Reduction};
use mctl_astro::kepler;
use mctl_astro::observer::Site;
use mctl_astro::oem::Oem;
use mctl_astro::sgp4model::GpModel;
use mctl_astro::sun::{illumination, sun_gcrf};
use mctl_astro::trajectory::{Trajectory, TrajectoryError};
use mctl_core::arbiter::{Candidate, select};
use mctl_core::bus::Bus;
use mctl_core::command::Command;
use mctl_core::config::{Config, Regime, VehicleDef};
use mctl_core::cw::{Condition, Latch, Metric, RuleDef, Sample as CwSample, Tracker};
use mctl_core::plan::{PlanCommand, PlanDef, PlanStatus, PlanView};
use mctl_core::prox::{ProxPoint, ProxView};
use mctl_core::schedule::{FieldRequest, FieldState, FieldView, Ops};
use mctl_core::state::{Elements, Geodetic, Quality, SourceKind, State};
use mctl_core::time::{EopTable, Utc};
use mctl_core::world::{
    ClockView, CwRuleView, CwView, DeepView, EopView, HISTORY_LEN, HISTORY_PERIOD_S, Health,
    LogEntry, LookView, ObserverView, PassSet, Residual, SiteView, Solution, SourceStatus,
    SourceView, TelemetryItem, TelemetryView, Track, TrackPoint, VehicleView, World,
};
use mctl_sources::tasks::{DataInfo, Event, Payload, SourceKey, SourceSet, TelemetryMeta};
use mctl_store::{Record, Recorder};

use crate::clock::Clock;

/// Period assumed for track sizing when elements cannot be computed (LEO), seconds.
const FALLBACK_PERIOD_S: f64 = 5_600.0;
/// Event-log entries kept in each snapshot.
const LOG_CAPACITY: usize = 200;
/// After the first snapshot, conditions only list (they do not latch the master annunciator) for
/// this long, so the normal start-up (sources still loading, EOP not yet read) is not an alarm.
const CW_STARTUP_GRACE_S: f64 = 10.0;
/// Samples per revolution of the event search (period / 200).
const EVENT_STEPS_PER_REV: f64 = 200.0;
/// Track is rebuilt at least this often, seconds.
const TRACK_REBUILD_S: f64 = 1.0;
/// Track window for an L2 vehicle, days either side of now (a halo orbit takes about six months).
const DEEP_TRACK_DAYS: f64 = 90.0;
/// The predicted relative path is recomputed when the clock has moved this far, seconds.
const PROX_PREDICT_REFRESH_S: f64 = 30.0;
/// Pictures shown per vehicle: JWST's current and next targets, or Hubble's first programs.
const FIELDS_PER_VEHICLE: usize = 4;
/// A picture that could not be had is asked for again after this long, seconds.
const FIELD_RETRY_S: f64 = 600.0;
/// Recorded states further apart than this are a gap in the recording; replay falls back to the sources.
const REPLAY_MAX_GAP_S: f64 = 5.0;
/// Replayed telemetry is re-read from the store when the clock has moved this far, seconds.
const REPLAY_TELEMETRY_REFRESH_S: f64 = 1.0;
/// Telemetry older than this at the replay time is not shown, seconds.
const REPLAY_TELEMETRY_WINDOW_S: f64 = 6.0 * 3600.0;
/// A telemetry value older than this is not added to the item's recent history, seconds.
const HISTORY_STALE_S: f64 = 120.0;

struct Loaded<T> {
    data: Arc<T>,
    fetched_at: Utc,
    from_cache: bool,
}

/// One vehicle's maneuver plan: what the operator defined and the latest result.
#[derive(Default)]
struct PlanState {
    def: PlanDef,
    view: Option<PlanView>,
    /// Job id in flight.
    pending: Option<u64>,
    /// The definition changed (or a rerun was asked for) and the result is out of date.
    dirty: bool,
}

/// Plan file name: letters, digits, dash and underscore only, so it cannot name another directory.
fn plan_path(name: &str) -> Result<std::path::PathBuf, String> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!("plan name {name:?}: use letters, digits, - and _"));
    }
    let dir = mctl_core::config::data_dir().ok_or("no home directory for plans")?;
    Ok(dir.join("plans").join(format!("{name}.toml")))
}

struct VehicleModel {
    def: VehicleDef,
    gp: Option<Loaded<GpModel>>,
    oem: Option<Loaded<Oem>>,
    gp_health: Health,
    oem_health: Health,
    supgp: Option<Loaded<mctl_astro::supgp::SupGp>>,
    supgp_health: Health,
    horizons: Option<Loaded<Oem>>,
    horizons_health: Health,
    /// While replaying: the recorded state in force now and the source it was recorded from.
    recorded_now: Option<(SourceKind, State)>,
    ops: Option<mctl_core::schedule::Ops>,
    ops_health: Option<Health>,
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
    /// Latest tick of the telemetry stream's own clock.
    feed_clock: Option<Utc>,
    /// Recent numeric samples per item, and the same as shared slices rebuilt only when sampled.
    history: HashMap<String, VecDeque<f32>>,
    history_view: HashMap<String, Arc<[f32]>>,
    history_last: f64,
    cw_rules: Vec<(RuleDef, Tracker)>,
    latch: Latch,
    prev_conditions: HashSet<String>,
    first_wall_s: Option<f64>,
    recorder: Option<Sender<Record>>,
    state_period_s: f64,
    last_state_rec_s: f64,
    prev: HashMap<String, Observed>,
    prev_eop_loaded: Option<bool>,
    sites: Vec<(Site, String)>,
    active_site: usize,
    /// The user picked or added a site, so a late location lookup must not replace it.
    site_pinned: bool,
    trend_worker: Option<crate::trends::Worker>,
    trends_latest: Option<Arc<mctl_core::world::TrendSet>>,
    pass_worker: Option<crate::passes::Worker>,
    plan_worker: Option<crate::planner::Worker>,
    /// Recorded relative positions and the cached prediction per visiting vehicle.
    prox_hist: HashMap<String, VecDeque<ProxPoint>>,
    prox_pred: HashMap<String, (f64, Arc<Vec<ProxPoint>>)>,
    field_tx: Option<std::sync::mpsc::Sender<(String, FieldRequest)>>,
    /// Picture requests: state and when it was last asked for (a failure is retried after a while).
    fields: HashMap<(String, String), (FieldState, Instant)>,
    /// The recording, opened read-only on the first replay.
    db_path: Option<std::path::PathBuf>,
    replay_store: Option<mctl_store::Store>,
    replay_tel: HashMap<String, (Option<String>, Utc)>,
    replay_tel_t: f64,
    replay_newest_rx: Option<Utc>,
    plans: HashMap<String, PlanState>,
    plan_job_counter: u64,
    pass_entries: HashMap<String, PassEntry>,
    job_counter: u64,
}

/// Latest pass prediction for a vehicle, and the job (if any) that will replace it.
struct PassEntry {
    key: String,
    from: Utc,
    to: Utc,
    sets: Vec<PassSet>,
    beta: Arc<Vec<(Utc, f64)>>,
    pending: Option<(u64, String)>,
}

/// What the previous snapshot said about a vehicle, for transition logging.
#[derive(Default)]
struct Observed {
    active: Option<SourceKind>,
    sources: Vec<(SourceKind, Health, SourceStatus)>,
}

/// The value a rule watches right now, and its data quality.
/// The trajectory a vehicle has from one source, for pairing two vehicles on the same kind.
fn traj_of(vm: &VehicleModel, kind: SourceKind) -> Option<Arc<dyn Trajectory>> {
    match kind {
        SourceKind::Gp => vm
            .gp
            .as_ref()
            .map(|l| l.data.clone() as Arc<dyn Trajectory>),
        SourceKind::SupGp => vm
            .supgp
            .as_ref()
            .map(|l| l.data.clone() as Arc<dyn Trajectory>),
        SourceKind::Oem => vm
            .oem
            .as_ref()
            .map(|l| l.data.clone() as Arc<dyn Trajectory>),
        SourceKind::Horizons => vm
            .horizons
            .as_ref()
            .map(|l| l.data.clone() as Arc<dyn Trajectory>),
    }
}

fn empty_plan(def: &PlanDef) -> PlanView {
    PlanView {
        def: def.clone(),
        status: PlanStatus::Empty,
        t0: Utc(0.0),
        t_end: Utc(0.0),
        burns: vec![],
        nominal: Arc::new(vec![]),
        planned: Arc::new(vec![]),
        report: None,
        separation_km: Arc::new(vec![]),
        passes: vec![],
        notes: vec![],
    }
}

fn failed_plan(def: &PlanDef, t: Utc, why: &str) -> PlanView {
    PlanView {
        status: PlanStatus::Failed(why.to_owned()),
        t0: t,
        t_end: t,
        ..empty_plan(def)
    }
}

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
            Some(i) if telemetry.is_some_and(|t| t.is_stale(i, def.stale_after_s)) => {
                (CwSample::Stale, i.value)
            }
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

/// The configured sites plus, when no site is active in the configuration, the fallback.
fn initial_sites(cfg: &Config) -> (Vec<(Site, String)>, usize) {
    let site = |d: &mctl_core::config::SiteDef| Site {
        name: d.name.clone(),
        lat_deg: d.lat_deg,
        lon_deg: d.lon_deg,
        alt_m: d.alt_m,
    };
    let mut sites: Vec<(Site, String)> = cfg
        .sites
        .iter()
        .map(|d| (site(d), "CONFIGURED".to_owned()))
        .collect();
    let active = if cfg.observer.active.is_empty() {
        sites.push((site(&cfg.observer.fallback), "DEFAULT".to_owned()));
        sites.len() - 1
    } else {
        sites
            .iter()
            .position(|(s, _)| s.name.eq_ignore_ascii_case(&cfg.observer.active))
            .expect("validated by Config")
    };
    (sites, active)
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
                supgp: None,
                supgp_health: Health::Starting,
                horizons: None,
                horizons_health: Health::Starting,
                recorded_now: None,
                ops: None,
                ops_health: d.schedule.as_ref().map(|_| Health::Starting),
                oem_health: Health::Starting,
                track: None,
            })
            .collect();
        let cw_rules = cfg
            .rules
            .iter()
            .map(|r| (r.clone(), Tracker::default()))
            .collect();
        let state_period_s = 1.0 / cfg.store.state_hz;
        let (sites, active_site) = initial_sites(&cfg);
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
            feed_clock: None,
            history: HashMap::new(),
            history_view: HashMap::new(),
            history_last: f64::NEG_INFINITY,
            cw_rules,
            latch: Latch::default(),
            prev_conditions: HashSet::new(),
            first_wall_s: None,
            recorder: None,
            state_period_s,
            last_state_rec_s: f64::NEG_INFINITY,
            prev: HashMap::new(),
            prev_eop_loaded: None,
            sites,
            active_site,
            site_pinned: false,
            trend_worker: None,
            trends_latest: None,
            pass_worker: None,
            plan_worker: None,
            prox_hist: HashMap::new(),
            prox_pred: HashMap::new(),
            field_tx: None,
            fields: HashMap::new(),
            db_path: None,
            replay_store: None,
            replay_tel: HashMap::new(),
            replay_tel_t: f64::NEG_INFINITY,
            replay_newest_rx: None,
            plans: HashMap::new(),
            plan_job_counter: 0,
            pass_entries: HashMap::new(),
            job_counter: 0,
        }
    }

    /// Record an event in the log (wall-clock timestamped).
    pub fn note(&mut self, at: Utc, text: impl Into<String>) {
        let text = text.into();
        self.record(Record::log_event(at, None, "log", &text));
        if self.log.len() == LOG_CAPACITY {
            self.log.pop_front();
        }
        self.log.push_back(LogEntry { at, text });
    }

    /// Send a record to the recorder, if there is one. A recorder that has gone away is logged once
    /// and then dropped, so a dead disk cannot slow the engine.
    fn record(&mut self, r: Record) {
        if let Some(tx) = &self.recorder
            && tx.send(r).is_err()
        {
            tracing::error!("the recorder has stopped; recording is off");
            self.recorder = None;
        }
    }

    pub fn set_recorder(&mut self, tx: Sender<Record>) {
        self.recorder = Some(tx);
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

    /// Serve trend requests from the recording at `db`.
    pub fn enable_trends(&mut self, db: std::path::PathBuf) {
        self.db_path = Some(db.clone());
        self.trend_worker = Some(crate::trends::Worker::start(db));
    }

    /// Ask for trend plots; the result appears in later snapshots (a log line when there is no
    /// recording to read).
    pub fn request_trend(&mut self, vehicle: &str, window_s: f64, wall: Utc) {
        match &self.trend_worker {
            Some(w) => w.submit(crate::trends::Job {
                vehicle: vehicle.to_owned(),
                now: wall,
                window_s,
            }),
            None => self.note(
                wall,
                "trends need the recording, which is off or unavailable",
            ),
        }
    }

    /// Open the recording and tell the clock its span. Returns a reason when replay is impossible.
    pub fn prepare_replay(&mut self, clock: &mut Clock) -> Result<(), String> {
        let path = self
            .db_path
            .clone()
            .ok_or("recording is off; there is nothing to replay")?;
        let store = mctl_store::Store::open_readonly(&path).map_err(|e| e.to_string())?;
        let mut span: Option<(Utc, Utc)> = None;
        for v in &self.vehicles {
            if let Some((a, b)) = store.state_span(&v.def.id).map_err(|e| e.to_string())? {
                span = Some(span.map_or((a, b), |(x, y)| (Utc(x.0.min(a.0)), Utc(y.0.max(b.0)))));
            }
        }
        let span = span.ok_or("the recording has no vehicle states yet")?;
        self.replay_store = Some(store);
        self.replay_tel_t = f64::NEG_INFINITY;
        clock.set_replay_span(Some(span));
        Ok(())
    }

    /// Load what the recording says at `t`: each vehicle's state (first source in priority order that
    /// has one) and the telemetry values in force. Re-queries telemetry at most once per
    /// `REPLAY_TELEMETRY_REFRESH_S` of clock time.
    fn refresh_replay(&mut self, t: Utc, wall: Utc) {
        let Some(store) = &self.replay_store else {
            return;
        };
        let mut failures = Vec::new();
        for v in &mut self.vehicles {
            v.recorded_now = None;
            for k in &v.def.sources {
                match store.state_at(&v.def.id, k.label(), t, REPLAY_MAX_GAP_S) {
                    Ok(Some((r, vel))) => {
                        v.recorded_now = Some((*k, State::new(r, vel)));
                        break;
                    }
                    Ok(None) => {}
                    Err(e) => failures.push(e.to_string()),
                }
            }
        }
        if (t.0 - self.replay_tel_t).abs() >= REPLAY_TELEMETRY_REFRESH_S {
            match store.telemetry_at(t, REPLAY_TELEMETRY_WINDOW_S) {
                Ok(rows) => {
                    self.replay_newest_rx =
                        rows.iter().map(|r| r.3).max_by(|a, b| a.0.total_cmp(&b.0));
                    self.replay_tel = rows
                        .into_iter()
                        .map(|(id, value, text, rx)| {
                            (id, (text.or_else(|| value.map(|v| v.to_string())), rx))
                        })
                        .collect();
                    self.replay_tel_t = t.0;
                }
                Err(e) => failures.push(e.to_string()),
            }
        }
        for f in failures {
            self.note(wall, format!("replay read failed: {f}"));
        }
    }

    pub fn set_field_requests(
        &mut self,
        tx: Option<std::sync::mpsc::Sender<(String, FieldRequest)>>,
    ) {
        self.field_tx = tx;
    }

    /// The pictures worth showing for `vehicle` at `t`, as (request, caption) in display order.
    fn wanted_fields(&self, vm: &VehicleModel, t: Utc) -> Vec<(FieldRequest, String)> {
        match &vm.ops {
            Some(Ops::Jwst(s)) => {
                let mut out: Vec<(FieldRequest, String)> = Vec::new();
                let visits = s.current(t).into_iter().chain(s.upcoming(t));
                for v in visits {
                    if v.is_maintenance() || v.target.is_empty() {
                        continue;
                    }
                    let req = FieldRequest::Target(v.target.clone());
                    if out.iter().all(|(r, _)| *r != req) {
                        out.push((req, v.instrument.clone()));
                    }
                    if out.len() == FIELDS_PER_VEHICLE {
                        break;
                    }
                }
                out
            }
            Some(Ops::Hst(r)) => r
                .science
                .iter()
                .take(FIELDS_PER_VEHICLE)
                .map(|p| {
                    (
                        FieldRequest::HstProgram(p.id.clone()),
                        format!("{} {}", p.id, p.title),
                    )
                })
                .collect(),
            None => Vec::new(),
        }
    }

    /// Ask for pictures not yet requested, and return the views for the current wishes.
    fn field_views(&mut self, t: Utc) -> Vec<Vec<Arc<FieldView>>> {
        let mut out = Vec::new();
        for i in 0..self.vehicles.len() {
            let id = self.vehicles[i].def.id.clone();
            let wanted = self.wanted_fields(&self.vehicles[i], t);
            let mut views = Vec::new();
            for (req, label) in wanted {
                let key = req.key();
                let known = self.fields.get(&(id.clone(), key.clone())).cloned();
                let state = match known {
                    Some((s, at))
                        if !matches!(s, FieldState::Unavailable(_))
                            || at.elapsed().as_secs_f64() < FIELD_RETRY_S =>
                    {
                        s
                    }
                    _ => {
                        let sent = self
                            .field_tx
                            .as_ref()
                            .is_some_and(|tx| tx.send((id.clone(), req.clone())).is_ok());
                        let state = if sent {
                            FieldState::Loading
                        } else {
                            FieldState::Unavailable("pictures are not being fetched".into())
                        };
                        self.fields
                            .insert((id.clone(), key.clone()), (state.clone(), Instant::now()));
                        state
                    }
                };
                views.push(Arc::new(FieldView { key, label, state }));
            }
            out.push(views);
        }
        out
    }

    /// Relative motion of each visiting vehicle about its target, from a source both vehicles have.
    fn prox_views(&mut self, vehicles: &[VehicleView], t: Utc) -> Vec<Arc<ProxView>> {
        let cfg = self.cfg.prox.clone();
        let cap = (cfg.window_min * 60.0 / crate::prox::STEP_S) as usize + 1;
        let mut out = Vec::new();
        for ci in 0..self.vehicles.len() {
            let Some(target_id) = self.vehicles[ci].def.visits.clone() else {
                continue;
            };
            let chaser_id = self.vehicles[ci].def.id.clone();
            let (Some(cv), Some(tv)) = (
                vehicles.iter().find(|v| v.id == chaser_id),
                vehicles.iter().find(|v| v.id == target_id),
            ) else {
                continue;
            };
            let (Some(csol), Some(tsol)) = (&cv.solution, &tv.solution) else {
                continue;
            };
            // Compare like with like: the target's state from the visitor's own source when it has one.
            let kind = csol.source;
            let same = tv
                .sources
                .iter()
                .find(|s| s.kind == kind)
                .and_then(|s| s.state_gcrf);
            let (tstate, note) = match same {
                Some(st) => (
                    State::new([st[0], st[1], st[2]], [st[3], st[4], st[5]]),
                    format!("{} FOR BOTH VEHICLES", kind.label()),
                ),
                None => (
                    tsol.state_gcrf,
                    format!(
                        "MIXED SOURCES: {} FOR THE VISITOR, {} FOR THE TARGET",
                        kind.label(),
                        tsol.source.label()
                    ),
                ),
            };
            let hist = self.prox_hist.entry(chaser_id.clone()).or_default();
            if let Some((rel, _, _)) = crate::prox::relative(&tstate, &csol.state_gcrf) {
                let last = hist.back().map(|p| p.t.0);
                match last {
                    Some(l) if t.0 - l < 0.0 || t.0 - l > 5.0 * crate::prox::STEP_S => hist.clear(),
                    _ => {}
                }
                if hist
                    .back()
                    .is_none_or(|p| t.0 - p.t.0 >= crate::prox::STEP_S)
                {
                    hist.push_back(ProxPoint { t, lvlh_km: rel });
                    while hist.len() > cap {
                        hist.pop_front();
                    }
                }
            }
            let history = Arc::new(hist.iter().copied().collect::<Vec<_>>());
            let cached = self.prox_pred.get(&chaser_id);
            let predicted = match cached {
                Some((at, p)) if (t.0 - at).abs() < PROX_PREDICT_REFRESH_S && t.0 >= *at => {
                    p.clone()
                }
                _ => {
                    let pair = (
                        traj_of(&self.vehicles[ci], kind),
                        self.vehicles
                            .iter()
                            .find(|v| v.def.id == target_id)
                            .and_then(|v| traj_of(v, kind)),
                    );
                    let p = Arc::new(match pair {
                        (Some(c), Some(tg)) => {
                            crate::prox::predict(tg.as_ref(), c.as_ref(), t, cfg.window_min)
                        }
                        _ => Vec::new(),
                    });
                    self.prox_pred.insert(chaser_id.clone(), (t.0, p.clone()));
                    p
                }
            };
            if let Some(v) = crate::prox::view(
                &chaser_id,
                &target_id,
                &tstate,
                &csol.state_gcrf,
                note,
                history,
                predicted,
                &cfg,
            ) {
                out.push(Arc::new(v));
            }
        }
        out
    }

    pub fn enable_plan_worker(&mut self) {
        self.plan_worker = Some(crate::planner::Worker::start());
    }

    /// Apply an edit to a vehicle's plan. The result is recomputed on the next snapshot.
    pub fn plan_edit(&mut self, vehicle: &str, edit: PlanCommand, wall: Utc) {
        let Some(vm) = self.vehicles.iter().find(|v| v.def.id == vehicle) else {
            self.note(wall, format!("plan rejected: unknown vehicle {vehicle:?}"));
            return;
        };
        if vm.def.regime != Regime::Earth {
            self.note(
                wall,
                format!(
                    "plan rejected: the planner is for Earth-orbit vehicles, not {}",
                    vm.def.name
                ),
            );
            return;
        }
        let st = self.plans.entry(vehicle.to_owned()).or_default();
        let mut def = st.def.clone();
        let mut msg = String::new();
        let outcome: Result<(), String> = (|| {
            match edit {
                PlanCommand::AddBurn(b) => {
                    msg = format!("plan: burn {} added", def.burns.len() + 1);
                    def.burns.push(b);
                }
                PlanCommand::DelBurn(n) => {
                    if n == 0 || n > def.burns.len() {
                        return Err(format!("no burn {n}; the plan has {}", def.burns.len()));
                    }
                    def.burns.remove(n - 1);
                    msg = format!("plan: burn {n} removed");
                }
                PlanCommand::Clear => {
                    def.burns.clear();
                    msg = "plan: burns cleared".into();
                }
                PlanCommand::Horizon(r) => {
                    def.horizon_revs = r;
                    msg = format!("plan: horizon {r} revolutions");
                }
                PlanCommand::Force { name, on } => {
                    match name.as_str() {
                        "zonal" | "j2" => def.zonal = on,
                        "sun" => def.sun = on,
                        "moon" => def.moon = on,
                        "drag" => def.drag = on,
                        "srp" => def.srp = on,
                        other => {
                            return Err(format!(
                                "unknown force {other:?}; use zonal, sun, moon, drag or srp"
                            ));
                        }
                    }
                    msg = format!("plan: {name} {}", if on { "on" } else { "off" });
                }
                PlanCommand::DragScale(x) => {
                    def.drag_scale = x;
                    msg = format!("plan: drag scale {x}");
                }
                PlanCommand::Save(name) => {
                    let path = plan_path(&name)?;
                    std::fs::create_dir_all(path.parent().expect("has a parent"))
                        .map_err(|e| format!("{}: {e}", path.display()))?;
                    std::fs::write(&path, def.to_toml().map_err(|e| e.to_string())?)
                        .map_err(|e| format!("{}: {e}", path.display()))?;
                    msg = format!("plan saved to {}", path.display());
                    return Ok(());
                }
                PlanCommand::Load(name) => {
                    let path = plan_path(&name)?;
                    let text = std::fs::read_to_string(&path)
                        .map_err(|e| format!("{}: {e}", path.display()))?;
                    def = PlanDef::from_toml(&text).map_err(|e| e.to_string())?;
                    msg = format!("plan loaded from {}", path.display());
                }
            }
            def.validate().map_err(|e| e.to_string())
        })();
        match outcome {
            Ok(()) => {
                if def != st.def {
                    st.def = def;
                    st.dirty = true;
                }
                self.note(wall, msg);
            }
            Err(e) => self.note(wall, format!("plan rejected: {e}")),
        }
    }

    pub fn plan_rerun(&mut self, vehicle: &str) {
        if let Some(st) = self.plans.get_mut(vehicle) {
            st.dirty = true;
        }
    }

    /// Start planner jobs for plans that changed, collect finished ones.
    fn update_plans(
        &mut self,
        vehicles: &[VehicleView],
        t: Utc,
        orientation: Orientation,
        wall: Utc,
    ) {
        if let Some(w) = &self.plan_worker {
            for done in w.poll() {
                for st in self
                    .plans
                    .values_mut()
                    .filter(|s| s.pending == Some(done.id))
                {
                    st.pending = None;
                    st.view = Some(match &done.result {
                        Ok(v) => v.clone(),
                        Err(e) => failed_plan(&st.def, t, e),
                    });
                }
                if let Err(e) = &done.result {
                    self.log_plan_failure(wall, e);
                }
            }
        }
        let ids: Vec<String> = self
            .plans
            .iter()
            .filter(|(_, s)| s.dirty)
            .map(|(k, _)| k.clone())
            .collect();
        for id in ids {
            let (Some(vm), Some(view)) = (
                self.vehicles.iter().find(|v| v.def.id == id),
                vehicles.iter().find(|v| v.id == id),
            ) else {
                continue;
            };
            let st = self.plans.get_mut(&id).expect("listed");
            if st.def.burns.is_empty() {
                st.dirty = false;
                st.view = None;
                continue;
            }
            // Wait for a state to start from; the plan stays dirty until there is one.
            let Some(sol) = &view.solution else { continue };
            let mut parts: Vec<Arc<dyn Trajectory>> = Vec::new();
            for k in &vm.def.sources {
                match k {
                    SourceKind::Gp => parts.extend(
                        vm.gp
                            .as_ref()
                            .map(|l| l.data.clone() as Arc<dyn Trajectory>),
                    ),
                    SourceKind::SupGp => parts.extend(
                        vm.supgp
                            .as_ref()
                            .map(|l| l.data.clone() as Arc<dyn Trajectory>),
                    ),
                    SourceKind::Oem => parts.extend(
                        vm.oem
                            .as_ref()
                            .map(|l| l.data.clone() as Arc<dyn Trajectory>),
                    ),
                    SourceKind::Horizons => parts.extend(
                        vm.horizons
                            .as_ref()
                            .map(|l| l.data.clone() as Arc<dyn Trajectory>),
                    ),
                }
            }
            let ballistic = vm
                .oem
                .as_ref()
                .map(|o| o.data.drag.cd_a_over_m())
                .filter(|b| b.is_finite())
                .or(vm.def.ballistic_m2_per_kg);
            self.plan_job_counter += 1;
            let job = crate::planner::Job {
                id: self.plan_job_counter,
                def: st.def.clone(),
                t0: t,
                start: sol.state_gcrf,
                nominal: Arc::new(crate::passes::Chain { parts }),
                orientation,
                ballistic,
                sites: self.sites.iter().map(|(s, _)| s.clone()).collect(),
                mask_deg: self.cfg.observer.mask_deg,
            };
            st.dirty = false;
            st.pending = Some(job.id);
            match &self.plan_worker {
                Some(w) => w.submit(job),
                None => {
                    let result = crate::planner::compute(&job);
                    st.pending = None;
                    st.view = Some(match &result {
                        Ok(v) => v.clone(),
                        Err(e) => failed_plan(&st.def, t, e),
                    });
                    if let Err(e) = &result {
                        let e = e.clone();
                        self.log_plan_failure(wall, &e);
                    }
                }
            }
        }
    }

    fn log_plan_failure(&mut self, wall: Utc, e: &str) {
        self.note(wall, format!("plan failed: {e}"));
    }

    fn plans_view(&self) -> Vec<(String, Arc<PlanView>)> {
        self.plans
            .iter()
            .map(|(id, st)| {
                let mut v = st.view.clone().unwrap_or_else(|| empty_plan(&st.def));
                v.def = st.def.clone();
                v.status = if st.pending.is_some() || st.dirty {
                    if st.def.burns.is_empty() {
                        PlanStatus::Empty
                    } else {
                        PlanStatus::Computing
                    }
                } else if st.def.burns.is_empty() {
                    PlanStatus::Empty
                } else {
                    v.status
                };
                (id.clone(), Arc::new(v))
            })
            .collect()
    }

    /// Run pass predictions on a worker thread instead of inline.
    pub fn enable_pass_worker(&mut self) {
        self.pass_worker = Some(crate::passes::Worker::start());
    }

    pub fn select_site(&mut self, name: &str, wall: Utc) -> Result<(), String> {
        let i = self
            .sites
            .iter()
            .position(|(s, _)| s.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| {
                format!(
                    "unknown site {name:?}; known: {}",
                    self.sites
                        .iter()
                        .map(|(s, _)| s.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;
        self.active_site = i;
        self.site_pinned = true;
        let n = self.sites[i].0.name.clone();
        self.note(wall, format!("observer site: {n}"));
        Ok(())
    }

    /// A site found by GeoClue or IP lookup. It replaces an earlier lookup of the same name and becomes
    /// active unless the user has already chosen a site themselves.
    pub fn adopt_resolved_site(&mut self, site: Site, origin: String, wall: Utc) {
        let active_is_fallback = !self.site_pinned
            && self
                .sites
                .get(self.active_site)
                .is_some_and(|(_, o)| o == "DEFAULT");
        self.sites
            .retain(|(s, o)| !(s.name == site.name && o != "CONFIGURED"));
        self.note(
            wall,
            format!(
                "observer site {} resolved: {:.4}, {:.4} ({origin})",
                site.name, site.lat_deg, site.lon_deg
            ),
        );
        self.sites.push((site, origin));
        if active_is_fallback {
            self.active_site = self.sites.len() - 1;
        } else {
            self.active_site = self.active_site.min(self.sites.len() - 1);
        }
    }

    /// Persist the active site to the user configuration (`:site save [name]`).
    pub fn save_site(&mut self, name: Option<&str>, wall: Utc) -> Result<(), String> {
        let path = mctl_core::config::user_config_path()
            .ok_or("no home directory for the user configuration")?;
        self.save_site_at(name, &path, wall)
    }

    fn save_site_at(
        &mut self,
        name: Option<&str>,
        path: &std::path::Path,
        wall: Utc,
    ) -> Result<(), String> {
        let (site, origin) = self
            .sites
            .get(self.active_site)
            .cloned()
            .ok_or("no active site")?;
        let name = match (name, origin.as_str()) {
            (Some(n), _) => n.to_owned(),
            (None, "DEFAULT") => {
                return Err(
                    "the default site is not your location; give a name: site save <name>".into(),
                );
            }
            (None, _) => site.name.clone(),
        };
        let def = mctl_core::config::SiteDef {
            name: name.clone(),
            lat_deg: site.lat_deg,
            lon_deg: site.lon_deg,
            alt_m: site.alt_m,
        };
        mctl_core::config::append_site(path, &def).map_err(|e| e.to_string())?;
        if let Some((s, o)) = self.sites.get_mut(self.active_site) {
            s.name = name.clone();
            *o = "CONFIGURED".into();
        }
        self.note(wall, format!("site saved to {}: {name}", path.display()));
        Ok(())
    }

    pub fn add_site(&mut self, site: Site, wall: Utc) -> Result<(), String> {
        mctl_core::config::SiteDef {
            name: site.name.clone(),
            lat_deg: site.lat_deg,
            lon_deg: site.lon_deg,
            alt_m: site.alt_m,
        }
        .validate()?;
        if self
            .sites
            .iter()
            .any(|(s, _)| s.name.eq_ignore_ascii_case(&site.name))
        {
            return Err(format!("site {:?} already exists", site.name));
        }
        self.note(
            wall,
            format!(
                "site added for this session: {} ({:.4}, {:.4}, {:.0} m)",
                site.name, site.lat_deg, site.lon_deg, site.alt_m
            ),
        );
        self.sites.push((site, "ADDED".to_owned()));
        self.active_site = self.sites.len() - 1;
        self.site_pinned = true;
        Ok(())
    }

    fn observer_view(
        &self,
        sun: Option<(&[f64; 3], &[[f64; 3]; 3])>,
        moon: Option<[f64; 3]>,
    ) -> ObserverView {
        let sites = self
            .sites
            .iter()
            .map(|(s, origin)| {
                let sun_look = sun.map(|(dir, m)| {
                    let d = nalgebra::Vector3::new(dir[0], dir[1], dir[2]).normalize();
                    let ecef = nalgebra::Matrix3::from_fn(|r, c| m[r][c]) * d;
                    let enu = s.enu_from_ecef() * ecef;
                    (
                        enu.x.atan2(enu.y).to_degrees().rem_euclid(360.0),
                        enu.z.asin().to_degrees(),
                    )
                });
                // The Moon is near enough that parallax (up to a degree) matters: use the topocentric vector.
                let moon_look = moon.zip(sun.map(|(_, m)| m)).map(|(r, m)| {
                    let ecef = nalgebra::Matrix3::from_fn(|i, j| m[i][j])
                        * nalgebra::Vector3::new(r[0], r[1], r[2]);
                    let l = s.look(&ecef);
                    (l.az_deg, l.el_deg)
                });
                SiteView {
                    name: s.name.clone(),
                    lat_deg: s.lat_deg,
                    lon_deg: s.lon_deg,
                    alt_m: s.alt_m,
                    origin: origin.clone(),
                    sun_az_deg: sun_look.map(|x| x.0),
                    sun_el_deg: sun_look.map(|x| x.1),
                    moon_az_deg: moon_look.map(|x| x.0),
                    moon_el_deg: moon_look.map(|x| x.1),
                }
            })
            .collect();
        ObserverView {
            sites,
            active: self.active_site,
            mask_deg: self.cfg.observer.mask_deg,
        }
    }

    /// Collect finished predictions, start new ones when the inputs or the clock moved out of the
    /// window, and fill each vehicle's pass sets (with live look angles).
    fn update_passes(&mut self, vehicles: &mut [VehicleView], t: Utc, orientation: Orientation) {
        let done = self
            .pass_worker
            .as_ref()
            .map(crate::passes::Worker::poll)
            .unwrap_or_default();
        for d in done {
            if let Some(e) = self.pass_entries.get_mut(&d.vehicle)
                && e.pending.as_ref().is_some_and(|(id, _)| *id == d.id)
            {
                let (_, key) = e.pending.take().expect("checked");
                e.key = key;
                e.from = d.sets.first().map_or(t, |s| s.from);
                e.to = d.sets.first().map_or(t, |s| s.to);
                e.sets = d.sets;
                e.beta = Arc::new(d.beta);
            }
        }
        let o = self.cfg.observer.clone();
        let sites_key: String = self
            .sites
            .iter()
            .map(|(s, _)| format!("{}@{},{},{}", s.name, s.lat_deg, s.lon_deg, s.alt_m))
            .collect::<Vec<_>>()
            .join(";");
        for (vm, view) in self.vehicles.iter().zip(vehicles.iter_mut()) {
            // A vehicle at L2 is above the horizon for about half of every day; passes are meaningless.
            if vm.def.regime == Regime::L2 {
                continue;
            }
            let mut parts: Vec<Arc<dyn Trajectory>> = Vec::new();
            let mut labels = Vec::new();
            let mut key = format!("{sites_key}|{}|", o.mask_deg);
            for k in &vm.def.sources {
                match k {
                    SourceKind::Gp => {
                        if let Some(g) = &vm.gp {
                            parts.push(g.data.clone());
                            labels.push("GP/SGP4");
                            key += &format!("gp{}", g.fetched_at.0);
                        }
                    }
                    SourceKind::SupGp => {
                        if let Some(x) = &vm.supgp {
                            parts.push(x.data.clone());
                            labels.push("SUP-GP");
                            key += &format!("sup{}", x.fetched_at.0);
                        }
                    }
                    SourceKind::Oem => {
                        if let Some(x) = &vm.oem {
                            parts.push(x.data.clone());
                            labels.push("OEM");
                            key += &format!("oem{}", x.fetched_at.0);
                        }
                    }
                    SourceKind::Horizons => {
                        if let Some(x) = &vm.horizons {
                            parts.push(x.data.clone());
                            labels.push("HORIZONS");
                            key += &format!("hz{}", x.fetched_at.0);
                        }
                    }
                }
            }
            if parts.is_empty() {
                continue;
            }
            let entry = self
                .pass_entries
                .entry(vm.def.id.clone())
                .or_insert(PassEntry {
                    key: String::new(),
                    from: t,
                    to: t,
                    sets: vec![],
                    beta: Arc::new(vec![]),
                    pending: None,
                });
            let in_window = t.0 >= entry.from.0 && t.0 <= entry.to.0 - 86_400.0;
            let fresh = entry.key == key && in_window;
            let already_asked = entry.pending.as_ref().is_some_and(|(_, k)| *k == key);
            if !fresh && !already_asked {
                self.job_counter += 1;
                let job = crate::passes::Job {
                    id: self.job_counter,
                    vehicle: vm.def.id.clone(),
                    traj: Arc::new(crate::passes::Chain { parts }),
                    source: labels.join(" then "),
                    sites: self.sites.iter().map(|(s, _)| s.clone()).collect(),
                    t0: t,
                    days: o.pass_days,
                    mask_deg: o.mask_deg,
                    step_s: o.step_s,
                    orientation,
                };
                entry.pending = Some((job.id, key));
                match &self.pass_worker {
                    Some(w) => w.submit(job),
                    None => {
                        let d = crate::passes::compute(&job);
                        let (_, key) = entry.pending.take().expect("just set");
                        entry.key = key;
                        entry.from = t;
                        entry.to = t.plus(o.pass_days * 86_400.0);
                        entry.sets = d.sets;
                        entry.beta = Arc::new(d.beta);
                    }
                }
            }
            let mut sets = entry.sets.clone();
            if let Some(sol) = &view.solution {
                for set in &mut sets {
                    if let Some((site, _)) = self.sites.iter().find(|(s, _)| s.name == set.site) {
                        let l = site.look(&sol.state_itrf.r);
                        set.look_now = Some(LookView {
                            az_deg: l.az_deg,
                            el_deg: l.el_deg,
                            range_km: l.range_km,
                        });
                    }
                }
            }
            view.passes = sets;
            view.beta_series = entry.beta.clone();
        }
    }

    /// Acknowledge the master caution and warning latches.
    pub fn ack(&mut self, wall: Utc) {
        let was = (self.latch.caution, self.latch.warning);
        self.latch.ack(wall);
        self.record(Record::CwLog {
            t: wall,
            rule: "MASTER".into(),
            from_state: "LATCHED".into(),
            to_state: "ACKNOWLEDGED".into(),
            value: None,
            acked_at: Some(wall),
        });
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
                vehicle: def.scope().map(str::to_owned),
            });
        }
        for (def, tr, value) in events {
            let v = value.map_or(String::new(), |v| format!(" ({v:.2} {})", def.units));
            self.record(Record::CwLog {
                t: wall,
                rule: def.id.clone(),
                from_state: tr.from.label().into(),
                to_state: tr.to.label().into(),
                value,
                acked_at: None,
            });
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

    /// Append one sample per numeric item once `HISTORY_PERIOD_S` has passed. A value that has not
    /// been refreshed for a few periods is skipped (a gap), never repeated as if it were current.
    fn sample_history(&mut self, wall: Utc) {
        if wall.0 - self.history_last < HISTORY_PERIOD_S {
            return;
        }
        self.history_last = wall.0;
        for (id, (text, rx)) in &self.telemetry {
            let Some(v) = text.as_deref().and_then(|t| t.trim().parse::<f32>().ok()) else {
                continue;
            };
            if !v.is_finite() || wall.minus(*rx) > HISTORY_STALE_S {
                continue;
            }
            let h = self.history.entry(id.clone()).or_default();
            h.push_back(v);
            while h.len() > HISTORY_LEN {
                h.pop_front();
            }
            self.history_view
                .insert(id.clone(), h.iter().copied().collect());
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
                let value = text.as_deref().and_then(|t| t.trim().parse::<f64>().ok());
                self.record(Record::Telemetry {
                    item_id: id.clone(),
                    t_rx: rx,
                    value,
                    text: text.clone().filter(|_| value.is_none()),
                });
                self.telemetry.insert(id, (text, rx));
            }
            Event::TelemetryHealth(h) => self.telemetry_health = Some(h),
            Event::TelemetryClock { data_time } => {
                // Never move backwards: a reconnect snapshot can replay an older stamp.
                if self.feed_clock.is_none_or(|c| data_time.0 > c.0) {
                    self.feed_clock = Some(data_time);
                }
            }
            Event::Field {
                vehicle,
                key,
                state,
            } => {
                self.fields.insert((vehicle, key), (state, Instant::now()));
            }
            Event::Ops { vehicle, ops, .. } => {
                if let Some(v) = self.vehicles.iter_mut().find(|v| v.def.id == vehicle) {
                    v.ops = Some(ops);
                }
            }
            Event::ObserverNote(text) => {
                let now = Utc::now().unwrap_or(Utc(0.0));
                self.note(now, text);
            }
            Event::Observer(r) => {
                let now = Utc::now().unwrap_or(Utc(0.0));
                self.adopt_resolved_site(
                    Site {
                        name: r.name,
                        lat_deg: r.lat_deg,
                        lon_deg: r.lon_deg,
                        alt_m: r.alt_m,
                    },
                    r.origin,
                    now,
                );
            }
            Event::Data {
                key,
                payload,
                info:
                    DataInfo {
                        from_cache,
                        fetched_at,
                        raw,
                    },
            } => match (key, payload) {
                (SourceKey::Gp(id), Payload::Gp(m)) => {
                    if let Some(raw) = raw {
                        self.record(Record::Elements {
                            vehicle: id.clone(),
                            source: SourceKind::Gp.label().into(),
                            epoch: Some(m.epoch),
                            fetched_at,
                            blob: raw.to_string(),
                        });
                    }
                    if let Some(v) = self.vehicles.iter_mut().find(|v| v.def.id == id) {
                        v.gp = Some(Loaded {
                            data: Arc::new(m),
                            fetched_at,
                            from_cache,
                        });
                        v.track = None;
                    }
                }
                (SourceKey::SupGp(id), Payload::SupGp(s)) => {
                    if let Some(raw) = raw {
                        self.record(Record::Elements {
                            vehicle: id.clone(),
                            source: SourceKind::SupGp.label().into(),
                            epoch: Some(s.newest_epoch()),
                            fetched_at,
                            blob: raw.to_string(),
                        });
                    }
                    if let Some(v) = self.vehicles.iter_mut().find(|v| v.def.id == id) {
                        v.supgp = Some(Loaded {
                            data: Arc::new(*s),
                            fetched_at,
                            from_cache,
                        });
                        v.track = None;
                    }
                }
                (SourceKey::Oem(id), Payload::Oem(o)) => {
                    let notes: Vec<String> = o
                        .maneuver_notes()
                        .into_iter()
                        .map(|n| format!("OEM comment (possible maneuver, raw): {n}"))
                        .collect();
                    let now = Utc::now().unwrap_or(Utc(0.0));
                    for n in notes {
                        self.note(now, n);
                    }
                    if let Some(raw) = raw {
                        self.record(Record::Elements {
                            vehicle: id.clone(),
                            source: SourceKind::Oem.label().into(),
                            epoch: Some(o.creation_date),
                            fetched_at,
                            blob: raw.to_string(),
                        });
                    }
                    if let Some(v) = self.vehicles.iter_mut().find(|v| v.def.id == id) {
                        v.oem = Some(Loaded {
                            data: Arc::new(*o),
                            fetched_at,
                            from_cache,
                        });
                        v.track = None;
                    }
                }
                (SourceKey::Horizons(id), Payload::Horizons(mut o)) => {
                    // The reply carries no creation time; its age is the age of the download.
                    o.creation_date = fetched_at;
                    if let Some(raw) = raw {
                        self.record(Record::Elements {
                            vehicle: id.clone(),
                            source: SourceKind::Horizons.label().into(),
                            epoch: Some(fetched_at),
                            fetched_at,
                            blob: raw.to_string(),
                        });
                    }
                    if let Some(v) = self.vehicles.iter_mut().find(|v| v.def.id == id) {
                        v.horizons = Some(Loaded {
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
                SourceKey::SupGp(id) => {
                    if let Some(v) = self.vehicles.iter_mut().find(|v| v.def.id == id) {
                        v.supgp_health = health;
                    }
                }
                SourceKey::Oem(id) => {
                    if let Some(v) = self.vehicles.iter_mut().find(|v| v.def.id == id) {
                        v.oem_health = health;
                    }
                }
                SourceKey::Horizons(id) => {
                    if let Some(v) = self.vehicles.iter_mut().find(|v| v.def.id == id) {
                        v.horizons_health = health;
                    }
                }
                SourceKey::Schedule(id) => {
                    if let Some(v) = self.vehicles.iter_mut().find(|v| v.def.id == id) {
                        v.ops_health = Some(health);
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
            self.cfg.sources.celestrak_supgp.stale_after_s,
        );
        let limits = self.cfg.limits.clone();
        let (past_rev, future_rev, samples) = (
            self.cfg.engine.track_past_rev,
            self.cfg.engine.track_future_rev,
            self.cfg.engine.track_samples,
        );
        let replaying = clock.mode == mctl_core::world::ClockMode::Replay;
        if replaying {
            self.refresh_replay(t, wall);
        } else {
            for v in &mut self.vehicles {
                v.recorded_now = None;
            }
        }
        let eop = self.eop.as_ref().map(|l| l.data.clone());
        let mut revision = self.track_revision;
        let mut vehicles = self
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
        for view in &mut vehicles {
            view.visitors = self
                .vehicles
                .iter()
                .filter(|m| m.def.visits.as_deref() == Some(view.id.as_str()))
                .map(|m| m.def.id.clone())
                .collect();
        }
        self.seq += 1;
        self.observe(&vehicles, &eop_view, wall);
        // Only real time is recorded: a SIM state is a simulation, not an observation.
        if clock.is_live() && wall.0 - self.last_state_rec_s >= self.state_period_s {
            self.last_state_rec_s = wall.0;
            for v in &vehicles {
                // Every source with a state is recorded (FR-60), so source differences can be trended.
                for src in &v.sources {
                    let Some(st) = src.state_gcrf else { continue };
                    let quality = match (&v.solution, src.status == SourceStatus::Active) {
                        (Some(s), true) => s.quality.label().to_owned(),
                        _ => "STANDBY".to_owned(),
                    };
                    self.record(Record::State {
                        vehicle: v.id.clone(),
                        source: src.kind.label().into(),
                        t,
                        frame: "GCRF",
                        r_km: [st[0], st[1], st[2]],
                        v_km_s: [st[3], st[4], st[5]],
                        quality,
                    });
                }
            }
        }
        if !replaying {
            self.sample_history(wall);
        }
        // Replaying, "now" is the replay time and the stream clock is the newest recorded value.
        let (tel_map, tel_now, feed_age_s) = if replaying {
            (
                &self.replay_tel,
                t,
                Some(self.replay_newest_rx.map_or(f64::INFINITY, |r| t.minus(r))),
            )
        } else {
            (
                &self.telemetry,
                wall,
                self.feed_clock.map(|c| wall.minus(c)),
            )
        };
        let telemetry = self.telemetry_health.clone().map(|health| TelemetryView {
            health,
            subscribed: self.telemetry_meta.len(),
            group_totals: group_totals(&self.telemetry_meta),
            feed_age_s,
            items: self
                .telemetry_meta
                .iter()
                .filter_map(|m| {
                    let (text, rx) = tel_map.get(&m.id)?;
                    Some(TelemetryItem {
                        id: m.id.clone(),
                        group: m.group.clone(),
                        description: m.description.clone(),
                        label: m.label.clone(),
                        section: m.section.clone(),
                        row: m.row.clone(),
                        col: m.col.clone(),
                        units: m.units.clone(),
                        value: text.as_deref().and_then(|t| t.trim().parse().ok()),
                        text: text.clone(),
                        rx: *rx,
                        age_s: tel_now.minus(*rx),
                        history: if replaying {
                            Default::default()
                        } else {
                            self.history_view.get(&m.id).cloned().unwrap_or_default()
                        },
                    })
                })
                .collect(),
        });
        self.update_passes(&mut vehicles, t, orientation);
        self.update_plans(&vehicles, t, orientation, wall);
        let prox = self.prox_views(&vehicles, t);
        let field_views = self.field_views(t);
        for (view, fields) in vehicles.iter_mut().zip(field_views) {
            view.fields = fields;
        }
        let finished = self
            .trend_worker
            .as_ref()
            .map(crate::trends::Worker::poll)
            .unwrap_or_default();
        for r in finished {
            match r {
                Ok(set) => self.trends_latest = Some(Arc::new(set)),
                Err(e) => self.note(wall, format!("trends failed: {e}")),
            }
        }
        let sun = vehicles
            .iter()
            .find_map(|v| v.solution.as_ref())
            .map(|s| (s.sun_gcrf_km, s.ecef_from_gcrf));
        let moon = mctl_astro::moon::moon_gcrf(t).ok().map(|m| [m.x, m.y, m.z]);
        let observer = self.observer_view(sun.as_ref().map(|(a, b)| (a, b)), moon);
        let cw = self.cw_step(&vehicles, &eop_view, telemetry.as_ref(), wall);
        World {
            telemetry,
            cw,
            observer,
            trends: self.trends_latest.clone(),
            plans: self.plans_view(),
            prox,
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
    use mctl_astro::local_frames::{LocalFrame, to_local};
    let d = other.r - reference.r;
    let rtn = to_local(LocalFrame::Rtn, reference, &d).unwrap_or(d);
    ([rtn.x, rtn.y, rtn.z], d.norm())
}

#[allow(clippy::too_many_arguments)]
fn vehicle_view(
    v: &mut VehicleModel,
    t: Utc,
    orientation: Orientation,
    stale: (f64, f64, f64),
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
            SourceKind::SupGp => sample(
                *k,
                &v.supgp,
                &v.supgp_health,
                |s| s.segment_for(t).epoch,
                stale.2,
                t,
            ),
            SourceKind::Oem => sample(*k, &v.oem, &v.oem_health, |o| o.creation_date, stale.1, t),
            SourceKind::Horizons => sample(
                *k,
                &v.horizons,
                &v.horizons_health,
                |o| o.creation_date,
                v.def.horizons.as_ref().map_or(0.0, |h| h.stale_after_s),
                t,
            ),
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
    // Replaying: what was recorded beats what the sources say now (FR-21); a gap falls back to them.
    let (active, quality, active_state) = match v.recorded_now {
        Some((kind, st)) => (
            samples.iter().position(|s| s.kind == kind),
            Quality::Rec,
            Some(st),
        ),
        None => (active, quality, active_state),
    };

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
                state_gcrf: match &s.state {
                    Some(Ok(st)) => Some([st.r.x, st.r.y, st.r.z, st.v.x, st.v.y, st.v.z]),
                    _ => None,
                },
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
                    SourceKind::SupGp => v.supgp.as_ref().map(|l| &*l.data as &dyn Trajectory),
                    SourceKind::Oem => v.oem.as_ref().map(|l| &*l.data as &dyn Trajectory),
                    SourceKind::Horizons => {
                        v.horizons.as_ref().map(|l| &*l.data as &dyn Trajectory)
                    }
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
                            v.def.regime,
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
    let deep_view = (v.def.regime == Regime::L2)
        .then(|| {
            solution.as_ref().map(|s| {
                let (r, vel) = (s.state_gcrf.r, s.state_gcrf.v);
                let sun = nalgebra::Vector3::from(s.sun_gcrf_km);
                let d = mctl_astro::l2::summary(&r, &vel, &sun);
                DeepView {
                    dist_km: d.dist_km,
                    speed_km_s: d.speed_km_s,
                    range_rate_km_s: d.range_rate_km_s,
                    light_time_s: d.light_time_s,
                    off_axis_deg: d.off_axis_deg,
                    rot_km: d.rot_km,
                }
            })
        })
        .flatten();
    VehicleView {
        id: v.def.id.clone(),
        name: v.def.name.clone(),
        norad: if v.def.norad == 0 {
            v.gp.as_ref().map_or(0, |g| g.data.norad)
        } else {
            v.def.norad
        },
        resolved_name: v
            .gp
            .as_ref()
            .map(|g| g.data.name.clone())
            .filter(|_| v.def.lookup.is_some()),
        visits: v.def.visits.clone(),
        visitors: vec![],
        telemetry: v.def.telemetry.clone(),
        regime: v.def.regime,
        schedule: v.def.schedule.clone(),
        deep: deep_view,
        reference_burns: v
            .oem
            .as_ref()
            .map(|o| o.data.reference_burns())
            .unwrap_or_default(),
        fields: vec![],
        ops: v.ops.clone(),
        ops_health: v.ops_health.clone(),
        met_epoch: v.def.met_epoch.as_deref().and_then(|m| Utc::parse(m).ok()),
        sources,
        solution,
        track,
        passes: vec![],
        beta_series: Arc::new(vec![]),
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

#[allow(clippy::too_many_arguments)]
fn build_track(
    traj: &dyn Trajectory,
    kind: SourceKind,
    now: Utc,
    period_s: f64,
    (past_rev, future_rev, samples): (f64, f64, usize),
    regime: Regime,
    eop: Option<&EopTable>,
    revision: u64,
) -> Track {
    let period = if period_s.is_finite() && period_s > 600.0 {
        period_s
    } else {
        FALLBACK_PERIOD_S
    };
    let (past, future) = match regime {
        Regime::Earth => (past_rev * period, future_rev * period),
        Regime::L2 => (DEEP_TRACK_DAYS * 86_400.0, DEEP_TRACK_DAYS * 86_400.0),
    };
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
            rot_km: mctl_astro::l2::to_rotating(&s.r, &sun),
        });
    }
    let now_index = points.partition_point(|p| p.t.0 < now.0);
    // Events over the predicted part of the window, by root finding rather than track interpolation.
    // Nodes, apsides and eclipses have no meaning for a halo orbit about L2.
    let events = match regime {
        Regime::Earth => mctl_astro::events::find_events(
            traj,
            now,
            now.plus(future),
            period / EVENT_STEPS_PER_REV,
        )
        .into_iter()
        .map(|e| (e.t, e.kind))
        .collect(),
        Regime::L2 => Vec::new(),
    };
    Track {
        revision,
        source: kind,
        points,
        now_index,
        events,
    }
}

/// A running engine. Dropping it stops the thread.
pub struct Engine {
    pub bus: Arc<Bus>,
    /// Commands for the simulation clock (the GUI never touches engine state directly).
    pub commands: Sender<Command>,
    stop: Option<SyncSender<()>>,
    thread: Option<JoinHandle<()>>,
    /// Dropped last, after the engine thread has stopped sending, so its queue is flushed.
    _recorder: Option<Recorder>,
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Apply every queued command to the clock, logging the outcome.
fn drain_commands(rx: &Receiver<Command>, clock: &mut Clock, model: &mut Model, wall_utc: Utc) {
    while let Ok(cmd) = rx.try_recv() {
        match &cmd {
            Command::Ack => {
                model.ack(wall_utc);
                continue;
            }
            Command::Replay(_) => {
                if let Err(e) = model.prepare_replay(clock) {
                    model.note(wall_utc, format!("command rejected: {e}"));
                    continue;
                }
            }
            Command::Plan { vehicle, edit } => {
                model.plan_edit(vehicle, edit.clone(), wall_utc);
                continue;
            }
            Command::PlanRun(vehicle) => {
                model.plan_rerun(vehicle);
                continue;
            }
            Command::SelectSite(n) => {
                if let Err(e) = model.select_site(n, wall_utc) {
                    model.note(wall_utc, format!("command rejected: {e}"));
                }
                continue;
            }
            Command::Trend { vehicle, window_s } => {
                model.request_trend(vehicle, *window_s, wall_utc);
                continue;
            }
            Command::SaveSite(n) => {
                if let Err(e) = model.save_site(n.as_deref(), wall_utc) {
                    model.note(wall_utc, format!("command rejected: {e}"));
                }
                continue;
            }
            Command::AddSite {
                name,
                lat_deg,
                lon_deg,
                alt_m,
            } => {
                let site = Site {
                    name: name.clone(),
                    lat_deg: *lat_deg,
                    lon_deg: *lon_deg,
                    alt_m: *alt_m,
                };
                if let Err(e) = model.add_site(site, wall_utc) {
                    model.note(wall_utc, format!("command rejected: {e}"));
                }
                continue;
            }
            _ => {}
        }
        match clock.apply(&cmd, Instant::now(), wall_utc) {
            Ok(text) => model.note(wall_utc, text),
            Err(why) => model.note(wall_utc, format!("command rejected: {why}")),
        }
    }
}

/// Start the engine thread. It owns `sources` and publishes to the returned bus.
/// `trends_db` is the recording to read trend plots from (usually the one `recorder` writes).
pub fn start(
    cfg: Config,
    sources: SourceSet,
    recorder: Option<Recorder>,
    trends_db: Option<std::path::PathBuf>,
) -> Engine {
    let record_tx = recorder.as_ref().map(Recorder::sender);
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
            model.enable_pass_worker();
            model.enable_plan_worker();
            if let Some(db) = trends_db {
                model.enable_trends(db);
            }
            if let Some(tx) = record_tx {
                model.set_recorder(tx);
            }
            model.set_telemetry(sources.telemetry.clone());
            model.set_field_requests(sources.field_tx.clone());
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
        stop: Some(stop_tx),
        thread: Some(thread),
        _recorder: recorder,
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
                    raw: None,
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
                    raw: None,
                },
            });
        }
    }

    /// Inside the sample OEM's span, shortly before the GP epoch.
    fn t_in_oem() -> Utc {
        Utc::parse("2026-10-22T10:00:00").unwrap()
    }

    /// 13 hourly JWST vectors from a real Horizons reply (2026-10-21 00:00 .. 12:00 UT).
    const HORIZONS_JWST: &str = include_str!("../../mctl-astro/fixtures/horizons-jwst-sample.txt");

    fn jwst_model() -> (Model, Utc) {
        let mut m = model();
        let o = mctl_astro::horizons::parse(HORIZONS_JWST, Utc(0.0)).unwrap();
        let t = Utc(o.states[6].t.0 + 600.0);
        m.apply(Event::Data {
            key: SourceKey::Horizons("jwst".into()),
            payload: Payload::Horizons(Box::new(o)),
            info: DataInfo {
                from_cache: false,
                fetched_at: t,
                raw: None,
            },
        });
        (m, t)
    }

    #[test]
    fn an_l2_vehicle_gets_distance_light_time_and_halo_coordinates_not_orbit_events() {
        let (mut m, t) = jwst_model();
        let w = m.world(t);
        let v = w.vehicles.iter().find(|v| v.id == "jwst").unwrap();
        let sol = v.solution.as_ref().unwrap();
        assert_eq!(sol.source, SourceKind::Horizons);
        let d = v.deep.expect("an L2 vehicle has a deep-space view");
        assert!((1.3e6..1.7e6).contains(&d.dist_km), "{}", d.dist_km);
        // About 5 s of light time. The halo is wide (hundreds of thousands of km): the angle at Earth
        // between the anti-Sun line and JWST reaches about 30 degrees, but not 45.
        assert!((4.0..6.0).contains(&d.light_time_s), "{}", d.light_time_s);
        assert!((5.0..45.0).contains(&d.off_axis_deg), "{}", d.off_axis_deg);
        assert!(
            d.rot_km[0] > 1.0e6,
            "L2 is beyond Earth, away from the Sun: {:?}",
            d.rot_km
        );
        assert!(d.speed_km_s < 1.0);
        let tr = v.track.as_ref().expect("track");
        assert!(
            tr.events.is_empty(),
            "no nodes or eclipses for a halo orbit"
        );
        assert!(tr.points.iter().all(|p| p.rot_km[0] > 1.0e6));
        assert!(v.passes.is_empty(), "no pass prediction for an L2 vehicle");
        // An Earth-orbit vehicle has no deep view.
        assert!(w.vehicles[0].deep.is_none());
    }

    #[test]
    fn the_horizons_source_is_stale_by_its_own_configured_age() {
        let (mut m, t) = jwst_model();
        // Data was fetched at `t`; far later the arbiter still returns it, but labelled STALE.
        let later = Utc(t.0 + 3.0 * 86_400.0);
        let w = m.world(t);
        assert_eq!(
            w.vehicles
                .iter()
                .find(|v| v.id == "jwst")
                .unwrap()
                .solution
                .as_ref()
                .unwrap()
                .quality,
            Quality::Pred
        );
        let w = m.world(later);
        // Outside the 13-row (12 h) span there is no state at all; the source reports it.
        let v = w.vehicles.iter().find(|v| v.id == "jwst").unwrap();
        assert!(v.solution.is_none());
        assert_eq!(v.sources[0].status, SourceStatus::OutOfSpan);
    }

    #[test]
    fn a_visitor_on_the_stations_element_set_is_docked_and_one_a_little_ahead_is_near_on_the_v_bar()
    {
        use mctl_core::prox::ProxState;
        let mut m = model();
        feed(&mut m, true, true);
        let dragon = |m: &mut Model, json: &str| {
            m.apply(Event::Data {
                key: SourceKey::Gp("dragon-crew".into()),
                payload: Payload::Gp(GpModel::from_omm_json(json).unwrap()),
                info: DataInfo {
                    from_cache: false,
                    fetched_at: Utc(0.0),
                    raw: None,
                },
            });
        };
        dragon(&mut m, OMM);
        let t = t_in_oem();
        let w = m.world(t);
        let p = w
            .prox
            .iter()
            .find(|p| p.chaser == "dragon-crew")
            .expect("a prox view");
        assert_eq!(p.target, "iss");
        assert_eq!(p.state, ProxState::Docked, "{:?}", p.rel_lvlh_km);
        assert_eq!(
            p.range_km, 0.0,
            "the ISS GP is the visitor's GP, so the same-source difference is exactly zero"
        );
        assert!(
            p.source_note.contains("GP/SGP4 FOR BOTH"),
            "{}",
            p.source_note
        );
        assert!(!p.ephemeris_grade);
        // Advance the visitor's mean anomaly by 0.02 degrees: about 2.4 km further along the orbit.
        let mut v: serde_json::Value = serde_json::from_str(OMM).unwrap();
        let ma = v[0]["MEAN_ANOMALY"].as_f64().unwrap();
        v[0]["MEAN_ANOMALY"] = serde_json::json!(ma + 0.02);
        dragon(&mut m, &v.to_string());
        let w = m.world(t.plus(1.0));
        let p = w.prox.iter().find(|p| p.chaser == "dragon-crew").unwrap();
        assert_eq!(p.state, ProxState::Near);
        assert!(
            (p.rel_lvlh_km[0] - 2.4).abs() < 0.2,
            "ahead on V-bar: {:?}",
            p.rel_lvlh_km
        );
        assert!(p.rel_lvlh_km[2].abs() < 0.2);
        assert!(p.predicted.len() > 80, "{}", p.predicted.len());
        assert!(p.history.len() <= 2);
        // The cargo Dragon has no data yet and so no view; the ISS itself is not a visitor.
        assert!(
            w.prox
                .iter()
                .all(|p| p.chaser != "dragon-cargo" && p.chaser != "iss")
        );
    }

    #[test]
    fn field_pictures_are_requested_once_for_the_current_and_next_targets_and_retried_after_a_failure()
     {
        use mctl_core::schedule::{JwstSchedule, Visit};
        let visit = |id: &str, start: f64, instrument: &str, target: &str| Visit {
            id: id.into(),
            start: Utc(start),
            duration_s: 100.0,
            kind: "PRIME TARGETED FIXED".into(),
            pcs_mode: String::new(),
            instrument: instrument.into(),
            target: target.into(),
            category: String::new(),
            parallels: vec![],
        };
        let sched = JwstSchedule {
            visits: vec![
                visit("1", 1000.0, "NIRCam Imaging", "V339-Del"),
                visit("2", 1100.0, "", ""),
                visit("3", 1200.0, "MIRI Dark", "V339-Del"),
                visit("4", 1300.0, "NIRSpec MOS", "o16"),
            ],
            files: vec![],
            fetched_at: Utc(0.0),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let mut m = model();
        m.set_field_requests(Some(tx));
        m.apply(Event::Ops {
            vehicle: "jwst".into(),
            ops: Ops::Jwst(Arc::new(sched)),
            from_cache: true,
        });
        let fields = |m: &mut Model, t: f64| {
            m.world(Utc(t))
                .vehicles
                .iter()
                .find(|v| v.id == "jwst")
                .unwrap()
                .fields
                .clone()
        };
        let f = fields(&mut m, 1050.0);
        // The current visit's target, then the next different one; the same target twice is one picture.
        let keys: Vec<&str> = f.iter().map(|x| x.key.as_str()).collect();
        assert_eq!(keys, ["target:V339-Del", "target:o16"]);
        assert_eq!(f[0].label, "NIRCam Imaging");
        assert!(f.iter().all(|x| x.state == FieldState::Loading));
        let asked: Vec<_> = rx.try_iter().collect();
        assert_eq!(asked.len(), 2, "{asked:?}");
        // Asking again at the next tick sends nothing new.
        fields(&mut m, 1051.0);
        assert!(rx.try_recv().is_err());
        // The answer shows up; a failure is reported as such and not retried at once.
        m.apply(Event::Field {
            vehicle: "jwst".into(),
            key: "target:o16".into(),
            state: FieldState::Unavailable("not catalogued".into()),
        });
        let f = fields(&mut m, 1052.0);
        assert_eq!(f[1].state, FieldState::Unavailable("not catalogued".into()));
        assert!(rx.try_recv().is_err());
        // Vehicles with no schedule have no pictures.
        assert!(m.world(Utc(1050.0)).vehicles[0].fields.is_empty());
    }

    #[test]
    fn replay_serves_recorded_states_and_telemetry_and_falls_back_across_gaps() {
        use mctl_core::world::ClockMode;
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("mctl.db");
        let t0 = t_in_oem();
        {
            let mut store = mctl_store::Store::open(&db).unwrap();
            let mut batch = Vec::new();
            for k in 0..=10 {
                // A recorded ISS state that deliberately differs from every source: 1000 km on x.
                batch.push(Record::State {
                    vehicle: "iss".into(),
                    source: "OEM".into(),
                    t: t0.plus(f64::from(k)),
                    frame: "GCRF",
                    r_km: [1000.0 + f64::from(k), 6900.0, 0.0],
                    v_km_s: [0.0, 0.0, 7.6],
                    quality: "PRED".into(),
                });
            }
            batch.push(Record::Telemetry {
                item_id: "USLAB000058".into(),
                t_rx: t0.plus(2.0),
                value: Some(741.5),
                text: None,
            });
            store.write(&batch).unwrap();
        }
        let mut m = model();
        feed(&mut m, true, true);
        m.set_telemetry(vec![meta("USLAB000058")]);
        m.enable_trends(db);
        let mut clock = Clock::live(Instant::now());
        // No replay span yet: the clock refuses.
        assert!(
            clock
                .apply(&Command::Replay(None), Instant::now(), t0)
                .is_err()
        );
        m.prepare_replay(&mut clock).unwrap();
        clock
            .apply(&Command::Replay(Some(t0.plus(5.0))), Instant::now(), t0)
            .unwrap();
        assert_eq!(clock.view().mode, ClockMode::Replay);
        let view = clock.view();
        let w = m.world_at(t0.plus(5.5), view, t0);
        let sol = w.vehicles[0].solution.as_ref().unwrap();
        assert_eq!(sol.quality, Quality::Rec);
        assert_eq!(sol.source, SourceKind::Oem);
        assert!(
            (sol.state_gcrf.r.x - 1005.5).abs() < 1e-9,
            "{}",
            sol.state_gcrf.r.x
        );
        let tel = w.telemetry.unwrap();
        assert_eq!(tel.get("USLAB000058").unwrap().value, Some(741.5));
        assert_eq!(
            tel.get("USLAB000058").unwrap().age_s,
            3.5,
            "age is relative to the replay time"
        );
        assert!(!tel.signal_lost());
        // A minute later the recording ends; the arbiter's sources answer, labelled SIM, not REC.
        let w = m.world_at(t0.plus(60.0), view, t0);
        assert_ne!(
            w.vehicles[0].solution.as_ref().unwrap().quality,
            Quality::Rec
        );
        assert!(
            w.telemetry.unwrap().signal_lost(),
            "no recorded value for a minute means signal lost"
        );
        // The clock pauses at the end of the recording rather than running off it.
        let mut c = Clock::live(Instant::now());
        m.prepare_replay(&mut c).unwrap();
        let wall = Instant::now();
        c.apply(&Command::Replay(Some(t0.plus(9.0))), wall, t0)
            .unwrap();
        c.apply(&Command::SetRate(100.0), wall, t0).unwrap();
        let later = wall + std::time::Duration::from_secs(5);
        assert!(c.enforce_bounds(later, t0).is_some());
        assert!(
            c.apply(&Command::Seek(t0.plus(10_000.0)), later, t0)
                .unwrap_err()
                .contains("outside the recording")
        );
    }

    #[test]
    fn plan_edits_produce_a_plan_view_from_the_active_source_and_reject_bad_input() {
        use mctl_core::plan::{BurnDef, BurnFrame};
        let mut m = model();
        feed(&mut m, true, true);
        let t = t_in_oem();
        let burn = BurnDef {
            trigger: "apo".into(),
            frame: BurnFrame::Vnb,
            dv_m_s: [1.0, 0.0, 0.0],
        };
        m.plan_edit("iss", PlanCommand::AddBurn(burn), t);
        // Empty until the next snapshot, which has a state to start from.
        let w = m.world(t);
        let (id, p) = &w.plans[0];
        assert_eq!(id, "iss");
        assert_eq!(p.status, PlanStatus::Ready, "{:?}", p.status);
        assert_eq!(p.burns.len(), 1);
        let r = p.report.as_ref().unwrap();
        assert!(r.d_period_s > 0.0 && r.planned.sma_km > r.nominal.sma_km);
        // The ISS OEM carries a ballistic coefficient, so drag is on and nothing is noted.
        assert!(p.notes.is_empty(), "{:?}", p.notes);
        // Bad edits are refused with a log line and leave the plan alone.
        m.plan_edit("iss", PlanCommand::DelBurn(5), t);
        m.plan_edit("iss", PlanCommand::Horizon(-1.0), t);
        m.plan_edit(
            "iss",
            PlanCommand::Force {
                name: "gravity".into(),
                on: false,
            },
            t,
        );
        m.plan_edit("jwst", PlanCommand::Clear, t);
        m.plan_edit("nobody", PlanCommand::Clear, t);
        let w = m.world(t);
        assert_eq!(w.plans[0].1.def.burns.len(), 1);
        assert_eq!(
            w.log
                .iter()
                .filter(|l| l.text.starts_with("plan rejected"))
                .count(),
            5
        );
        // Clearing the burns empties the plan.
        m.plan_edit("iss", PlanCommand::Clear, t);
        assert_eq!(m.world(t).plans[0].1.status, PlanStatus::Empty);
        // A plan file name cannot escape the plans directory.
        assert!(
            plan_path("../x").is_err() && plan_path("").is_err() && plan_path("ok-1_a").is_ok()
        );
    }

    #[test]
    fn a_frozen_stream_clock_makes_every_item_stale_and_never_runs_backwards() {
        let mut m = model();
        m.set_telemetry(vec![meta("A")]);
        m.apply(Event::Telemetry {
            id: "A".into(),
            text: Some("1".into()),
            rx: Utc(1000.0),
        });
        m.apply(Event::TelemetryClock {
            data_time: Utc(990.0),
        });
        let view = |m: &mut Model, wall: f64| {
            m.world_at(Utc(wall), ClockView::LIVE, Utc(wall))
                .telemetry
                .unwrap()
        };
        let v = view(&mut m, 1000.0);
        assert_eq!(v.feed_age_s, Some(10.0));
        assert!(!v.signal_lost());
        // The clock stops; 100 s later everything is stale although the item was just "received".
        let v = view(&mut m, 1090.0);
        assert!(v.signal_lost());
        assert!(v.is_stale(v.get("A").unwrap(), 30.0));
        // A replayed older stamp does not move it back.
        m.apply(Event::TelemetryClock {
            data_time: Utc(500.0),
        });
        assert_eq!(view(&mut m, 1090.0).feed_age_s, Some(100.0));
        m.apply(Event::TelemetryClock {
            data_time: Utc(1085.0),
        });
        assert!(!view(&mut m, 1090.0).signal_lost());
    }

    #[test]
    fn telemetry_history_samples_once_per_period_and_leaves_gaps_for_old_values() {
        let mut m = model();
        m.set_telemetry(vec![meta("A"), meta("B")]);
        let at = |m: &mut Model, id: &str, v: &str, rx: f64| {
            m.apply(Event::Telemetry {
                id: id.into(),
                text: Some(v.into()),
                rx: Utc(rx),
            })
        };
        at(&mut m, "A", "1.5", 100.0);
        at(&mut m, "B", "open", 100.0);
        let hist = |m: &mut Model, wall: f64, id: &str| -> Vec<f32> {
            let w = m.world_at(Utc(wall), ClockView::LIVE, Utc(wall));
            w.telemetry.unwrap().get(id).unwrap().history.to_vec()
        };
        assert_eq!(hist(&mut m, 100.0, "A"), [1.5]);
        // Ticks inside one period add nothing, however many there are.
        assert_eq!(hist(&mut m, 103.0, "A"), [1.5]);
        at(&mut m, "A", "2.5", 109.0);
        assert_eq!(hist(&mut m, 110.0, "A"), [1.5, 2.5]);
        // Text items have no history.
        assert!(hist(&mut m, 110.0, "B").is_empty());
        // A value last refreshed long ago leaves a gap instead of being repeated.
        assert_eq!(hist(&mut m, 300.0, "A"), [1.5, 2.5]);
        // The window is bounded.
        for k in 0..(HISTORY_LEN + 20) {
            let t = 1000.0 + HISTORY_PERIOD_S * k as f64;
            at(&mut m, "A", "3.0", t);
            hist(&mut m, t, "A");
        }
        let last = 1000.0 + HISTORY_PERIOD_S * (HISTORY_LEN + 19) as f64;
        assert_eq!(hist(&mut m, last, "A").len(), HISTORY_LEN);
    }

    #[test]
    fn an_observing_schedule_reaches_the_vehicle_view() {
        let mut m = model();
        let sched = mctl_core::schedule::JwstSchedule {
            visits: vec![],
            files: vec!["a.txt".into()],
            fetched_at: Utc(1.0),
        };
        m.apply(Event::Ops {
            vehicle: "jwst".into(),
            ops: mctl_core::schedule::Ops::Jwst(Arc::new(sched)),
            from_cache: true,
        });
        m.apply(Event::Health {
            key: SourceKey::Schedule("jwst".into()),
            health: Health::Ok,
        });
        let w = m.world(Utc(1.0));
        let j = w.vehicles.iter().find(|v| v.id == "jwst").unwrap();
        assert!(matches!(j.ops, Some(mctl_core::schedule::Ops::Jwst(_))));
        assert_eq!(j.ops_health, Some(Health::Ok));
        assert!(
            w.vehicles[0].ops_health.is_none(),
            "the ISS has no schedule source"
        );
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
        let text = include_str!("../fixtures/finals2000A.all");
        with.apply(Event::Data {
            key: SourceKey::Eop,
            payload: Payload::Eop(EopTable::parse(text).unwrap()),
            info: DataInfo {
                from_cache: false,
                fetched_at: Utc(0.0),
                raw: None,
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
        let eng = start(cfg, set, None, None);
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
        let mut cfg = Config::from_layers(mctl_core::config::DEFAULT_CONFIG, None).unwrap();
        cfg.observer.geoclue = false; // tests must not depend on the machine's GeoClue
        let dir = std::env::temp_dir().join(format!("mctl-engine-cmd-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let set = mctl_sources::tasks::start(&cfg, dir.clone(), true).unwrap();
        let eng = start(cfg, set, None, None);
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
            section: "s".into(),
            row: id.into(),
            col: String::new(),
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

    #[test]
    fn the_engine_records_state_telemetry_elements_events_and_cw_changes_but_not_sim_states() {
        use mctl_core::command::Command as C;
        let _ = C::Ack;
        let mut m = model();
        let (tx, rx) = std::sync::mpsc::channel();
        m.set_recorder(tx);
        m.set_telemetry(vec![meta("USLAB000058")]);
        feed(&mut m, true, true);
        // Network downloads are recorded with their raw body; cache loads are not (recorded when fetched).
        m.apply(Event::Data {
            key: SourceKey::Gp("iss".into()),
            payload: Payload::Gp(GpModel::from_omm_json(OMM).unwrap()),
            info: DataInfo {
                from_cache: false,
                fetched_at: Utc(1.0),
                raw: Some(Arc::from(OMM)),
            },
        });
        let t = t_in_oem();
        m.apply(Event::Telemetry {
            id: "USLAB000058".into(),
            text: Some("749.5".into()),
            rx: t,
        });
        m.apply(Event::Telemetry {
            id: "USLAB000058".into(),
            text: Some("ON".into()),
            rx: t.plus(1.0),
        });
        m.world_at(t, ClockView::LIVE, t);
        m.world_at(t.plus(0.5), ClockView::LIVE, t.plus(0.5));
        m.world_at(t.plus(1.1), ClockView::LIVE, t.plus(1.1));
        let sim = ClockView {
            mode: mctl_core::world::ClockMode::Sim,
            rate: 10.0,
            paused: false,
        };
        m.world_at(t.plus(500.0), sim, t.plus(2.5));
        m.world_at(t.plus(800.0), sim, t.plus(4.0));
        let recs: Vec<Record> = rx.try_iter().collect();
        let count = |f: fn(&Record) -> bool| recs.iter().filter(|r| f(r)).count();
        assert_eq!(
            count(|r| matches!(r, Record::Elements { source, .. } if source == "GP/SGP4")),
            1
        );
        assert_eq!(count(|r| matches!(r, Record::Telemetry { .. })), 2);
        assert!(recs.iter().any(
            |r| matches!(r, Record::Telemetry { value: Some(v), text: None, .. } if *v == 749.5)
        ));
        assert!(recs.iter().any(
            |r| matches!(r, Record::Telemetry { value: None, text: Some(t), .. } if t == "ON")
        ));
        assert_eq!(
            count(|r| matches!(r, Record::State { .. })),
            4,
            "1 Hz while live (t and t+1.1, not t+0.5) for each of the two sources; nothing in SIM"
        );
        assert!(
            recs.iter()
                .all(|r| !matches!(r, Record::State { t: st, .. } if st.0 > t.0 + 100.0)),
            "no sim-time states"
        );
        assert!(
            count(|r| matches!(r, Record::Event { .. })) >= 1,
            "log entries are recorded as events"
        );
        // A rule transition lands in cw_log.
        m.apply(Event::Telemetry {
            id: "USLAB000058".into(),
            text: Some("749".into()),
            rx: t,
        });
        assert!(rx.try_iter().any(|r| matches!(r, Record::Telemetry { .. })));
    }

    #[test]
    fn a_stopped_recorder_is_dropped_without_disturbing_the_engine() {
        let mut m = model();
        let (tx, rx) = std::sync::mpsc::channel();
        m.set_recorder(tx);
        drop(rx);
        m.apply(Event::Telemetry {
            id: "X".into(),
            text: Some("1".into()),
            rx: Utc(1.0),
        });
        m.apply(Event::Telemetry {
            id: "X".into(),
            text: Some("2".into()),
            rx: Utc(2.0),
        });
        let t = t_in_oem();
        assert!(m.world(t).seq > 0, "still publishing");
    }

    #[test]
    fn passes_are_predicted_for_every_site_with_the_fallback_site_marked_default() {
        let mut m = model();
        feed(&mut m, true, true);
        let t = t_in_oem();
        let w = m.world(t);
        let o = &w.observer;
        assert_eq!(o.sites.len(), 1);
        assert_eq!(
            (o.sites[0].name.as_str(), o.sites[0].origin.as_str()),
            ("NASA HQ", "DEFAULT"),
            "no site configured: the fallback, labelled"
        );
        let v = &w.vehicles[0];
        assert_eq!(v.passes.len(), 1);
        let set = &v.passes[0];
        assert_eq!(
            set.source,
            "GP/SGP4 then OEM".replace("GP/SGP4 then OEM", "OEM then GP/SGP4"),
            "OEM has priority, GP covers beyond its span"
        );
        assert!((set.to.minus(set.from) - 7.0 * 86_400.0).abs() < 1.0);
        assert!(
            set.passes.len() >= 10,
            "a week gives dozens of passes over DC, got {}",
            set.passes.len()
        );
        assert!(set.passes.iter().all(|p| p.max_el_deg >= 10.0 - 0.01
            && p.aos.0 < p.tca.0
            && p.tca.0 < p.los.0
            && p.path.len() >= 2));
        assert!(
            set.look_now.is_some(),
            "live look angles accompany the predictions"
        );
        assert!(
            o.sites[0]
                .sun_el_deg
                .is_some_and(|e| (-90.0..=90.0).contains(&e))
        );
    }

    #[test]
    fn selecting_and_adding_sites_validates_and_logs() {
        let mut m = model();
        let t = t_in_oem();
        assert!(
            m.select_site("Atlantis", t)
                .unwrap_err()
                .contains("known: NASA HQ")
        );
        m.add_site(
            Site {
                name: "Home".into(),
                lat_deg: 38.883,
                lon_deg: -77.0163,
                alt_m: 10.0,
            },
            t,
        )
        .unwrap();
        assert!(
            m.add_site(
                Site {
                    name: "home".into(),
                    lat_deg: 0.0,
                    lon_deg: 0.0,
                    alt_m: 0.0
                },
                t
            )
            .unwrap_err()
            .contains("already exists")
        );
        assert!(
            m.add_site(
                Site {
                    name: "Bad".into(),
                    lat_deg: 95.0,
                    lon_deg: 0.0,
                    alt_m: 0.0
                },
                t
            )
            .is_err()
        );
        let w = m.world(t);
        assert_eq!(w.observer.sites.len(), 2);
        assert_eq!(
            w.observer.active_site().unwrap().name,
            "Home",
            "a new site becomes active"
        );
        m.select_site("nasa hq", t).unwrap();
        assert_eq!(m.world(t).observer.active_site().unwrap().name, "NASA HQ");
        assert!(
            m.world(t)
                .log
                .iter()
                .any(|e| e.text.contains("site added for this session: Home"))
        );
    }

    #[test]
    fn a_resolved_site_becomes_active_only_while_the_fallback_is() {
        let mut m = model();
        let t = t_in_oem();
        m.adopt_resolved_site(
            Site {
                name: "GeoClue".into(),
                lat_deg: 51.0,
                lon_deg: -114.0,
                alt_m: 0.0,
            },
            "GEOCLUE +-26 km".into(),
            t,
        );
        let w = m.world(t);
        assert_eq!(
            w.observer.active_site().unwrap().origin,
            "GEOCLUE +-26 km",
            "the lookup replaces the fallback as the active site"
        );
        assert_eq!(w.observer.sites.len(), 2, "the fallback stays available");
        // A second lookup replaces the first rather than piling up.
        m.adopt_resolved_site(
            Site {
                name: "GeoClue".into(),
                lat_deg: 51.1,
                lon_deg: -114.1,
                alt_m: 0.0,
            },
            "GEOCLUE +-5 km".into(),
            t,
        );
        assert_eq!(m.world(t).observer.sites.len(), 2);
        // Once the user picks a site, a late lookup does not take over.
        m.select_site("NASA HQ", t).unwrap();
        m.adopt_resolved_site(
            Site {
                name: "IP".into(),
                lat_deg: 40.0,
                lon_deg: -100.0,
                alt_m: 0.0,
            },
            "IP +-25 km".into(),
            t,
        );
        assert_eq!(m.world(t).observer.active_site().unwrap().name, "NASA HQ");
    }

    #[test]
    fn saving_a_site_writes_the_user_config_once_and_marks_it_configured() {
        let dir = std::env::temp_dir().join(format!("mctl-save-site-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("config.toml");
        let mut m = model();
        let t = t_in_oem();
        let e = m.save_site_at(None, &path, t).unwrap_err();
        assert!(e.contains("default site is not your location"), "{e}");
        assert!(!path.exists(), "nothing written on refusal");
        m.adopt_resolved_site(
            Site {
                name: "GeoClue".into(),
                lat_deg: 51.0,
                lon_deg: -114.0,
                alt_m: 12.0,
            },
            "GEOCLUE +-26 km".into(),
            t,
        );
        m.save_site_at(Some("Home"), &path, t).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("name = \"Home\"")
                && text.contains("lat_deg = 51")
                && text.contains("alt_m = 12"),
            "{text}"
        );
        assert_eq!(
            m.world(t).observer.active_site().unwrap().origin,
            "CONFIGURED"
        );
        // The written file is valid configuration that selects the saved site.
        let layered = format!("{text}\n[observer]\nactive = \"Home\"\n");
        let cfg = Config::from_layers(mctl_core::config::DEFAULT_CONFIG, Some(("saved", &layered)));
        assert!(cfg.is_ok(), "{cfg:?}");
        assert!(
            m.save_site_at(Some("Home"), &path, t)
                .unwrap_err()
                .contains("already in"),
            "no duplicate entries"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn supplemental_gp_outranks_standard_gp_but_not_the_oem() {
        let mut cfg = Config::from_layers(mctl_core::config::DEFAULT_CONFIG, None).unwrap();
        cfg.observer.geoclue = false;
        cfg.vehicles[0].sources = vec![SourceKind::Oem, SourceKind::SupGp, SourceKind::Gp];
        let mut m = Model::new(cfg);
        feed(&mut m, true, false);
        // Segments around the GP fixture's epoch, so the supplemental set is valid there.
        let base: serde_json::Value = serde_json::from_str(OMM).unwrap();
        let ep = GpModel::from_omm_json(OMM).unwrap().epoch;
        let segs: Vec<serde_json::Value> = (0..4)
            .map(|k| {
                let mut r = base[0].clone();
                r["EPOCH"] = ep.plus(f64::from(k) * 6.0 * 3600.0).iso().into();
                r
            })
            .collect();
        let sup = mctl_astro::supgp::SupGp::parse(&serde_json::to_string(&segs).unwrap()).unwrap();
        let t = ep.plus(3_600.0);
        let sup_fetched = t;
        m.apply(Event::Data {
            key: SourceKey::SupGp("iss".into()),
            payload: Payload::SupGp(Box::new(sup)),
            info: DataInfo {
                from_cache: false,
                fetched_at: sup_fetched,
                raw: None,
            },
        });
        let w = m.world(t);
        let v = &w.vehicles[0];
        assert_eq!(
            v.sources.iter().map(|s| s.kind).collect::<Vec<_>>(),
            vec![SourceKind::Oem, SourceKind::SupGp, SourceKind::Gp]
        );
        assert_eq!(
            v.solution.as_ref().unwrap().source,
            SourceKind::SupGp,
            "no OEM here, so SUP-GP wins over GP"
        );
        assert!(
            v.sources[2].residual.is_some(),
            "GP is a standby with a residual against the active source"
        );
        feed(&mut m, false, true);
        assert_eq!(
            m.world(t_in_oem()).vehicles[0]
                .solution
                .as_ref()
                .unwrap()
                .source,
            SourceKind::Oem,
            "the OEM keeps priority"
        );
    }
}
