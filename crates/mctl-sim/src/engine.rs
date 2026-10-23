//! The propagation engine: consumes source events, runs the arbiter every tick, and publishes an
//! immutable [`World`] snapshot (DESIGN.md §6.2, §3.2). It runs on its own thread at `engine.tick_hz`;
//! the UI only ever reads the latest snapshot.

use std::sync::Arc;
use std::sync::mpsc::{SyncSender, TryRecvError, sync_channel};
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
use mctl_core::config::{Config, VehicleDef};
use mctl_core::state::{Elements, Geodetic, Quality, SourceKind, State};
use mctl_core::time::{EopTable, Utc};
use mctl_core::world::{
    EopView, Health, Residual, Solution, SourceStatus, SourceView, Track, TrackPoint, VehicleView,
    World,
};
use mctl_sources::tasks::{DataInfo, Event, Payload, SourceKey, SourceSet};

/// Period assumed for track sizing when elements cannot be computed (LEO), seconds.
const FALLBACK_PERIOD_S: f64 = 5_600.0;
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
        Self {
            cfg,
            vehicles,
            eop: None,
            track_revision: 0,
            seq: 0,
        }
    }

    pub fn apply(&mut self, event: Event) {
        match event {
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

    /// Build the snapshot for time `t`.
    pub fn world(&mut self, t: Utc) -> World {
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
                )
            })
            .collect();
        self.track_revision = revision;
        self.seq += 1;
        World {
            t,
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
    let is_stale = |s: &Sample| age(s).is_some_and(|a| a > s.stale_after_s);
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
        Some(i) => (Some(i), Quality::Pred),
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
    _stop: SyncSender<()>,
    _thread: JoinHandle<()>,
}

/// Start the engine thread. It owns `sources` and publishes to the returned bus.
pub fn start(cfg: Config, sources: SourceSet) -> Engine {
    let bus = Bus::new(World::empty(Utc::now().unwrap_or(Utc(0.0))));
    let (stop_tx, stop_rx) = sync_channel::<()>(1);
    let (b, tick) = (
        bus.clone(),
        Duration::from_secs_f64(1.0 / cfg.engine.tick_hz),
    );
    let thread = std::thread::Builder::new()
        .name("engine".into())
        .spawn(move || {
            let mut model = Model::new(cfg);
            let mut next = Instant::now();
            loop {
                loop {
                    match sources.events.try_recv() {
                        Ok(e) => model.apply(e),
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => break,
                    }
                }
                match Utc::now() {
                    Ok(t) => b.publish(model.world(t)),
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
}
