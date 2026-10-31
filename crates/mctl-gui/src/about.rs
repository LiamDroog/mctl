//! Licence, credits and the not-official notice: the SYS / ABOUT page and `mctl about`.

use crate::grid::{CellRect, Grid, Role, Style};
use crate::page::{Layout, Lines, PageInput};
use crate::screens::{body, boxed};
use crate::vehicle_pages::wrap;

/// Where the source lives.
pub const REPOSITORY: &str = "https://github.com/LiamDroog/mctl";

/// Heading and paragraphs, in plain sentence case; the console shows them upper-cased.
pub const SECTIONS: &[(&str, &[&str])] = &[
    (
        "Licence",
        &[
            "mctl is free software under the GNU General Public License, version 3 or (at your option) any later version. It comes with NO WARRANTY.",
            "The licences of the libraries and assets it includes are in the third-party notices (`mctl about`).",
        ],
    ),
    (
        "Not official",
        &[
            "mctl is a hobby display. It is not affiliated with or endorsed by NASA, ESA, STScI, SpaceX or any other organisation whose vehicles or data it shows, and it must not be used for operations, navigation or anything safety-critical.",
        ],
    ),
    (
        "Data",
        &[
            "Element sets: CelesTrak. ISS trajectory and ISSLIVE telemetry: NASA's public data. Earth orientation: IERS and USNO. JWST and Roman trajectories: JPL Horizons. Observing schedules and programs: STScI.",
            "Survey pictures: the Digitized Sky Survey (DSS2) through CDS Strasbourg (Sesame and hips2fits).",
        ],
    ),
    (
        "Imagery and fonts",
        &[
            "Earth: Blue Marble Next Generation (R. Stockli) and Black Marble 2012 (NOAA NGDC, Suomi NPP VIIRS), NASA Earth Observatory. Coastlines: Natural Earth. Fonts: JetBrains Mono (SIL OFL 1.1) and Unscii (public domain).",
        ],
    ),
];

/// Who made it and what it stands on.
pub const CREDITS: &[(&str, &[&str])] = &[
    (
        "Made by",
        &[
            "Liam Droog (github.com/LiamDroog): the idea, the design and direction, the testing, and the Raspberry Pi and CRT hardware it was tuned on.",
            "Built in collaboration with Claude, Anthropic's AI model, which wrote much of the code and documentation from that design and under that direction.",
        ],
    ),
    (
        "Data",
        &[
            "CelesTrak (element sets), NASA (ISS trajectory and the ISSLIVE telemetry feed), IERS and USNO (earth orientation), JPL Horizons (JWST and Roman), STScI (JWST visit plans, Hubble reports and programs), and the Digitized Sky Survey through CDS Strasbourg (survey pictures).",
            "Thanks to the people who keep these services public.",
        ],
    ),
    (
        "Imagery and fonts",
        &[
            "Earth textures: NASA Earth Observatory (Blue Marble Next Generation, R. Stockli; Black Marble 2012, NOAA NGDC and Suomi NPP VIIRS). Coastlines: Natural Earth. Fonts: JetBrains Mono and Unscii.",
        ],
    ),
    (
        "Software",
        &[
            "Written in Rust. Graphics: wgpu, egui and winit. Orbit propagation: the sgp4 crate for SGP4 and ode_solvers for the numerical integrator, with nalgebra. Recording: SQLite through rusqlite. Networking: ureq with rustls. Every library is listed in the third-party notices (`mctl about`).",
            "The Sun, Moon and frame reductions follow Vallado, Fundamentals of Astrodynamics and Applications.",
        ],
    ),
];

/// The About text for the terminal.
pub fn text() -> String {
    let mut out = format!("mctl {}\n{REPOSITORY}\n\n", env!("CARGO_PKG_VERSION"));
    for (heading, paras) in CREDITS.iter().chain(SECTIONS) {
        out.push_str(&format!("{heading}\n"));
        for p in *paras {
            out.push_str(&format!("  {p}\n"));
        }
        out.push('\n');
    }
    out
}

/// SYS / ABOUT.
pub(crate) fn page(grid: &mut Grid, inp: &PageInput<'_>) -> Layout {
    sections_page(grid, inp, "ABOUT", SECTIONS)
}

/// SYS / CREDITS.
pub(crate) fn credits(grid: &mut Grid, inp: &PageInput<'_>) -> Layout {
    sections_page(grid, inp, "CREDITS", CREDITS)
}

fn sections_page(
    grid: &mut Grid,
    _inp: &PageInput<'_>,
    title: &str,
    sections: &[(&str, &[&str])],
) -> Layout {
    let (top, h) = body(grid.rows);
    let rect = CellRect::new(0, top, grid.cols, h);
    boxed(
        grid,
        rect,
        &format!("{title}  MCTL {}", env!("CARGO_PKG_VERSION")),
    );
    let width = rect.cols.saturating_sub(8).max(20);
    let mut l = Lines::new(grid, rect);
    for (heading, paras) in sections {
        l.line(&heading.to_uppercase(), Style::new(Role::Bright));
        for p in *paras {
            for line in wrap(&p.to_uppercase(), width) {
                l.line(&format!("  {line}"), Style::new(Role::Fg));
            }
        }
        l.row += 1;
    }
    l.line(
        "FULL THIRD-PARTY NOTICES: RUN `mctl about`",
        Style::new(Role::Dim),
    );
    Layout::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_credits_name_the_author_and_the_ai_collaborator() {
        let t = text();
        assert!(t.contains("Liam Droog"));
        assert!(t.contains("Claude"));
        assert!(t.contains("CelesTrak") && t.contains("JPL Horizons") && t.contains("STScI"));
    }

    #[test]
    fn the_text_names_the_licence_and_says_it_is_not_official() {
        let t = text();
        assert!(t.contains("version 3 or (at your option) any later version"));
        assert!(t.contains("NO WARRANTY"));
        assert!(t.contains("not affiliated"));
        assert!(t.contains(REPOSITORY));
    }
}
