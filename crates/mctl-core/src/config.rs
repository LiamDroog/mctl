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
    #[serde(rename = "vehicle")]
    pub vehicles: Vec<VehicleDef>,
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
        let c = Config::from_layers(DEFAULT_CONFIG, Some(("u", "[limits]\nresidual_caution_km = 9.0\n[[vehicle]]\nid=\"x\"\nname=\"X\"\nnorad=1\nsources=[\"gp\"]\n"))).unwrap();
        assert_eq!(c.limits.residual_caution_km, 9.0);
        assert_eq!(
            c.limits.residual_warning_km, 20.0,
            "untouched keys keep defaults"
        );
        assert!(c.vehicles.iter().any(|v| v.id == "x"));
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
