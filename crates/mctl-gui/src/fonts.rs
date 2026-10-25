//! Embedded fonts and the cell-metrics check.

use std::sync::Arc;

use anyhow::{Result, bail};
use egui::{Context, FontData, FontDefinitions, FontFamily, FontId};

use crate::assets::{FONT_JETBRAINS, FONT_UNSCII};
use crate::theme::{FontFamily as ThemeFont, Theme};

/// Largest allowed difference between the font's real glyph advance and the theme's `cell_w_px`.
const CELL_WIDTH_TOLERANCE_PX: f32 = 0.01;

pub fn install_fonts(ctx: &Context) {
    let mut defs = FontDefinitions::empty();
    defs.font_data.insert(
        "jetbrains".into(),
        Arc::new(FontData::from_static(FONT_JETBRAINS)),
    );
    defs.font_data.insert(
        "unscii".into(),
        Arc::new(FontData::from_static(FONT_UNSCII)),
    );
    // egui insists that the built-in families exist, even though the console only uses named ones.
    defs.families
        .insert(FontFamily::Proportional, vec!["jetbrains".into()]);
    defs.families
        .insert(FontFamily::Monospace, vec!["jetbrains".into()]);
    defs.families.insert(
        FontFamily::Name("jetbrains".into()),
        vec!["jetbrains".into()],
    );
    defs.families
        .insert(FontFamily::Name("unscii".into()), vec!["unscii".into()]);
    ctx.set_fonts(defs);
}

pub fn font_id(theme: &Theme) -> FontId {
    let family = match theme.font.family {
        ThemeFont::Unscii => "unscii",
        ThemeFont::Jetbrains => "jetbrains",
    };
    FontId::new(theme.font.size_px, FontFamily::Name(family.into()))
}

/// Check that every theme's configured cell width equals the font's real advance.
///
/// A mismatch would silently misalign every column and break box-drawing joins, so it is an error.
/// Returns the texture upload produced by the probe pass; it must be delivered with the first real frame,
/// because egui sends later font-atlas changes as partial updates of that texture.
pub fn verify_cell_metrics(ctx: &Context, themes: &[Theme]) -> Result<egui::TexturesDelta> {
    let mut measured = Vec::new();
    // Fonts only become available inside a pass.
    let out = ctx.run_ui(egui::RawInput::default(), |ui| {
        for t in themes {
            let w = ui.ctx().fonts_mut(|f| f.glyph_width(&font_id(t), 'M'));
            measured.push((t.name.clone(), t.font.cell_w_px, w));
        }
        // The Pi display's dense font is not a theme's, but it must fit its cells just the same.
        let dense = crate::theme::dense_font();
        let id = FontId::new(dense.size_px, FontFamily::Name("unscii".into()));
        let w = ui.ctx().fonts_mut(|f| f.glyph_width(&id, 'M'));
        measured.push(("dense display font".to_owned(), dense.cell_w_px, w));
    });
    for (name, want, got) in measured {
        if (want - got).abs() > CELL_WIDTH_TOLERANCE_PX {
            bail!(
                "theme {name}: cell_w_px = {want} but the font advance at its size_px is {got}; fix the theme"
            );
        }
    }
    Ok(out.textures_delta)
}
