//! Trend series computed from the recording: elements and beta from recorded states, the difference
//! between sources, and the epochs of recorded element sets (DESIGN.md §9.6 F2.3).
//!
//! Runs on a worker thread with its own read-only connection, so the engine never waits on the disk.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

use mctl_astro::kepler;
use mctl_astro::sun::sun_gcrf;
use mctl_core::state::State;
use mctl_core::time::Utc;
use mctl_core::world::{TrendPlot, TrendSeries, TrendSet};
use mctl_store::{StateRow, Store};
use nalgebra::Vector3;

/// Most points per source read back; plots are a few hundred pixels wide.
pub const MAX_ROWS: usize = 700;
/// Source preference when several recorded the same instant (the arbiter's usual priority).
const PREFERRED: [&str; 2] = ["OEM", "GP/SGP4"];
/// Two rows closer in time than this are the same recorded instant, seconds.
const SAME_INSTANT_S: f64 = 0.5;

fn state_of(r: &StateRow) -> State {
    State::new(r.r_km, r.v_km_s)
}

/// Compute the plots for `vehicle` over `[now - window_s, now]`.
pub fn compute(db: &Path, vehicle: &str, now: Utc, window_s: f64) -> Result<TrendSet, String> {
    let store = Store::open_readonly(db).map_err(|e| e.to_string())?;
    let (from, to) = (now.plus(-window_s), now.plus(1.0));
    let rows = store
        .states(vehicle, from, to, MAX_ROWS)
        .map_err(|e| e.to_string())?;
    let rank = |s: &str| PREFERRED.iter().position(|p| *p == s).unwrap_or(usize::MAX);
    // Per recorded instant keep the most preferred source for the element series.
    let mut best: Vec<&StateRow> = Vec::new();
    for r in &rows {
        match best.last_mut() {
            Some(last) if (last.t.0 - r.t.0).abs() < SAME_INSTANT_S => {
                if rank(&r.source) < rank(&last.source) {
                    *last = r;
                }
            }
            _ => best.push(r),
        }
    }
    let (mut sma, mut ha, mut hp, mut ecc, mut inc, mut raan, mut argp, mut beta) = (
        vec![],
        vec![],
        vec![],
        vec![],
        vec![],
        vec![],
        vec![],
        vec![],
    );
    for r in &best {
        let st = state_of(r);
        let Some(el) = kepler::elements(&st) else {
            continue;
        };
        let t = r.t.0;
        sma.push((t, el.sma_km as f32));
        ha.push((t, el.apo_alt_km as f32));
        hp.push((t, el.peri_alt_km as f32));
        ecc.push((t, el.ecc as f32));
        inc.push((t, el.inc_deg as f32));
        raan.push((t, el.raan_deg as f32));
        argp.push((t, el.argp_deg as f32));
        if let Some(b) = sun_gcrf(r.t, 0.0)
            .ok()
            .and_then(|sun| kepler::beta_deg(&st, &sun))
        {
            beta.push((t, b as f32));
        }
    }
    // Difference between sources that recorded the same instant.
    let mut diff = vec![];
    for (i, r) in rows.iter().enumerate() {
        let other = rows[..i]
            .iter()
            .rev()
            .take(6)
            .find(|o| (o.t.0 - r.t.0).abs() < SAME_INSTANT_S && o.source != r.source);
        if let Some(o) = other {
            diff.push((
                r.t.0,
                (Vector3::from(r.r_km) - Vector3::from(o.r_km)).norm() as f32,
            ));
        }
    }
    let plot = |title: &str, units: &str, series: Vec<(&str, Vec<(f64, f32)>)>| TrendPlot {
        title: title.into(),
        units: units.into(),
        series: series
            .into_iter()
            .map(|(label, points)| TrendSeries {
                label: label.into(),
                points,
            })
            .collect(),
    };
    let marks = store
        .element_epochs(vehicle, from, to)
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|(s, _)| s == "GP/SGP4" || s == "GP")
        .map(|(_, e)| e)
        .collect();
    Ok(TrendSet {
        vehicle: vehicle.to_owned(),
        from,
        to: now,
        plots: vec![
            plot("Semi-major axis", "km", vec![("SMA", sma)]),
            plot("Apsis altitudes", "km", vec![("Ha", ha), ("Hp", hp)]),
            plot("Eccentricity", "", vec![("e", ecc)]),
            plot("Inclination", "deg", vec![("i", inc)]),
            plot("RAAN", "deg", vec![("RAAN", raan)]),
            plot("Argument of perigee", "deg", vec![("argp", argp)]),
            plot("Beta angle", "deg", vec![("beta", beta)]),
            plot("Source difference |dr|", "km", vec![("OEM vs GP", diff)]),
        ],
        element_marks: marks,
        rows: rows.len(),
    })
}

