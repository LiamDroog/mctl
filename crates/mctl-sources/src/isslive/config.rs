//! Spike configuration, loaded from TOML. Invalid values are rejected with the field named.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

/// Default configuration shipped in the binary.
pub const DEFAULT_CONFIG: &str = include_str!("../../../../config/isslive.toml");

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub server: Server,
    pub run: Run,
    pub fields: Fields,
    pub time_item: TimeItem,
    #[serde(rename = "item")]
    pub items: Vec<Item>,
    /// Groups of four item IDs forming a quaternion, used for a unit-norm sanity check.
    #[serde(rename = "quaternion", default)]
    pub quaternions: Vec<Quaternion>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Server {
    pub url: String,
    pub adapter_set: String,
    /// Data adapter inside the adapter set; omit for the set's DEFAULT (ISSLIVE).
    #[serde(default)]
    pub data_adapter: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub duration_s: f64,
    pub connect_timeout_s: f64,
    pub setup_timeout_s: f64,
    /// Silence longer than keep-alive × this is treated as a dead feed.
    pub stall_factor: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fields {
    pub data: Vec<String>,
    pub time: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimeItem {
    pub id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Item {
    pub id: String,
    pub group: String,
    /// Documentation text from ISS Mimic's spreadsheet; not verified.
    pub description: String,
    /// Short display name; the description when absent.
    #[serde(default)]
    pub label: Option<String>,
    /// Display layout: a titled section, and a row (and column, for matrices) within it.
    #[serde(default)]
    pub section: Option<String>,
    #[serde(default)]
    pub row: Option<String>,
    #[serde(default)]
    pub col: Option<String>,
    pub units: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Quaternion {
    pub name: String,
    pub items: [String; 4],
}

impl Config {
    pub fn parse(source: &str, text: &str) -> Result<Self> {
        let cfg: Config =
            toml::from_str(text).with_context(|| format!("parsing config {source}"))?;
        cfg.validate()
            .with_context(|| format!("validating config {source}"))?;
        Ok(cfg)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&path.display().to_string(), &text)
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            self.server.url.starts_with("wss://"),
            "server.url must be wss:// (plain ws:// is refused)"
        );
        ensure!(
            !self.server.adapter_set.is_empty(),
            "server.adapter_set is empty"
        );
        let r = &self.run;
        ensure!(
            r.duration_s > 0.0 && r.connect_timeout_s > 0.0 && r.setup_timeout_s > 0.0,
            "run.* durations must be > 0"
        );
        ensure!(r.stall_factor >= 1.5, "run.stall_factor must be >= 1.5");
        ensure!(!self.items.is_empty(), "no [[item]] entries");
        for list in [&self.fields.data, &self.fields.time] {
            ensure!(
                !list.is_empty() && list.iter().all(|f| !f.is_empty() && !f.contains(' ')),
                "field names must be non-empty and contain no spaces"
            );
        }
        let mut seen = HashSet::new();
        for id in self.items.iter().map(|i| &i.id).chain([&self.time_item.id]) {
            if id.is_empty() || id.contains(' ') {
                bail!("item id {id:?} must be non-empty and contain no spaces");
            }
            ensure!(seen.insert(id.as_str()), "duplicate item id {id}");
        }
        for q in &self.quaternions {
            for id in &q.items {
                ensure!(
                    self.items.iter().any(|i| &i.id == id),
                    "quaternion {:?} uses {id}, which is not a configured item",
                    q.name
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_config_is_valid() {
        let c = Config::parse("default", DEFAULT_CONFIG).unwrap();
        assert_eq!(c.server.adapter_set, "ISSLIVE");
        assert!(c.items.len() >= 10);
    }

    #[test]
    fn rejects_plaintext_server_and_duplicates() {
        let bad = DEFAULT_CONFIG.replace("wss://", "ws://");
        assert!(
            Config::parse("t", &bad)
                .unwrap_err()
                .to_string()
                .contains("validating")
        );
        let dup = format!(
            "{DEFAULT_CONFIG}\n[[item]]\nid = \"USLAB000018\"\ngroup = \"x\"\ndescription = \"x\"\nunits = \"x\"\n"
        );
        assert!(format!("{:#}", Config::parse("t", &dup).unwrap_err()).contains("duplicate"));
    }

    #[test]
    fn rejects_unknown_keys() {
        let bad = format!("{DEFAULT_CONFIG}\n[bogus]\nx = 1\n");
        assert!(Config::parse("t", &bad).is_err());
    }
}
