//! winit + wgpu + egui front end: character-grid console, GPU panels, CRT post-processing, themes.
//!
//! The GUI is a pure consumer of the engine's published `World` snapshot (DESIGN.md §6.2): it never
//! calls into sources or propagators.

mod app;
mod assets;
mod coast;
mod console;
mod fonts;
mod globe;
mod gpu;
mod grid;
mod offline;
mod page;
mod render;
mod theme;

pub use app::{Options, Present, run};
pub use offline::{BenchOptions, Live, ShotOptions, bench, parse_size, shots};
pub use theme::{Theme, ThemeError, embedded as embedded_themes, from_dir as themes_from_dir};
