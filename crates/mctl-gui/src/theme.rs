//! Theme palettes and effect parameters, loaded from TOML.
//!
//! Themes are data so users can add their own; every value is range-checked on load so a
//! typo fails loudly with the file and field instead of rendering something subtly wrong.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Themes shipped in the binary, in `T`-key cycle order. Name is for error messages only.
const EMBEDDED_THEMES: &[(&str, &str)] = &[
    (
        "phosphor.toml",
        include_str!("../../../config/themes/phosphor.toml"),
    ),
    (
        "retro.toml",
        include_str!("../../../config/themes/retro.toml"),
    ),
    (
        "modern.toml",
        include_str!("../../../config/themes/modern.toml"),
    ),
    (
        "retrowave.toml",
        include_str!("../../../config/themes/retrowave.toml"),
    ),
];

/// The display theme for `--pi`: a small CRT fed by the Pi's composite output.
const CRT_THEME: (&str, &str) = ("crt.toml", include_str!("../../../config/themes/crt.toml"));

/// The font of the Pi display's dense scenes: Unscii at its native 8 x 16, 80 x 30 cells at 640 x 480.
pub fn dense_font() -> FontSpec {
    FontSpec {
        family: FontFamily::Unscii,
        size_px: 16.0,
        cell_w_px: 8.0,
        cell_h_px: 16.0,
    }
}

/// Smallest fraction of its nominal size the desktop font shrinks to; below it the panels clip instead,
/// since text under half size is no longer legible.
pub const MIN_FONT_SCALE: f32 = 0.5;

impl FontSpec {
    /// This font shrunk, if need be, so a `min_cols` x `min_rows` grid fits in `size_px`, never below
    /// `MIN_FONT_SCALE` and never larger than nominal: a bigger window just gets more cells. The advance and
    /// line height scale with the size, so the cell-metrics check made at startup still holds.
    pub fn fitted(&self, size_px: [u32; 2], min_cols: usize, min_rows: usize) -> FontSpec {
        let fit_w = size_px[0] as f32 / (min_cols as f32 * self.cell_w_px);
        let fit_h = size_px[1] as f32 / (min_rows as f32 * self.cell_h_px);
        // A hair under the exact fit, so rounding cannot leave the grid a cell short.
        let k = (fit_w.min(fit_h) * 0.999).clamp(MIN_FONT_SCALE, 1.0);
        if k >= 1.0 {
            return self.clone();
        }
        FontSpec {
            family: self.family,
            size_px: self.size_px * k,
            cell_w_px: self.cell_w_px * k,
            cell_h_px: self.cell_h_px * k,
        }
    }
}

impl Theme {
    /// This theme as the CRT shows it: the display font and no effects, since the tube supplies them.
    fn for_crt(mut self, font: &FontSpec) -> Self {
        self.font = font.clone();
        self.flat = true;
        self.effects.scanlines.enabled = false;
        self.effects.bloom.enabled = false;
        self.effects.persistence.enabled = false;
        self.effects.curvature.enabled = false;
        self
    }
}

/// The themes of the Pi display, in `T` cycle order: its own cyan one, then the green phosphor, amber and
/// retrowave looks of the console with the display font and every effect off.
pub fn crt_themes() -> Result<Vec<Theme>, ThemeError> {
    let crt = Theme::from_toml(CRT_THEME.0, CRT_THEME.1)?;
    let mut out = vec![crt.clone()];
    for (name, text) in EMBEDDED_THEMES {
        if matches!(*name, "phosphor.toml" | "retro.toml" | "retrowave.toml") {
            out.push(Theme::from_toml(name, text)?.for_crt(&crt.font));
        }
    }
    Ok(out)
}

#[derive(Debug, thiserror::Error)]
pub enum ThemeError {
    #[error("cannot read theme file {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid theme {name}: {source}")]
    Parse {
        name: String,
        source: toml::de::Error,
    },
    #[error("invalid theme {name}: {field}: {reason}")]
    Value {
        name: String,
        field: &'static str,
        reason: String,
    },
    #[error("no themes found in {0}")]
    Empty(PathBuf),
}

/// Linear-agnostic sRGB colour as authored (`#rrggbb`), components 0..=1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgb(pub [f32; 3]);

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        let hex = s
            .strip_prefix('#')
            .filter(|h| h.len() == 6)
            .ok_or_else(|| {
                serde::de::Error::custom(format!("colour {s:?} must look like \"#rrggbb\""))
            })?;
        let mut out = [0.0f32; 3];
        for (i, c) in out.iter_mut().enumerate() {
            let byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
                .map_err(|_| serde::de::Error::custom(format!("colour {s:?} is not hex")))?;
            *c = f32::from(byte) / 255.0;
        }
        Ok(Rgb(out))
    }
}

