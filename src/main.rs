//! `mctl`: mission control display. One binary: the console (default), plus `check`, `shot`,
//! `bench` and `isslive` subcommands (DESIGN.md §11).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use mctl_core::bus::Bus;
use mctl_core::config::{self, Config};
use mctl_core::world::EopView;
use mctl_gui::Live;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

const DEFAULT_WINDOW_SIZE: &str = "1920x1080";
const DEFAULT_BENCH_SIZES: &str = "1920x1080,2560x1440";
/// How long `shot` and `bench` wait for the sources to deliver before rendering what they have.
const DEFAULT_WAIT_S: f64 = 25.0;

#[derive(Clone, Copy, ValueEnum)]
enum PresentArg {
    Fifo,
    Immediate,
    Mailbox,
}

#[derive(Parser)]
#[command(about = "mctl: mission control display", version)]
struct Cli {
    /// Run from cached data only; make no network requests.
    #[arg(long, global = true)]
    offline: bool,
    /// Vehicle to show first (an id from the configuration).
    #[arg(long, global = true)]
    vehicle: Option<String>,
    /// Directory of theme `*.toml` files replacing the embedded set.
    #[arg(long, global = true)]
    themes_dir: Option<PathBuf>,
    /// Prefer the low-power (integrated) adapter.
    #[arg(long, global = true)]
    low_power: bool,
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    gui: GuiArgs,
}

#[derive(clap::Args)]
struct GuiArgs {
    /// Start theme (name, case-insensitive).
    #[arg(long)]
    theme: Option<String>,
    #[arg(long, default_value = DEFAULT_WINDOW_SIZE)]
    size: String,
    #[arg(long)]
    fullscreen: bool,
    #[arg(long, value_enum, default_value = "fifo")]
    present: PresentArg,
    /// Exit after this many presented frames (smoke tests).
    #[arg(long)]
    exit_after_frames: Option<u64>,
}

#[derive(Subcommand)]
enum Command {
    /// Validate the configuration and fetch every source once, then exit (non-zero on any failure).
    Check,
    /// Headless screenshots of the live page, one PNG per theme.
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
        #[arg(long, default_value_t = 90)]
        warmup: u64,
        /// Seconds to wait for sources before rendering.
        #[arg(long, default_value_t = DEFAULT_WAIT_S)]
        wait_s: f64,
    },
    /// Headless frame-time benchmark of the full pipeline.
    Bench {
        #[arg(long, default_value = DEFAULT_BENCH_SIZES, value_delimiter = ',')]
        sizes: Vec<String>,
        #[arg(long, default_value_t = 600)]
        frames: u64,
        #[arg(long, default_value_t = 120)]
        warmup: u64,
        #[arg(long)]
        theme: Option<String>,
        #[arg(long)]
        breakdown: bool,
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long, default_value_t = DEFAULT_WAIT_S)]
        wait_s: f64,
    },
    /// Print live ISS telemetry from ISSLIVE (the adapter behind the GNC/EPS pages arrives in M3).
    Isslive {
        /// Use Lightstreamer's ticking DEMO feed to check the client itself.
        #[arg(long)]
        demo: bool,
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        duration_s: Option<f64>,
        #[arg(long)]
        quiet: bool,
    },
}

/// Log to `$XDG_STATE_HOME/mctl/mctl.log`; only warnings and errors reach the terminal.
fn init_logging() -> Result<Option<PathBuf>> {
    let filter = || {
        EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("info,wgpu=warn,naga=warn"))
    };
    let terminal = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(tracing_subscriber::filter::LevelFilter::WARN);
    let Some(dir) = config::state_dir() else {
        tracing_subscriber::registry().with(terminal).init();
        return Ok(None);
    };
    config::ensure_dir(&dir)?;
    let path = dir.join("mctl.log");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening log {}", path.display()))?;
    let file_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(std::sync::Mutex::new(file))
        .with_filter(filter());
    tracing_subscriber::registry()
        .with(terminal)
        .with(file_layer)
        .init();
    Ok(Some(path))
}

use tracing_subscriber::Layer;

/// Start sources and the engine; returns the handles that keep them alive.
fn start_live(
    cfg: &Config,
    vehicle: Option<&str>,
    offline: bool,
) -> Result<(Live, mctl_sim::engine::Engine)> {
    let vehicle_id = match vehicle {
        Some(v) if cfg.vehicles.iter().any(|d| d.id == v) => v.to_owned(),
        Some(v) => bail!(
            "unknown vehicle {v:?}; configured: {:?}",
            cfg.vehicles
                .iter()
                .map(|d| d.id.as_str())
                .collect::<Vec<_>>()
        ),
        None => cfg.vehicles[0].id.clone(),
    };
    let cache =
        config::cache_dir().context("cannot determine a cache directory (no home directory?)")?;
    let sources = mctl_sources::tasks::start(cfg, cache, offline).context("starting sources")?;
    let engine = mctl_sim::engine::start(cfg.clone(), sources);
    Ok((
        Live {
            bus: engine.bus.clone(),
            limits: cfg.limits.clone(),
            vehicle_id,
        },
        engine,
    ))
}

