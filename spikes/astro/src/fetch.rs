//! Network fetches for the evaluation fixtures. Every failure mode is an error: timeouts, HTTP
//! status, empty bodies and bodies that are not what was asked for.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};

/// Per-request timeout, seconds.
const TIMEOUT_S: u64 = 30;
/// Largest body accepted, bytes (the OEM is ~0.7 MB; this stops a runaway response).
const MAX_BODY_BYTES: u64 = 20 * 1024 * 1024;
const USER_AGENT: &str = "mctl-m0-spike (research; contact via repo owner)";

/// CelesTrak GP query for the ISS. `FORMAT=JSON` is explicit because the server default is now CSV.
pub const CELESTRAK_ISS_OMM_URL: &str =
    "https://celestrak.org/NORAD/elements/gp.php?CATNR=25544&FORMAT=JSON";
/// NASA ISS trajectory (OEM, EME2000).
pub const NASA_ISS_OEM_URL: &str =
    "https://nasa-public-data.s3.amazonaws.com/iss-coords/current/ISS_OEM/ISS.OEM_J2K_EPH.txt";
/// JPL Horizons API endpoint.
pub const HORIZONS_URL: &str = "https://ssd.jpl.nasa.gov/api/horizons.api";

pub fn get_text(url: &str) -> Result<String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(TIMEOUT_S)))
        .http_status_as_error(false)
        .build()
        .into();
    let mut resp = agent
        .get(url)
        .header("User-Agent", USER_AGENT)
        .call()
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    ensure!(status.is_success(), "GET {url}: HTTP {status}");
    let body = resp
        .body_mut()
        .with_config()
        .limit(MAX_BODY_BYTES)
        .read_to_string()
        .with_context(|| format!("reading body of {url}"))?;
    ensure!(!body.trim().is_empty(), "GET {url}: empty body");
    Ok(body)
}

/// Fetch the ISS OMM and validate that it parses as a JSON array with a NORAD id of 25544.
pub fn fetch_omm() -> Result<String> {
    let body = get_text(CELESTRAK_ISS_OMM_URL)?;
    let v: serde_json::Value = serde_json::from_str(&body).with_context(|| {
        format!(
            "CelesTrak did not return JSON (default is CSV): {:.80}",
            body
        )
    })?;
    let id = v[0]["NORAD_CAT_ID"].as_u64();
    if id != Some(25_544) {
        bail!("CelesTrak returned NORAD id {id:?}, expected 25544");
    }
    Ok(body)
}

/// Fetch the NASA OEM and check it at least looks like a CCSDS OEM.
pub fn fetch_oem() -> Result<String> {
    let body = get_text(NASA_ISS_OEM_URL)?;
    ensure!(
        body.contains("CCSDS_OEM_VERS"),
        "response is not a CCSDS OEM"
    );
    Ok(body)
}

/// Geocentric ICRF Sun position from Horizons at the given Julian dates (UT), km.
/// `vec_corr` is Horizons' `VEC_CORR`: `NONE` for geometric positions, `LT+S` for the apparent direction
/// (light-time and stellar aberration), which is what an analytic Sun series models.
pub fn fetch_sun_horizons(jd_ut: &[f64], vec_corr: &str) -> Result<Vec<(f64, [f64; 3])>> {
    ensure!(!jd_ut.is_empty(), "no epochs requested");
    let tlist = jd_ut
        .iter()
        .map(|j| format!("{j:.6}"))
        .collect::<Vec<_>>()
        .join(" ");
    let q = |k: &str, v: &str| format!("{k}={}", crate::urlenc(&format!("'{v}'")));
    let url = format!(
        "{HORIZONS_URL}?format=json&{}&{}&{}&{}&{}&{}&{}&{}&{}&{}&{}&{}&{}",
        q("COMMAND", "10"),
        q("OBJ_DATA", "NO"),
        q("MAKE_EPHEM", "YES"),
        q("EPHEM_TYPE", "VECTORS"),
        q("CENTER", "500@399"),
        q("REF_PLANE", "FRAME"),
        q("REF_SYSTEM", "ICRF"),
        q("OUT_UNITS", "KM-S"),
        q("VEC_TABLE", "1"),
        q("CSV_FORMAT", "YES"),
        q("VEC_CORR", vec_corr),
        q("TIME_TYPE", "UT"),
        q("TLIST", &tlist),
    );
    let body = get_text(&url)?;
    let v: serde_json::Value =
        serde_json::from_str(&body).context("Horizons response is not JSON")?;
    let result = v["result"]
        .as_str()
        .with_context(|| format!("Horizons response has no result: {:.200}", body))?;
    parse_horizons_vectors(result)
}

/// Parse the `$$SOE .. $$EOE` block of a Horizons CSV vector table.
pub fn parse_horizons_vectors(result: &str) -> Result<Vec<(f64, [f64; 3])>> {
    let start = result
        .find("$$SOE")
        .context("no $$SOE marker in Horizons output (query rejected?)")?;
    let end = result
        .find("$$EOE")
        .context("no $$EOE marker in Horizons output")?;
    result[start + 5..end]
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let c: Vec<&str> = l.split(',').map(str::trim).collect();
            ensure!(c.len() >= 5, "short Horizons row {l:?}");
            let n = |i: usize| {
                c[i].parse::<f64>()
                    .with_context(|| format!("bad number {:?} in row {l:?}", c[i]))
            };
            Ok((n(0)?, [n(2)?, n(3)?, n(4)?]))
        })
        .collect()
}

pub fn write_fixture(dir: &Path, name: &str, body: &str) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(name);
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn horizons_table_parses() {
        let text = "header\n$$SOE\n2461336.500000000, A.D. 2026-Oct-23 00:00:00.0000, -1.299070233802826E+08, -6.669470768402286E+07, -2.890982784687529E+07,\n$$EOE\nfooter";
        let rows = parse_horizons_vectors(text).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, 2_461_336.5);
        assert!((rows[0].1[0] + 1.299_070_233_802_826e8).abs() < 1.0);
    }

    #[test]
    fn horizons_rejections() {
        assert!(parse_horizons_vectors("no markers").is_err());
        assert!(parse_horizons_vectors("$$SOE\n1.0, x\n$$EOE").is_err());
        assert!(parse_horizons_vectors("$$SOE\n1.0, x, a, b, c,\n$$EOE").is_err());
        assert!(parse_horizons_vectors("$$SOE only").is_err());
    }

    #[test]
    fn unreachable_host_is_an_error_not_a_hang() {
        assert!(get_text("https://nonexistent.invalid/").is_err());
    }
}
