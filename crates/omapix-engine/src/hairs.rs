//! Stray Hairs (Auto Retouch): dark hairs lying on the skin, found as thin,
//! long strokes darker than the skin on both sides of them, and filled
//! smoothly from the skin at their edges ([`crate::poisson`]), with its
//! texture from beside them, on a layer of their own.

use crate::document::Document;
use crate::filters::blur_buffer;
use crate::layer::{Layer, Mask};
use crate::poisson;
use crate::raster::Raster;
use crate::selection::{Combine, Selection};
use crate::tiled::{TILE, Tiled};

const MAX: f32 = u16::MAX as f32;

/// How far from the skin's edge hairs are looked for, in distances between
/// the eyes: the hairline, brows and lashes are hair too.
const MARGIN: f32 = 0.03;
/// The blurs a hair is seen at and the skin round it is measured at, in
/// distances between the eyes.
const FINE: f32 = 0.003;
const ROUND: f32 = 0.02;
/// How much darker than the skin round it a hair is, as a fraction of that
/// skin's lightness, at least.
const DARKER: f32 = 0.1;
/// A hair is at least this long and at most this wide, in distances between
/// the eyes, and this many times longer than wide: pores are round, lines
/// and eyeliner wider.
const LONG: f32 = 0.05;
const THIN: f32 = 0.012;
const SLENDER: f32 = 6.0;
/// What fraction of [`DARKER`] a hair's faint parts are darker by.
const FAINT: f32 = 0.5;
/// What fraction of a hair is darker than the skin on both sides of it, at
/// least ([`ridge`]): the edge of the skin, a leaf or a nostril, or a
/// stroke in a brow or the hairline, isn't.
const RIDGE: f32 = 0.55;
/// How much more saturated than the skin round it a stroke is, at most, to
/// be a hair: a line is the skin in shadow, which is more saturated.
const SHADED: f32 = 0.018;
/// How far round a hair is filled, in pixels: its soft edge.
const EDGE: f32 = 1.5;

/// The stray hairs on `skin` in `image`, for eyes `iod` apart, apart from
/// in `avoid`: round the eyes, where lines, lashes and eyeliner look as
/// hairs do.
pub fn find(image: &Raster, skin: &Selection, avoid: &Selection, iod: f32) -> Selection {
    let (w, h) = (skin.width(), skin.height());
    let none = || Selection::from_coverage(Tiled::new(w, h, 0));
    let inside = skin.contract(MARGIN * iod).combine(avoid, Combine::Subtract);
    let Some([bx, by, bw, bh]) = inside.bounds() else {
        return none();
    };
    let (bw_, bh_) = (bw as usize, bh as usize);
    let light: Vec<[f32; 4]> = (0..bw * bh)
        .map(|i| {
            let p = image.get(bx + i % bw, by + i / bw);
            [0.2126 * f32::from(p[0]) + 0.7152 * f32::from(p[1]) + 0.0722 * f32::from(p[2]); 4]
        })
        .collect();
    let fine = blur_buffer(light.clone(), bw_, bh_, FINE * iod);
    let round = blur_buffer(light, bw_, bh_, ROUND * iod);
    let on = |i: usize| inside.at(bx + i as u32 % bw, by + i as u32 / bw) > 0.5;
    let darker = |i: usize, by: f32| on(i) && round[i][0] - fine[i][0] > by * round[i][0];
    let dark: Vec<bool> = (0..fine.len()).map(|i| darker(i, DARKER)).collect();
    let faint: Vec<bool> = (0..fine.len()).map(|i| darker(i, FAINT * DARKER)).collect();
    // Each stroke of dark pixels, kept if it's long and thin, darker than
    // both its sides and not the skin in shadow, and then followed along
    // its faint parts.
    let mut seen = vec![false; dark.len()];
    let mut hairs = vec![false; dark.len()];
    for start in 0..dark.len() {
        if !dark[start] || seen[start] {
            continue;
        }
        let stroke = connected(start, &dark, &mut seen, bw_, bh_);
        if long_and_thin(&stroke, bw_, iod) && ridge(&fine, &stroke, bw_, bh_, iod) >= RIDGE && !shaded(image, [bx, by, bw], &stroke, &dark, &on, iod) {
            for i in stroke {
                hairs[i] = true;
            }
        }
    }
    let mut followed = vec![false; dark.len()];
    for start in 0..dark.len() {
        if hairs[start] && !followed[start] {
            for i in connected(start, &faint, &mut followed, bw_, bh_) {
                hairs[i] = true;
            }
        }
    }
    let soft = hairs.iter().map(|&h| [if h { 1.0 } else { 0.0 }; 4]).collect();
    let soft = blur_buffer(soft, bw_, bh_, EDGE);
    let mut coverage = vec![0u16; (w * h) as usize];
    for (i, s) in soft.iter().enumerate() {
        let (x, y) = (bx + i as u32 % bw, by + i as u32 / bw);
        coverage[(y * w + x) as usize] = ((s[0] * 3.0).min(1.0) * skin.at(x, y) * MAX) as u16;
    }
    Selection::from_coverage(Tiled::from_slice(w, h, 0, &coverage))
}