impl Rgb {
    pub fn to_egui(self) -> egui::Color32 {
        let [r, g, b] = self.0;
        egui::Color32::from_rgb(
            (r * 255.0).round() as u8,
            (g * 255.0).round() as u8,
            (b * 255.0).round() as u8,
        )
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FontFamily {
    Unscii,
    Jetbrains,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FontSpec {
    pub family: FontFamily,
    pub size_px: f32,
    /// Must equal the font's advance at `size_px`; checked at startup against the real font.
    pub cell_w_px: f32,
    pub cell_h_px: f32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Palette {
    pub background: Rgb,
    pub foreground: Rgb,
    pub dim: Rgb,
    pub bright: Rgb,
    pub border: Rgb,
    pub nominal: Rgb,
    pub caution: Rgb,
    pub warning: Rgb,
    pub info: Rgb,
}

/// How a limit state is encoded without relying on hue alone.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct LimitStyle {
    pub inverse: bool,
    /// 0 disables blinking.
    pub blink_hz: f32,
    pub dim: bool,
    pub semantic_colour: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LimitStyles {
    pub caution: LimitStyle,
    pub warning: LimitStyle,
    pub stale: LimitStyle,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Scanlines {
    pub enabled: bool,
    /// Fraction of brightness removed in the dark part of each line, 0..=1.
    pub strength: f32,
    /// Distance between scanlines in output pixels.
    pub period_px: f32,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Bloom {
    pub enabled: bool,
    /// Luminance (0..=1) above which pixels contribute to glow.
    pub threshold: f32,
    pub intensity: f32,
    pub radius_px: f32,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Persistence {
    pub enabled: bool,
    /// Fraction of the previous frame's brightness retained per frame, 0..1. Frame-rate dependent.
    pub decay: f32,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Curvature {
    pub enabled: bool,
    /// Barrel distortion strength; ~0.03 is "slight".
    pub amount: f32,
    pub vignette: f32,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Effects {
    pub scanlines: Scanlines,
    pub bloom: Bloom,
    pub persistence: Persistence,
    pub curvature: Curvature,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum GlobeMode {
    /// Texture luminance is tinted with the theme colour (monochrome phosphor look).
    Mono,
    Colour,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GlobeStyle {
    pub mode: GlobeMode,
    pub day_tint: Rgb,
    pub night_tint: Rgb,
    pub graticule: Rgb,
    pub coast: Rgb,
    pub orbit: Rgb,
    pub vehicle: Rgb,
    pub ambient: f32,
    /// Half-width of the terminator blend, as N·L.
    pub terminator_softness: f32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Theme {
    pub name: String,
    pub font: FontSpec,
    pub palette: Palette,
    pub limit: LimitStyles,
    pub effects: Effects,
    pub globe: GlobeStyle,
    /// Draw the cheap way: no glow passes and no line anti-aliasing. For a CRT, which supplies both, and for
    /// software rendering.
    #[serde(default)]
    pub flat: bool,
}

impl Theme {
    pub fn from_toml(source_name: &str, text: &str) -> Result<Self, ThemeError> {
        let theme: Theme = toml::from_str(text).map_err(|source| ThemeError::Parse {
            name: source_name.to_owned(),
            source,
        })?;
        theme.validate(source_name)?;
        Ok(theme)
    }

    fn validate(&self, name: &str) -> Result<(), ThemeError> {
        let bad = |field: &'static str, reason: &str| ThemeError::Value {
            name: name.to_owned(),
            field,
            reason: reason.to_owned(),
        };
        let unit = |v: f32| (0.0..=1.0).contains(&v);
        let f = &self.font;
        if !(f.size_px > 0.0 && f.cell_w_px > 0.0 && f.cell_h_px > 0.0) {
            return Err(bad("font", "size_px, cell_w_px and cell_h_px must be > 0"));
        }
        let e = &self.effects;
        if !unit(e.scanlines.strength) || e.scanlines.period_px < 2.0 {
            return Err(bad(
                "effects.scanlines",
                "strength must be 0..=1 and period_px >= 2",
            ));
        }
        if !unit(e.bloom.threshold) || e.bloom.intensity < 0.0 || e.bloom.radius_px < 0.0 {
            return Err(bad(
                "effects.bloom",
                "threshold must be 0..=1; intensity and radius_px >= 0",
            ));
        }
        if !(0.0..1.0).contains(&e.persistence.decay) {
            return Err(bad(
                "effects.persistence.decay",
                "must be in 0..1 (1 would never fade)",
            ));
        }
        if !(0.0..=0.3).contains(&e.curvature.amount) || !unit(e.curvature.vignette) {
            return Err(bad(
                "effects.curvature",
                "amount must be 0..=0.3 and vignette 0..=1",
            ));
        }
        let g = &self.globe;
        if !unit(g.ambient) || !(0.01..=1.0).contains(&g.terminator_softness) {
            return Err(bad(
                "globe",
                "ambient must be 0..=1 and terminator_softness 0.01..=1",
            ));
        }
        for (field, l) in [
            ("limit.caution", self.limit.caution),
            ("limit.warning", self.limit.warning),
            ("limit.stale", self.limit.stale),
        ] {
            if l.blink_hz < 0.0 || l.blink_hz > 10.0 {
                return Err(bad(field, "blink_hz must be 0..=10"));
            }
            // Limit states must not rely on hue alone.
            if !(l.inverse || l.dim || l.blink_hz > 0.0 || l.semantic_colour) {
                return Err(bad(field, "state has no visible encoding"));
            }
        }
        Ok(())
    }
}

/// Load the embedded themes in cycle order.
pub fn embedded() -> Result<Vec<Theme>, ThemeError> {
    EMBEDDED_THEMES
        .iter()
        .map(|(n, t)| Theme::from_toml(n, t))
        .collect()
}

/// Load every `*.toml` in `dir`, sorted by file name, replacing the embedded set.
pub fn from_dir(dir: &Path) -> Result<Vec<Theme>, ThemeError> {
    let io = |path: &Path, source| ThemeError::Io {
        path: path.to_owned(),
        source,
    };
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| io(dir, e))?
        .map(|entry| entry.map(|e| e.path()).map_err(|e| io(dir, e)))
        .collect::<Result<_, _>>()?;
    paths.retain(|p| p.extension().is_some_and(|e| e == "toml"));
    paths.sort();
    if paths.is_empty() {
        return Err(ThemeError::Empty(dir.to_owned()));
    }
    paths
        .iter()
        .map(|p| {
            let text = std::fs::read_to_string(p).map_err(|e| io(p, e))?;
            Theme::from_toml(&p.display().to_string(), &text)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_font_shrinks_to_fit_the_minimum_grid_and_never_grows_or_goes_illegible() {
        let f = super::FontSpec {
            family: super::FontFamily::Unscii,
            size_px: 24.0,
            cell_w_px: 12.0,
            cell_h_px: 24.0,
        };
        let grid = |f: &super::FontSpec, w: u32, h: u32| {
            (
                (w as f32 / f.cell_w_px) as usize,
                (h as f32 / f.cell_h_px) as usize,
            )
        };
        assert_eq!(
            f.fitted([3840, 2160], 160, 45).cell_w_px,
            12.0,
            "a big window keeps the nominal size"
        );
        for (w, h) in [(1920, 985), (1366, 700), (1280, 720)] {
            let g = grid(&f.fitted([w, h], 160, 45), w, h);
            assert!(g.0 >= 160 && g.1 >= 45, "{w}x{h} gives {g:?}");
        }
        let tiny = f.fitted([400, 300], 160, 45);
        assert_eq!(tiny.cell_w_px, 6.0, "never below half size");
        assert_eq!(tiny.size_px, 12.0);
    }

    use super::*;

    #[test]
    fn embedded_themes_load_and_are_the_four_from_the_design() {
        let names: Vec<_> = embedded().unwrap().into_iter().map(|t| t.name).collect();
        assert_eq!(names, ["Phosphor", "Retro", "Modern", "Retrowave"]);
    }

    #[test]
    fn the_crt_themes_have_every_effect_off_and_the_display_font() {
        let themes = crt_themes().unwrap();
        let names: Vec<_> = themes.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["CRT", "Phosphor", "Retro", "Retrowave"]);
        for t in &themes {
            let e = t.effects;
            assert!(!e.scanlines.enabled && !e.bloom.enabled, "{}", t.name);
            assert!(!e.persistence.enabled && !e.curvature.enabled, "{}", t.name);
            assert_eq!((t.font.cell_w_px, t.font.cell_h_px), (12.0, 24.0));
        }
    }

    #[test]
    fn bad_colour_is_rejected_with_context() {
        let text = EMBEDDED_THEMES[0].1.replace("#010b03", "green");
        let err = Theme::from_toml("t.toml", &text).unwrap_err().to_string();
        assert!(err.contains("t.toml") && err.contains("#rrggbb"), "{err}");
    }

    #[test]
    fn out_of_range_decay_is_rejected() {
        let text = EMBEDDED_THEMES[0].1.replace("decay = 0.82", "decay = 1.0");
        let err = Theme::from_toml("t.toml", &text).unwrap_err().to_string();
        assert!(err.contains("effects.persistence.decay"), "{err}");
    }

    #[test]
    fn limit_state_without_encoding_is_rejected() {
        let text = EMBEDDED_THEMES[2]
            .1
            .replace("semantic_colour = true", "semantic_colour = false");
        let err = Theme::from_toml("t.toml", &text).unwrap_err().to_string();
        assert!(err.contains("no visible encoding"), "{err}");
    }
}
