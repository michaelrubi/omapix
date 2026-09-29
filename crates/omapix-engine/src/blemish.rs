//! Finding blemishes on skin and healing them (docs/AI.md, feature 4).
//!
//! The finder is classical, not a model: it looks for blobs, patches darker
//! or redder than the ring of skin round them, at several sizes, as blob
//! detectors (SIFT's, for one) do. It works at a scale where the eyes are
//! about [`WORKING_IOD`] pixels apart, in CIE Lab, and every blur only sees
//! skin, so brows, lips and hair don't count. At each size, differences are
//! measured against how much the skin varies at that size, so pores and
//! stubble, which are everywhere, score low, while a pimple among them
//! stands out. Lighter alone doesn't count (oily highlights), but a raised
//! pimple lighter than dark skin counts by its redness. Creases and stray
//! hairs are long rather than round, and are left.

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
    /// How much it stands out from the skin round it, in multiples of how
    /// much the skin varies at its size.
    pub score: f32,
}

impl Spot {
    /// The diameter of the Spot Healing dab that covers it: with
    /// [`HEAL_HARDNESS`], fully over the spot, fading out a little beyond.
    pub fn heal_size(&self) -> f32 {
        self.radius * 2.6 + 4.0
    }
}

/// The healing dab's hardness: its middle 80 % covers fully.
const HEAL_HARDNESS: f32 = 0.8;

/// The distance between the eyes, in pixels, that spots are looked for at.
const WORKING_IOD: f32 = 150.0;
/// The blob sizes looked at, half an octave apart, as the standard
/// deviation of the blur that finds each, in fractions of the distance
/// between the eyes. Spots are found at all but the first and the last
/// two, which are only there to compare with: a blob that stands out most
/// at the smallest size is a pore, and at the largest, shading or a
/// flush.
const SIZES: [f32; 9] = [0.0057, 0.008, 0.0113, 0.016, 0.0226, 0.032, 0.045, 0.064, 0.09];
/// A spot's radius, in multiples of the blur that finds it best (measured
/// on discs).
const RADIUS: f32 = 1.9;
/// How much wider the ring of skin a blob is compared with is.
const SURROUND: f32 = 2.5;
/// The least score a spot can have.
pub const LEAST_SCORE: f32 = 4.0;
/// How far from round a spot can be: the ratio of its curvatures across
/// and along (SIFT uses 10; creases are longer).
const LONGEST: f32 = 6.0;

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
    let skin: Vec<f32> = weighted.iter().map(|p| p[2]).collect();
    let scores: Vec<Vec<f32>> = SIZES
        .par_iter()
        .map(|size| blob_scores(&weighted, &skin, gw, gh, size * iod))
        .collect();

    let mut found = Vec::new();
    for level in 1..SIZES.len() - 2 {
        let [below, here, above] = [&scores[level - 1], &scores[level], &scores[level + 1]];
        let sigma = SIZES[level] * iod;
        for y in 1..gh - 1 {
            for x in 1..gw - 1 {
                let i = y * gw + x;
                let v = here[i];
                if v < LEAST_SCORE {
                    continue;
                }
                // Highest of its neighbours, at its size and either side.
                let peak = [-1isize, 0, 1].iter().all(|&dy| {
                    [-1isize, 0, 1].iter().all(|&dx| {
                        let j = (i as isize + dy * gw as isize + dx) as usize;
                        below[j] <= v && above[j] <= v && (j == i || here[j] <= v)
                    })
                });
                if peak && round(here, gw, gh, x, y, sigma) {
                    // Its whole extent, redness round it and all: the
                    // biggest size at which it still stands out a third as
                    // much.
                    let widest = (level..SIZES.len()).take_while(|&l| scores[l][i] >= v / 3.0).last().unwrap_or(level);
                    // Still standing out at the biggest sizes: a flush or
                    // shading, not a spot.
                    if widest >= SIZES.len() - 2 {
                        continue;
                    }
                    // Its middle: the peak at that size, which a highlight
                    // on one side doesn't pull over.
                    let (x, y) = climb(&scores[widest], gw, gh, x, y, SIZES[widest] * iod * RADIUS);
                    found.push(Spot {
                        x: x as f32,
                        y: y as f32,
                        radius: SIZES[widest] * iod * RADIUS,
                        score: v,
                    });
                }
            }
        }
    }

    // Strongest first; one spot where the same one's found twice, but small
    // spots in a big one are kept, since one big heal leaves them.
    found.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut spots: Vec<Spot> = Vec::new();
    for spot in found {
        let overlaps = spots.iter().any(|s| (s.x - spot.x).hypot(s.y - spot.y) < s.radius.min(spot.radius));
        if !overlaps && skin_round(&skin, gw, gh, &spot, iod) {
            spots.push(spot);
        }
    }
    let k = k as f32;
    spots
        .into_iter()
        .map(|s| Spot {
            x: bx as f32 + (s.x + 0.5) * k,
            y: by as f32 + (s.y + 0.5) * k,
            radius: s.radius * k,
            score: s.score,
        })
        .collect()
}