/// The pixels joined to `start` through those that are `on`, marked
/// `seen`, in a box `width` × `height`.
fn connected(start: usize, on: &[bool], seen: &mut [bool], width: usize, height: usize) -> Vec<usize> {
    seen[start] = true;
    let mut found = vec![start];
    let mut next = 0;
    while next < found.len() {
        let i = found[next];
        next += 1;
        let (x, y) = ((i % width) as i64, (i / width) as i64);
        for (dx, dy) in [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)] {
            let (nx, ny) = (x + dx, y + dy);
            if nx < 0 || ny < 0 || nx >= width as i64 || ny >= height as i64 {
                continue;
            }
            let n = ny as usize * width + nx as usize;
            if on[n] && !seen[n] {
                seen[n] = true;
                found.push(n);
            }
        }
    }
    found
}

/// How far across a hair its sides are looked at, in distances between the
/// eyes.
const SIDE: f32 = 0.008;

/// What fraction of `stroke`'s pixels (in `light`, a box `width` ×
/// `height`) are darker than the skin on both sides of it, across the way
/// it runs: a hair's are, an edge's only on one.
fn ridge(light: &[[f32; 4]], stroke: &[usize], width: usize, height: usize, iod: f32) -> f32 {
    let n = stroke.len() as f32;
    let at = |i: usize| ((i % width) as f32, (i / width) as f32);
    let (mx, my) = stroke.iter().fold((0.0, 0.0), |(sx, sy), &i| (sx + at(i).0 / n, sy + at(i).1 / n));
    let (mut xx, mut xy, mut yy) = (0.0, 0.0, 0.0);
    for &i in stroke {
        let (dx, dy) = (at(i).0 - mx, at(i).1 - my);
        (xx, xy, yy) = (xx + dx * dx, xy + dx * dy, yy + dy * dy);
    }
    // Across: the axis it spreads least along.
    let angle = 0.5 * (2.0 * xy).atan2(xx - yy) + std::f32::consts::FRAC_PI_2;
    let reach = (SIDE * iod).max(2.0);
    let (ax, ay) = (angle.cos() * reach, angle.sin() * reach);
    let sample = |x: f32, y: f32| {
        let (x, y) = (x.round() as i64, y.round() as i64);
        (x >= 0 && y >= 0 && (x as usize) < width && (y as usize) < height).then(|| light[y as usize * width + x as usize][0])
    };
    let darker = stroke
        .iter()
        .filter(|&&i| {
            let (x, y) = at(i);
            let v = light[i][0];
            match (sample(x + ax, y + ay), sample(x - ax, y - ay)) {
                (Some(a), Some(b)) => a > v * (1.0 + DARKER / 2.0) && b > v * (1.0 + DARKER / 2.0),
                _ => false,
            }
        })
        .count();
    darker as f32 / n
}

/// Whether `stroke` (indices in the box at (x, y), `width` wide) is the skin
/// in shadow, a line: more saturated than the skin [`ROUND`] off it, on
/// `on` and not `dark`.
fn shaded(image: &Raster, [x, y, width]: [u32; 3], stroke: &[usize], dark: &[bool], on: &impl Fn(usize) -> bool, iod: f32) -> bool {
    let (width, height) = (width as i64, (dark.len() / width as usize) as i64);
    let mean = |pixels: &mut dyn Iterator<Item = usize>| {
        let (mut sum, mut n) = ([0.0f32; 3], 0.0f32);
        for i in pixels {
            let p = image.get(x + (i as i64 % width) as u32, y + (i as i64 / width) as u32);
            sum = [0, 1, 2].map(|c| sum[c] + f32::from(p[c]));
            n += 1.0;
        }
        (n > 0.0).then(|| sum.map(|v| v / n))
    };
    let reach = (ROUND * iod).round() as i64;
    let round = stroke.iter().flat_map(|&i| {
        let (sx, sy) = (i as i64 % width, i as i64 / width);
        [(reach, 0), (-reach, 0), (0, reach), (0, -reach)].into_iter().filter_map(move |(dx, dy)| {
            let (nx, ny) = (sx + dx, sy + dy);
            (nx >= 0 && ny >= 0 && nx < width && ny < height).then_some((ny * width + nx) as usize)
        })
    });
    let (Some(hair), Some(skin)) = (mean(&mut stroke.iter().copied()), mean(&mut round.filter(|&n| on(n) && !dark[n]))) else {
        return false;
    };
    let saturation = |p: [f32; 3]| (p[0] - p[2]) / (p[0] + p[1] + p[2]).max(1.0);
    saturation(hair) - saturation(skin) > SHADED
}

