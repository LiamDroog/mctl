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
    /// Key bindings by action name (validated by the front end that owns the actions).
    pub keys: std::collections::BTreeMap<String, Vec<String>>,
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
    pub celestrak_supgp: GpSource,
    pub nasa_oem: OemSource,
    pub iers_eop: EopSource,
    pub horizons: HorizonsSource,
    pub schedules: ScheduleSources,
}

/// JPL Horizons vector tables (DESIGN.md gives Horizons as the source for non-ISS vehicles).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HorizonsSource {
    pub url: String,
    pub min_refresh_s: f64,
}

/// Public observing schedules published by STScI.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleSources {
    /// Page listing the weekly JWST schedule files.
    pub jwst_index_url: String,
    /// Scheme and host that the index page's relative links are resolved against.
    pub jwst_origin: String,
    /// Hubble daily status report list for one year; `{year}` is replaced.
    pub hst_index_template: String,
    pub hst_origin: String,
    pub min_refresh_s: f64,
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

/// IP geolocation (DESIGN.md §8 step 3): disabled by default; queries the configured HTTPS endpoint
/// once per session. The endpoint, its JSON field names and its accuracy are all configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IpLookup {
    pub enabled: bool,
    pub url: String,
    pub lat_key: String,
    pub lon_key: String,
    /// Typical accuracy of the service, km, shown as the site's uncertainty.
    pub accuracy_km: f64,
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
    /// Ask GeoClue2 when no site is configured as active (DESIGN.md §8 step 2).
    pub geoclue: bool,
    pub geoclue_timeout_s: f64,
    pub ip: IpLookup,
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
    /// NORAD catalogue number; 0 for a vehicle with none (it then cannot use the `gp` source).
    #[serde(default)]
    pub norad: u32,
    /// Where the vehicle is: `earth` orbit, or `l2`, the Sun-Earth L2 point (an Earth-centred
    /// Kepler orbit means nothing there, so those vehicles get a different display).
    #[serde(default)]
    pub regime: Regime,
    /// Request for the `horizons` source; required when that source is listed.
    #[serde(default)]
    pub horizons: Option<HorizonsDef>,
    /// Observing schedule shown on the OPS page: `jwst` or `hst`.
    #[serde(default)]
    pub schedule: Option<String>,
    /// Trajectory sources in priority order (DESIGN.md §3.2).
    pub sources: Vec<SourceKind>,
    /// Telemetry source for the GNC/EPS/TCS/COMM pages, if the vehicle has one (`"isslive"`).
    #[serde(default)]
    pub telemetry: Option<String>,
    /// Epoch for mission elapsed time in the header, UTC `YYYY-MM-DDThh:mm:ss`.
    #[serde(default)]
    pub met_epoch: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Regime {
    #[default]
    Earth,
    L2,
}

/// What to ask Horizons for one vehicle.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HorizonsDef {
    /// Horizons `COMMAND`, e.g. `-170`.
    pub id: String,
    pub step_s: f64,
    pub past_days: f64,
    pub future_days: f64,
    /// Data fetched longer ago than this is STALE.
    pub stale_after_s: f64,
}

