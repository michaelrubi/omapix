//! Finding blemishes on skin and healing them (docs/AI.md, feature 4).
//!
//! The finder is classical, not a model: spots are small patches darker or
//! redder than the skin round them. It works at a scale where the eyes are
//! about [`WORKING_IOD`] pixels apart, in CIE Lab, comparing a lightly
//! blurred image with a heavily blurred one. Both blurs only see skin, so
//! brows, lips and hair don't count as spots. The differences are measured
//! against how much the skin varies as a rule, so smooth, oily and textured
//! skin all use the same threshold. Round blobs of spot size are the spots.

use rayon::prelude::*;

use crate::brush::{BrushSettings, Paint, Stroke, Surface};
use crate::document::Document;
use crate::filters::blur_buffer;
use crate::layer::Layer;
use crate::selection::Selection;
use crate::tiled::Tiled;

/// A spot found on skin, in image pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spot {
    pub x: f32,
    pub y: f32,
    pub radius: f32,
    /// How much it stands out: its peak difference from the skin round it,
    /// in multiples of the skin's usual variation.
    pub score: f32,
}

impl Spot {
    /// The diameter of the Spot Healing dab that covers it: its found
    /// outline is where it stands out most, and redness spreads further.
    pub fn heal_size(&self) -> f32 {
        self.radius * 3.0 + 4.0
    }
}

/// The distance between the eyes, in pixels, that spots are looked for at.
const WORKING_IOD: f32 = 150.0;
/// The lighter blur, taking out noise and pores, and the heavier one, the
/// skin round a spot. Fractions of the distance between the eyes.
const FINE: f32 = 0.006;
const SURROUNDINGS: f32 = 0.12;
/// Where a spot's outline is drawn.
const OUTLINE: f32 = 3.0;
/// The least score a spot can have. Below it, on real faces, is mostly
/// pores and texture.
pub const LEAST_SCORE: f32 = 4.0;
/// Spot radii, as fractions of the distance between the eyes.
const RADII: std::ops::RangeInclusive<f32> = 0.008..=0.06;
/// How far from round a spot can be: the ratio of its longest axis to its
/// shortest. Stray hairs and wrinkles are longer.
const LONGEST: f32 = 3.0;

/// The spots on `skin` in `srgb` (8-bit sRGB, the same size), for faces
/// with eyes `iod` pixels apart. Most prominent first.
pub fn find(srgb: &[[u8; 4]], skin: &Selection, iod: f32) -> Vec<Spot> {
    let w = skin.width() as usize;
    // Only the box round the skin is looked at.
    let Some([bx, by, bw, bh]) = skin.bounds() else {
        return Vec::new();
    };
    let (bx, by, x_end, y_end) = (bx as usize, by as usize, (bx + bw) as usize, (by + bh) as usize);
    // Each working pixel is k × k image pixels.
    let k = (iod / WORKING_IOD).floor().max(1.0) as usize;
    let (gw, gh) = ((bw as usize).div_ceil(k), (bh as usize).div_ceil(k));
    let iod = iod / k as f32;
    if gw < 3 || gh < 3 || iod < 10.0 {
        return Vec::new();
    }

    // Lab and skin coverage at the working size: [L·m, a·m, m, 0], ready
    // to blur with skin weighting the average.
    let linear: Vec<f32> = (0..256).map(|v| to_linear(v as f32 / 255.0)).collect();
    let weighted: Vec<[f32; 4]> = (0..gw * gh)
        .into_par_iter()
        .map(|i| {
            let (x0, y0) = (bx + i % gw * k, by + i / gw * k);
            let (x1, y1) = ((x0 + k).min(x_end), (y0 + k).min(y_end));
            let mut rgb = [0.0f32; 3];
            for y in y0..y1 {
                for p in &srgb[y * w + x0..y * w + x1] {
                    for c in 0..3 {
                        rgb[c] += linear[p[c] as usize];
                    }
                }
            }
            let n = ((x1 - x0) * (y1 - y0)) as f32;
            let [l, a] = lab(rgb.map(|v| v / n));
            let m = skin.at(((x0 + x1) / 2) as u32, ((y0 + y1) / 2) as u32);
            [l * m, a * m, m, 0.0]
        })
        .collect();
    let normalised = |blurred: Vec<[f32; 4]>| -> Vec<[f32; 2]> {
        blurred
            .into_iter()
            .map(|[l, a, m, _]| if m > 1e-3 { [l / m, a / m] } else { [0.0; 2] })
            .collect()
    };
    let fine = normalised(blur_buffer(weighted.clone(), gw, gh, (FINE * iod).max(0.7)));
    let round = normalised(blur_buffer(weighted.clone(), gw, gh, SURROUNDINGS * iod));
    let skin: Vec<f32> = weighted.iter().map(|p| p[2]).collect();

    // Darker and redder than the surroundings.
    let diff: Vec<[f32; 2]> = fine.iter().zip(&round).map(|(f, r)| [r[0] - f[0], f[1] - r[1]]).collect();
    // The skin's usual variation, from the median absolute difference.
    let spread = |c: usize| {
        let mut all: Vec<f32> = diff.iter().zip(&skin).filter(|(_, m)| **m > 0.9).map(|(d, _)| d[c].abs()).collect();
        if all.is_empty() {
            return 1.0;
        }
        let mid = all.len() / 2;
        (*all.select_nth_unstable_by(mid, f32::total_cmp).1).max(0.2)
    };
    let (dark, red) = (spread(0), spread(1));
    let score: Vec<f32> = diff
        .iter()
        .zip(&skin)
        .map(|(d, m)| {
            if *m < 0.5 {
                return 0.0;
            }
            (d[0].max(0.0) / dark).hypot(d[1].max(0.0) / red)
        })
        .collect();

    let mut spots = Vec::new();
    let mut seen = vec![false; gw * gh];
    for start in 0..gw * gh {
        if seen[start] || score[start] < OUTLINE {
            continue;
        }
        let blob = flood(&score, &mut seen, gw, gh, start);
        let Some(spot) = measure(&blob, &score, &skin, gw, gh, iod) else {
            continue;
        };
        let k = k as f32;
        spots.push(Spot {
            x: bx as f32 + (spot.x + 0.5) * k,
            y: by as f32 + (spot.y + 0.5) * k,
            radius: spot.radius * k,
            score: spot.score,
        });
    }
    spots.sort_by(|a, b| b.score.total_cmp(&a.score));
    spots
}

