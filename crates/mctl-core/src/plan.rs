//! The maneuver plan: a burn list and force-model choices, which the planner
//! turns into a [`PlanView`]. Plans are saved and loaded as TOML (FR-34).

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::state::Elements;
use crate::time::Utc;

/// Longest horizon the planner will propagate, seconds (at most 7 days).
pub const MAX_HORIZON_S: f64 = 7.0 * 86_400.0;
/// Most burns in one plan.
pub const MAX_BURNS: usize = 20;
/// A burn larger than this is almost certainly a typo, m/s.
pub const MAX_DV_M_S: f64 = 1000.0;

#[derive(Debug, Error, PartialEq)]
pub enum PlanError {
    #[error("{0}")]
    Invalid(String),
    #[error("plan file: {0}")]
    File(String),
}

fn bad<T>(s: impl Into<String>) -> Result<T, PlanError> {
    Err(PlanError::Invalid(s.into()))
}

/// When a burn happens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Trigger {
    Epoch(Utc),
    /// The next event of this kind after the plan's start.
    NextAscendingNode,
    NextDescendingNode,
    NextApoapsis,
    NextPeriapsis,
}

impl Trigger {
    /// `an`, `dn`, `apo`, `peri`, or an ISO epoch `YYYY-MM-DDThh:mm:ss`.
    pub fn parse(s: &str) -> Result<Self, PlanError> {
        match s.trim().to_ascii_lowercase().as_str() {
            "an" => Ok(Self::NextAscendingNode),
            "dn" => Ok(Self::NextDescendingNode),
            "apo" | "apoapsis" => Ok(Self::NextApoapsis),
            "peri" | "periapsis" => Ok(Self::NextPeriapsis),
            other => Utc::parse(&other.to_ascii_uppercase())
                .map(Self::Epoch)
                .or_else(|_| Utc::parse(s.trim()).map(Self::Epoch))
                .map_err(|_| {
                    PlanError::Invalid(format!(
                        "trigger {s:?}: use an, dn, apo, peri or an epoch such as 2026-10-25T12:00:00"
                    ))
                }),
        }
    }

    pub fn text(&self) -> String {
        match self {
            Self::Epoch(t) => t.iso()[..19].to_owned(),
            Self::NextAscendingNode => "an".into(),
            Self::NextDescendingNode => "dn".into(),
            Self::NextApoapsis => "apo".into(),
            Self::NextPeriapsis => "peri".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BurnFrame {
    /// Velocity, orbit normal, binormal.
    Vnb,
    /// Radial, along-track, orbit normal.
    Rtn,
}

impl BurnFrame {
    pub fn parse(s: &str) -> Result<Self, PlanError> {
        match s.trim().to_ascii_lowercase().as_str() {
            "vnb" => Ok(Self::Vnb),
            "rtn" => Ok(Self::Rtn),
            _ => bad(format!("frame {s:?}: use vnb or rtn")),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Vnb => "VNB",
            Self::Rtn => "RTN",
        }
    }
}

/// One impulsive burn as the operator wrote it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BurnDef {
    /// `an`, `dn`, `apo`, `peri` or an epoch.
    pub trigger: String,
    pub frame: BurnFrame,
    /// Delta-v components in the frame, m/s.
    pub dv_m_s: [f64; 3],
}

impl BurnDef {
    pub fn validate(&self) -> Result<(), PlanError> {
        Trigger::parse(&self.trigger)?;
        let mag = self.dv_m_s.iter().map(|c| c * c).sum::<f64>().sqrt();
        if !self.dv_m_s.iter().all(|c| c.is_finite()) || mag > MAX_DV_M_S {
            return bad(format!(
                "delta-v {:?} m/s is not finite or exceeds {MAX_DV_M_S} m/s",
                self.dv_m_s
            ));
        }
        Ok(())
    }

    pub fn dv_mag_m_s(&self) -> f64 {
        self.dv_m_s.iter().map(|c| c * c).sum::<f64>().sqrt()
    }
}

fn default_horizon() -> f64 {
    3.0
}
fn default_true() -> bool {
    true
}
fn default_one() -> f64 {
    1.0
}

/// The plan an operator edits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanDef {
    #[serde(default, rename = "burn")]
    pub burns: Vec<BurnDef>,
    /// Horizon in revolutions (capped at 7 days).
    #[serde(default = "default_horizon")]
    pub horizon_revs: f64,
    #[serde(default = "default_true")]
    pub zonal: bool,
    #[serde(default = "default_true")]
    pub sun: bool,
    #[serde(default = "default_true")]
    pub moon: bool,
    #[serde(default = "default_true")]
    pub drag: bool,
    #[serde(default)]
    pub srp: bool,
    /// Multiplier on the ballistic coefficient (the drag model is order-of-magnitude).
    #[serde(default = "default_one")]
    pub drag_scale: f64,
}

impl Default for PlanDef {
    fn default() -> Self {
        Self {
            burns: Vec::new(),
            horizon_revs: default_horizon(),
            zonal: true,
            sun: true,
            moon: true,
            drag: true,
            srp: false,
            drag_scale: 1.0,
        }
    }
}

impl PlanDef {
    pub fn validate(&self) -> Result<(), PlanError> {
        if self.burns.len() > MAX_BURNS {
            return bad(format!("more than {MAX_BURNS} burns"));
        }
        if !(self.horizon_revs > 0.0 && self.horizon_revs <= 200.0) {
            return bad(format!(
                "horizon {} revolutions is not in (0, 200]",
                self.horizon_revs
            ));
        }
        if !(self.drag_scale > 0.0 && self.drag_scale <= 100.0) {
            return bad(format!("drag scale {} is not in (0, 100]", self.drag_scale));
        }
        self.burns.iter().try_for_each(BurnDef::validate)
    }

