//! Coastline data (Natural Earth, compact `.bin` from `tools/shp_to_coast.py`) and its
//! rasterisation into an equirectangular mask the globe shader samples.
//!
//! A mask rather than 3D line geometry: it needs no occlusion handling, is trivially
//! antialiased by mipmapping, and costs one texture fetch per globe pixel.

#[derive(Debug, thiserror::Error)]
pub enum CoastError {
    #[error("coastline data truncated at byte {0}")]
    Truncated(usize),
    #[error("coastline data contains no lines")]
    Empty,
    #[error("coastline point ({lon}, {lat}) is outside lon/lat range")]
    OutOfRange { lon: f32, lat: f32 },
    #[error("trailing bytes after the last coastline")]
    Trailing,
}

/// A polyline of (lon°, lat°) points.
pub type Line = Vec<(f32, f32)>;

/// Parse the `u32 count; { u32 npoints; { f32 lon; f32 lat }* }*` little-endian format.
pub fn parse(bytes: &[u8]) -> Result<Vec<Line>, CoastError> {
    let mut at = 0usize;
    let u32_at = |at: &mut usize| -> Result<u32, CoastError> {
        let b = bytes.get(*at..*at + 4).ok_or(CoastError::Truncated(*at))?;
        *at += 4;
        Ok(u32::from_le_bytes(b.try_into().expect("slice is 4 bytes")))
    };
    let count = u32_at(&mut at)?;
    if count == 0 {
        return Err(CoastError::Empty);
    }
    let mut lines = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let n = u32_at(&mut at)? as usize;
        let body = bytes.get(at..at + n * 8).ok_or(CoastError::Truncated(at))?;
        at += n * 8;
        let mut line = Vec::with_capacity(n);
        for p in body.as_chunks::<8>().0 {
            let lon = f32::from_le_bytes(p[0..4].try_into().expect("4 bytes"));
            let lat = f32::from_le_bytes(p[4..8].try_into().expect("4 bytes"));
            if !(-180.0..=180.0).contains(&lon) || !(-90.0..=90.0).contains(&lat) {
                return Err(CoastError::OutOfRange { lon, lat });
            }
            line.push((lon, lat));
        }
        lines.push(line);
    }
    if at != bytes.len() {
        return Err(CoastError::Trailing);
    }
    Ok(lines)
}

/// One mip level of an 8-bit mask.
pub struct MaskLevel {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

fn plot_segment(img: &mut [u8], w: u32, h: u32, a: (f32, f32), b: (f32, f32)) {
    let to_px = |(lon, lat): (f32, f32)| {
        (
            (lon + 180.0) / 360.0 * w as f32,
            (90.0 - lat) / 180.0 * h as f32,
        )
    };
    let (ax, ay) = to_px(a);
    let (bx, by) = to_px(b);
    let steps = (bx - ax).abs().max((by - ay).abs()).ceil().max(1.0) as u32;
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let x = ((ax + (bx - ax) * t) as u32).min(w - 1);
        let y = ((ay + (by - ay) * t) as u32).min(h - 1);
        img[(y * w + x) as usize] = 255;
    }
}

/// Rasterise coastlines to a mip chain, level 0 being `width`×`width/2`.
///
/// Segments spanning more than 180° of longitude cross the antimeridian and are skipped
/// rather than drawn across the whole map.
pub fn rasterise(lines: &[Line], width: u32, levels: u32) -> Vec<MaskLevel> {
    let height = width / 2;
    let mut base = vec![0u8; (width * height) as usize];
    for line in lines {
        for seg in line.windows(2) {
            if (seg[0].0 - seg[1].0).abs() <= 180.0 {
                plot_segment(&mut base, width, height, seg[0], seg[1]);
            }
        }
    }
    let mut out = vec![MaskLevel {
        width,
        height,
        data: base,
    }];
    for _ in 1..levels {
        let prev = out.last().expect("at least level 0");
        let (w, h) = ((prev.width / 2).max(1), (prev.height / 2).max(1));
        let mut data = vec![0u8; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let px = |dx: u32, dy: u32| {
                    let sx = (x * 2 + dx).min(prev.width - 1);
                    let sy = (y * 2 + dy).min(prev.height - 1);
                    u32::from(prev.data[(sy * prev.width + sx) as usize])
                };
                let sum = px(0, 0) + px(1, 0) + px(0, 1) + px(1, 1);
                // Thin lines vanish under plain averaging; the 2x boost keeps them visible at distance.
                data[(y * w + x) as usize] = (sum / 2).min(255) as u8;
            }
        }
        out.push(MaskLevel {
            width: w,
            height: h,
            data,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(lines: &[&[(f32, f32)]]) -> Vec<u8> {
        let mut v = (lines.len() as u32).to_le_bytes().to_vec();
        for l in lines {
            v.extend((l.len() as u32).to_le_bytes());
            for (lon, lat) in *l {
                v.extend(lon.to_le_bytes());
                v.extend(lat.to_le_bytes());
            }
        }
        v
    }

    #[test]
    fn roundtrip() {
        let lines = parse(&encode(&[&[(0.0, 0.0), (10.0, 5.0)]])).unwrap();
        assert_eq!(lines, vec![vec![(0.0, 0.0), (10.0, 5.0)]]);
    }

    #[test]
    fn truncated_and_empty_and_range_errors() {
        let good = encode(&[&[(0.0, 0.0), (10.0, 5.0)]]);
        assert!(matches!(
            parse(&good[..good.len() - 1]),
            Err(CoastError::Truncated(_))
        ));
        assert!(matches!(parse(&0u32.to_le_bytes()), Err(CoastError::Empty)));
        assert!(matches!(
            parse(&encode(&[&[(0.0, 0.0), (200.0, 0.0)]])),
            Err(CoastError::OutOfRange { .. })
        ));
        let mut trailing = good;
        trailing.push(0);
        assert!(matches!(parse(&trailing), Err(CoastError::Trailing)));
    }

    #[test]
    fn real_data_parses() {
        let lines = parse(include_bytes!(
            "../../../assets/coastlines/ne_110m_coastline.bin"
        ))
        .unwrap();
        assert!(lines.len() > 100);
    }

    #[test]
    fn rasterise_skips_antimeridian_jump_and_builds_mips() {
        let lines = vec![vec![(-179.0, 0.0), (179.0, 0.0)]];
        let mips = rasterise(&lines, 64, 3);
        assert_eq!(mips.len(), 3);
        assert_eq!(mips[1].width, 32);
        assert!(
            mips[0].data.iter().all(|&p| p == 0),
            "wrapping segment must not be drawn"
        );
    }

    #[test]
    fn rasterise_draws_equator() {
        let mips = rasterise(&[vec![(-90.0, 0.0), (90.0, 0.0)]], 64, 1);
        let lit = mips[0].data.iter().filter(|&&p| p == 255).count();
        assert!(lit >= 30, "{lit}");
    }
}
