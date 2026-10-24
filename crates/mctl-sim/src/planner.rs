//! The maneuver planner (DESIGN.md 7.4): resolve burn triggers on the nominal trajectory, propagate
//! numerically through the burns, propagate the same start without burns for comparison, and report the
//! difference. Runs on a worker thread; the engine publishes the result.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use nalgebra::Vector3;

use mctl_astro::events::find_events;
use mctl_astro::frames::{Orientation, Reduction};
use mctl_astro::kepler;
use mctl_astro::local_frames::{LocalFrame, from_local};
use mctl_astro::numerical::{ForceModel, Table, propagate};
use mctl_astro::observer::{self, Site};
use mctl_astro::trajectory::Trajectory;
use mctl_core::plan::{
    BurnFrame, BurnView, MAX_HORIZON_S, PassShift, PlanDef, PlanPoint, PlanReport, PlanStatus,
    PlanView, Trigger,
};
use mctl_core::state::State;
use mctl_core::time::Utc;
use mctl_core::world::EventKind;

/// Spacing of the numerical output, seconds.
const STEP_S: f64 = 30.0;
/// Gap between the pre- and post-burn samples at a burn epoch, seconds (a table needs distinct times).
const BURN_GAP_S: f64 = 1.0e-3;
/// Most points kept for display in a plan view.
const MAX_DISPLAY_POINTS: usize = 2000;
/// Sample step of the pass search, seconds.
const PASS_STEP_S: f64 = 20.0;

pub struct Job {
    pub id: u64,
    pub def: PlanDef,
    pub t0: Utc,
    /// Where the vehicle is at `t0`, from the active source.
    pub start: State,
    /// The source chain used to resolve triggers (nodes, apsides).
    pub nominal: Arc<dyn Trajectory>,
    pub orientation: Orientation,
    /// Cd*A/m, m^2/kg, from the OEM header or the vehicle configuration.
    pub ballistic: Option<f64>,
    pub sites: Vec<Site>,
    pub mask_deg: f64,
}

pub struct Done {
    pub id: u64,
    pub result: Result<PlanView, String>,
}

fn kind_of(t: &Trigger) -> Option<EventKind> {
    match t {
        Trigger::NextAscendingNode => Some(EventKind::AscendingNode),
        Trigger::NextDescendingNode => Some(EventKind::DescendingNode),
        Trigger::NextApoapsis => Some(EventKind::Apoapsis),
        Trigger::NextPeriapsis => Some(EventKind::Periapsis),
        Trigger::Epoch(_) => None,
    }
}

fn point(t: Utc, s: &State, orientation: Orientation) -> Option<PlanPoint> {
    let red = Reduction::at(t, orientation).ok()?;
    let itrf = red.gcrf_to_itrf(s);
    let g = kepler::geodetic(&itrf.r);
    Some(PlanPoint {
        t,
        r_gcrf_km: [s.r.x, s.r.y, s.r.z],
        lat_deg: g.lat_deg,
        lon_deg: g.lon_deg,
        alt_km: g.alt_km,
    })
}

fn points(table: &Table, orientation: Orientation) -> Vec<PlanPoint> {
    let n = table.samples.len();
    let stride = n.div_ceil(MAX_DISPLAY_POINTS).max(1);
    table
        .samples
        .iter()
        .step_by(stride)
        .filter_map(|(t, s)| point(*t, s, orientation))
        .collect()
}

/// Angle from the nominal position to the planned one, in the nominal orbital plane, degrees.
fn phase_deg(nominal: &State, planned: &State) -> f64 {
    let h = nominal.r.cross(&nominal.v).normalize();
    nominal
        .r
        .cross(&planned.r)
        .dot(&h)
        .atan2(nominal.r.dot(&planned.r))
        .to_degrees()
}

/// Add samples strictly later than what is already there.
fn append(table: &mut Vec<(Utc, State)>, seg: Vec<(Utc, State)>) {
    let last = table.last().map_or(f64::NEG_INFINITY, |(t, _)| t.0);
    table.extend(seg.into_iter().filter(|(t, _)| t.0 > last));
}

