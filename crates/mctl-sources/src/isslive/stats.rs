//! Per-item observations from a run: update rates, ages, value ranges, quaternion norms.

use std::collections::{BTreeMap, HashSet};
use std::time::Instant;

use super::config::Config;
use super::session::Update;

/// Distinct values remembered per item; beyond this the count is reported as a lower bound.
const DISTINCT_CAP: usize = 1000;

#[derive(Default)]
struct ItemStats {
    updates: u64,
    value_changes: u64,
    first: Option<Instant>,
    last: Option<Instant>,
    intervals_s: Vec<f64>,
    ages_s: Vec<f64>,
    distinct: HashSet<String>,
    min: Option<f64>,
    max: Option<f64>,
    last_value: Option<String>,
}

struct QuatTrack {
    name: String,
    items: [String; 4],
    latest: [Option<f64>; 4],
    norms: Vec<f64>,
}

pub struct Stats {
    items: BTreeMap<String, ItemStats>,
    quats: Vec<QuatTrack>,
}

fn median(v: &mut [f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    Some(v[v.len() / 2])
}

impl Stats {
    pub fn new(cfg: &Config) -> Self {
        let ids = cfg
            .items
            .iter()
            .map(|i| i.id.clone())
            .chain([cfg.time_item.id.clone()]);
        Self {
            items: ids.map(|id| (id, ItemStats::default())).collect(),
            quats: cfg
                .quaternions
                .iter()
                .map(|q| QuatTrack {
                    name: q.name.clone(),
                    items: q.items.clone(),
                    latest: [None; 4],
                    norms: Vec::new(),
                })
                .collect(),
        }
    }

    pub fn record(&mut self, u: &Update<'_>, value: Option<&str>, age_s: Option<f64>) {
        let now = Instant::now();
        let s = self.items.entry(u.item_id.to_owned()).or_default();
        s.updates += 1;
        if let (Some(prev), true) = (s.last, s.first.is_some()) {
            s.intervals_s.push(now.duration_since(prev).as_secs_f64());
        }
        s.first.get_or_insert(now);
        s.last = Some(now);
        if let Some(a) = age_s {
            s.ages_s.push(a);
        }
        let value_changed = u.changed.iter().any(|&i| u.fields[i] == "Value");
        if value_changed {
            s.value_changes += 1;
        }
        if let Some(v) = value {
            if s.distinct.len() < DISTINCT_CAP {
                s.distinct.insert(v.to_owned());
            }
            if let Ok(x) = v.parse::<f64>() {
                s.min = Some(s.min.map_or(x, |m| m.min(x)));
                s.max = Some(s.max.map_or(x, |m| m.max(x)));
                for q in &mut self.quats {
                    if let Some(k) = q.items.iter().position(|id| id == u.item_id) {
                        q.latest[k] = Some(x);
                        if let [Some(a), Some(b), Some(c), Some(d)] = q.latest {
                            q.norms.push((a * a + b * b + c * c + d * d).sqrt());
                        }
                    }
                }
            }
        }
        s.last_value = value.map(str::to_owned);
    }

    pub fn print(&mut self, cfg: &Config, duration_s: f64) {
        println!("\n=== per item ({duration_s:.0} s requested) ===");
        println!(
            "{:<12} {:<8} {:>7} {:>8} {:>9} {:>7} {:>8} {:>14} {:>14} {:>10}  last value",
            "item",
            "group",
            "updates",
            "upd/min",
            "mean dt s",
            "chgs",
            "distinct",
            "min",
            "max",
            "age med s"
        );
        for (id, s) in &mut self.items {
            let span = match (s.first, s.last) {
                (Some(a), Some(b)) => b.duration_since(a).as_secs_f64(),
                _ => 0.0,
            };
            let mean_dt = if s.intervals_s.is_empty() {
                f64::NAN
            } else {
                s.intervals_s.iter().sum::<f64>() / s.intervals_s.len() as f64
            };
            let rate = if span > 0.0 {
                (s.updates.saturating_sub(1)) as f64 / span * 60.0
            } else {
                0.0
            };
            let distinct = if s.distinct.len() >= DISTINCT_CAP {
                format!(">={DISTINCT_CAP}")
            } else {
                s.distinct.len().to_string()
            };
            let group = cfg
                .items
                .iter()
                .find(|i| &i.id == id)
                .map_or("time", |i| i.group.as_str());
            println!(
                "{:<12} {:<8} {:>7} {:>8.1} {:>9.2} {:>7} {:>8} {:>14} {:>14} {:>10}  {}",
                id,
                group,
                s.updates,
                rate,
                mean_dt,
                s.value_changes,
                distinct,
                s.min.map_or("-".into(), |v| format!("{v:.6}")),
                s.max.map_or("-".into(), |v| format!("{v:.6}")),
                median(&mut s.ages_s).map_or("-".into(), |v| format!("{v:.2}")),
                s.last_value.as_deref().unwrap_or("<none>"),
            );
        }
        for q in &mut self.quats {
            let (min, max) = q
                .norms
                .iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &n| {
                    (lo.min(n), hi.max(n))
                });
            if q.norms.is_empty() {
                println!("\nquaternion {}: no complete set received", q.name);
            } else {
                println!(
                    "\nquaternion {}: |q| over {} samples: min {min:.6} max {max:.6}",
                    q.name,
                    q.norms.len()
                );
            }
        }
    }
}