/// Wait until the selected vehicle has a solution and EOP is loaded, or `wait_s` passes.
fn wait_for_data(bus: &Arc<Bus>, vehicle_id: &str, wait_s: f64) {
    let start = Instant::now();
    while start.elapsed().as_secs_f64() < wait_s {
        let w = bus.latest();
        let have = w
            .vehicles
            .iter()
            .find(|v| v.id == vehicle_id)
            .is_some_and(|v| v.solution.is_some());
        if have && matches!(w.eop, EopView::Loaded { .. }) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    tracing::warn!(
        wait_s,
        "timed out waiting for sources; rendering what is available"
    );
}

fn load_themes(dir: &Option<PathBuf>) -> Result<Vec<mctl_gui::Theme>> {
    match dir {
        Some(d) => mctl_gui::themes_from_dir(d),
        None => mctl_gui::embedded_themes(),
    }
    .context("loading themes")
}

fn run_isslive(
    demo: bool,
    config_path: Option<PathBuf>,
    duration_s: Option<f64>,
    quiet: bool,
) -> Result<()> {
    use mctl_sources::isslive::{config::Config as IssConfig, run_cli};
    // `--demo` swaps in the ticking public demo feed, which proves the client independently of ISSLIVE's state.
    let demo_cfg;
    let path = match (demo, config_path) {
        (true, None) => {
            demo_cfg = std::env::temp_dir().join("mctl-isslive-demo.toml");
            std::fs::write(&demo_cfg, include_str!("../config/isslive-demo.toml"))
                .context("writing demo config")?;
            Some(demo_cfg.clone())
        }
        (_, p) => p,
    };
    if let Some(p) = &path {
        IssConfig::load(p).with_context(|| format!("config {}", p.display()))?;
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting async runtime")?;
    rt.block_on(run_cli(path.as_deref(), duration_s, quiet))
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let log_path = init_logging()?;
    let cfg = Config::load().context("loading configuration")?;
    tracing::info!(log = ?log_path, "mctl starting");

    match cli.command {
        Some(Command::Isslive {
            demo,
            config,
            duration_s,
            quiet,
        }) => run_isslive(demo, config, duration_s, quiet),
        Some(Command::Check) => {
            println!(
                "configuration: {} vehicle(s): {}",
                cfg.vehicles.len(),
                cfg.vehicles
                    .iter()
                    .map(|v| v.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            println!(
                "user config:   {}",
                config::user_config_path().map_or("(none)".into(), |p| format!(
                    "{} ({})",
                    p.display(),
                    if p.exists() {
                        "present"
                    } else {
                        "absent, using defaults"
                    }
                ))
            );
            let cache = config::cache_dir().context("cannot determine a cache directory")?;
            println!("cache:         {}", cache.display());
            if cli.offline {
                bail!("`check` tests network connectivity; it cannot be combined with --offline");
            }
            let lines = mctl_sources::tasks::check(&cfg, cache).context("preparing cache")?;
            let mut failed = 0;
            for l in &lines {
                println!(
                    "  {:<4} {:<22} {}",
                    if l.ok { "OK" } else { "FAIL" },
                    l.name,
                    l.detail
                );
                failed += usize::from(!l.ok);
            }
            if failed > 0 {
                bail!("{failed} of {} sources failed", lines.len());
            }
            println!("all {} sources OK", lines.len());
            Ok(())
        }
        Some(Command::Shot {
            size,
            theme,
            out_dir,
            fx_off,
            warmup,
            wait_s,
        }) => {
            let (live, _engine) = start_live(&cfg, cli.vehicle.as_deref(), cli.offline)?;
            wait_for_data(&live.bus, &live.vehicle_id, wait_s);
            let opts = mctl_gui::ShotOptions {
                size: mctl_gui::parse_size(&size)?,
                theme,
                out_dir,
                effects_off: fx_off,
                warmup_frames: warmup,
                low_power: cli.low_power,
            };
            mctl_gui::shots(&live, load_themes(&cli.themes_dir)?, &opts).map(|_| ())
        }
        Some(Command::Bench {
            sizes,
            frames,
            warmup,
            theme,
            breakdown,
            out,
            wait_s,
        }) => {
            let (live, _engine) = start_live(&cfg, cli.vehicle.as_deref(), cli.offline)?;
            wait_for_data(&live.bus, &live.vehicle_id, wait_s);
            let sizes = sizes
                .iter()
                .map(|s| mctl_gui::parse_size(s))
                .collect::<Result<Vec<_>>>()?;
            mctl_gui::bench(
                &live,
                load_themes(&cli.themes_dir)?,
                &mctl_gui::BenchOptions {
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
        None => {
            let (live, _engine) = start_live(&cfg, cli.vehicle.as_deref(), cli.offline)?;
            let g = cli.gui;
            mctl_gui::run(
                load_themes(&cli.themes_dir)?,
                mctl_gui::Options {
                    size: mctl_gui::parse_size(&g.size)?,
                    fullscreen: g.fullscreen,
                    present: match g.present {
                        PresentArg::Fifo => mctl_gui::Present::Fifo,
                        PresentArg::Immediate => mctl_gui::Present::Immediate,
                        PresentArg::Mailbox => mctl_gui::Present::Mailbox,
                    },
                    low_power: cli.low_power,
                    exit_after_frames: g.exit_after_frames,
                    start_theme: g.theme,
                    live,
                },
            )
        }
    }
}
