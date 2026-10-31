//! Pass prediction jobs: computed off the engine thread, because a week of passes
//! over several sites takes about a second.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use mctl_astro::frames::{Orientation, Reduction};
use mctl_astro::observer::{self, Site};
use mctl_astro::sun::{illumination, sun_gcrf};
use mctl_astro::trajectory::{Trajectory, TrajectoryError};
use mctl_core::time::Utc;
use mctl_core::world::{LookView, PassSet, PassView};

/// Spacing of the sky-track samples stored with each pass, seconds.
const PATH_STEP_S: f64 = 10.0;

/// Primary trajectory, with a fallback for times the primary does not cover (an OEM's span ends
/// days before a GP element set stops being useful).
pub struct Chain {
    pub parts: Vec<Arc<dyn Trajectory>>,
}

impl Trajectory for Chain {
    fn state_gcrf(&self, t: Utc) -> Result<mctl_core::state::State, TrajectoryError> {
        let mut last = TrajectoryError::Failed("no trajectory source".into());
        for p in &self.parts {
            match p.state_gcrf(t) {
                Ok(s) => return Ok(s),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    fn validity(&self) -> Option<(Utc, Utc)> {
        None
    }
}

pub struct Job {
    pub id: u64,
    pub vehicle: String,
    pub traj: Arc<dyn Trajectory>,
    pub source: String,
    pub sites: Vec<Site>,
    pub t0: Utc,
    pub days: f64,
    pub mask_deg: f64,
    pub step_s: f64,
    pub orientation: Orientation,
}

pub struct Done {
    pub id: u64,
    pub vehicle: String,
    pub sets: Vec<PassSet>,
    pub beta: Vec<(Utc, f64)>,
}

/// Days either side of the prediction time covered by the beta-angle series.
pub const BETA_DAYS: i32 = 30;

pub fn compute(job: &Job) -> Done {
    let t1 = job.t0.plus(job.days * 86_400.0);
    let sets =
        job.sites
            .iter()
            .map(|site| {
                let look = |t: Utc| observer::look_at(job.traj.as_ref(), site, t, job.orientation);
                let passes = observer::find_passes(&look, job.t0, t1, job.step_s, job.mask_deg)
                    .into_iter()
                    .map(|p| {
                        let mut path = Vec::new();
                        let mut t = p.aos.0;
                        while t < p.los.0 {
                            if let Some(l) = look(Utc(t)) {
                                path.push([l.az_deg as f32, l.el_deg as f32]);
                            }
                            t += PATH_STEP_S;
                        }
                        if let Some(l) = look(p.los) {
                            path.push([l.az_deg as f32, l.el_deg as f32]);
                        }
                        PassView {
                            aos: p.aos,
                            tca: p.tca,
                            los: p.los,
                            max_el_deg: p.max_el_deg,
                            aos_az_deg: p.aos_az_deg,
                            tca_az_deg: p.tca_az_deg,
                            los_az_deg: p.los_az_deg,
                            started_before_window: p.started_before_window,
                            ends_after_window: p.ends_after_window,
                            visible: visible_at(job, site, p.tca),
                            path,
                        }
                    })
                    .collect();
                PassSet {
                    site: site.name.clone(),
                    from: job.t0,
                    to: t1,
                    source: job.source.clone(),
                    passes,
                    look_now: observer::look_at(job.traj.as_ref(), site, job.t0, job.orientation)
                        .map(|l| LookView {
                            az_deg: l.az_deg,
                            el_deg: l.el_deg,
                            range_km: l.range_km,
                        }),
                }
            })
            .collect();
    Done {
        id: job.id,
        vehicle: job.vehicle.clone(),
        sets,
        beta: beta_series(job),
    }
}

/// Beta angle once a day around `t0`; days where no state or Sun position exists are skipped.
fn beta_series(job: &Job) -> Vec<(Utc, f64)> {
    (-BETA_DAYS..=BETA_DAYS)
        .filter_map(|d| {
            let t = job.t0.plus(f64::from(d) * 86_400.0);
            let state = job.traj.state_gcrf(t).ok()?;
            let sun = sun_gcrf(t, job.orientation.dut1_s).ok()?;
            Some((t, mctl_astro::kepler::beta_deg(&state, &sun)?))
        })
        .collect()
}

fn visible_at(job: &Job, site: &Site, t: Utc) -> Option<bool> {
    let sun = sun_gcrf(t, job.orientation.dut1_s).ok()?;
    let red = Reduction::at(t, job.orientation).ok()?;
    let r = job.traj.state_gcrf(t).ok()?.r;
    Some(observer::visible(
        site.elevation_of_direction(&sun, &red),
        illumination(&r, &sun),
    ))
}

/// Runs jobs on a worker thread; only the newest queued job per request matters.
pub struct Worker {
    tx: Sender<Job>,
    rx: Receiver<Done>,
}

impl Worker {
    pub fn start() -> Self {
        let (tx, jobs) = channel::<Job>();
        let (done_tx, rx) = channel::<Done>();
        std::thread::Builder::new()
            .name("passes".into())
            .spawn(move || {
                while let Ok(mut job) = jobs.recv() {
                    // Skip superseded jobs for the same vehicle.
                    let mut queue = vec![];
                    while let Ok(next) = jobs.try_recv() {
                        queue.push(next);
                    }
                    for next in queue {
                        if next.vehicle == job.vehicle {
                            job = next;
                        } else if done_tx.send(compute(&next)).is_err() {
                            return;
                        }
                    }
                    if done_tx.send(compute(&job)).is_err() {
                        return;
                    }
                }
            })
            .expect("spawn pass worker");
        Self { tx, rx }
    }

    pub fn submit(&self, job: Job) {
        let _ = self.tx.send(job);
    }

    pub fn poll(&self) -> Vec<Done> {
        self.rx.try_iter().collect()
    }
}
