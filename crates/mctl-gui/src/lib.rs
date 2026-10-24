//! winit + wgpu + egui front end: character-grid console, GPU panels, CRT post-processing, themes.
//!
//! The GUI is a pure consumer of the engine's published `World` snapshot (DESIGN.md §6.2): it never
//! calls into sources or propagators.

mod adi;
mod app;
mod assets;
mod coast;
mod commands;
mod console;
mod deep;
mod fonts;
mod globe;
mod gpu;
mod grid;
mod keys;
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
mod widgets;

pub use app::{Options, Present, run};
/// Check the `[keys]` table, so a bad binding fails at start-up with its reason.
pub fn check_keys(keys: &std::collections::BTreeMap<String, Vec<String>>) -> Result<(), String> {
    keys::KeyMap::from_config(keys).map(|_| ())
}

pub use commands::parse_replay_start;
pub use offline::{BenchOptions, Live, ShotOptions, bench, parse_size, shots};
pub use theme::{Theme, ThemeError, embedded as embedded_themes, from_dir as themes_from_dir};
