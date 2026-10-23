//! Supervised source tasks (NFR-5): one thread per source, so a hung or failing provider can never
//! block the UI or another source. On start each task publishes its cached copy immediately, then
//! refreshes in the background with exponential backoff and jitter after failures (DESIGN.md §3.3).

use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::thread::JoinHandle;
use std::time::Duration;

use mctl_astro::oem::Oem;
use mctl_astro::sgp4model::GpModel;
use mctl_core::config::{Config, Fetch};
use mctl_core::state::SourceKind;
use mctl_core::time::{EopTable, Utc};
use mctl_core::world::Health;

use crate::cache::{CacheError, HttpCache, Refresh};

/// Events queued between sources and the engine before senders block.
const EVENT_QUEUE: usize = 64;
/// Fraction of random extra delay added to every wait, to avoid synchronised requests.
const JITTER_FRACTION: f64 = 0.1;
/// Shortest wait between loop iterations, seconds.
const MIN_WAIT_S: f64 = 5.0;
/// Interval at which an offline task re-checks for shutdown, seconds.
const OFFLINE_POLL_S: f64 = 3600.0;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SourceKey {
    /// CelesTrak GP for the vehicle with this id.
    Gp(String),
    /// The NASA ISS OEM, used by the one vehicle that lists the `oem` source.
    Oem(String),
    Eop,
}

pub enum Payload {
    Gp(GpModel),
    Oem(Box<Oem>),
    Eop(EopTable),
}

pub struct DataInfo {
    pub from_cache: bool,
    pub fetched_at: Utc,
}

/// Static description of a subscribed telemetry item.
#[derive(Debug, Clone)]
pub struct TelemetryMeta {
    pub id: String,
    pub group: String,
    pub description: String,
    pub label: String,
    pub units: String,
}

pub enum Event {
    /// A telemetry item changed. `text` is the raw `Value` field.
    Telemetry {
        id: String,
        text: Option<String>,
        rx: Utc,
    },
    TelemetryHealth(Health),
    Data {
        key: SourceKey,
        payload: Payload,
        info: DataInfo,
    },
    Health {
        key: SourceKey,
        health: Health,
    },
}

type Parser = Arc<dyn Fn(&str) -> Result<Payload, String> + Send + Sync>;

#[derive(Clone)]
pub struct Spec {
    pub key: SourceKey,
    pub cache_key: String,
    pub urls: Vec<String>,
    pub min_refresh_s: f64,
    parse: Parser,
}

impl Spec {
    pub fn parse(&self, body: &str) -> Result<Payload, String> {
        (self.parse)(body)
    }
}

/// The sources the configuration calls for.
pub fn specs(cfg: &Config) -> Vec<Spec> {
    let mut out = Vec::new();
    for v in &cfg.vehicles {
        if v.sources.contains(&SourceKind::Gp) {
            let (id, norad) = (v.id.clone(), v.norad);
            out.push(Spec {
                key: SourceKey::Gp(v.id.clone()),
                cache_key: format!("gp-{}.json", v.norad),
                urls: vec![
                    cfg.sources
                        .celestrak_gp
                        .url_template
                        .replace("{norad}", &v.norad.to_string()),
                ],
                min_refresh_s: cfg.sources.celestrak_gp.min_refresh_s,
                parse: Arc::new(move |body| {
                    let m = GpModel::from_omm_json(body).map_err(|e| e.to_string())?;
                    // A response for the wrong object must never be accepted as this vehicle's data.
                    if m.norad != norad {
                        return Err(format!(
                            "response is for NORAD {} but vehicle {id} is {norad}",
                            m.norad
                        ));
                    }
                    Ok(Payload::Gp(m))
                }),
            });
        }
        if v.sources.contains(&SourceKind::Oem) {
            out.push(Spec {
                key: SourceKey::Oem(v.id.clone()),
                cache_key: "oem-iss.txt".into(),
                urls: vec![cfg.sources.nasa_oem.url.clone()],
                min_refresh_s: cfg.sources.nasa_oem.min_refresh_s,
                parse: Arc::new(|body| {
                    Oem::parse(body)
                        .map(|o| Payload::Oem(Box::new(o)))
                        .map_err(|e| e.to_string())
                }),
            });
        }
    }
    out.push(Spec {
        key: SourceKey::Eop,
        cache_key: "finals2000A.all".into(),
        urls: cfg.sources.iers_eop.urls.clone(),
        min_refresh_s: cfg.sources.iers_eop.min_refresh_s,
        parse: Arc::new(|body| {
            EopTable::parse(body)
                .map(Payload::Eop)
                .map_err(|e| e.to_string())
        }),
    });
    out
}

