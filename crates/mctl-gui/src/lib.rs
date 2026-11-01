//! winit + wgpu + egui front end: character-grid console, GPU panels, CRT post-processing, themes.
//!
//! The GUI is a pure consumer of the engine's published `World` snapshot: it never
//! calls into sources or propagators.

mod about;
mod adi;
mod app;
mod art;
mod assets;
mod coast;
mod commands;
mod console;
mod deep;
mod flags;
mod fonts;
mod globe;
mod gpu;
mod grid;
mod interplanetary;
mod keys;
mod kiosk;
mod offline;
mod overlays;
mod page;
mod pages;
mod planner_page;
mod render;
mod screens;
mod sky;
mod telemetry_view;
mod theme;
mod trendplot;
mod vehicle_pages;
mod widgets;
mod window;

pub use app::{Options, Present, run};
pub use kiosk::{
    DEFAULT_DWELL_S as KIOSK_DEFAULT_DWELL_S, ENGINE_TICK_HZ as KIOSK_ENGINE_TICK_HZ, KioskConfig,
    Mix as KioskStyle, Order as KioskOrder, TRACK_REBUILD_S as KIOSK_TRACK_REBUILD_S,
};
/// Check the `[keys]` table, so a bad binding fails at start-up with its reason.
pub fn check_keys(keys: &std::collections::BTreeMap<String, Vec<String>>) -> Result<(), String> {
    keys::KeyMap::from_config(keys).map(|_| ())
}

pub use about::text as about_text;
pub use commands::parse_replay_start;
pub use offline::{BenchOptions, Live, ShotOptions, bench, parse_size, shots};
pub use theme::{
    Theme, ThemeError, crt_themes, embedded as embedded_themes, from_dir as themes_from_dir,
};