/// How much each point stands out as a blob of Gaussian `sigma`: how much
/// darker and redder it is, blurred by `sigma`, than blurred by
/// [`SURROUND`] times as much, each against how much that varies over the
/// skin.
fn blob_scores(weighted: &[[f32; 4]], skin: &[f32], gw: usize, gh: usize, sigma: f32) -> Vec<f32> {
    let lab = |sigma: f32| -> Vec<[f32; 2]> {
        blur_buffer(weighted.to_vec(), gw, gh, sigma)
            .into_iter()
            .map(|[l, a, m, _]| if m > 1e-3 { [l / m, a / m] } else { [0.0; 2] })
            .collect()
    };
    let (centre, round) = (lab(sigma.max(0.5)), lab(sigma * SURROUND));
    // Darker, redder.
    let diff: Vec<[f32; 2]> = centre.iter().zip(&round).map(|(c, r)| [r[0] - c[0], c[1] - r[1]]).collect();
    // Darker counts, and so does lighter if it's also redder (a raised
    // pimple on dark skin), but not lighter alone (a highlight).
    let stands_out = |dark: f32, red: f32| if red > 0.0 { dark.abs().hypot(red) } else { dark.max(0.0) };
    // How much it varies, from the median absolute difference.
    let spread = |c: usize| {
        let mut all: Vec<f32> = diff.iter().zip(skin).filter(|(_, m)| **m > 0.9).map(|(d, _)| d[c].abs()).collect();
        if all.is_empty() {
            return 1.0;
        }
        let mid = all.len() / 2;
        (*all.select_nth_unstable_by(mid, f32::total_cmp).1).max(0.05)
    };
    let (dark, red) = (spread(0), spread(1));
    diff.iter()
        .zip(skin)
        .map(|(d, m)| if *m < 0.5 { 0.0 } else { stands_out(d[0] / dark, d[1] / red) })
        .collect()
}

/// Where going uphill on `score` from (x, y) leads, going at most `reach`.
fn climb(score: &[f32], gw: usize, gh: usize, mut x: usize, mut y: usize, reach: f32) -> (usize, usize) {
    let (x0, y0) = (x as f32, y as f32);
    loop {
        let mut best = (x, y);
        for (dx, dy) in [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)] {
            let (nx, ny) = (x as isize + dx, y as isize + dy);
            if nx < 0 || ny < 0 || nx as usize >= gw || ny as usize >= gh {
                continue;
            }
            let (nx, ny) = (nx as usize, ny as usize);
            if score[ny * gw + nx] > score[best.1 * gw + best.0] && (nx as f32 - x0).hypot(ny as f32 - y0) <= reach {
                best = (nx, ny);
            }
        }
        if best == (x, y) {
            return best;
        }
        (x, y) = best;
    }
}

/// Whether the peak of `score` at (x, y) is round rather than a ridge, from
/// its curvatures (SIFT's edge test), measured 1.5 `sigma` either side.
fn round(score: &[f32], gw: usize, gh: usize, x: usize, y: usize, sigma: f32) -> bool {
    let h = ((sigma * 1.5).round() as usize).max(1);
    if x < h || y < h || x + h >= gw || y + h >= gh {
        return false;
    }
    let at = |dx: isize, dy: isize| score[(y as isize + dy * h as isize) as usize * gw + (x as isize + dx * h as isize) as usize];
    let v = at(0, 0);
    let dxx = at(1, 0) + at(-1, 0) - 2.0 * v;
    let dyy = at(0, 1) + at(0, -1) - 2.0 * v;
    let dxy = (at(1, 1) + at(-1, -1) - at(1, -1) - at(-1, 1)) / 4.0;
    let (trace, det) = (dxx + dyy, dxx * dyy - dxy * dxy);
    trace < 0.0 && det > 0.0 && trace * trace / det < (LONGEST + 1.0).powi(2) / LONGEST
}

/// Whether `spot` (working pixels) is on skin, with skin all round it as
/// far as healing matches its tone to: not at the edge of the face, a
/// feature or the hair.
fn skin_round(skin: &[f32], gw: usize, gh: usize, spot: &Spot, iod: f32) -> bool {
    let on_skin = |x: f32, y: f32| x >= 0.0 && y >= 0.0 && x < gw as f32 && y < gh as f32 && skin[y as usize * gw + x as usize] >= 0.5;
    on_skin(spot.x, spot.y)
        && [spot.radius * 0.7, spot.radius * 2.5 + 0.02 * iod].iter().all(|ring| {
            (0..12).all(|step| {
                let angle = step as f32 / 12.0 * std::f32::consts::TAU;
                on_skin(spot.x + angle.cos() * ring, spot.y + angle.sin() * ring)
            })
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
            hardness: HEAL_HARDNESS,
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
            assert!((2.0..10.0).contains(&s.radius), "{s:?}");
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
    fn spots_close_together_are_each_found() {
        let photo: Vec<[u8; 4]> = (0..W * H)
            .map(|i| {
                let (x, y) = (i % W, i / W);
                let mut rgb = skin_at(x, y);
                disc(&mut rgb, (x, y), (300.0, 200.0, 3.0), [215.0, 110.0, 110.0]);
                disc(&mut rgb, (x, y), (312.0, 200.0, 3.0), [215.0, 110.0, 110.0]);
                [rgb[0] as u8, rgb[1] as u8, rgb[2] as u8, 255]
            })
            .collect();
        let spots = find(&photo, &all_skin(), 150.0);
        for x in [300.0, 312.0] {
            assert!(spots.iter().any(|s| (s.x - x).hypot(s.y - 200.0) < 3.0), "{x}: {spots:?}");
        }
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

