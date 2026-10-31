//! Spike B: evaluation of satkit, brahe and an in-house sgp4 + nalgebra approach.
//!
//! `astro fetch` refreshes the fixtures from the network; `astro eval` runs every check against
//! every candidate from the fixtures (offline, reproducible) and writes a JSON results file.

mod cand_brahe;
mod cand_satkit;
mod candidate;
mod eval;
mod fetch;
mod geom;
mod inhouse;
mod oem;
mod timeutil;
mod vallado;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde::Serialize;
use tracing_subscriber::EnvFilter;

use candidate::Candidate;
use inhouse::{EopTable, InHouse, InHouseCandidate};
use timeutil::Utc;

/// Default fixture directory, resolved at compile time so the binary works from any cwd.
const DEFAULT_FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures");
const FILE_OMM: &str = "iss-omm.json";
const FILE_OEM: &str = "ISS.OEM_J2K_EPH.txt";
const FILE_EOP: &str = "finals2000A.all";
const FILE_SUN_GEOMETRIC: &str = "sun-horizons-geometric.json";
const FILE_SUN_APPARENT: &str = "sun-horizons-apparent.json";
const FILE_VALLADO_TLE: &str = "SGP4-VER.TLE";
const FILE_VALLADO_OUT: &str = "tmatverDec2015.out";
/// Julian date (UT) of the first Horizons Sun epoch, 2026-10-23T00:00, and the spacing and count of the rest.
const SUN_FIRST_JD_UT: f64 = 2_461_336.5;
const SUN_SPACING_DAYS: f64 = 7.0;
const SUN_EPOCHS: usize = 13;
/// Start of the numerical-propagation test: on the OEM's 4-minute grid and well inside its span.
const NUMERICAL_START: &str = "2026-10-22T12:00:00";
/// Index of `satkit` in the candidate list: the most complete frame implementation, used as the frames reference.
const SATKIT_INDEX: usize = 2;
/// Highest zonal harmonic used in the numerical comparison (J2-J4).
const ZONAL_DEGREE: u16 = 4;

