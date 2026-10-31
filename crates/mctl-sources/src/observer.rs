//! Observer location resolution after a configured site: GeoClue2, then optional IP
//! geolocation. The fallback site is handled by the engine; this only reports what it found, or why not.

use std::sync::mpsc::SyncSender;
use std::time::Duration;

use mctl_core::config::{Config, IpLookup};

use crate::geoclue;
use crate::tasks::Event;

/// A location found by a lookup.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub name: String,
    pub lat_deg: f64,
    pub lon_deg: f64,
    pub alt_m: f64,
    /// For display, e.g. `GEOCLUE +-26 km`.
    pub origin: String,
}

/// Pull `lat_key` and `lon_key` out of a JSON object.
pub fn parse_ip_json(body: &str, ip: &IpLookup) -> Result<(f64, f64), String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("response is not JSON: {e}"))?;
    let num = |k: &str| {
        v.get(k)
            .and_then(serde_json::Value::as_f64)
            .ok_or_else(|| format!("no numeric {k:?} in the response"))
    };
    let (lat, lon) = (num(&ip.lat_key)?, num(&ip.lon_key)?);
    if lat.abs() > 90.0 || lon.abs() > 180.0 {
        return Err(format!("coordinates out of range: {lat}, {lon}"));
    }
    Ok((lat, lon))
}

fn ip_lookup(cfg: &Config) -> Result<Resolved, String> {
    let ip = &cfg.observer.ip;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs_f64(cfg.fetch.timeout_s)))
        .user_agent(cfg.fetch.user_agent.clone())
        .build()
        .into();
    let body = agent
        .get(&ip.url)
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    let (lat, lon) = parse_ip_json(&body, ip)?;
    Ok(Resolved {
        name: "IP".into(),
        lat_deg: lat,
        lon_deg: lon,
        alt_m: 0.0,
        origin: format!("IP +-{:.0} km", ip.accuracy_km),
    })
}

/// Resolve once and send the outcome. Runs on its own thread; GeoClue can take its whole timeout.
pub fn run(cfg: &Config, offline: bool, tx: &SyncSender<Event>) {
    let note = |s: String| {
        tracing::info!("{s}");
        let _ = tx.send(Event::ObserverNote(s));
    };
    if cfg.observer.geoclue {
        match geoclue::locate(cfg.observer.geoclue_timeout_s) {
            Ok(l) => {
                let _ = tx.send(Event::Observer(Resolved {
                    name: "GeoClue".into(),
                    lat_deg: l.lat_deg,
                    lon_deg: l.lon_deg,
                    alt_m: l.alt_m,
                    origin: format!("GEOCLUE +-{:.0} km", (l.accuracy_m / 1000.0).max(1.0)),
                }));
                return;
            }
            Err(e) => note(format!("observer: GeoClue gave no location: {e}")),
        }
    }
    if cfg.observer.ip.enabled {
        if offline {
            note("observer: IP lookup skipped (offline)".into());
        } else {
            match ip_lookup(cfg) {
                Ok(r) => {
                    let _ = tx.send(Event::Observer(r));
                    return;
                }
                Err(e) => note(format!("observer: IP lookup failed: {e}")),
            }
        }
    }
    note("observer: using the fallback site".into());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip() -> IpLookup {
        IpLookup {
            enabled: true,
            url: "https://x".into(),
            lat_key: "latitude".into(),
            lon_key: "longitude".into(),
            accuracy_km: 25.0,
        }
    }

    #[test]
    fn ip_json_is_read_by_the_configured_keys_and_validated() {
        assert_eq!(
            parse_ip_json(
                r#"{"latitude": 38.883, "longitude": -77.0163, "city": "x"}"#,
                &ip()
            ),
            Ok((38.883, -77.0163))
        );
        assert!(
            parse_ip_json(r#"{"latitude": "51"}"#, &ip())
                .unwrap_err()
                .contains("numeric")
        );
        assert!(
            parse_ip_json("<html>", &ip())
                .unwrap_err()
                .contains("not JSON")
        );
        assert!(
            parse_ip_json(r#"{"latitude": 95, "longitude": 0}"#, &ip())
                .unwrap_err()
                .contains("out of range")
        );
        let mut other = ip();
        other.lat_key = "lat".into();
        assert!(
            parse_ip_json(r#"{"latitude": 1, "longitude": 2}"#, &other).is_err(),
            "keys are configuration, not guesses"
        );
    }
}
