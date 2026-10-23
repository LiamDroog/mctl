//! Footprint probe: satkit doing SGP4 from OMM, a frame transform and a numerical propagation.
use satkit::{Duration, Frame, Instant};

fn main() -> anyhow::Result<()> {
    let json = std::fs::read_to_string(
        std::env::args()
            .nth(1)
            .ok_or_else(|| anyhow::anyhow!("usage: probe <omm.json>"))?,
    )?;
    let mut omm = satkit::omm::OMM::from_json_string(&json)?.remove(0);
    let t = Instant::from_datetime(2026, 10, 23, 0, 0, 0.0)?;
    let s = satkit::sgp4::sgp4(&mut omm, &[t])?;
    let (p, v) = (
        satkit::Vector3::from_slice(s.pos.col_slice(0)),
        satkit::Vector3::from_slice(s.vel.col_slice(0)),
    );
    let (p, _) = satkit::frametransform::transform_state(Frame::TEME, Frame::GCRF, &t, &p, &v)?;
    let pv = satkit::mathtypes::Vector6::from_array([p[0], p[1], p[2], 0.0, 7600.0, 0.0]);
    let res = satkit::orbitprop::propagate(
        &pv,
        &t,
        &(t + Duration::from_hours(1.0)),
        &satkit::orbitprop::PropSettings::default(),
        None,
    )?;
    println!("{:.3}", res.state_end[0]);
    Ok(())
}
