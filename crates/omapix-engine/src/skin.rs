//! Smooth Skin (docs/AI.md, feature 3): frequency separation in three
//! bands, done for you within a skin mask.
//!
//! ```text
//! high = image − blur(fine)             pores, fine hair: kept
//! mid  = blur(fine) − blur(coarse)      blotches, bumps, uneven skin: removed
//! low  = blur(coarse)                   colour and overall shape: kept
//! ```
//!
//! The result is `low + high`, on a layer masked by the skin, whose
//! opacity is how much of the mid band goes. Both blurs only see skin (a
//! blur of the image weighted by the mask, divided by the blurred mask), so
//! brows, lips and hair don't bleed into it. Their sizes are fractions of
//! the distance between the eyes, so the same settings suit a headshot and
//! a full-length portrait.

use rayon::prelude::*;

use crate::blemish::Spot;
use crate::document::Document;
use crate::filters::blur_buffer;
use crate::layer::{Layer, Mask};
use crate::raster::{Pixel, Raster};
use crate::selection::Selection;
use crate::tiled::{TILE, Tiled};

const MAX: f32 = u16::MAX as f32;

/// Smooth Skin's settings, each 0–100.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Smoothing {
    /// The layer's opacity: how much of the blotchiness goes.
    pub amount: f32,
    /// How large the blotches evened out are.
    pub smoothness: f32,
    /// How coarse the texture kept is.
    pub detail: f32,
}

impl Default for Smoothing {
    fn default() -> Self {
        Self {
            amount: 70.0,
            smoothness: 50.0,
            detail: 50.0,
        }
    }
}

impl Smoothing {
    /// The fine and coarse blurs' standard deviations, in pixels, for eyes
    /// `iod` apart: at 50, 0.02 (just above pores) and 0.05 of it. Coarser
    /// than about 0.1, the shading that gives the face its shape goes too:
    /// the sides of the nose, the cheekbones' highlights.
    pub fn radii(&self, iod: f32) -> [f32; 2] {
        let fine = 0.01 * 4f32.powf(self.detail / 100.0);
        let coarse = (0.025 * 4f32.powf(self.smoothness / 100.0)).max(fine);
        [fine * iod, coarse * iod]
    }
}

/// The part `[x, y, width, height]` of `image` with its mid band taken out
/// within `skin`, for blurs of `radii` (see [`Smoothing::radii`]), row by
/// row. Where there's no skin nearby, it's left as it was.
pub fn smooth(image: &Raster, skin: &Selection, [x, y, w, h]: [u32; 4], [fine, coarse]: [f32; 2]) -> Vec<Pixel> {
    // Enough round it for the blurs to see what they would in the whole
    // image.
    let pad = (coarse * 3.0).ceil() as u32;
    let (x0, y0) = (x.saturating_sub(pad), y.saturating_sub(pad));
    let (x1, y1) = ((x + w + pad).min(image.width()), (y + h + pad).min(image.height()));
    let pw = (x1 - x0) as usize;
    let mut weighted = vec![[0f32; 4]; pw * (y1 - y0) as usize];
    weighted.par_chunks_mut(pw).enumerate().for_each(|(row, out)| {
        let py = y0 + row as u32;
        for (px, v) in (x0..x1).zip(out) {
            let p = image.get(px, py);
            let m = skin.at(px, py) * f32::from(p[3]) / MAX;
            *v = [f32::from(p[0]) * m, f32::from(p[1]) * m, f32::from(p[2]) * m, m];
        }
    });
    let ph = (y1 - y0) as usize;
    let blurred_fine = blur_buffer(weighted.clone(), pw, ph, fine);
    let blurred_coarse = blur_buffer(weighted, pw, ph, coarse);
    (0..h * w)
        .into_par_iter()
        .map(|i| {
            let (px, py) = (x + i % w, y + i / w);
            let p = image.get(px, py);
            let at = ((py - y0) as usize) * pw + (px - x0) as usize;
            let (f, c) = (blurred_fine[at], blurred_coarse[at]);
            if f[3] < 1e-4 || c[3] < 1e-4 {
                return p;
            }
            let v = |k: usize| (f32::from(p[k]) - f[k] / f[3] + c[k] / c[3]).round().clamp(0.0, MAX) as u16;
            [v(0), v(1), v(2), p[3]]
        })
        .collect()
}

