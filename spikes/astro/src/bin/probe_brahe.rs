//! Footprint probe: brahe doing SGP4 from OMM, a frame transform and a numerical propagation.
use brahe::propagators::traits::DStatePropagator;
use brahe::propagators::{
    DNumericalOrbitPropagator, ForceModelConfig, NumericalPropagationConfig, SGPPropagator,
};
use brahe::time::{Epoch, TimeSystem};
use brahe::utils::DStateProvider;

fn main() -> anyhow::Result<()> {
    let json = std::fs::read_to_string(
        std::env::args()
            .nth(1)
            .ok_or_else(|| anyhow::anyhow!("usage: probe <omm.json>"))?,
    )?;
    brahe::eop::set_global_eop_provider(brahe::eop::StaticEOPProvider::from_zero());
    let rec: Vec<brahe::types::GPRecord> = serde_json::from_str(&json)?;
    let prop = SGPPropagator::from_gp_record(&rec[0], 60.0)?;
    let e = Epoch::from_datetime(2026, 10, 23, 0, 0, 0.0, 0.0, TimeSystem::UTC);
    let x = brahe::frames::state_teme_to_gcrf(e, prop.state_pef(e)?);
    let mut n = DNumericalOrbitPropagator::new(
        e,
        nalgebra::DVector::from_row_slice(x.as_slice()),
        NumericalPropagationConfig::default(),
        ForceModelConfig::two_body_gravity(),
        None,
        None,
        None,
        None,
    )?;
    n.propagate_to(e + 3600.0)?;
    println!("{:.3}", n.state(e + 3600.0)?[0]);
    Ok(())
}
