//! Embedded assets (licences in `assets/LICENSES.md`).

pub const EARTH_JPEG: &[u8] = include_bytes!("../../../assets/earth/blue_marble_4096x2048.jpg");
pub const NIGHT_JPEG: &[u8] = include_bytes!("../../../assets/earth/black_marble_2048x1024.jpg");
pub const COAST_110M: &[u8] = include_bytes!("../../../assets/coastlines/ne_110m_coastline.bin");
pub const COAST_50M: &[u8] = include_bytes!("../../../assets/coastlines/ne_50m_coastline.bin");
pub const FONT_JETBRAINS: &[u8] = include_bytes!("../../../assets/fonts/JetBrainsMono-Regular.ttf");
pub const FONT_UNSCII: &[u8] = include_bytes!("../../../assets/fonts/unscii-16.ttf");

/// Coastline mask width in texels (height is half): ~1 px lines at the globe's on-screen size.
pub const COAST_MASK_WIDTH: u32 = 4096;
/// Mip levels of the coastline mask.
pub const COAST_MASK_LEVELS: u32 = 6;