/// Percent-encode for a URL query value.
pub fn urlenc(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

#[derive(Parser)]
struct Cli {
    #[arg(long, default_value = DEFAULT_FIXTURES)]
    fixtures: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Refresh the OMM, OEM and Horizons fixtures from the network.
    Fetch,
    /// Run all checks and write the JSON report.
    Eval {
        #[arg(long)]
        out: PathBuf,
    },
}

#[derive(Serialize)]
struct Report {
    omm_epoch: String,
    oem_span: (String, String),
    oem_spacing_s: (f64, f64),
    oem_hold_out: HoldOut,
    t1_sgp4_from_omm: Vec<eval::PerCandidate<eval::OmmAgreement>>,
    t2_sgp4_vs_oem: Vec<eval::PerCandidate<eval::OemComparison>>,
    t3_vallado: Vec<eval::PerCandidate<eval::ValladoSummary>>,
    t4_frames_vs_satkit: Vec<eval::PerCandidate<eval::FrameAgreement>>,
    t5_sun_vs_horizons_geometric: Vec<eval::PerCandidate<eval::SunError>>,
    t5_sun_vs_horizons_apparent: Vec<eval::PerCandidate<eval::SunError>>,
    t6_numerical_vs_oem: Vec<eval::PerCandidate<eval::NumericalResult>>,
    t7_time: Vec<(String, Vec<eval::TimeProbe>)>,
}

#[derive(Serialize)]
struct HoldOut {
    max_position_error_m: f64,
    max_velocity_error_mm_s: f64,
}

fn read(dir: &Path, name: &str) -> Result<String> {
    let p = dir.join(name);
    std::fs::read_to_string(&p)
        .with_context(|| format!("reading fixture {} (run `astro fetch`?)", p.display()))
}

fn fetch_all(dir: &Path) -> Result<()> {
    fetch::write_fixture(dir, FILE_OMM, &fetch::fetch_omm()?)?;
    tracing::info!("fetched ISS OMM (FORMAT=JSON)");
    fetch::write_fixture(dir, FILE_OEM, &fetch::fetch_oem()?)?;
    tracing::info!("fetched NASA ISS OEM");
    let jds: Vec<f64> = (0..SUN_EPOCHS)
        .map(|k| SUN_FIRST_JD_UT + k as f64 * SUN_SPACING_DAYS)
        .collect();
    for (file, corr) in [(FILE_SUN_GEOMETRIC, "NONE"), (FILE_SUN_APPARENT, "LT+S")] {
        let rows = fetch::fetch_sun_horizons(&jds, corr)?;
        anyhow::ensure!(
            rows.len() == jds.len(),
            "Horizons returned {} rows for {} epochs",
            rows.len(),
            jds.len()
        );
        fetch::write_fixture(dir, file, &serde_json::to_string(&rows)?)?;
    }
    tracing::info!(
        epochs = jds.len(),
        "fetched Horizons Sun vectors (geometric and apparent)"
    );
    Ok(())
}

fn eval_all(dir: &Path, out: &Path) -> Result<()> {
    let omm_json = read(dir, FILE_OMM)?;
    let omm_epoch = Utc::parse(
        serde_json::from_str::<serde_json::Value>(&omm_json)?[0]["EPOCH"]
            .as_str()
            .context("OMM has no EPOCH")?,
    )?;
    let oem = oem::Oem::parse(&read(dir, FILE_OEM)?)?;
    let cases = vallado::load(&read(dir, FILE_VALLADO_TLE)?, &read(dir, FILE_VALLADO_OUT)?)?;
    let eop = EopTable::parse(&read(dir, FILE_EOP)?)?;
    let sun_geometric: Vec<(f64, [f64; 3])> = serde_json::from_str(&read(dir, FILE_SUN_GEOMETRIC)?)
        .context("parsing geometric Horizons fixture")?;
    let sun_apparent: Vec<(f64, [f64; 3])> = serde_json::from_str(&read(dir, FILE_SUN_APPARENT)?)
        .context("parsing apparent Horizons fixture")?;

    let eop_path = dir.join(FILE_EOP);
    cand_satkit::Satkit::init_offline(&eop_path)?;
    cand_brahe::Brahe::init(&eop_path)?;

    let (hold_r, hold_v) = oem.hold_out_errors();
    tracing::info!(hold_out_pos_m = hold_r, hold_out_vel_mm_s = hold_v, spacing_s = ?oem.spacing_range_s(), "OEM interpolation hold-out (every other sample)");

    let mut inhouse_design = InHouseCandidate {
        cfg: InHouse {
            polar_motion: false,
        },
        eop: &eop,
    };
    let mut inhouse_polar = InHouseCandidate {
        cfg: InHouse { polar_motion: true },
        eop: &eop,
    };
    let mut satkit = cand_satkit::Satkit;
    let mut brahe = cand_brahe::Brahe;
    let mut cands: Vec<&mut dyn Candidate> = vec![
        &mut inhouse_design,
        &mut inhouse_polar,
        &mut satkit,
        &mut brahe,
    ];

    let (t1, run) = eval::t1_sgp4_omm(&mut cands, &omm_json, omm_epoch);
    let t2 = eval::t2_sgp4_vs_oem(&cands, &run, &oem);
    let t3 = eval::t3_vallado(&mut cands, &cases);
    let t4 = eval::t4_frames(&cands, &run, SATKIT_INDEX);
    let t5_geometric = eval::t5_sun(&cands, &sun_geometric);
    let t5_apparent = eval::t5_sun(&cands, &sun_apparent);
    let t6 = eval::t6_numerical(&mut cands, &oem, Utc::parse(NUMERICAL_START)?, ZONAL_DEGREE);

    let probes = [
        (
            "2016-12-31T23:59:59 (before leap second)",
            Utc::parse("2016-12-31T23:59:59")?,
        ),
        (
            "2017-01-01T00:00:00 (after leap second)",
            Utc::parse("2017-01-01T00:00:00")?,
        ),
        (
            "2026-10-22T00:00:00 (EOP observed row)",
            Utc::parse("2026-10-22T00:00:00")?,
        ),
        (
            "2026-10-22T12:00:00 (EOP between rows)",
            Utc::parse("2026-10-22T12:00:00")?,
        ),
        (
            "2040-01-01T00:00:00 (beyond EOP table)",
            Utc::parse("2040-01-01T00:00:00")?,
        ),
        (
            "1971-06-01T00:00:00 (before leap seconds)",
            Utc::parse("1971-06-01T00:00:00")?,
        ),
    ];
    let t7 = cands
        .iter()
        .map(|c| (c.name(), eval::t7_time(&**c, &probes)))
        .collect();

    let report = Report {
        omm_epoch: omm_epoch.iso(),
        oem_span: (oem.span().0.iso(), oem.span().1.iso()),
        oem_spacing_s: oem.spacing_range_s(),
        oem_hold_out: HoldOut {
            max_position_error_m: hold_r,
            max_velocity_error_mm_s: hold_v,
        },
        t1_sgp4_from_omm: t1,
        t2_sgp4_vs_oem: t2,
        t3_vallado: t3,
        t4_frames_vs_satkit: t4,
        t5_sun_vs_horizons_geometric: t5_geometric,
        t5_sun_vs_horizons_apparent: t5_apparent,
        t6_numerical_vs_oem: t6,
        t7_time: t7,
    };
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(out, serde_json::to_string_pretty(&report)?)
        .with_context(|| format!("writing {}", out.display()))?;
    tracing::info!(path = %out.display(), "report written");
    Ok(())
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let cli = Cli::parse();
    match cli.command {
        Command::Fetch => fetch_all(&cli.fixtures),
        Command::Eval { out } => eval_all(&cli.fixtures, &out),
    }
}