pub struct Job {
    pub vehicle: String,
    pub now: Utc,
    pub window_s: f64,
}

pub struct Worker {
    tx: Sender<Job>,
    rx: Receiver<Result<TrendSet, String>>,
}

impl Worker {
    pub fn start(db: PathBuf) -> Self {
        let (tx, jobs) = channel::<Job>();
        let (out_tx, rx) = channel();
        std::thread::Builder::new()
            .name("trends".into())
            .spawn(move || {
                while let Ok(mut job) = jobs.recv() {
                    // Only the newest request matters.
                    while let Ok(next) = jobs.try_recv() {
                        job = next;
                    }
                    if out_tx
                        .send(compute(&db, &job.vehicle, job.now, job.window_s))
                        .is_err()
                    {
                        return;
                    }
                }
            })
            .expect("spawn trends worker");
        Self { tx, rx }
    }

    pub fn submit(&self, job: Job) {
        let _ = self.tx.send(job);
    }

    pub fn poll(&self) -> Vec<Result<TrendSet, String>> {
        self.rx.try_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use mctl_astro::kepler::MU_KM3_S2;
    use mctl_store::Record;

    use super::*;

    fn circular(source: &str, t: f64, a: f64, off_km: f64) -> Record {
        // Circular orbit in a plane tilted so that cos(i) = 0.8, advancing with time; `off_km` shifts x.
        let n = (MU_KM3_S2 / (a * a * a)).sqrt();
        let (s, c) = (n * t).sin_cos();
        let v = (MU_KM3_S2 / a).sqrt();
        Record::State {
            vehicle: "iss".into(),
            source: source.into(),
            t: Utc(t),
            frame: "GCRF",
            r_km: [a * c + off_km, a * s * 0.8, a * s * 0.6],
            v_km_s: [-v * s, v * c * 0.8, v * c * 0.6],
            quality: "PRED".into(),
        }
    }

    #[test]
    fn trends_recover_the_orbit_the_states_were_recorded_from() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut s = Store::open(&db).unwrap();
        let now = Utc::now().unwrap();
        let mut rows: Vec<Record> = (0..600)
            .map(|i| circular("OEM", now.0 - 600.0 + f64::from(i), 6_798.0, 0.0))
            .collect();
        rows.extend(
            (0..600).map(|i| circular("GP/SGP4", now.0 - 600.0 + f64::from(i), 6_798.0, 2.0)),
        );
        rows.push(Record::Elements {
            vehicle: "iss".into(),
            source: "GP/SGP4".into(),
            epoch: Some(now.plus(-300.0)),
            fetched_at: now,
            blob: "x".into(),
        });
        s.write(&rows).unwrap();
        drop(s);
        let set = compute(&db, "iss", now, 900.0).unwrap();
        assert_eq!(set.plots.len(), 8);
        let sma = &set.plots[0].series[0].points;
        assert!(
            sma.len() > 100
                && sma
                    .iter()
                    .all(|(_, v)| (f64::from(*v) - 6_798.0).abs() < 1.5),
            "SMA is flat near 6798 km: {:?}",
            &sma[..3]
        );
        let inc = &set.plots[3].series[0].points;
        assert!(
            inc.iter().all(|(_, v)| (f64::from(*v) - 36.87).abs() < 0.2),
            "inclination: the orbit normal is (0, -0.6, 0.8), so cos(i) = 0.8: {:?}",
            &inc[..2]
        );
        let diff = &set.plots[7].series[0].points;
        assert!(
            !diff.is_empty() && diff.iter().all(|(_, d)| (d - 2.0).abs() < 1e-3),
            "sources are 2 km apart: {:?}",
            diff.first()
        );
        assert_eq!(set.element_marks.len(), 1);
        assert!(set.rows >= 200);
    }

    #[test]
    fn an_empty_window_gives_empty_plots_and_a_missing_database_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        drop(Store::open(&db).unwrap());
        let set = compute(&db, "iss", Utc::now().unwrap(), 3600.0).unwrap();
        assert!(
            set.plots
                .iter()
                .all(|p| p.series.iter().all(|s| s.points.is_empty()))
        );
        assert!(compute(&dir.path().join("nope.db"), "iss", Utc(1.0e9), 60.0).is_err());
    }
}