pub fn compute(job: &Job) -> Result<PlanView, String> {
    job.def.validate().map_err(|e| e.to_string())?;
    let start_el = kepler::elements(&job.start).ok_or("the start state has no elliptical orbit")?;
    if !start_el.period_s.is_finite() || start_el.period_s < 600.0 {
        return Err(format!(
            "start orbit period {:.0} s is not an Earth orbit",
            start_el.period_s
        ));
    }
    let horizon = (job.def.horizon_revs * start_el.period_s).min(MAX_HORIZON_S);
    let (t0, t_end) = (job.t0, job.t0.plus(horizon));
    let mut notes = Vec::new();
    if job.def.horizon_revs * start_el.period_s > MAX_HORIZON_S {
        notes.push("horizon capped at 7 days".to_owned());
    }

    // Resolve triggers on the nominal trajectory. A named trigger means the next such event after the
    // previous burn (or the plan start).
    let events = find_events(job.nominal.as_ref(), t0, t_end, start_el.period_s / 200.0);
    let mut resolved: Vec<BurnView> = Vec::new();
    let mut after = t0;
    for (i, b) in job.def.burns.iter().enumerate() {
        let trig = Trigger::parse(&b.trigger).map_err(|e| e.to_string())?;
        let t = match (&trig, kind_of(&trig)) {
            (Trigger::Epoch(t), _) => *t,
            (_, Some(kind)) => events
                .iter()
                .find(|e| e.kind == kind && e.t.0 > after.0)
                .map(|e| e.t)
                .ok_or_else(|| {
                    format!(
                        "burn {}: no {} after {} within the horizon",
                        i + 1,
                        b.trigger,
                        after.iso()
                    )
                })?,
            _ => unreachable!("every trigger is an epoch or an event kind"),
        };
        if t.0 < t0.0 || t.0 > t_end.0 {
            return Err(format!(
                "burn {}: epoch {} is outside the plan window {} .. {}",
                i + 1,
                t.iso(),
                t0.iso(),
                t_end.iso()
            ));
        }
        after = t;
        resolved.push(BurnView {
            trigger: b.trigger.clone(),
            t,
            frame: b.frame,
            dv_m_s: b.dv_m_s,
            dv_mag_m_s: b.dv_mag_m_s(),
            reference: false,
        });
    }
    resolved.sort_by(|a, b| a.t.0.total_cmp(&b.t.0));

    let drag = match (job.def.drag, job.ballistic) {
        (true, Some(b)) => Some(b * job.def.drag_scale),
        (true, None) => {
            notes.push("drag is off: this vehicle has no ballistic coefficient".to_owned());
            None
        }
        _ => None,
    };
    let model = ForceModel {
        zonal: job.def.zonal,
        sun: job.def.sun,
        moon: job.def.moon,
        drag,
        srp: job.def.srp.then_some(0.01),
    };
    if job.def.srp {
        notes.push("SRP uses Cr*A/m = 0.01 m^2/kg, a placeholder".to_owned());
    }
    let red0 = Reduction::at(t0, job.orientation).map_err(|e| e.to_string())?;
    let m = red0.ecef_from_gcrf();
    let pole = Vector3::new(m[(2, 0)], m[(2, 1)], m[(2, 2)]);

    let nominal =
        propagate(&model, &pole, t0, &job.start, t_end, STEP_S).map_err(|e| e.to_string())?;
    let mut planned: Vec<(Utc, State)> = Vec::new();
    let (mut t, mut state) = (t0, job.start);
    for b in &resolved {
        let seg = propagate(&model, &pole, t, &state, b.t, STEP_S).map_err(|e| e.to_string())?;
        let (tb, sb) = *seg.last().expect("a segment has samples");
        append(&mut planned, seg);
        let frame = match b.frame {
            BurnFrame::Vnb => LocalFrame::Vnb,
            BurnFrame::Rtn => LocalFrame::Rtn,
        };
        let dv = from_local(frame, &sb, &(Vector3::from(b.dv_m_s) / 1000.0))
            .ok_or("the burn state has no orbit plane")?;
        // Post-burn sample a hair later, so the table keeps strictly increasing times.
        let after_burn = State::new((sb.r + (sb.v + dv) * BURN_GAP_S).into(), (sb.v + dv).into());
        t = tb.plus(BURN_GAP_S);
        state = after_burn;
        planned.push((t, state));
    }
    if t.0 < t_end.0 {
        let seg = propagate(&model, &pole, t, &state, t_end, STEP_S).map_err(|e| e.to_string())?;
        append(&mut planned, seg);
    }
    let nominal = Table::new(nominal).ok_or("nominal propagation produced no table")?;
    let planned = Table::new(planned).ok_or("planned propagation produced no table")?;

    let end_n = nominal.at(t_end).ok_or("no nominal end state")?;
    let end_p = planned.at(t_end).ok_or("no planned end state")?;
    let (en, ep) = (
        kepler::elements(&end_n).ok_or("no nominal end elements")?,
        kepler::elements(&end_p).ok_or("no planned end elements")?,
    );
    let sn = nominal.at(t_end).expect("end exists");
    let sp = planned.at(t_end).expect("end exists");
    let report = PlanReport {
        d_period_s: ep.period_s - en.period_s,
        d_phase_deg: phase_deg(&sn, &sp),
        phase_epoch: t_end,
        separation_km: (sp.r - sn.r).norm(),
        nominal: en,
        planned: ep,
    };

    let stride = nominal.samples.len().div_ceil(MAX_DISPLAY_POINTS).max(1);
    let separation: Vec<(Utc, f64)> = nominal
        .samples
        .iter()
        .step_by(stride)
        .filter_map(|(t, s)| Some((*t, (planned.at(*t)?.r - s.r).norm())))
        .collect();

    let passes = job
        .sites
        .iter()
        .map(|site| {
            let first = |table: &Table| {
                let look = |t: Utc| observer::look_at(table, site, t, job.orientation);
                observer::find_passes(&look, t0, t_end, PASS_STEP_S, job.mask_deg)
                    .into_iter()
                    .find(|p| !p.started_before_window)
                    .map(|p| (p.aos, p.max_el_deg))
            };
            let (n, p) = (first(&nominal), first(&planned));
            PassShift {
                site: site.name.clone(),
                nominal_aos: n.map(|x| x.0),
                planned_aos: p.map(|x| x.0),
                nominal_max_el_deg: n.map(|x| x.1),
                planned_max_el_deg: p.map(|x| x.1),
            }
        })
        .collect();

    Ok(PlanView {
        def: job.def.clone(),
        status: PlanStatus::Ready,
        t0,
        t_end,
        burns: resolved,
        nominal: Arc::new(points(&nominal, job.orientation)),
        planned: Arc::new(points(&planned, job.orientation)),
        report: Some(report),
        separation_km: Arc::new(separation),
        passes,
        notes,
    })
}