/// Handle to the running tasks. Dropping it asks them to stop.
pub struct SourceSet {
    pub events: Receiver<Event>,
    /// Items the telemetry feed subscribes to (empty without a telemetry vehicle).
    pub telemetry: Vec<TelemetryMeta>,
    _stop: Vec<SyncSender<()>>,
    _threads: Vec<JoinHandle<()>>,
}

fn jitter(seconds: f64) -> f64 {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    seconds * (1.0 + JITTER_FRACTION * f64::from(n % 1000) / 1000.0)
}

/// Start one supervisor thread per source. `offline` disables all network access (cache only).
pub fn start(
    cfg: &Config,
    cache_dir: std::path::PathBuf,
    offline: bool,
) -> Result<SourceSet, CacheError> {
    let cache = Arc::new(HttpCache::new(
        cache_dir,
        &cfg.fetch.user_agent,
        cfg.fetch.timeout_s,
    )?);
    let (tx, rx) = sync_channel(EVENT_QUEUE);
    let (mut stops, mut threads) = (Vec::new(), Vec::new());
    for spec in specs(cfg) {
        let (stop_tx, stop_rx) = sync_channel::<()>(1);
        let (cache, tx, fetch) = (cache.clone(), tx.clone(), cfg.fetch.clone());
        let name = format!("src-{:?}", spec.key);
        threads.push(
            std::thread::Builder::new()
                .name(name)
                .spawn(move || supervise(&spec, &cache, &tx, &stop_rx, &fetch, offline))
                .expect("spawn source thread"),
        );
        stops.push(stop_tx);
    }
    let mut telemetry = Vec::new();
    if cfg
        .vehicles
        .iter()
        .any(|v| v.telemetry.as_deref() == Some("isslive"))
    {
        let icfg = crate::isslive::config::Config::parse(
            "embedded default",
            crate::isslive::config::DEFAULT_CONFIG,
        )
        .expect("embedded ISSLIVE config is valid (checked by tests)");
        telemetry = icfg
            .items
            .iter()
            .map(|i| TelemetryMeta {
                id: i.id.clone(),
                group: i.group.clone(),
                description: i.description.clone(),
                label: i.label.clone().unwrap_or_else(|| i.description.clone()),
                units: i.units.clone(),
            })
            .collect();
        let (stop_tx, stop_rx) = sync_channel::<()>(1);
        let (tx, fetch) = (tx.clone(), cfg.fetch.clone());
        threads.push(
            std::thread::Builder::new()
                .name("src-isslive".into())
                .spawn(move || crate::isslive::feed::run(icfg, &tx, &stop_rx, &fetch, offline))
                .expect("spawn telemetry thread"),
        );
        stops.push(stop_tx);
    }
    Ok(SourceSet {
        events: rx,
        telemetry,
        _stop: stops,
        _threads: threads,
    })
}

fn send(tx: &SyncSender<Event>, e: Event) -> bool {
    tx.send(e).is_ok()
}

/// Wait up to `seconds`; `false` means stop was requested (or the owner went away).
fn wait(stop: &Receiver<()>, seconds: f64) -> bool {
    matches!(
        stop.recv_timeout(Duration::from_secs_f64(seconds.max(0.0))),
        Err(RecvTimeoutError::Timeout)
    )
}