/// The pixels joined to `start` (8-connected) that score at least
/// [`OUTLINE`], marking them seen.
fn flood(score: &[f32], seen: &mut [bool], gw: usize, gh: usize, start: usize) -> Vec<usize> {
    let mut blob = Vec::new();
    let mut stack = vec![start];
    seen[start] = true;
    while let Some(i) = stack.pop() {
        blob.push(i);
        let (x, y) = ((i % gw) as isize, (i / gw) as isize);
        for (dx, dy) in [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)] {
            let (nx, ny) = (x + dx, y + dy);
            if nx < 0 || ny < 0 || nx >= gw as isize || ny >= gh as isize {
                continue;
            }
            let j = ny as usize * gw + nx as usize;
            if !seen[j] && score[j] >= OUTLINE {
                seen[j] = true;
                stack.push(j);
            }
        }
    }
    blob
}

/// `blob` as a spot, in working pixels, if it's spot-sized, round enough,
/// and has skin all round it.
fn measure(blob: &[usize], score: &[f32], skin: &[f32], gw: usize, gh: usize, iod: f32) -> Option<Spot> {
    let radius = (blob.len() as f32 / std::f32::consts::PI).sqrt();
    let peak = blob.iter().map(|&i| score[i]).fold(0.0, f32::max);
    if !RADII.contains(&(radius / iod)) || peak < LEAST_SCORE {
        return None;
    }
    let n = blob.len() as f32;
    let at = |i: usize| ((i % gw) as f32, (i / gw) as f32);
    let (cx, cy) = blob.iter().map(|&i| at(i)).fold((0.0, 0.0), |(sx, sy), (x, y)| (sx + x / n, sy + y / n));
    // Second moments: the spread along its longest and shortest axes.
    let (mut xx, mut yy, mut xy) = (0.0, 0.0, 0.0);
    for &i in blob {
        let (x, y) = at(i);
        xx += (x - cx).powi(2) / n;
        yy += (y - cy).powi(2) / n;
        xy += (x - cx) * (y - cy) / n;
    }
    let (mid, half) = ((xx + yy) / 2.0, (((xx - yy) / 2.0).powi(2) + xy * xy).sqrt());
    if mid + half > LONGEST.powi(2) * (mid - half).max(0.25) {
        return None;
    }
    // Skin all round: not at the edge of the face, a feature or the hair.
    let ring = radius * 2.0 + 0.02 * iod;
    for step in 0..12 {
        let angle = step as f32 / 12.0 * std::f32::consts::TAU;
        let (x, y) = (cx + angle.cos() * ring, cy + angle.sin() * ring);
        if x < 0.0 || y < 0.0 || x >= gw as f32 || y >= gh as f32 || skin[y as usize * gw + x as usize] < 0.5 {
            return None;
        }
    }
    Some(Spot {
        x: cx,
        y: cy,
        radius,
        score: peak,
    })
}