/// How far a patch reaches from a spot, at least, as a fraction of the
/// distance between the eyes.
const PATCH: f32 = 0.12;

/// Soft patches of `skin` round `spots`, for eyes `iod` apart: all of it on
/// a spot, fading out with distance. This is how a retoucher works on the
/// mid band, a patch at a time, rather than over all the skin.
pub fn patches(skin: &Selection, spots: &[Spot], iod: f32) -> Selection {
    let c = &skin.coverage;
    let reach = |s: &Spot| (s.heal_size() / 2.0).max(PATCH * iod);
    Selection::from_coverage(Tiled::from_tiles(c.width(), c.height(), 0, |col, row| {
        let (tx, ty) = ((col * TILE) as f32, (row * TILE) as f32);
        let side = TILE as f32;
        let near: Vec<_> = (spots.iter())
            .filter(|s| {
                let (dx, dy) = ((tx - s.x).max(s.x - tx - side).max(0.0), (ty - s.y).max(s.y - ty - side).max(0.0));
                dx.hypot(dy) < 2.0 * reach(s)
            })
            .collect();
        let skin_tile = c.tile(col, row);
        if near.is_empty() || (skin_tile.is_none() && c.fill() == 0) {
            return None;
        }
        let tile = (0..(TILE * TILE) as usize)
            .map(|i| {
                let (x, y) = (tx + (i as u32 % TILE) as f32, ty + (i as u32 / TILE) as f32);
                let strength = (near.iter())
                    .map(|s| {
                        let d = (x - s.x).hypot(y - s.y) / reach(s);
                        if d < 2.0 { (-d * d).exp() } else { 0.0 }
                    })
                    .fold(0.0, f32::max);
                let covered = skin_tile.map_or(c.fill(), |t| t[i]);
                (f32::from(covered) * strength) as u16
            })
            .collect();
        Some(tile)
    }))
}

/// The fine and coarse blurs' standard deviations for eyes `iod` apart, as
/// in hand retouches: 1.6 and 8 px with the eyes about 200 px apart, the
/// coarse one about 13 px at 500, so they grow more slowly than the face.
pub fn hand_radii(iod: f32) -> [f32; 2] {
    let coarse = 0.57 * iod.sqrt();
    [coarse / 5.0, coarse]
}

/// `image`'s lightness in `area`'s box and `pad` pixels round it: the box
/// (left, top, width, height) and, to blur, [L; 4] for each pixel.
fn lightness(image: &Raster, [x, y, w, h]: [u32; 4], pad: u32) -> ([u32; 4], Vec<[f32; 4]>) {
    let (x0, y0) = (x.saturating_sub(pad), y.saturating_sub(pad));
    let (x1, y1) = ((x + w + pad).min(image.width()), (y + h + pad).min(image.height()));
    let (pw, ph) = (x1 - x0, y1 - y0);
    let light = (0..pw * ph)
        .into_par_iter()
        .map(|i| {
            let p = image.get(x0 + i % pw, y0 + i / pw);
            let l = 0.2126 * f32::from(p[0]) + 0.7152 * f32::from(p[1]) + 0.0722 * f32::from(p[2]);
            [l; 4]
        })
        .collect();
    ([x0, y0, pw, ph], light)
}

/// `values`' median, or `None` with none.
fn median(mut values: Vec<f32>) -> Option<f32> {
    let mid = values.len() / 2;
    (!values.is_empty()).then(|| *values.select_nth_unstable_by(mid, f32::total_cmp).1)
}