/// Whether the pixels of `stroke` (indices in a row `width` wide) are a
/// hair's: their length along the way they run, and their width across it.
fn long_and_thin(stroke: &[usize], width: usize, iod: f32) -> bool {
    let n = stroke.len() as f32;
    if n < LONG * iod {
        return false;
    }
    let at = |i: usize| ((i % width) as f32, (i / width) as f32);
    let (mx, my) = stroke.iter().fold((0.0, 0.0), |(sx, sy), &i| (sx + at(i).0, sy + at(i).1));
    let (mx, my) = (mx / n, my / n);
    let (mut xx, mut xy, mut yy) = (0.0, 0.0, 0.0);
    for &i in stroke {
        let (dx, dy) = (at(i).0 - mx, at(i).1 - my);
        (xx, xy, yy) = (xx + dx * dx, xy + dx * dy, yy + dy * dy);
    }
    let (xx, xy, yy) = (xx / n, xy / n, yy / n);
    // The spread along its longest axis, as a straight stroke's length.
    let along = (xx + yy) / 2.0 + (((xx - yy) / 2.0).powi(2) + xy * xy).sqrt();
    let length = (12.0 * along).sqrt();
    let thickness = n / length;
    length >= LONG * iod && thickness <= THIN * iod && length >= SLENDER * thickness
}

/// How far past a tile the pixels round its hairs are taken from.
const PAD: u32 = 16;
/// The blur that parts the skin's fine texture from its tone, and how far
/// beside a hair it's taken from, in pixels.
const GRAIN: f32 = 2.0;
const BESIDE: u32 = 10;