    pub fn to_toml(&self) -> Result<String, PlanError> {
        toml::to_string_pretty(self).map_err(|e| PlanError::File(e.to_string()))
    }

    pub fn from_toml(text: &str) -> Result<Self, PlanError> {
        let p: Self = toml::from_str(text).map_err(|e| PlanError::File(e.to_string()))?;
        p.validate()?;
        Ok(p)
    }
}

/// A maneuver mentioned in the ISS OEM's comments: read-only, shown for reference (FR-33). The comment
/// format is not specified, so everything but the raw text is best effort.
#[derive(Debug, Clone, PartialEq)]
pub struct ReferenceBurn {
    pub raw: String,
    pub epoch: Option<Utc>,
    /// Magnitude, m/s, when the comment gave one with a recognised unit.
    pub dv_m_s: Option<f64>,
}

/// A burn with its trigger resolved to an epoch.
#[derive(Debug, Clone, PartialEq)]
pub struct BurnView {
    pub trigger: String,
    pub t: Utc,
    pub frame: BurnFrame,
    pub dv_m_s: [f64; 3],
    pub dv_mag_m_s: f64,
    /// Read-only reference burn from the ISS OEM rather than one the operator defined.
    pub reference: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlanPoint {
    pub t: Utc,
    pub r_gcrf_km: [f64; 3],
    pub lat_deg: f64,
    pub lon_deg: f64,
    pub alt_km: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlanReport {
    pub nominal: Elements,
    pub planned: Elements,
    /// Orbital period change, seconds.
    pub d_period_s: f64,
    /// Where the planned vehicle is relative to the nominal one at `phase_epoch`, degrees along the
    /// nominal orbit (positive = ahead).
    pub d_phase_deg: f64,
    pub phase_epoch: Utc,
    /// Separation at `phase_epoch`, km.
    pub separation_km: f64,
}

/// First pass over a site with and without the burns.
#[derive(Debug, Clone, PartialEq)]
pub struct PassShift {
    pub site: String,
    pub nominal_aos: Option<Utc>,
    pub planned_aos: Option<Utc>,
    pub nominal_max_el_deg: Option<f64>,
    pub planned_max_el_deg: Option<f64>,
}

impl PassShift {
    pub fn shift_s(&self) -> Option<f64> {
        Some(self.planned_aos?.minus(self.nominal_aos?))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlanStatus {
    /// No burns: nothing to compute.
    Empty,
    Computing,
    Ready,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlanView {
    pub def: PlanDef,
    pub status: PlanStatus,
    pub t0: Utc,
    pub t_end: Utc,
    pub burns: Vec<BurnView>,
    pub nominal: Arc<Vec<PlanPoint>>,
    pub planned: Arc<Vec<PlanPoint>>,
    pub report: Option<PlanReport>,
    /// Distance between the planned and nominal vehicle over time, km.
    pub separation_km: Arc<Vec<(Utc, f64)>>,
    pub passes: Vec<PassShift>,
    /// Things the operator should know about the result (a force switched off, a burn dropped).
    pub notes: Vec<String>,
}

/// Edits the console can make to the plan.
#[derive(Debug, Clone, PartialEq)]
pub enum PlanCommand {
    AddBurn(BurnDef),
    DelBurn(usize),
    Clear,
    Horizon(f64),
    Force { name: String, on: bool },
    DragScale(f64),
    Save(String),
    Load(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triggers_parse_names_and_epochs() {
        assert_eq!(Trigger::parse("APO").unwrap(), Trigger::NextApoapsis);
        assert_eq!(Trigger::parse(" an ").unwrap(), Trigger::NextAscendingNode);
        let t = Trigger::parse("2026-10-25T12:00:00").unwrap();
        assert_eq!(
            t,
            Trigger::Epoch(Utc::parse("2026-10-25T12:00:00").unwrap())
        );
        assert_eq!(t.text(), "2026-10-25T12:00:00");
        assert!(Trigger::parse("tomorrow").is_err());
    }

    #[test]
    fn burns_and_plans_are_validated() {
        let ok = BurnDef {
            trigger: "apo".into(),
            frame: BurnFrame::Vnb,
            dv_m_s: [1.0, 0.0, 0.0],
        };
        assert!(ok.validate().is_ok());
        assert!(
            BurnDef {
                dv_m_s: [5000.0, 0.0, 0.0],
                ..ok.clone()
            }
            .validate()
            .is_err()
        );
        assert!(
            BurnDef {
                dv_m_s: [f64::NAN, 0.0, 0.0],
                ..ok.clone()
            }
            .validate()
            .is_err()
        );
        assert!(
            BurnDef {
                trigger: "soon".into(),
                ..ok.clone()
            }
            .validate()
            .is_err()
        );
        let mut p = PlanDef {
            burns: vec![ok; 3],
            ..PlanDef::default()
        };
        assert!(p.validate().is_ok());
        p.horizon_revs = 0.0;
        assert!(p.validate().is_err());
        p.horizon_revs = 3.0;
        p.drag_scale = -1.0;
        assert!(p.validate().is_err());
    }

    #[test]
    fn plans_round_trip_through_toml_and_reject_bad_files() {
        let p = PlanDef {
            burns: vec![BurnDef {
                trigger: "2026-10-25T12:00:00".into(),
                frame: BurnFrame::Rtn,
                dv_m_s: [0.0, 2.5, 0.0],
            }],
            horizon_revs: 5.0,
            srp: true,
            drag_scale: 0.4,
            ..PlanDef::default()
        };
        let text = p.to_toml().unwrap();
        assert_eq!(PlanDef::from_toml(&text).unwrap(), p);
        assert!(PlanDef::from_toml("horizon_revs = -2").is_err());
        assert!(
            PlanDef::from_toml("[[burn]]\ntrigger = \"apo\"\nframe = \"xyz\"\ndv_m_s = [0,0,0]")
                .is_err()
        );
        assert_eq!(
            PlanDef::from_toml("").unwrap(),
            PlanDef::default(),
            "an empty file is the default plan"
        );
    }

    #[test]
    fn pass_shift_is_planned_minus_nominal() {
        let s = PassShift {
            site: "x".into(),
            nominal_aos: Some(Utc(100.0)),
            planned_aos: Some(Utc(160.0)),
            nominal_max_el_deg: None,
            planned_max_el_deg: None,
        };
        assert_eq!(s.shift_s(), Some(60.0));
        assert_eq!(
            PassShift {
                planned_aos: None,
                ..s
            }
            .shift_s(),
            None
        );
    }
}