pub struct Worker {
    tx: Sender<Job>,
    rx: Receiver<Done>,
}

impl Worker {
    pub fn start() -> Self {
        let (tx, jobs) = channel::<Job>();
        let (out, rx) = channel();
        std::thread::Builder::new()
            .name("planner".into())
            .spawn(move || {
                while let Ok(job) = jobs.recv() {
                    let result = compute(&job);
                    if out.send(Done { id: job.id, result }).is_err() {
                        return;
                    }
                }
            })
            .expect("spawn planner worker");
        Self { tx, rx }
    }

    pub fn submit(&self, job: Job) {
        let _ = self.tx.send(job);
    }

    pub fn poll(&self) -> Vec<Done> {
        self.rx.try_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mctl_astro::numerical::{MU_EARTH, RE_KM};
    use mctl_core::plan::BurnDef;

    /// A circular 51.6 degree orbit that the "active source" is a numerical two-body table of.
    fn job(burns: Vec<BurnDef>, revs: f64) -> Job {
        let t0 = Utc::parse("2026-10-24T00:00:00").unwrap();
        let r = RE_KM + 420.0;
        let v = (MU_EARTH / r).sqrt();
        let i = 51.6f64.to_radians();
        let start = State::new([r, 0.0, 0.0], [0.0, v * i.cos(), v * i.sin()]);
        let two = ForceModel::TWO_BODY;
        let table = Table::new(
            propagate(
                &two,
                &Vector3::z(),
                t0,
                &start,
                t0.plus(20.0 * 86_400.0 / 10.0),
                60.0,
            )
            .unwrap(),
        )
        .unwrap();
        Job {
            id: 1,
            def: PlanDef {
                burns,
                horizon_revs: revs,
                zonal: false,
                sun: false,
                moon: false,
                drag: false,
                ..PlanDef::default()
            },
            t0,
            start,
            nominal: Arc::new(table),
            orientation: Orientation::ASSUMED_ZERO,
            ballistic: None,
            sites: vec![Site {
                name: "x".into(),
                lat_deg: 40.0,
                lon_deg: -100.0,
                alt_m: 0.0,
            }],
            mask_deg: 10.0,
        }
    }

    fn burn(trigger: &str, frame: BurnFrame, dv: [f64; 3]) -> BurnDef {
        BurnDef {
            trigger: trigger.into(),
            frame,
            dv_m_s: dv,
        }
    }

    #[test]
    fn with_no_burns_planned_equals_nominal() {
        let v = compute(&job(vec![], 2.0)).unwrap();
        let r = v.report.unwrap();
        assert!(r.separation_km < 1e-6 && r.d_period_s.abs() < 1e-9 && r.d_phase_deg.abs() < 1e-9);
        assert_eq!(v.nominal.len(), v.planned.len());
    }

    #[test]
    fn a_prograde_burn_lengthens_the_period_and_the_orbit_falls_behind_by_the_predicted_amount() {
        let dv = 2.0; // m/s
        let v = compute(&job(vec![burn("an", BurnFrame::Vnb, [dv, 0.0, 0.0])], 3.0)).unwrap();
        let r = v.report.clone().unwrap();
        let a = RE_KM + 420.0;
        let n = (MU_EARTH / a.powi(3)).sqrt();
        // da = 2 a dv / v, dP = 3 pi da / (n a)... use P = 2 pi sqrt(a^3/mu): dP = 1.5 P da / a.
        let da = 2.0 * a * (dv / 1000.0) / (n * a);
        let period = std::f64::consts::TAU / n;
        let want_dp = 1.5 * period * da / a;
        assert!(
            (r.d_period_s - want_dp).abs() / want_dp < 0.02,
            "{} vs {}",
            r.d_period_s,
            want_dp
        );
        // A longer period means the planned vehicle trails: negative phase after the burn.
        assert!(r.d_phase_deg < 0.0, "{}", r.d_phase_deg);
        assert_eq!(v.burns.len(), 1);
        // The ascending-node trigger resolved to a real node: the next one after the start (z = 0 crossing).
        let at = job(vec![], 1.0).nominal.state_gcrf(v.burns[0].t).unwrap();
        assert!(at.r.z.abs() < 5.0, "z at burn {}", at.r.z);
        assert!(r.planned.sma_km > r.nominal.sma_km);
        assert_eq!(v.passes.len(), 1);
    }

    #[test]
    fn a_burn_at_apoapsis_of_a_circular_orbit_shifts_the_phase_the_same_way_whatever_the_trigger() {
        let v = compute(&job(
            vec![burn("peri", BurnFrame::Rtn, [0.0, 1.0, 0.0])],
            2.0,
        ))
        .unwrap();
        assert!(
            v.report.unwrap().d_period_s > 0.0,
            "RTN along-track is prograde"
        );
    }

    #[test]
    fn bad_triggers_and_epochs_fail_loudly() {
        let e = compute(&job(
            vec![burn("2026-10-30T00:00:00", BurnFrame::Vnb, [1.0, 0.0, 0.0])],
            1.0,
        ))
        .unwrap_err();
        assert!(e.contains("outside the plan window"), "{e}");
        let e = compute(&job(
            vec![burn("2026-10-23T00:00:00", BurnFrame::Vnb, [1.0, 0.0, 0.0])],
            1.0,
        ))
        .unwrap_err();
        assert!(e.contains("outside the plan window"), "{e}");
        let mut j = job(vec![], 1.0);
        j.def.burns = vec![burn("soon", BurnFrame::Vnb, [1.0, 0.0, 0.0])];
        assert!(compute(&j).is_err());
    }

    #[test]
    fn a_second_named_burn_resolves_after_the_first() {
        let v = compute(&job(
            vec![
                burn("an", BurnFrame::Vnb, [1.0, 0.0, 0.0]),
                burn("an", BurnFrame::Vnb, [-1.0, 0.0, 0.0]),
            ],
            3.0,
        ))
        .unwrap();
        assert!(
            v.burns[1].t.0 - v.burns[0].t.0 > 2500.0,
            "two different nodes, a half orbit or more apart"
        );
        // An equal and opposite burn one full orbit later... leaves a closed orbit of the original size.
        let r = v.report.unwrap();
        assert!(r.d_period_s.abs() < 0.01, "{}", r.d_period_s);
    }

    #[test]
    fn drag_without_a_coefficient_is_reported_not_guessed() {
        let mut j = job(vec![], 1.0);
        j.def.drag = true;
        let v = compute(&j).unwrap();
        assert!(
            v.notes
                .iter()
                .any(|n| n.contains("no ballistic coefficient")),
            "{:?}",
            v.notes
        );
        j.ballistic = Some(0.005);
        assert!(compute(&j).unwrap().notes.is_empty());
    }
}