/// A "Stray Hairs" layer above the layer at `above`, with `hairs` in
/// `image` filled smoothly from the skin at their edges, with the skin's
/// fine texture from beside them on top, masked to them, at `amount`
/// (0–100, its opacity). Returns its id, or `None` with no hairs.
pub fn add_layer(doc: &mut Document, above: usize, image: &Raster, hairs: &Selection, amount: f32) -> Option<u64> {
    hairs.bounds()?;
    let (w, h) = (image.width(), image.height());
    let pixels = Tiled::from_tiles(w, h, [0; 4], |col, row| {
        let (tx, ty) = (col * TILE, row * TILE);
        let (x0, y0) = (tx.saturating_sub(PAD), ty.saturating_sub(PAD));
        let (x1, y1) = ((tx + TILE + PAD).min(w), (ty + TILE + PAD).min(h));
        let (cw, ch) = ((x1 - x0) as usize, (y1 - y0) as usize);
        let at = |i: usize| (x0 + (i % cw) as u32, y0 + (i / cw) as u32);
        let hole: Vec<bool> = (0..cw * ch).map(|i| hairs.at(at(i).0, at(i).1) > 0.0).collect();
        let in_tile = |i: usize| {
            let (x, y) = at(i);
            x >= tx && y >= ty && x < tx + TILE && y < ty + TILE
        };
        if !(0..hole.len()).any(|i| hole[i] && in_tile(i)) {
            return None;
        }
        let rgb: Vec<[f32; 3]> = (0..cw * ch)
            .map(|i| image.get(at(i).0, at(i).1))
            .map(|p| [0, 1, 2].map(|c| f32::from(p[c])))
            .collect();
        let filled = poisson::membrane_fill(cw, ch, &rgb, &hole);
        // The skin's fine texture, from just beside each hole, on top.
        let grain: Vec<[f32; 3]> = {
            let soft = blur_buffer(rgb.iter().map(|p| [p[0], p[1], p[2], 0.0]).collect(), cw, ch, GRAIN);
            rgb.iter().zip(&soft).map(|(p, s)| [p[0] - s[0], p[1] - s[1], p[2] - s[2]]).collect()
        };
        let beside = |i: usize| {
            let (x, y) = ((i % cw) as i64, (i / cw) as i64);
            let o = BESIDE as i64;
            [(o, 0), (-o, 0), (0, o), (0, -o), (o, o), (-o, -o), (o, -o), (-o, o)]
                .into_iter()
                .find_map(|(dx, dy)| {
                    let (nx, ny) = (x + dx, y + dy);
                    let n = (ny * cw as i64 + nx) as usize;
                    (nx >= 0 && ny >= 0 && nx < cw as i64 && ny < ch as i64 && !hole[n]).then(|| grain[n])
                })
        };
        let filled: Vec<[f32; 3]> = (0..filled.len())
            .map(|i| match beside(i).filter(|_| hole[i]) {
                Some(g) => [0, 1, 2].map(|c| filled[i][c] + g[c]),
                None => filled[i],
            })
            .collect();
        let mut tile = vec![[0; 4]; (TILE * TILE) as usize];
        for (i, f) in filled.iter().enumerate().filter(|&(i, _)| hole[i] && in_tile(i)) {
            let (x, y) = at(i);
            let v = f.map(|c| c.clamp(0.0, MAX).round() as u16);
            tile[((y - ty) * TILE + x - tx) as usize] = [v[0], v[1], v[2], u16::MAX];
        }
        Some(tile)
    });
    let id = doc.next_layer_id();
    let mut layer = Layer::from_pixels(id, "Stray Hairs", pixels);
    layer.mask = Some(Mask {
        pixels: hairs.coverage.clone(),
        enabled: true,
    });
    layer.opacity = amount / 100.0;
    doc.insert_above(above, layer);
    Some(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::ColorProfile;

    const W: u32 = 400;
    const H: u32 = 300;
    /// Eyes 300 px apart: a hair is 15 px long and under 3.6 px wide.
    const IOD: f32 = 300.0;
    const SKIN: [f32; 3] = [45000.0, 36000.0, 32000.0];

    /// Skin with a little grain, a thin dark hair across it from (50, 80)
    /// to (250, 140), a wide soft groove (skin in shadow) along y 220, and
    /// darker skin right of x 330.
    fn image() -> Raster {
        let pixels = (0..W * H)
            .map(|i| {
                let (x, y) = ((i % W) as f32, (i / W) as f32);
                let grain = if (i % 7) % 3 == 0 { 600.0 } else { 0.0 };
                let shade = 1.0 - 0.25 * (-((y - 220.0) / 5.0).powi(2)).exp() - if x > 330.0 { 0.3 } else { 0.0 };
                let mut p = SKIN.map(|c| c * shade + grain);
                if (50.0..250.0).contains(&x) && (y - (80.0 + 0.3 * (x - 50.0))).abs() < 1.0 {
                    p = [20000.0, 17000.0, 15000.0];
                }
                [p[0] as u16, p[1] as u16, p[2] as u16, u16::MAX]
            })
            .collect();
        Raster::new(W, H, pixels)
    }

    fn none() -> Selection {
        Selection::from_coverage(Tiled::new(W, H, 0))
    }

    #[test]
    fn a_hair_is_found_and_a_groove_and_an_edge_are_not() {
        let found = find(&image(), &Selection::all(W, H), &none(), IOD);
        assert!(found.at(150, 110) > 0.9, "{}", found.at(150, 110));
        assert_eq!(found.at(150, 220), 0.0);
        assert_eq!(found.at(331, 150), 0.0);
        // Nor where it's told to stay away from.
        let avoid = Selection::rectangle(W, H, (0.0, 0.0), (W as f32, H as f32));
        assert!(find(&image(), &Selection::all(W, H), &avoid, IOD).is_empty());
    }

    #[test]
    fn the_layer_puts_skin_over_the_hair_at_its_amount_and_nothing_else() {
        let image = image();
        let mut doc = Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16);
        let hairs = find(&image, &Selection::all(W, H), &none(), IOD);
        let id = add_layer(&mut doc, 0, &image, &hairs, 80.0).unwrap();
        let layer = doc.layer(id).unwrap();
        assert_eq!((layer.name.as_str(), layer.opacity), ("Stray Hairs", 0.8));
        doc.layer_mut(id).unwrap().opacity = 1.0;
        let after = doc.composite();
        let red = f32::from(after.get(150, 110)[0]);
        assert!((red - SKIN[0]).abs() < 2500.0, "{red}");
        assert_eq!(after.get(150, 200), image.get(150, 200));
        assert!(add_layer(&mut doc, 0, &image, &none(), 80.0).is_none());
    }
}