fn supervise(
    spec: &Spec,
    cache: &HttpCache,
    tx: &SyncSender<Event>,
    stop: &Receiver<()>,
    fetch: &Fetch,
    offline: bool,
) {
    let key = spec.key.clone();
    let health = |h| {
        send(
            tx,
            Event::Health {
                key: key.clone(),
                health: h,
            },
        )
    };
    let mut force_refresh = false;
    match cache.load(&spec.cache_key) {
        Ok(Some(f)) => match spec.parse(&f.body) {
            Ok(payload) => {
                tracing::info!(source = ?key, "loaded from cache");
                if !send(
                    tx,
                    Event::Data {
                        key: key.clone(),
                        payload,
                        info: DataInfo {
                            from_cache: true,
                            fetched_at: Utc(f.meta.fetched_at),
                        },
                    },
                ) || !health(Health::Ok)
                {
                    return;
                }
            }
            Err(e) => {
                tracing::warn!(source = ?key, error = %e, "cached copy unusable");
                force_refresh = true;
                if !health(Health::Failing(format!("cached data unusable: {e}"))) {
                    return;
                }
            }
        },
        Ok(None) => {}
        Err(e) => {
            force_refresh = true;
            if !health(Health::Failing(e.to_string())) {
                return;
            }
        }
    }
    if offline {
        health(Health::Offline);
        while wait(stop, OFFLINE_POLL_S) {}
        return;
    }
    let mut backoff = fetch.backoff_initial_s;
    loop {
        let now = Utc::now().map_or(0.0, |u| u.0);
        let interval = if force_refresh {
            0.0
        } else {
            spec.min_refresh_s
        };
        let (mut done, mut errors) = (false, Vec::new());
        for url in &spec.urls {
            match cache.refresh(&spec.cache_key, url, interval, now) {
                Ok(Refresh::NotDue | Refresh::NotModified) => {
                    done = true;
                    break;
                }
                Ok(Refresh::Updated(f)) => match spec.parse(&f.body) {
                    Ok(payload) => {
                        tracing::info!(source = ?key, url, "downloaded");
                        if !send(
                            tx,
                            Event::Data {
                                key: key.clone(),
                                payload,
                                info: DataInfo {
                                    from_cache: false,
                                    fetched_at: Utc(f.meta.fetched_at),
                                },
                            },
                        ) {
                            return;
                        }
                        done = true;
                        break;
                    }
                    Err(e) => errors.push(format!("{url}: unparseable response: {e}")),
                },
                Err(e) => errors.push(e.to_string()),
            }
        }
        let wait_s = if done {
            force_refresh = false;
            backoff = fetch.backoff_initial_s;
            if !health(Health::Ok) {
                return;
            }
            let checked = cache
                .load(&spec.cache_key)
                .ok()
                .flatten()
                .map_or(now, |f| f.meta.checked_at);
            (checked + spec.min_refresh_s - now).max(MIN_WAIT_S)
        } else {
            tracing::warn!(source = ?key, errors = ?errors, backoff_s = backoff, "refresh failed");
            if !health(Health::Failing(errors.join("; "))) {
                return;
            }
            let w = backoff;
            backoff = (backoff * 2.0).min(fetch.backoff_max_s);
            w
        };
        if !wait(stop, jitter(wait_s)) {
            return;
        }
    }
}

