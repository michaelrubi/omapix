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

/// A "Smooth Skin" layer above the layer at `above`, made from `image`
/// (what that layer and those below it show) with the mid band taken out,
/// masked by `skin`, at `smoothing`'s amount for eyes `iod` apart. Returns
/// the new layer's id, or `None` if there's no skin.
pub fn add_layer(doc: &mut Document, above: usize, image: &Raster, skin: &Selection, iod: f32, smoothing: &Smoothing) -> Option<u64> {
    let bounds @ [x, y, w, h] = skin.bounds()?;
    let smoothed = smooth(image, skin, bounds, smoothing.radii(iod));
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
    let mut layer = Layer::from_pixels(id, "Smooth Skin", pixels);
    layer.mask = Some(Mask {
        pixels: skin.coverage.clone(),
        enabled: true,
    });
    layer.opacity = smoothing.amount / 100.0;
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
        let more = Smoothing { smoothness: 100.0, detail: 100.0, ..Default::default() }.radii(IOD);
        assert!(more[0] > fine && more[1] > coarse);
        // Never coarser texture than blotches: then nothing's smoothed.
        let [fine, coarse] = Smoothing { smoothness: 0.0, detail: 100.0, ..Default::default() }.radii(IOD);
        assert_eq!(fine, coarse);
    }

    #[test]
    fn blotches_go_pores_stay_and_the_brow_neither_moves_nor_bleeds() {
        let image = image();
        let smoothed = smooth(&image, &skin(), [0, 0, W, H], [2.0, 12.0]);
        let at = |x: u32, y: u32| f32::from(smoothed[(y * W + x) as usize][0]);
        let before = |x: u32, y: u32| f32::from(image.get(x, y)[0]);
        // The blotch's middle comes up most of the way to the skin round it.
        let (middle, round) = (at(150, 150) + at(151, 150) + at(152, 150) + at(153, 150), at(300, 150) + at(301, 150) + at(302, 150) + at(303, 150));
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
    fn the_layer_is_masked_by_the_skin_at_the_amount_and_hides_to_the_before() {
        let image = image();
        let mut doc = Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16);
        let skin = Selection::rectangle(W, H, (100.0, 100.0), (200.0, 200.0));
        let id = add_layer(&mut doc, 0, &image, &skin, IOD, &Smoothing::default()).unwrap();
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
        assert_eq!(add_layer(&mut doc, 0, &image, &none, IOD, &Smoothing::default()), None);
    }
}
