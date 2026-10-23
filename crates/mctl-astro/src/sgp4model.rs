//! SGP4 from an OMM JSON record, in AFSPC compatibility mode (WGS-72), with a decay guard.
//!
//! `Constants::from_elements` would use WGS-84, which costs ~68 m per day for the ISS and fails the
//! Vallado verification vectors (ADR 0001); CelesTrak and Space-Track elements are fit with WGS-72.
//! The crate also returns states inside the Earth for decayed objects instead of an error, so the
//! radius is checked here.

use std::sync::Arc;

use mctl_core::state::State;
use mctl_core::time::Utc;
use thiserror::Error;

use crate::sun::EARTH_RADIUS_KM;
use crate::trajectory::TrajectoryError;

#[derive(Debug, Error)]
pub enum GpError {
    #[error("OMM JSON: {0}")]
    Json(String),
    #[error("OMM JSON contains no records")]
    Empty,
    #[error("OMM record has no valid EPOCH: {0}")]
    Epoch(String),
    #[error("SGP4 initialisation failed: {0}")]
    Init(String),
}

/// An initialised SGP4 model. Cheap to clone (shared constants).
#[derive(Clone)]
pub struct GpModel {
    constants: Arc<sgp4::Constants>,
    pub epoch: Utc,
    pub name: String,
    pub norad: u32,
}

impl std::fmt::Debug for GpModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpModel")
            .field("epoch", &self.epoch.iso())
            .field("name", &self.name)
            .field("norad", &self.norad)
            .finish()
    }
}

impl GpModel {
    pub fn from_omm_json(json: &str) -> Result<Self, GpError> {
        let value: serde_json::Value =
            serde_json::from_str(json).map_err(|e| GpError::Json(e.to_string()))?;
        let epoch_text = value
            .get(0)
            .and_then(|r| r["EPOCH"].as_str())
            .ok_or(GpError::Empty)?
            .to_owned();
        let epoch = Utc::parse(&epoch_text).map_err(|e| GpError::Epoch(e.to_string()))?;
        let records: Vec<sgp4::Elements> =
            serde_json::from_value(value.clone()).map_err(|e| GpError::Json(e.to_string()))?;
        let el = records.first().ok_or(GpError::Empty)?;
        let constants = sgp4::Constants::from_elements_afspc_compatibility_mode(el)
            .map_err(|e| GpError::Init(e.to_string()))?;
        Ok(Self {
            constants: Arc::new(constants),
            epoch,
            name: value[0]["OBJECT_NAME"].as_str().unwrap_or("").to_owned(),
            norad: value[0]["NORAD_CAT_ID"].as_u64().unwrap_or(0) as u32,
        })
    }

    /// TEME state at `t` (UTC minutes since epoch, as Vallado's reference does).
    pub fn state_teme(&self, t: Utc) -> Result<State, TrajectoryError> {
        let p = self
            .constants
            .propagate_afspc_compatibility_mode(sgp4::MinutesSinceEpoch(t.minus(self.epoch) / 60.0))
            .map_err(|e| TrajectoryError::Failed(e.to_string()))?;
        let s = State::new(p.position, p.velocity);
        let r = s.r.norm();
        if !r.is_finite() || r < EARTH_RADIUS_KM {
            return Err(TrajectoryError::Decayed(r));
        }
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trajectory::Trajectory;

    const OMM: &str = include_str!("../fixtures/iss-omm.json");

    #[test]
    fn iss_omm_propagates_to_a_plausible_orbit() {
        let m = GpModel::from_omm_json(OMM).unwrap();
        assert_eq!(m.norad, 25544);
        let s = m.state_gcrf(m.epoch.plus(3600.0)).unwrap();
        let alt = s.r.norm() - EARTH_RADIUS_KM;
        assert!((380.0..450.0).contains(&alt), "altitude {alt}");
        assert!((s.v.norm() - 7.66).abs() < 0.1);
    }

    #[test]
    fn reproduces_the_vallado_reference_state_for_satellite_00005() {
        // TLE 1 00005U 58002B 00179.78495062 ... : the first case of the AIAA 2006-6753 verification
        // set, whose published TEME state at epoch is (7022.46529266, -1400.08296755, 0.03995155) km.
        let json = r#"[{"OBJECT_NAME":"VANGUARD 1","OBJECT_ID":"1958-002B","EPOCH":"2000-06-27T18:50:19.733568","MEAN_MOTION":10.82419157,"ECCENTRICITY":0.1859667,"INCLINATION":34.2682,"RA_OF_ASC_NODE":348.7242,"ARG_OF_PERICENTER":331.7664,"MEAN_ANOMALY":19.3264,"EPHEMERIS_TYPE":0,"CLASSIFICATION_TYPE":"U","NORAD_CAT_ID":5,"ELEMENT_SET_NO":475,"REV_AT_EPOCH":41366,"BSTAR":2.8098e-5,"MEAN_MOTION_DOT":2.3e-7,"MEAN_MOTION_DDOT":0.0}]"#;
        let m = GpModel::from_omm_json(json).unwrap();
        let s = m.state_teme(m.epoch).unwrap();
        let want = nalgebra::Vector3::new(7022.46529266, -1400.08296755, 0.03995155);
        assert!(
            (s.r - want).norm() < 1e-5,
            "position error {} km",
            (s.r - want).norm()
        );
        let want_v = nalgebra::Vector3::new(1.893841015, 6.405893759, 4.534807250);
        assert!((s.v - want_v).norm() < 1e-7);
    }

    #[test]
    fn bad_input_is_an_error_not_a_panic() {
        assert!(matches!(
            GpModel::from_omm_json("not json"),
            Err(GpError::Json(_))
        ));
        assert!(matches!(GpModel::from_omm_json("[]"), Err(GpError::Empty)));
        assert!(matches!(
            GpModel::from_omm_json(r#"[{"EPOCH":"garbage"}]"#),
            Err(GpError::Epoch(_))
        ));
        assert!(
            matches!(
                GpModel::from_omm_json(r#"[{"EPOCH":"2026-01-01T00:00:00"}]"#),
                Err(GpError::Json(_))
            ),
            "missing orbital elements"
        );
    }

    #[test]
    fn a_decayed_object_is_an_error() {
        // Very low, high-drag orbit far past epoch: the crate returns a state under the surface; we refuse it.
        let json = r#"[{"OBJECT_NAME":"X","OBJECT_ID":"2000-001A","EPOCH":"2026-01-01T00:00:00.000000","MEAN_MOTION":16.2,"ECCENTRICITY":0.001,"INCLINATION":51.6,"RA_OF_ASC_NODE":10.0,"ARG_OF_PERICENTER":10.0,"MEAN_ANOMALY":10.0,"EPHEMERIS_TYPE":0,"CLASSIFICATION_TYPE":"U","NORAD_CAT_ID":99999,"ELEMENT_SET_NO":999,"REV_AT_EPOCH":1,"BSTAR":0.05,"MEAN_MOTION_DOT":0.01,"MEAN_MOTION_DDOT":0.0}]"#;
        let m = GpModel::from_omm_json(json).unwrap();
        let outcome = (1..400)
            .map(|d| m.state_teme(m.epoch.plus(d as f64 * 86_400.0)))
            .find(|r| r.is_err());
        assert!(
            outcome.is_some(),
            "an object with this drag must fail within 400 days"
        );
    }
}
