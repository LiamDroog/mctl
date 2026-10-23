//! Configuration: embedded defaults, optionally overridden by a user file. Everything tunable
//! (URLs, rates, limits) lives here (NFR-8); invalid values are an error, never a silent default.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

use crate::state::SourceKind;

pub const DEFAULT_CONFIG: &str = include_str!("../../../config/default.toml");

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{origin}: {source}")]
    Parse {
        origin: String,
        source: toml::de::Error,
    },
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub fetch: Fetch,
    pub sources: Sources,
    pub limits: Limits,
    pub engine: Engine,
    #[serde(default)]
    pub store: StoreConfig,
    pub observer: ObserverConfig,
    #[serde(rename = "site", default)]
    pub sites: Vec<SiteDef>,
    #[serde(rename = "vehicle")]
    pub vehicles: Vec<VehicleDef>,
    /// Caution and warning threshold rules (FR-40).
    #[serde(rename = "rule", default)]
    pub rules: Vec<crate::cw::RuleDef>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fetch {
    pub user_agent: String,
    pub timeout_s: f64,
    pub backoff_initial_s: f64,
    pub backoff_max_s: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sources {
    pub celestrak_gp: GpSource,
    pub nasa_oem: OemSource,
    pub iers_eop: EopSource,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GpSource {
    pub url_template: String,
    pub min_refresh_s: f64,
    pub stale_after_s: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OemSource {
    pub url: String,
    pub min_refresh_s: f64,
    pub stale_after_s: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EopSource {
    pub urls: Vec<String>,
    pub min_refresh_s: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub residual_caution_km: f64,
    pub residual_warning_km: f64,
}

/// A named observer site (DESIGN.md §8, FR-51).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiteDef {
    pub name: String,
    pub lat_deg: f64,
    pub lon_deg: f64,
    pub alt_m: f64,
}

impl SiteDef {
    pub fn validate(&self) -> Result<(), String> {
        let ok = self.lat_deg.abs() <= 90.0
            && self.lon_deg.abs() <= 180.0
            && self.alt_m.is_finite()
            && (-500.0..=9000.0).contains(&self.alt_m);
        if self.name.trim().is_empty() || !ok {
            return Err(format!(
                "site {:?}: need a name, |lat| <= 90, |lon| <= 180 and altitude -500..9000 m",
                self.name
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObserverConfig {
    /// Name of the site to use; empty means the fallback.
    pub active: String,
    pub mask_deg: f64,
    pub pass_days: f64,
    /// Elevation sampling step of the pass search, seconds.
    pub step_s: f64,
    /// Used when no site is configured as active (DESIGN.md §8 step 4).
    pub fallback: SiteDef,
}

/// Recording to SQLite (DESIGN.md §10).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct StoreConfig {
    pub enabled: bool,
    /// Database path; empty means `$XDG_DATA_HOME/mctl/mctl.db`.
    pub path: String,
    /// Rate at which the active solution is recorded, Hz.
    pub state_hz: f64,
    pub retention_days: f64,
    pub max_size_mb: f64,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            path: String::new(),
            state_hz: 1.0,
            retention_days: 30.0,
            max_size_mb: 2048.0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Engine {
    pub tick_hz: f64,
    pub track_past_rev: f64,
    pub track_future_rev: f64,
    pub track_samples: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VehicleDef {
    pub id: String,
    pub name: String,
    pub norad: u32,
    /// Trajectory sources in priority order (DESIGN.md §3.2).
    pub sources: Vec<SourceKind>,
    /// Telemetry source for the GNC/EPS/TCS/COMM pages, if the vehicle has one (`"isslive"`).
    #[serde(default)]
    pub telemetry: Option<String>,
    /// Epoch for mission elapsed time in the header, UTC `YYYY-MM-DDThh:mm:ss`.
    #[serde(default)]
    pub met_epoch: Option<String>,
}

/// Merge `over` into `base`: tables merge key by key, anything else replaces.
fn merge(base: &mut toml::Value, over: toml::Value) {
    match (base, over) {
        (toml::Value::Table(b), toml::Value::Table(o)) => {
            for (k, v) in o {
                match b.get_mut(&k) {
                    Some(slot) => merge(slot, v),
                    None => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (slot, v) => *slot = v,
    }
}

impl Config {
    /// Parse `defaults` with an optional override document applied on top.
    pub fn from_layers(defaults: &str, user: Option<(&str, &str)>) -> Result<Self, ConfigError> {
        let mut value: toml::Value =
            toml::from_str(defaults).map_err(|source| ConfigError::Parse {
                origin: "embedded defaults".into(),
                source,
            })?;
        if let Some((origin, text)) = user {
            let over: toml::Value = toml::from_str(text).map_err(|source| ConfigError::Parse {
                origin: origin.to_owned(),
                source,
            })?;
            merge(&mut value, over);
        }
        let cfg: Config = value.try_into().map_err(|source| ConfigError::Parse {
            origin: user.map_or("embedded defaults", |u| u.0).to_owned(),
            source,
        })?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Embedded defaults plus `$XDG_CONFIG_HOME/mctl/config.toml` if it exists.
    pub fn load() -> Result<Self, ConfigError> {
        let path = user_config_path();
        let text = match &path {
            Some(p) if p.exists() => Some((
                p.display().to_string(),
                std::fs::read_to_string(p).map_err(|source| ConfigError::Io {
                    path: p.clone(),
                    source,
                })?,
            )),
            _ => None,
        };
        Self::from_layers(
            DEFAULT_CONFIG,
            text.as_ref().map(|(o, t)| (o.as_str(), t.as_str())),
        )
    }

    fn validate(&self) -> Result<(), ConfigError> {
        let bad = |m: String| Err(ConfigError::Invalid(m));
        let o = &self.observer;
        if !(0.0..=89.0).contains(&o.mask_deg)
            || !(o.pass_days > 0.0 && o.pass_days <= 14.0)
            || !(o.step_s >= 1.0 && o.step_s <= 120.0)
        {
            return bad(
                "observer: mask_deg must be 0..89, pass_days in (0, 14], step_s 1..120".into(),
            );
        }
        if let Err(m) = o.fallback.validate() {
            return bad(m);
        }
        for (i, s) in self.sites.iter().enumerate() {
            if let Err(m) = s.validate() {
                return bad(m);
            }
            if self.sites[..i]
                .iter()
                .any(|x| x.name.eq_ignore_ascii_case(&s.name))
            {
                return bad(format!("duplicate site {:?}", s.name));
            }
        }
        if !o.active.is_empty()
            && !self
                .sites
                .iter()
                .any(|s| s.name.eq_ignore_ascii_case(&o.active))
        {
            return bad(format!(
                "observer.active {:?} is not a configured [[site]]",
                o.active
            ));
        }
        let s = &self.store;
        if !(s.state_hz > 0.0
            && s.state_hz <= 50.0
            && s.retention_days > 0.0
            && s.max_size_mb > 0.0)
        {
            return bad(
                "store: state_hz must be in (0, 50], retention_days and max_size_mb > 0".into(),
            );
        }
        for (i, r) in self.rules.iter().enumerate() {
            if let Err(m) = r.validate() {
                return bad(m);
            }
            if self.rules[..i].iter().any(|o| o.id == r.id) {
                return bad(format!("duplicate rule id {:?}", r.id));
            }
            use crate::cw::Metric;
            let vehicle = match &r.metric {
                Metric::GpAgeHours(v) | Metric::ResidualKm(v) => Some(v),
                _ => None,
            };
            if let Some(v) = vehicle.filter(|v| !self.vehicles.iter().any(|d| &d.id == *v)) {
                return bad(format!("rule {}: unknown vehicle {v:?}", r.id));
            }
        }
        let f = &self.fetch;
        if !(f.timeout_s > 0.0
            && f.backoff_initial_s > 0.0
            && f.backoff_max_s >= f.backoff_initial_s)
        {
            return bad("fetch: timeout_s and backoff_initial_s must be > 0 and backoff_max_s >= backoff_initial_s".into());
        }
        let s = &self.sources;
        if !s.celestrak_gp.url_template.contains("{norad}")
            || s.celestrak_gp.url_template.contains("FORMAT=CSV")
        {
            return bad(
                "sources.celestrak_gp.url_template must contain {norad} and request JSON".into(),
            );
        }
        if s.celestrak_gp.min_refresh_s < 60.0
            || s.nasa_oem.min_refresh_s < 60.0
            || s.iers_eop.min_refresh_s < 3600.0
        {
            return bad(
                "source min_refresh_s below the polite minimum (60 s, 60 s, 3600 s)".into(),
            );
        }
        if s.iers_eop.urls.is_empty() {
            return bad("sources.iers_eop.urls is empty".into());
        }
        if !(self.limits.residual_caution_km > 0.0
            && self.limits.residual_warning_km >= self.limits.residual_caution_km)
        {
            return bad("limits: residual_warning_km must be >= residual_caution_km > 0".into());
        }
        let e = &self.engine;
        if !(1.0..=500.0).contains(&e.tick_hz)
            || e.track_samples < 16
            || e.track_past_rev < 0.0
            || e.track_future_rev <= 0.0
        {
            return bad("engine: tick_hz must be 1..=500, track_samples >= 16, track_past_rev >= 0, track_future_rev > 0".into());
        }
        if self.vehicles.is_empty() {
            return bad("no [[vehicle]] entries".into());
        }
        let mut seen = std::collections::HashSet::new();
        for v in &self.vehicles {
            if !seen.insert(v.id.as_str()) {
                return bad(format!("duplicate vehicle id {:?}", v.id));
            }
            if v.sources.is_empty() {
                return bad(format!("vehicle {:?} has no sources", v.id));
            }
            if let Some(m) = v
                .met_epoch
                .as_ref()
                .filter(|m| crate::time::Utc::parse(m).is_err())
            {
                return bad(format!(
                    "vehicle {:?}: met_epoch {m:?} is not YYYY-MM-DDThh:mm:ss",
                    v.id
                ));
            }
            if v.telemetry.as_deref().is_some_and(|t| t != "isslive") {
                return bad(format!(
                    "vehicle {:?}: unknown telemetry source {:?} (known: isslive)",
                    v.id, v.telemetry
                ));
            }
            if v.sources
                .iter()
                .enumerate()
                .any(|(i, k)| v.sources[..i].contains(k))
            {
                return bad(format!("vehicle {:?} lists a source twice", v.id));
            }
        }
        if self
            .vehicles
            .iter()
            .filter(|v| v.sources.contains(&SourceKind::Oem))
            .count()
            > 1
        {
            return bad("only one vehicle may use the oem source (the configured OEM URL is for a single object)".into());
        }
        Ok(())
    }
}

/// `$XDG_CONFIG_HOME/mctl/config.toml`, if a home directory can be determined.
pub fn user_config_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "mctl").map(|d| d.config_dir().join("config.toml"))
}

/// `$XDG_CACHE_HOME/mctl`, if a home directory can be determined.
pub fn cache_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "mctl").map(|d| d.cache_dir().to_path_buf())
}

/// `$XDG_DATA_HOME/mctl` (the recording database).
pub fn data_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "mctl").map(|d| d.data_dir().to_path_buf())
}

/// `$XDG_STATE_HOME/mctl` (logs), falling back to the cache directory.
pub fn state_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "mctl")
        .map(|d| d.state_dir().unwrap_or_else(|| d.cache_dir()).to_path_buf())
}

/// Create `dir` (and parents) if needed.
pub fn ensure_dir(dir: &Path) -> Result<(), ConfigError> {
    std::fs::create_dir_all(dir).map_err(|source| ConfigError::Io {
        path: dir.to_owned(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_defaults_are_valid() {
        let c = Config::from_layers(DEFAULT_CONFIG, None).unwrap();
        assert_eq!(c.vehicles[0].id, "iss");
        assert!(c.vehicles[0].sources == [SourceKind::Oem, SourceKind::Gp]);
    }

    #[test]
    fn user_layer_overrides_scalars_and_replaces_arrays() {
        let c = Config::from_layers(DEFAULT_CONFIG, Some(("u", "[limits]\nresidual_caution_km = 9.0\n[[vehicle]]\nid=\"x\"\nname=\"X\"\nnorad=1\nsources=[\"gp\"]\n[[rule]]\nid=\"r\"\ntitle=\"R\"\nmetric=\"gp_age_h:x\"\ndirection=\"above\"\ncaution=1.0\nunits=\"h\"\nbasis=\"test\"\n"))).unwrap();
        assert_eq!(c.limits.residual_caution_km, 9.0);
        assert_eq!(
            c.limits.residual_warning_km, 20.0,
            "untouched keys keep defaults"
        );
        assert!(c.vehicles.iter().any(|v| v.id == "x"));
    }

    #[test]
    fn a_vehicle_overlay_that_orphans_a_rule_is_an_error_not_a_silent_no_data() {
        let e = Config::from_layers(
            DEFAULT_CONFIG,
            Some((
                "u",
                "[[vehicle]]\nid=\"x\"\nname=\"X\"\nnorad=1\nsources=[\"gp\"]\n",
            )),
        )
        .unwrap_err();
        assert!(e.to_string().contains("unknown vehicle"), "{e}");
    }

    #[test]
    fn default_rules_are_valid_and_state_their_basis() {
        let c = Config::from_layers(DEFAULT_CONFIG, None).unwrap();
        assert!(c.rules.len() >= 6);
        assert!(c.rules.iter().all(|r| !r.basis.is_empty()));
        assert!(
            c.rules
                .iter()
                .any(|r| r.id == "iss-cabin-pressure" && r.basis.contains("illustrative"))
        );
    }

    #[test]
    fn invalid_values_are_loud() {
        for (patch, needle) in [
            (
                "[limits]\nresidual_caution_km = 50.0\n",
                "residual_warning_km",
            ),
            ("[engine]\ntick_hz = 0\n", "tick_hz"),
            ("[fetch]\ntimeout_s = -1\n", "timeout_s"),
            (
                "[sources.celestrak_gp]\nmin_refresh_s = 1\n",
                "polite minimum",
            ),
            (
                "[sources.celestrak_gp]\nurl_template = \"https://x/gp.php?FORMAT=CSV\"\n",
                "{norad}",
            ),
            ("[bogus]\nx=1\n", "bogus"),
        ] {
            let e = Config::from_layers(DEFAULT_CONFIG, Some(("u.toml", patch)))
                .unwrap_err()
                .to_string();
            assert!(e.contains(needle), "{patch} -> {e}");
        }
    }

    #[test]
    fn duplicate_and_double_oem_vehicles_are_rejected() {
        let dup = "[[vehicle]]\nid=\"iss\"\nname=\"A\"\nnorad=1\nsources=[\"gp\"]\n";
        let c = DEFAULT_CONFIG.to_owned() + dup;
        assert!(
            Config::from_layers(&c, None)
                .unwrap_err()
                .to_string()
                .contains("duplicate")
        );
        let oem2 = "[[vehicle]]\nid=\"b\"\nname=\"B\"\nnorad=2\nsources=[\"oem\"]\n";
        let c = DEFAULT_CONFIG.to_owned() + oem2;
        assert!(
            Config::from_layers(&c, None)
                .unwrap_err()
                .to_string()
                .contains("only one vehicle")
        );
    }
}