fn to_linear(v: f32) -> f32 {
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

/// CIE L* and a* (D65) of linear sRGB.
fn lab([r, g, b]: [f32; 3]) -> [f32; 2] {
    let f = |t: f32| if t > 0.008856 { t.cbrt() } else { 7.787 * t + 16.0 / 116.0 };
    let x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047;
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let (fx, fy) = (f(x), f(y));
    [116.0 * fy - 16.0, 500.0 * (fx - fy)]
}


/// Heal `spots` onto a new empty "Blemishes" layer above the layer at
/// `above`, each with one Spot Healing dab sampling the image below it, so
/// the layer's opacity fades them, erasing brings one back, and hiding it
/// shows the before. Returns the new layer's id.
pub fn heal(doc: &mut Document, above: usize, spots: &[Spot]) -> u64 {
    // What's below, as Sample Current & Below sees it: the groups the layer
    // is in stay visible.
    let mut below = doc.clone();
    let layer = doc.layers[above].id;
    for l in &mut below.layers[above + 1..] {
        l.visible = doc.is_inside(layer, l.id) && l.visible;
    }
    let source = Tiled::from_raster(&below.composite());
    let id = doc.next_layer_id();
    let at = doc.insert_above(above, Layer::empty(id, "Blemishes", doc.width, doc.height));
    let mut surface = Surface::Pixels(doc.layers[at].pixels.clone());
    for spot in spots {
        let settings = BrushSettings {
            size: spot.heal_size(),
            hardness: 0.5,
            ..Default::default()
        };
        let mut stroke = Stroke::new(settings, Paint::SpotHeal, surface.clone()).sampling(source.clone());
        let tiles = stroke.add_point(spot.x, spot.y, 1.0);
        stroke.apply(&mut surface, &tiles);
        stroke.finish(&mut surface);
    }
    if let Surface::Pixels(pixels) = surface {
        doc.layers[at].pixels = pixels;
    }
    id
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::ColorProfile;
    use crate::raster::Raster;

    const W: u32 = 600;
    const H: u32 = 400;

    /// Skin-coloured, with gentle shading and a little fine texture.
    fn skin_at(x: u32, y: u32) -> [f32; 3] {
        let shade = x as f32 * 0.03 + y as f32 * 0.02;
        let grain = ((x * 7 + y * 13) % 5) as f32 - 2.0;
        [205.0 - shade + grain, 160.0 - shade + grain, 140.0 - shade + grain]
    }

    /// Mixes `colour` into `rgb` inside a soft disc of `radius` round (cx, cy).
    fn disc(rgb: &mut [f32; 3], (x, y): (u32, u32), (cx, cy, radius): (f32, f32, f32), colour: [f32; 3]) {
        let d = (x as f32 - cx).hypot(y as f32 - cy) / radius;
        let k = (1.5 - d).clamp(0.0, 1.0);
        for c in 0..3 {
            rgb[c] += (colour[c] - rgb[c]) * k;
        }
    }

    /// A 600 × 400 patch of skin, eyes 150 px apart, with a dark spot at
    /// (150, 100) and a red one at (400, 250), a stray hair, and a large
    /// shadow too big to be a spot.
    fn photo() -> Vec<[u8; 4]> {
        (0..W * H)
            .map(|i| {
                let (x, y) = (i % W, i / W);
                let mut rgb = skin_at(x, y);
                disc(&mut rgb, (x, y), (150.0, 100.0, 3.0), [150.0, 100.0, 90.0]);
                disc(&mut rgb, (x, y), (400.0, 250.0, 3.0), [215.0, 110.0, 110.0]);
                // A hair: dark, long and thin.
                if (250..330).contains(&x) && (y as f32 - 320.0 - x as f32 * 0.1).abs() < 1.0 {
                    rgb = [90.0, 70.0, 60.0];
                }
                disc(&mut rgb, (x, y), (480.0, 90.0, 30.0), [170.0, 125.0, 110.0]);
                [rgb[0] as u8, rgb[1] as u8, rgb[2] as u8, 255]
            })
            .collect()
    }

    fn all_skin() -> Selection {
        Selection::all(W, H)
    }

    #[test]
    fn finds_dark_and_red_spots_but_not_hairs_or_shadows() {
        let spots = find(&photo(), &all_skin(), 150.0);
        let near = |x: f32, y: f32| spots.iter().any(|s| (s.x - x).hypot(s.y - y) < 3.0);
        assert!(near(150.0, 100.0), "{spots:?}");
        assert!(near(400.0, 250.0), "{spots:?}");
        assert_eq!(spots.len(), 2, "{spots:?}");
        for s in &spots {
            assert!((1.5..6.0).contains(&s.radius), "{s:?}");
        }
    }

    #[test]
    fn spots_off_the_skin_or_at_its_edge_are_left() {
        // Skin everywhere but a box round the dark spot, whose edge passes
        // near the red one.
        let skin = Selection::rectangle(W, H, (200.0, 0.0), (406.0, H as f32))
            .combine(&Selection::rectangle(W, H, (0.0, 200.0), (406.0, H as f32)), crate::selection::Combine::Add);
        let spots = find(&photo(), &skin, 150.0);
        assert!(spots.is_empty(), "{spots:?}");

        // Skin only in a box round the red spot: found where it is.
        let skin = Selection::rectangle(W, H, (290.0, 150.0), (560.0, 380.0));
        let spots = find(&photo(), &skin, 150.0);
        assert_eq!(spots.len(), 1, "{spots:?}");
        assert!((spots[0].x - 400.0).hypot(spots[0].y - 250.0) < 3.0, "{spots:?}");
    }

    #[test]
    fn works_at_a_larger_scale() {
        // The same photo at twice the size, eyes 300 px apart.
        let small = photo();
        let big: Vec<[u8; 4]> = (0..W * H * 4)
            .map(|i| {
                let (x, y) = (i % (W * 2), i / (W * 2));
                small[(y / 2 * W + x / 2) as usize]
            })
            .collect();
        let spots = find(&big, &Selection::all(W * 2, H * 2), 300.0);
        assert_eq!(spots.len(), 2, "{spots:?}");
        assert!(spots.iter().any(|s| (s.x - 300.0).hypot(s.y - 200.0) < 6.0), "{spots:?}");
    }

    #[test]
    fn healing_goes_on_a_new_layer_and_hides_the_spot() {
        let pixels = photo().iter().map(|p| p.map(|v| u16::from(v) * 257)).collect();
        let mut doc = Document::from_image("t.tif".into(), &Raster::new(W, H, pixels), ColorProfile::srgb(), 16);
        let base = doc.layers[0].pixels.clone();
        let spots = find(&photo(), &all_skin(), 150.0);
        let id = heal(&mut doc, 0, &spots);

        assert_eq!(doc.layers.len(), 2);
        assert_eq!((doc.layers[1].id, doc.layers[1].name.as_str()), (id, "Blemishes"));
        // The photo itself is untouched, and the new layer is empty away
        // from the spots.
        assert!(doc.layers[0].pixels.same_tiles(&base));
        assert_eq!(doc.layers[1].pixels.get(300, 50)[3], 0);
        // The spots are gone: close to the skin round them.
        assert_healed(&doc);
    }

    #[test]
    fn healing_a_layer_in_a_group_samples_the_group() {
        let pixels = photo().iter().map(|p| p.map(|v| u16::from(v) * 257)).collect();
        let mut doc = Document::from_image("t.tif".into(), &Raster::new(W, H, pixels), ColorProfile::srgb(), 16);
        let group = doc.next_layer_id();
        doc.layers.push(Layer::group(group, "Group", W, H));
        doc.layers[0].parent = Some(group);
        let spots = find(&photo(), &all_skin(), 150.0);
        heal(&mut doc, 0, &spots);
        assert_eq!(doc.layers[1].name, "Blemishes");
        assert_eq!(doc.layers[1].parent, Some(group));
        assert_healed(&doc);
    }

    /// Both spots in [`photo`] are gone from `doc`.
    fn assert_healed(doc: &Document) {
        let image = doc.composite();
        for (x, y) in [(150, 100), (400, 250)] {
            let (got, want) = (image.get(x, y), skin_at(x, y));
            for c in 0..3 {
                let got = f32::from(got[c]) / 257.0;
                assert!((got - want[c]).abs() < 8.0, "({x}, {y}) channel {c}: {got} for {}", want[c]);
            }
        }
    }
}
