//! Footprint probe: the in-house stack (sgp4 + nalgebra) doing one SGP4 propagation and a rotation.
use nalgebra::{Rotation3, Vector3};

fn main() -> anyhow::Result<()> {
    let json = std::fs::read_to_string(
        std::env::args()
            .nth(1)
            .ok_or_else(|| anyhow::anyhow!("usage: probe <omm.json>"))?,
    )?;
    let el: Vec<sgp4::Elements> = serde_json::from_str(&json)?;
    let c = sgp4::Constants::from_elements_afspc_compatibility_mode(&el[0])?;
    let p = c.propagate_afspc_compatibility_mode(sgp4::MinutesSinceEpoch(90.0))?;
    let r = Rotation3::from_axis_angle(&Vector3::z_axis(), 0.1) * Vector3::from(p.position);
    println!("{:.3}", r.norm());
    Ok(())
}