impl HorizonsDef {
    /// Whether the request is well formed (NaN fails every comparison, so it is rejected).
    pub fn is_valid(&self) -> bool {
        let rows = (self.past_days + self.future_days) * 86_400.0 / self.step_s;
        !self.id.trim().is_empty()
            && self.step_s >= 60.0
            && self.step_s % 60.0 == 0.0
            && self.past_days >= 0.0
            && self.future_days > 0.0
            && self.stale_after_s > 0.0
            && rows <= 20_000.0
    }
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
        if o.geoclue_timeout_s <= 0.0
            || (o.ip.enabled
                && (!o.ip.url.starts_with("https://")
                    || o.ip.lat_key.is_empty()
                    || o.ip.lon_key.is_empty()
                    || o.ip.accuracy_km <= 0.0))
        {
            return bad("observer: geoclue_timeout_s > 0; an enabled ip lookup needs an https:// url, lat_key, lon_key and accuracy_km > 0".into());
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
        if !s.celestrak_supgp.url_template.contains("{norad}")
            || s.celestrak_supgp.min_refresh_s < 60.0
        {
            return bad(
                "sources.celestrak_supgp: url_template must contain {norad}; min_refresh_s >= 60"
                    .into(),
            );
        }
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
        if s.horizons.min_refresh_s < 600.0 || s.schedules.min_refresh_s < 600.0 {
            return bad("sources.horizons and sources.schedules: min_refresh_s >= 600".into());
        }
        if !s.schedules.hst_index_template.contains("{year}") {
            return bad("sources.schedules.hst_index_template must contain {year}".into());
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
            if v.sources.contains(&SourceKind::Gp) && v.norad == 0 {
                return bad(format!(
                    "vehicle {:?} uses gp but has no norad number",
                    v.id
                ));
            }
            if v.schedule
                .as_deref()
                .is_some_and(|k| !matches!(k, "jwst" | "hst"))
            {
                return bad(format!(
                    "vehicle {:?}: unknown schedule {:?} (known: jwst, hst)",
                    v.id, v.schedule
                ));
            }
            match (&v.horizons, v.sources.contains(&SourceKind::Horizons)) {
                (None, true) => {
                    return bad(format!(
                        "vehicle {:?} lists horizons but has no `horizons` table",
                        v.id
                    ));
                }
                (Some(h), _) if !h.is_valid() => {
                    return bad(format!(
                        "vehicle {:?}: horizons needs an id, step_s a multiple of 60, past_days >= 0, future_days > 0, stale_after_s > 0 and at most 20000 rows",
                        v.id
                    ));
                }
                _ => {}
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

/// Append a `[[site]]` entry to the user configuration file, creating it if needed. Refuses a name
/// already present there, and never rewrites existing text.
pub fn append_site(path: &Path, site: &SiteDef) -> Result<(), ConfigError> {
    site.validate().map_err(ConfigError::Invalid)?;
    let io = |source| ConfigError::Io {
        path: path.to_owned(),
        source,
    };
    let existing = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(io(e)),
    };
    if let Ok(v) = existing.parse::<toml::Table>()
        && v.get("site")
            .and_then(toml::Value::as_array)
            .is_some_and(|a| {
                a.iter().any(|s| {
                    s.get("name")
                        .and_then(toml::Value::as_str)
                        .is_some_and(|n| n.eq_ignore_ascii_case(&site.name))
                })
            })
    {
        return Err(ConfigError::Invalid(format!(
            "site {:?} is already in {}",
            site.name,
            path.display()
        )));
    }
    if let Some(dir) = path.parent() {
        ensure_dir(dir)?;
    }
    let mut out = existing;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    let name = site.name.replace('\\', "\\\\").replace('"', "\\\"");
    out += &format!(
        "\n[[site]]\nname = \"{name}\"\nlat_deg = {}\nlon_deg = {}\nalt_m = {}\n",
        site.lat_deg, site.lon_deg, site.alt_m
    );
    std::fs::write(path, out).map_err(io)
}

/// Create the user configuration from the embedded defaults on first run (DESIGN.md §11). Every
/// line is commented out, so the file overrides nothing and later changes to the defaults still
/// apply; uncomment a line to override it. Returns whether a file was written.
pub fn write_template_if_missing(path: &Path) -> Result<bool, ConfigError> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        ensure_dir(dir)?;
    }
    let mut out = String::from(
        "# mctl user configuration.\n# Everything below is the built-in default, commented out. Uncomment and edit a line to\n# override it; invalid values are reported with their reason at start-up.\n# A [[vehicle]], [[rule]] or [[site]] list you uncomment REPLACES the built-in list.\n\n",
    );
    for line in DEFAULT_CONFIG.lines() {
        if line.trim().is_empty() {
            out.push('\n');
        } else if line.starts_with('#') {
            out.push_str(line);
            out.push('\n');
        } else {
            out.push_str("# ");
            out.push_str(line);
            out.push('\n');
        }
    }
    std::fs::write(path, out).map_err(|source| ConfigError::Io {
        path: path.to_owned(),
        source,
    })?;
    Ok(true)
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

    #[test]
    fn the_first_run_template_overrides_nothing_and_is_written_once() {
        let dir = std::env::temp_dir().join(format!("mctl-template-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("config.toml");
        assert!(write_template_if_missing(&path).unwrap());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("# [fetch]") && text.contains("# theme = [\"t\"]"),
            "defaults are present, commented out"
        );
        let layered = Config::from_layers(DEFAULT_CONFIG, Some(("template", &text))).unwrap();
        let plain = Config::from_layers(DEFAULT_CONFIG, None).unwrap();
        assert_eq!(layered.vehicles.len(), plain.vehicles.len());
        assert_eq!(
            layered.limits.residual_caution_km,
            plain.limits.residual_caution_km
        );
        assert_eq!(layered.rules.len(), plain.rules.len());
        std::fs::write(
            &path,
            "[limits]\nresidual_caution_km = 9.0\nresidual_warning_km = 30.0\n",
        )
        .unwrap();
        assert!(
            !write_template_if_missing(&path).unwrap(),
            "an existing file is never touched"
        );
        assert!(std::fs::read_to_string(&path).unwrap().contains("30.0"));
        std::fs::remove_dir_all(dir).ok();
    }
}