/// 0 at or below `low`, 1 at or above `high`, smoothly between.
fn step(low: f32, high: f32, v: f32) -> f32 {
    let t = ((v - low) / (high - low)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// How far a line's mask is softened, as a fraction of the distance between
/// the eyes.
const LINE_SOFTEN: f32 = 0.012;

/// The lines in `area`: grooves darker than the skin round them by more
/// than most of it varies, softened as strokes along them are. Pores and
/// skin between lines are left, for eyes `iod` apart.
pub fn lines(image: &Raster, area: &Selection, iod: f32) -> Selection {
    let (w, h) = (area.width(), area.height());
    let Some(bounds) = area.bounds() else {
        return Selection::from_coverage(Tiled::new(w, h, 0));
    };
    let [fine, coarse] = hand_radii(iod);
    let ([x0, y0, pw, ph], light) = lightness(image, bounds, (coarse * 3.0).ceil() as u32);
    let (pw, ph) = (pw as usize, ph as usize);
    let (f, c) = (blur_buffer(light.clone(), pw, ph, fine), blur_buffer(light, pw, ph, coarse));
    let mid: Vec<f32> = f.iter().zip(&c).map(|(f, c)| f[0] - c[0]).collect();
    let on_area = |i: usize| area.at(x0 + (i % pw) as u32, y0 + (i / pw) as u32) > 0.5;
    let spread = median((0..mid.len()).filter(|&i| on_area(i)).map(|i| mid[i].abs()).collect())
        .unwrap_or(1.0)
        .max(1.0);
    let grooves = mid.iter().map(|m| [step(spread, 2.0 * spread, -m); 4]).collect();
    let grooves = blur_buffer(grooves, pw, ph, LINE_SOFTEN * iod);
    let mut coverage = vec![0u16; (w * h) as usize];
    for (i, g) in grooves.iter().enumerate() {
        let (x, y) = (x0 + (i % pw) as u32, y0 + (i / pw) as u32);
        coverage[(y * w + x) as usize] = ((g[0] * 2.0).min(1.0) * area.at(x, y) * MAX) as u16;
    }
    Selection::from_coverage(Tiled::from_slice(w, h, 0, &coverage))
}

/// How far fine texture and lightness are averaged, as a fraction of the
/// distance between the eyes; how many times `reference`'s roughness it
/// takes to be stubble, from starting to being it; and what fraction of its
/// lightness, from being stubble to not: scars are as rough as stubble, but
/// not as dark.
const STUBBLE_REACH: f32 = 0.1;
const ROUGH: [f32; 2] = [1.3, 1.8];
const DARK: [f32; 2] = [0.8, 0.92];

/// `area` less what's rougher and darker than `reference`, bare skin on the
/// same face: a beard or stubble, which has to stay as it is.
pub fn without_stubble(image: &Raster, area: &Selection, reference: &Selection, iod: f32) -> Selection {
    let (w, h) = (area.width(), area.height());
    let (Some(a), Some(r)) = (area.bounds(), reference.bounds()) else {
        return area.clone();
    };
    let (l, t) = (a[0].min(r[0]), a[1].min(r[1]));
    let (right, bottom) = ((a[0] + a[2]).max(r[0] + r[2]), (a[1] + a[3]).max(r[1] + r[3]));
    let [fine, _] = hand_radii(iod);
    let ([x0, y0, pw, ph], light) = lightness(image, [l, t, right - l, bottom - t], 0);
    let (pw, ph) = (pw as usize, ph as usize);
    let blurred = blur_buffer(light.clone(), pw, ph, fine);
    // Roughness and lightness, averaged.
    let both = light.iter().zip(&blurred).map(|(l, b)| [(l[0] - b[0]).abs(), l[0], 0.0, 0.0]).collect();
    let both = blur_buffer(both, pw, ph, STUBBLE_REACH * iod);
    let at = |i: usize| (x0 + (i % pw) as u32, y0 + (i / pw) as u32);
    let bare = |c: usize| {
        let on_reference = (0..both.len()).filter(|&i| {
            let (x, y) = at(i);
            reference.at(x, y) > 0.5
        });
        median(on_reference.map(|i| both[i][c]).collect()).filter(|&m| m > 0.0)
    };
    let (Some(smooth), Some(lit)) = (bare(0), bare(1)) else {
        return area.clone();
    };
    let mut coverage = vec![0u16; (w * h) as usize];
    for (i, v) in both.iter().enumerate() {
        let (x, y) = at(i);
        let rough = step(ROUGH[0] * smooth, ROUGH[1] * smooth, v[0]);
        let dark = 1.0 - step(DARK[0] * lit, DARK[1] * lit, v[1]);
        coverage[(y * w + x) as usize] = ((1.0 - rough * dark) * area.at(x, y) * MAX) as u16;
    }
    Selection::from_coverage(Tiled::from_slice(w, h, 0, &coverage))
}

/// A "Smooth Skin" layer above the layer at `above`, made from `image`
/// (what that layer and those below it show) with the mid band of `skin`
/// taken out, masked by `mask`, at `smoothing`'s amount for eyes `iod`
/// apart. Returns the new layer's id, or `None` if the mask is empty.
pub fn add_layer(doc: &mut Document, above: usize, image: &Raster, skin: &Selection, mask: &Selection, iod: f32, smoothing: &Smoothing) -> Option<u64> {
    add_band_layer(doc, above, "Smooth Skin", image, skin, mask, smoothing.radii(iod), smoothing.amount)
}

/// A layer called `name` above the layer at `above`, made from `image`
/// with what's between the blurs `radii` of `skin` taken out, masked by
/// `mask`, at `amount` % opacity. A fine blur of 0 takes out the fine
/// texture too. Returns the new layer's id, or `None` if the mask is empty.
#[allow(clippy::too_many_arguments)]
pub fn add_band_layer(
    doc: &mut Document,
    above: usize,
    name: &str,
    image: &Raster,
    skin: &Selection,
    mask: &Selection,
    radii: [f32; 2],
    amount: f32,
) -> Option<u64> {
    let bounds @ [x, y, w, h] = mask.bounds()?;
    let smoothed = smooth(image, skin, bounds, radii);
    let pixels = Tiled::from_tiles(image.width(), image.height(), [0; 4], |col, row| {
        let (tx, ty) = (col * TILE, row * TILE);
        if tx >= x + w || ty >= y + h || tx + TILE <= x || ty + TILE <= y {
            return None;
        }
        let mut tile = vec![[0; 4]; (TILE * TILE) as usize];
        for py in ty.max(y)..(ty + TILE).min(y + h) {
            for px in tx.max(x)..(tx + TILE).min(x + w) {
                tile[((py - ty) * TILE + px - tx) as usize] = smoothed[((py - y) * w + px - x) as usize];
            }
        }
        Some(tile)
    });
    let id = doc.next_layer_id();
    let mut layer = Layer::from_pixels(id, name, pixels);
    layer.mask = Some(Mask {
        pixels: mask.coverage.clone(),
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

    /// Skin with fine grain (pores), a large soft blotch round (150, 150),
    /// and a dark "brow" band across y 40–60 that isn't skin.
    fn image() -> Raster {
        let pixels = (0..W * H)
            .map(|i| {
                let (x, y) = (i % W, i / W);
                if (40..60).contains(&y) {
                    return [8000, 6000, 5000, u16::MAX];
                }
                let grain = if (x + y * 3) % 4 == 0 { 1500.0 } else { 0.0 };
                let d = (x as f32 - 150.0).hypot(y as f32 - 150.0);
                let blotch = 6000.0 * (-(d / 7.0).powi(2)).exp();
                let v = |base: f32| (base + grain - blotch) as u16;
                [v(45000.0), v(36000.0), v(32000.0), u16::MAX]
            })
            .collect();
        Raster::new(W, H, pixels)
    }

    /// Everything but the brow.
    fn skin() -> Selection {
        let mut skin = Selection::rectangle(W, H, (0.0, 60.0), (W as f32, H as f32));
        skin = skin.combine(&Selection::rectangle(W, H, (0.0, 0.0), (W as f32, 40.0)), crate::Combine::Add);
        skin
    }

    /// Eyes 100 px apart: the blurs are 2 and 5 px.
    const IOD: f32 = 100.0;

    #[test]
    fn at_fifty_the_blurs_are_a_fiftieth_and_a_twentieth_of_the_eyes_distance() {
        let [fine, coarse] = Smoothing::default().radii(IOD);
        assert!((fine - 2.0).abs() < 0.01 && (coarse - 5.0).abs() < 0.01, "{fine} {coarse}");
        let more = Smoothing {
            smoothness: 100.0,
            detail: 100.0,
            ..Default::default()
        }
        .radii(IOD);
        assert!(more[0] > fine && more[1] > coarse);
        // Never coarser texture than blotches: then nothing's smoothed.
        let [fine, coarse] = Smoothing {
            smoothness: 0.0,
            detail: 100.0,
            ..Default::default()
        }
        .radii(IOD);
        assert_eq!(fine, coarse);
    }

    #[test]
    fn blotches_go_pores_stay_and_the_brow_neither_moves_nor_bleeds() {
        let image = image();
        let smoothed = smooth(&image, &skin(), [0, 0, W, H], [2.0, 12.0]);
        let at = |x: u32, y: u32| f32::from(smoothed[(y * W + x) as usize][0]);
        let before = |x: u32, y: u32| f32::from(image.get(x, y)[0]);
        // The blotch's middle comes up most of the way to the skin round it.
        let (middle, round) = (
            at(150, 150) + at(151, 150) + at(152, 150) + at(153, 150),
            at(300, 150) + at(301, 150) + at(302, 150) + at(303, 150),
        );
        assert!(round - middle < 0.35 * 4.0 * 6000.0, "{middle} {round}");
        assert!(before(300, 150) * 4.0 - (before(150, 150) + before(151, 150) + before(152, 150) + before(153, 150)) > 4.0 * 5000.0);
        // The grain is still there, as strong.
        let grain = |f: &dyn Fn(u32, u32) -> f32| f(300, 200) - f(301, 200);
        assert!((grain(&at) - grain(&before)).abs() < 150.0, "{} {}", grain(&at), grain(&before));
        // The brow isn't skin: left alone, and the skin beside it isn't
        // darkened by it.
        assert_eq!(smoothed[(50 * W + 300) as usize], image.get(300, 50));
        assert!((at(300, 64) - before(300, 64)).abs() < 400.0, "{} {}", at(300, 64), before(300, 64));
    }

    #[test]
    fn a_part_comes_out_as_it_does_in_the_whole() {
        let (image, skin) = (image(), skin());
        let radii = Smoothing::default().radii(IOD);
        let whole = smooth(&image, &skin, [0, 0, W, H], radii);
        let part = smooth(&image, &skin, [120, 130, 60, 40], radii);
        for y in 0..40 {
            for x in 0..60 {
                let (a, b) = (part[(y * 60 + x) as usize], whole[((130 + y) * W + 120 + x) as usize]);
                assert!((0..3).all(|c| a[c].abs_diff(b[c]) <= 40), "{a:?} {b:?} at {x}, {y}");
            }
        }
    }

    #[test]
    fn patches_are_strongest_on_a_spot_fade_out_and_stay_on_the_skin() {
        let skin = Selection::rectangle(300, 300, (0.0, 0.0), (150.0, 300.0));
        let spot = |x: f32| Spot {
            x,
            y: 100.0,
            radius: 4.0,
            score: 20.0,
            mole: false,
        };
        let patches = patches(&skin, &[spot(100.0), spot(148.0)], 100.0);
        assert!(patches.at(100, 100) > 0.99);
        let (near, far) = (patches.at(110, 100), patches.at(100, 118));
        assert!(near < 0.99 && far < near && far > 0.0, "{near} {far}");
        assert_eq!(patches.at(100, 130), 0.0);
        assert_eq!(patches.at(151, 100), 0.0);
        assert_eq!(super::patches(&skin, &[], 100.0).bounds(), None);
    }

    #[test]
    fn a_layer_masked_to_patches_stops_at_them_and_keeps_the_pores() {
        let (mut doc, image, skin) = (Document::from_image("t.tif".into(), &image(), ColorProfile::srgb(), 16), image(), skin());
        let spot = Spot {
            x: 150.0,
            y: 150.0,
            radius: 4.0,
            score: 20.0,
            mole: false,
        };
        let patch = patches(&skin, &[spot], IOD);
        add_layer(&mut doc, 0, &image, &skin, &patch, IOD, &Smoothing::default()).unwrap();
        let after = doc.composite();
        assert_ne!(after.get(150, 150), image.get(150, 150));
        assert_eq!(after.get(60, 60), image.get(60, 60));
    }

    #[test]
    fn hand_radii_grow_more_slowly_than_the_face() {
        let [fine, coarse] = hand_radii(200.0);
        assert!((fine - 1.6).abs() < 0.1 && (coarse - 8.0).abs() < 0.2, "{fine} {coarse}");
        assert!((hand_radii(516.0)[1] - 12.9).abs() < 0.3);
    }

    #[test]
    fn lines_finds_a_groove_and_leaves_the_skin_round_it() {
        let (w, h) = (300, 200);
        let image = Raster::new(
            w,
            h,
            (0..w * h)
                .map(|i| {
                    let (x, y) = (i % w, i / w);
                    let grain = ((x * 7 + y * 13) % 5) * 60;
                    let v = if (y as i32 - 100).abs() < 2 { 30000 } else { 40000 };
                    [(v + grain) as u16, (v + grain) as u16, (v + grain) as u16, u16::MAX]
                })
                .collect(),
        );
        let found = lines(&image, &Selection::all(w, h), 200.0);
        assert!(found.at(150, 100) > 0.8, "{}", found.at(150, 100));
        assert!(found.at(150, 40) < 0.1, "{}", found.at(150, 40));
    }

    #[test]
    fn without_stubble_keeps_bare_skin_and_drops_what_is_rough() {
        let (w, h) = (300, 200);
        let image = Raster::new(
            w,
            h,
            (0..w * h)
                .map(|i| {
                    let (x, y) = (i % w, i / w);
                    let rough = if y >= 100 {
                        ((x * 31 + y * 17) % 9 * 900) as i32 - 9000
                    } else {
                        ((x * 7 + y * 13) % 5 * 40) as i32
                    };
                    [(30000 + rough) as u16, (30000 + rough) as u16, (30000 + rough) as u16, u16::MAX]
                })
                .collect(),
        );
        let bare = Selection::rectangle(w, h, (0.0, 0.0), (300.0, 50.0));
        let kept = without_stubble(&image, &Selection::all(w, h), &bare, 200.0);
        assert!(kept.at(150, 60) > 0.9, "{}", kept.at(150, 60));
        assert!(kept.at(150, 170) < 0.1, "{}", kept.at(150, 170));
    }

    #[test]
    fn the_layer_is_masked_by_the_skin_at_the_amount_and_hides_to_the_before() {
        let image = image();
        let mut doc = Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16);
        let skin = Selection::rectangle(W, H, (100.0, 100.0), (200.0, 200.0));
        let id = add_layer(&mut doc, 0, &image, &skin, &skin, IOD, &Smoothing::default()).unwrap();
        let layer = doc.layer(id).unwrap();
        assert_eq!((layer.name.as_str(), layer.opacity), ("Smooth Skin", 0.7));
        assert_eq!(doc.index_of(id), Some(1));
        let mask = &layer.mask.as_ref().unwrap().pixels;
        assert_eq!((mask.get(150, 150), mask.get(50, 50)), (u16::MAX, 0));
        // Only the skin's tiles have pixels.
        assert!(layer.pixels.tile(1, 0).is_none());
        let composite = doc.composite();
        assert!(composite.get(150, 150)[0] > image.get(150, 150)[0] + 1000);
        assert_eq!(composite.get(50, 250), image.get(50, 250));
        // No skin, no layer.
        let none = Selection::from_coverage(Tiled::new(W, H, 0));
        assert_eq!(add_layer(&mut doc, 0, &image, &none, &none, IOD, &Smoothing::default()), None);
    }
}
