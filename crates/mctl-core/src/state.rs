//! Shared state types: vectors, quality labels, and the derived quantities the console displays.

use nalgebra::Vector3;
use serde::{Deserialize, Serialize};

/// Position (km) and velocity (km/s) in whichever frame the field name says.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub r: Vector3<f64>,
    pub v: Vector3<f64>,
}

impl State {
    pub fn new(r: [f64; 3], v: [f64; 3]) -> Self {
        Self {
            r: Vector3::from(r),
            v: Vector3::from(v),
        }
    }
}

/// Where a trajectory came from. Order of declaration is not priority; the vehicle config sets that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    Oem,
    /// CelesTrak supplemental GP: operator-derived element segments fitted forward in time.
    #[serde(rename = "supgp")]
    SupGp,
    Gp,
}

impl SourceKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Oem => "OEM",
            Self::SupGp => "SUP-GP",
            Self::Gp => "GP/SGP4",
        }
    }
}

/// Data-quality label carried by every displayed value (FR-6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Quality {
    Live,
    Pred,
    Sim,
    Stale,
    NoData,
}

impl Quality {
    pub fn label(self) -> &'static str {
        match self {
            Self::Live => "LIVE",
            Self::Pred => "PRED",
            Self::Sim => "SIM",
            Self::Stale => "STALE",
            Self::NoData => "NO DATA",
        }
    }
}

/// Classical osculating elements and derived orbit quantities. Angles in degrees, lengths in km.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Elements {
    pub sma_km: f64,
    pub ecc: f64,
    pub inc_deg: f64,
    pub raan_deg: f64,
    pub argp_deg: f64,
    pub true_anom_deg: f64,
    pub mean_anom_deg: f64,
    pub period_s: f64,
    /// Apoapsis and periapsis altitudes above the equatorial radius, km.
    pub apo_alt_km: f64,
    pub peri_alt_km: f64,
}

/// Geodetic position on WGS-84.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Geodetic {
    pub lat_deg: f64,
    pub lon_deg: f64,
    pub alt_km: f64,
}
