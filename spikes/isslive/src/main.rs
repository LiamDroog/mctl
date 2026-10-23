//! Spike C: minimal in-house Lightstreamer TLCP client for the public ISSLIVE feed.
//!
//! Throwaway: it exists to retire DESIGN.md §13 M0(c) and R2/R4. The per-update printout on stdout
//! is the point of the spike, so it uses `println!`; diagnostics go through `tracing`.

mod config;
mod session;
mod stats;
mod tlcp;

use std::path::PathBuf;

use anyhow::Result;
use chrono::{DateTime, Datelike, Duration, TimeZone, Utc};
use clap::Parser;
use tracing_subscriber::EnvFilter;

use config::Config;

#[derive(Parser)]
#[command(about = "mctl spike C: ISSLIVE Lightstreamer connectivity")]
struct Cli {
    /// Config TOML; the embedded default is used if omitted.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Override run.duration_s.
    #[arg(long)]
    duration_s: Option<f64>,
    /// Do not print each update, only the summary.
    #[arg(long)]
    quiet: bool,
}

/// ISSLIVE `TimeStamp` is fractional hours since 00:00 UTC on Dec 31 of the previous year, as ISS
/// Mimic's own client computes it (Pi/iss_telemetry.py `TimeListener`). Verified against wall-clock
/// receive times in the run log: the derived age is a small positive number of seconds.
fn source_time(timestamp_hours: f64, rx: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let base = Utc
        .with_ymd_and_hms(rx.year() - 1, 12, 31, 0, 0, 0)
        .single()?;
    Some(base + Duration::microseconds((timestamp_hours * 3600.0 * 1.0e6) as i64))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let cli = Cli::parse();
    let mut cfg = match &cli.config {
        Some(p) => Config::load(p)?,
        None => Config::parse("embedded default", config::DEFAULT_CONFIG)?,
    };
    if let Some(d) = cli.duration_s {
        anyhow::ensure!(d > 0.0, "--duration-s must be > 0");
        cfg.run.duration_s = d;
    }

    let mut stats = stats::Stats::new(&cfg);
    let quiet = cli.quiet;
    let summary = session::run(&cfg, |u| {
        let value = u
            .fields
            .iter()
            .position(|f| f == "Value")
            .and_then(|i| u.values[i].as_deref());
        let ts = u
            .fields
            .iter()
            .position(|f| f == "TimeStamp")
            .and_then(|i| u.values[i].as_deref());
        let age = ts
            .and_then(|t| t.parse::<f64>().ok())
            .and_then(|h| source_time(h, u.rx))
            .map(|s| (u.rx - s).num_milliseconds() as f64 / 1000.0);
        stats.record(u, value, age);
        if !quiet {
            let meta = cfg.items.iter().find(|i| i.id == u.item_id);
            let label = meta.map_or("", |i| i.description.as_str());
            let units = meta.map_or("", |i| i.units.as_str());
            println!(
                "{} {:<12} value={:<24} [{units}] ts={:<22} age={:>7} changed={:?} {}",
                u.rx.format("%H:%M:%S%.3f"),
                u.item_id,
                value.unwrap_or("<null>"),
                ts.unwrap_or("<null>"),
                age.map_or("n/a".to_owned(), |a| format!("{a:.2}s")),
                u.changed
                    .iter()
                    .map(|&i| u.fields[i].as_str())
                    .collect::<Vec<_>>(),
                label,
            );
            // Items with extra fields (the TIME item's status fields) show them all.
            // Without a `Value` field (non-ISSLIVE adapters) every field is shown.
            let skip = if u.fields.iter().any(|f| f == "Value") {
                2
            } else {
                0
            };
            if u.fields.len() > skip {
                let extra: Vec<String> = u
                    .fields
                    .iter()
                    .zip(u.values)
                    .skip(skip)
                    .map(|(f, v)| format!("{f}={}", v.as_deref().unwrap_or("<null>")))
                    .collect();
                println!("{:>13} {:<12} {}", "", u.item_id, extra.join(" "));
            }
        }
    })
    .await?;

    println!("\n=== session ===");
    println!(
        "session id {} keepalive {} ms; connect+CONOK {} ms; subscribe {} ms; ended by {}",
        summary.session_id,
        summary.keepalive_ms,
        summary.connect_ms,
        summary.subscribe_ms,
        summary.ended_by
    );
    let mut tags: Vec<_> = summary.notifications.iter().collect();
    tags.sort();
    println!("notifications: {tags:?}");
    stats.print(&cfg, cfg.run.duration_s);
    Ok(())
}