/// Result of a `mctl check` probe of one source.
pub struct CheckLine {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

/// Fetch every configured source once, ignoring the minimum-refresh interval, and report what happened.
pub fn check(cfg: &Config, cache_dir: std::path::PathBuf) -> Result<Vec<CheckLine>, CacheError> {
    let cache = HttpCache::new(cache_dir, &cfg.fetch.user_agent, cfg.fetch.timeout_s)?;
    let now = Utc::now().map_or(0.0, |u| u.0);
    Ok(specs(cfg)
        .into_iter()
        .map(|spec| {
            let name = format!("{:?}", spec.key);
            let mut errors = Vec::new();
            for url in &spec.urls {
                match cache.refresh(&spec.cache_key, url, 0.0, now) {
                    Ok(r) => {
                        let how = match r {
                            Refresh::Updated(_) => "downloaded",
                            Refresh::NotModified => "not modified (ETag/Last-Modified)",
                            Refresh::NotDue => "cache current",
                        };
                        return match cache
                            .load(&spec.cache_key)
                            .map_err(|e| e.to_string())
                            .and_then(|f| f.ok_or("cache empty after fetch".to_owned()))
                            .and_then(|f| spec.parse(&f.body).map(|p| (p, f.body.len())))
                        {
                            Ok((p, bytes)) => CheckLine {
                                name,
                                ok: true,
                                detail: format!("{how}, {bytes} bytes, {}", summarise(&p)),
                            },
                            Err(e) => CheckLine {
                                name,
                                ok: false,
                                detail: format!("{how} but unusable: {e}"),
                            },
                        };
                    }
                    Err(e) => errors.push(e.to_string()),
                }
            }
            CheckLine {
                name,
                ok: false,
                detail: errors.join("; "),
            }
        })
        .collect())
}

fn summarise(p: &Payload) -> String {
    match p {
        Payload::Gp(m) => format!("{} (NORAD {}), epoch {}", m.name, m.norad, m.epoch.iso()),
        Payload::Oem(o) => format!(
            "{} states, {} .. {}, created {}",
            o.states.len(),
            o.span().0.iso(),
            o.span().1.iso(),
            o.creation_date.iso()
        ),
        Payload::Eop(t) => format!("covers MJD {:.0} .. {:.0}", t.coverage().0, t.coverage().1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_yields_gp_for_each_vehicle_plus_one_oem_and_eop() {
        let cfg = Config::from_layers(mctl_core::config::DEFAULT_CONFIG, None).unwrap();
        let keys: Vec<SourceKey> = specs(&cfg).into_iter().map(|s| s.key).collect();
        assert!(
            keys.contains(&SourceKey::Gp("iss".into()))
                && keys.contains(&SourceKey::Gp("hubble".into()))
        );
        assert_eq!(
            keys.iter()
                .filter(|k| matches!(k, SourceKey::Oem(_)))
                .count(),
            1
        );
        assert!(keys.contains(&SourceKey::Eop));
        let gp = specs(&cfg)
            .into_iter()
            .find(|s| s.key == SourceKey::Gp("iss".into()))
            .unwrap();
        assert!(gp.urls[0].contains("CATNR=25544") && gp.urls[0].contains("FORMAT=JSON"));
    }

    #[test]
    fn a_gp_response_for_the_wrong_object_is_rejected() {
        let cfg = Config::from_layers(mctl_core::config::DEFAULT_CONFIG, None).unwrap();
        let hubble = specs(&cfg)
            .into_iter()
            .find(|s| s.key == SourceKey::Gp("hubble".into()))
            .unwrap();
        let iss_omm = include_str!("../../mctl-astro/fixtures/iss-omm.json");
        let Err(e) = hubble.parse(iss_omm) else {
            panic!("must reject")
        };
        assert!(e.contains("NORAD 25544") && e.contains("20580"), "{e}");
    }

    #[test]
    fn offline_with_empty_cache_reports_offline_and_no_data() {
        let cfg = Config::from_layers(mctl_core::config::DEFAULT_CONFIG, None).unwrap();
        let dir = std::env::temp_dir().join(format!("mctl-offline-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let set = start(&cfg, dir.clone(), true).unwrap();
        let mut offline = 0;
        while let Ok(e) = set.events.recv_timeout(Duration::from_secs(2)) {
            match e {
                Event::Health {
                    health: Health::Offline,
                    ..
                } => offline += 1,
                Event::Data { .. } => {
                    panic!("no data can exist with an empty cache and no network")
                }
                Event::Health { .. } | Event::Telemetry { .. } => {}
                Event::TelemetryHealth(h) => assert_eq!(h, Health::Offline),
            }
            if offline == specs(&cfg).len() {
                break;
            }
        }
        assert_eq!(offline, specs(&cfg).len());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn cached_data_is_published_before_any_network_use() {
        let cfg = Config::from_layers(mctl_core::config::DEFAULT_CONFIG, None).unwrap();
        let dir = std::env::temp_dir().join(format!("mctl-cachefirst-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("gp-25544.json"),
            include_str!("../../mctl-astro/fixtures/iss-omm.json"),
        )
        .unwrap();
        std::fs::write(
            dir.join("gp-25544.json.meta.json"),
            r#"{"etag":null,"last_modified":null,"fetched_at":1000.0,"checked_at":1000.0}"#,
        )
        .unwrap();
        let set = start(&cfg, dir.clone(), true).unwrap();
        let mut got = false;
        while let Ok(e) = set.events.recv_timeout(Duration::from_secs(2)) {
            if let Event::Data {
                key: SourceKey::Gp(id),
                info,
                ..
            } = e
            {
                assert_eq!(id, "iss");
                assert!(info.from_cache && info.fetched_at.0 == 1000.0);
                got = true;
                break;
            }
        }
        assert!(got, "cached GP must be published at startup, offline");
        std::fs::remove_dir_all(dir).ok();
    }
}
