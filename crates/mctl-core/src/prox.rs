//! Relative motion of a visiting vehicle about the station (DESIGN.md section 9.6, F8), published by the
//! engine for the PROX page.

use std::sync::Arc;

use crate::time::Utc;

/// A relative position in the target's LVLH frame at one instant: `x` forward along the velocity
/// (V-bar), `y` along the negative orbit normal, `z` toward the Earth (R-bar), km.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProxPoint {
    pub t: Utc,
    pub lvlh_km: [f64; 3],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxState {
    /// The two vehicles have the same published element set: CelesTrak gives a docked visitor the
    /// station's own elements, so there is no relative motion to show.
    Docked,
    /// Close enough for R-bar/V-bar to mean something, as far as these sources can say.
    Near,
    Far,
}

impl ProxState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Docked => "DOCKED",
            Self::Near => "NEAR",
            Self::Far => "FAR",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProxView {
    pub chaser: String,
    pub target: String,
    pub state: ProxState,
    pub rel_lvlh_km: [f64; 3],
    pub range_km: f64,
    pub range_rate_km_s: f64,
    /// Where the relative state came from, e.g. `GP/SGP4 both`, or a note that the sources differ.
    pub source_note: String,
    /// Both vehicles on ephemeris-grade sources (never true for TLE-derived visitors).
    pub ephemeris_grade: bool,
    pub history: Arc<Vec<ProxPoint>>,
    pub predicted: Arc<Vec<ProxPoint>>,
    /// Keep-out sphere radius and approach-ellipsoid semi-axes (along-track, radial), km: configured values.
    pub keep_out_km: f64,
    pub ellipsoid_semi_km: [f64; 2],
}
