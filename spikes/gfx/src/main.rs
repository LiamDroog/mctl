//! Spike A: GPU console renderer (winit + wgpu + egui, no eframe).
//!
//! Throwaway: it exists to retire the rendering risks in DESIGN.md §13 M0(a).

mod app;
mod coast;
mod console;
mod globe;
mod gpu;
mod grid;
mod offline;
mod render;
mod theme;
mod ui;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

/// Sizes measured by `bench` unless overridden (the M0 targets).
const DEFAULT_BENCH_SIZES: &str = "1920x1080,2560x1440";
const DEFAULT_WINDOW_SIZE: &str = "1280x720";

#[derive(Parser)]
#[command(about = "mctl spike A: GPU console renderer")]
struct Cli {
    /// Directory of theme `*.toml` files replacing the embedded set.
    #[arg(long, global = true)]
    themes_dir: Option<PathBuf>,
    /// Prefer the low-power (integrated) adapter.
    #[arg(long, global = true)]
    low_power: bool,
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    window: WindowArgs,
}

#[derive(clap::Args)]
struct WindowArgs {
    /// Start theme (name, case-insensitive).
    #[arg(long)]
    theme: Option<String>,
    #[arg(long, default_value = DEFAULT_WINDOW_SIZE)]
    size: String,
    #[arg(long)]
    fullscreen: bool,
    #[arg(long, value_enum, default_value = "fifo")]
    present: app::PresentArg,
    /// Exit after this many presented frames (smoke tests, unattended timing).
    #[arg(long)]
    exit_after_frames: Option<u64>,
}

#[derive(Subcommand)]
enum Command {
    /// Headless frame-time benchmark of the full pipeline.
    Bench {
        #[arg(long, default_value = DEFAULT_BENCH_SIZES, value_delimiter = ',')]
        sizes: Vec<String>,
        #[arg(long, default_value_t = 600)]
        frames: u64,
        #[arg(long, default_value_t = 120)]
        warmup: u64,
        /// Restrict to one theme.
        #[arg(long)]
        theme: Option<String>,
        /// Also measure each effect on its own.
        #[arg(long)]
        breakdown: bool,
        /// Write a JSON report here.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Headless screenshots, one PNG per theme.
    Shot {
        #[arg(long, default_value = "1920x1080")]
        size: String,
        #[arg(long)]
        theme: Option<String>,
        #[arg(long)]
        out_dir: PathBuf,
        /// Disable all post effects.
        #[arg(long)]
        fx_off: bool,
        /// Frames rendered before capture so persistence and animation settle.
        #[arg(long, default_value_t = 90)]
        warmup: u64,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,wgpu=warn,naga=warn")),
        )
        .init();
    let cli = Cli::parse();
    let themes = match &cli.themes_dir {
        Some(dir) => theme::from_dir(dir),
        None => theme::embedded(),
    }
    .context("loading themes")?;

    match cli.command {
        None => app::run(
            themes,
            app::Options {
                size: offline::parse_size(&cli.window.size)?,
                fullscreen: cli.window.fullscreen,
                present: cli.window.present,
                low_power: cli.low_power,
                exit_after_frames: cli.window.exit_after_frames,
                start_theme: cli.window.theme,
            },
        ),
        Some(Command::Bench {
            sizes,
            frames,
            warmup,
            theme,
            breakdown,
            out,
        }) => {
            let sizes = sizes
                .iter()
                .map(|s| offline::parse_size(s))
                .collect::<Result<Vec<_>>>()?;
            offline::bench(
                themes,
                &offline::BenchOptions {
                    sizes,
                    frames,
                    warmup,
                    theme,
                    out,
                    low_power: cli.low_power,
                    breakdown,
                },
            )
        }
        Some(Command::Shot {
            size,
            theme,
            out_dir,
            fx_off,
            warmup,
        }) => offline::shots(
            themes,
            &offline::ShotOptions {
                size: offline::parse_size(&size)?,
                theme,
                out_dir,
                effects_off: fx_off,
                warmup_frames: warmup,
                low_power: cli.low_power,
            },
        )
        .map(|_| ()),
    }
}
