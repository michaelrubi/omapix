//! Even Tone (docs/AI.md, feature 5): dodge & burn, done for you within a
//! skin mask.
//!
//! ```text
//! uneven = blur(luminance, small) − blur(luminance, large)
//! ```
//!
//! Where a patch of skin is darker than the skin round it, the Dodge curve's
//! mask is opened by as much as brings it level, and where it's lighter, the
//! Burn curve's: the curves-based Dodge & Burn setup
//! ([`ops::dodge_and_burn_curves`]) with its masks painted for you, and the
//! group's opacity for how much of it shows. Both blurs only see skin, and
//! their sizes are fractions of the distance between the eyes.
//!
//! The face's shape stays: light and shade larger than the large blur are
//! left alone, and what's smaller (a highlight, the shadow under the jaw)
//! only moves a little (`LIMIT`), while unevenness, which is slight, goes
//! entirely.
//!
//! It's all worked out on a small copy of the image, with the eyes at most
//! `WORKING_IOD` pixels apart: unevenness has no fine detail, and the
//! masks are limited to the skin at full size.

use rayon::prelude::*;

use crate::adjust::{Prepared, luminance};
use crate::document::Document;
use crate::filters::blur_buffer;
use crate::layer::Mask;
use crate::ops;
use crate::raster::{Pixel, Raster};
use crate::selection::Selection;
use crate::tiled::{TILE, Tiled};

const MAX: f32 = u16::MAX as f32;

/// The distance between the eyes in the small copy, at most.
const WORKING_IOD: f32 = 80.0;

/// The small blur's standard deviation, as a fraction of the distance
/// between the eyes: just under Smooth Skin's coarse blur, so the two meet.
const SMALL: f32 = 0.04;

/// The most a patch is lightened or darkened, of the whole range: patches
/// further from the skin round them are light and shade (a highlight on a
/// cheekbone, the shadow under the jaw), and only move this far.
const LIMIT: f32 = 0.08;

/// Where less than this much of what the large blur sees is skin, nothing's
/// evened, rising to all of it where it's all skin: the edge of a face is
/// in shade, which isn't unevenness.
const EDGE: f32 = 0.5;

/// Even Tone's settings, each 0–100.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Evening {
    /// The group's opacity: how much of the unevenness goes.
    pub amount: f32,
    /// How large the patches evened out are.
    pub size: f32,
}

impl Default for Evening {
    fn default() -> Self {
        Self { amount: 60.0, size: 50.0 }
    }
}

impl Evening {
    /// The small and large blurs' standard deviations, in pixels, for eyes
    /// `iod` apart: the large one 0.15 of it at 50. At 0.3 the patches
    /// evened are as large as a cheek, and the face starts to look flat.
    pub fn radii(&self, iod: f32) -> [f32; 2] {
        [SMALL * iod, 0.075 * 4f32.powf(self.size / 100.0) * iod]
    }
}

/// The skin of an image and how far to open the Dodge and Burn masks on it,
/// on a small copy of the part of the image with skin in it.
pub struct Tone {
    /// The copy's top left corner in the image, and the image's pixels to
    /// each of its own.
    origin: (u32, u32),
    step: u32,
    pub width: u32,
    pub height: u32,
    /// The image, shrunk.
    image: Vec<Pixel>,
    /// How much of each pixel is skin.
    skin: Vec<f32>,
    /// How far the Dodge and the Burn masks open, 0–1, before the skin
    /// limits them.
    masks: Vec<[f32; 2]>,
}

impl Tone {
    /// The skin of `image` shrunk, for eyes `iod` apart, evened with blurs
    /// of `radii` (see [`Evening::radii`]). `None` if there's no skin.
    pub fn new(image: &Raster, skin: &Selection, iod: f32, radii: [f32; 2]) -> Option<Self> {
        let [x, y, w, h] = skin.bounds()?;
        let step = ((iod / WORKING_IOD).ceil() as u32).max(1);
        let (width, height) = (w.div_ceil(step), h.div_ceil(step));
        let (x1, y1) = (x + w, y + h);
        let shrunk: Vec<(Pixel, f32)> = (0..width * height)
            .into_par_iter()
            .map(|i| {
                let (bx, by) = (x + i % width * step, y + i / width * step);
                let (mut sum, mut covered, mut n) = ([0u64; 4], 0.0, 0);
                for py in by..(by + step).min(y1) {
                    for px in bx..(bx + step).min(x1) {
                        let p = image.get(px, py);
                        for c in 0..4 {
                            sum[c] += u64::from(p[c]);
                        }
                        covered += skin.at(px, py) * f32::from(p[3]) / MAX;
                        n += 1;
                    }
                }
                (sum.map(|s| (s / n) as u16), covered / n as f32)
            })
            .collect();
        let (image, skin) = shrunk.into_iter().unzip();
        let mut tone = Self {
            origin: (x, y),
            step,
            width,
            height,
            image,
            skin,
            masks: Vec::new(),
        };
        tone.even(radii);
        Some(tone)
    }

    /// Where the image's pixel (`x`, `y`) is in the copy.
    pub fn at(&self, x: f32, y: f32) -> (f32, f32) {
        let step = self.step as f32;
        ((x - self.origin.0 as f32) / step, (y - self.origin.1 as f32) / step)
    }

    /// Work the masks out again, for blurs of `radii`.
    pub fn even(&mut self, radii: [f32; 2]) {
        let (w, h) = (self.width as usize, self.height as usize);
        let weighted: Vec<[f32; 4]> = (self.image.par_iter().zip(&self.skin))
            .map(|(p, &m)| [f32::from(p[0]) / MAX * m, f32::from(p[1]) / MAX * m, f32::from(p[2]) / MAX * m, m])
            .collect();
        let [small, large] = radii.map(|r| r / self.step as f32);
        let blurred_small = blur_buffer(weighted.clone(), w, h, small);
        let blurred_large = blur_buffer(weighted, w, h, large);
        let [dodge, burn] = ops::dodge_and_burn_adjustments().map(|a| a.prepare());
        self.masks = (blurred_small.par_iter().zip(&blurred_large))
            .map(|(s, l)| {
                if s[3] < 1e-4 || l[3] < 1e-4 {
                    return [0.0; 2];
                }
                let here = [s[0] / s[3], s[1] / s[3], s[2] / s[3]];
                let level = luminance(here);
                let uneven = level - luminance([l[0], l[1], l[2]]) / l[3];
                // Near the skin's edge the large blur only sees one side.
                let inside = ((l[3] - EDGE) / (1.0 - EDGE)).clamp(0.0, 1.0);
                let uneven = LIMIT * (uneven / LIMIT).tanh() * inside;
                // How far each curve moves this skin at full strength.
                let reach = |curve: &Prepared| (luminance(curve.apply(here)) - level).abs().max(1e-4);
                if uneven < 0.0 {
                    [(-uneven / reach(&dodge)).min(1.0), 0.0]
                } else {
                    [0.0, (uneven / reach(&burn)).min(1.0)]
                }
            })
            .collect();
    }

    /// The part `[x, y, width, height]` of the copy as the Dodge & Burn
    /// group shows it at `amount` (0–1), row by row: at 0, as it was.
    pub fn evened(&self, [x, y, w, h]: [u32; 4], amount: f32) -> Vec<Pixel> {
        let [dodge, burn] = ops::dodge_and_burn_adjustments().map(|a| a.prepare());
        (0..w * h)
            .into_par_iter()
            .map(|i| {
                let at = ((y + i / w) * self.width + x + i % w) as usize;
                let (p, m) = (self.image[at], self.masks[at]);
                let (curve, open) = if m[0] > 0.0 { (&dodge, m[0]) } else { (&burn, m[1]) };
                let before = [p[0], p[1], p[2]].map(|v| f32::from(v) / MAX);
                let after = curve.apply(before);
                let t = open * self.skin[at] * amount;
                let mix = |c: usize| ((before[c] + (after[c] - before[c]) * t) * MAX).round() as u16;
                [mix(0), mix(1), mix(2), p[3]]
            })
            .collect()
    }

    /// The Dodge (0) or Burn (1) mask at the image's size, limited to
    /// `skin`.
    fn mask(&self, which: usize, skin: &Selection) -> Tiled<u16> {
        let (x0, y0) = self.origin;
        let (w, h) = (self.width, self.height);
        let step = self.step as f32;
        let (x1, y1) = (x0 + w * self.step, y0 + h * self.step);
        Tiled::from_tiles(skin.width(), skin.height(), 0, |col, row| {
            let (tx, ty) = (col * TILE, row * TILE);
            let no_skin = skin.coverage.fill() == 0 && skin.coverage.tile(col, row).is_none();
            if no_skin || tx >= x1 || ty >= y1 || tx + TILE <= x0 || ty + TILE <= y0 {
                return None;
            }
            let mut tile = vec![0; (TILE * TILE) as usize];
            for py in ty.max(y0)..(ty + TILE).min(y1).min(skin.height()) {
                for px in tx.max(x0)..(tx + TILE).min(x1).min(skin.width()) {
                    // Between the middles of the copy's pixels.
                    let at = |p: u32, origin: u32, size: u32| {
                        let v = ((p - origin) as f32 + 0.5) / step - 0.5;
                        let i = (v.max(0.0) as u32).min(size - 1);
                        (i, (i + 1).min(size - 1), (v - i as f32).clamp(0.0, 1.0))
                    };
                    let ((ax, bx, fx), (ay, by, fy)) = (at(px, x0, w), at(py, y0, h));
                    let m = |x: u32, y: u32| self.masks[(y * w + x) as usize][which];
                    let top = m(ax, ay) + (m(bx, ay) - m(ax, ay)) * fx;
                    let bottom = m(ax, by) + (m(bx, by) - m(ax, by)) * fx;
                    let open = top + (bottom - top) * fy;
                    tile[((py - ty) * TILE + px - tx) as usize] = (open * skin.at(px, py) * MAX).round() as u16;
                }
            }
            Some(tile)
        })
    }
}

/// A "Dodge & Burn" group above the layer at `above`, as
/// [`ops::dodge_and_burn_curves`] makes it, with `tone`'s masks limited to
/// `skin`, at `amount` (0–100) as the group's opacity. Returns the group's
/// id.
pub fn add_layers(doc: &mut Document, above: usize, tone: &Tone, skin: &Selection, amount: f32) -> u64 {
    let (dodge, burn) = ops::dodge_and_burn_curves(doc, above);
    for (which, id) in [dodge, burn].into_iter().enumerate() {
        let pixels = tone.mask(which, skin);
        doc.layer_mut(id).expect("just added").mask = Some(Mask { pixels, enabled: true });
    }
    let group = doc.layer(dodge).and_then(|l| l.parent).expect("in its group");
    doc.layer_mut(group).expect("just added").opacity = amount / 100.0;
    group
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::ColorProfile;

    const W: u32 = 400;
    const H: u32 = 300;

    /// Eyes 80 px apart: the copy is the image's own size, and at 50 the
    /// blurs are 3.2 and 12 px.
    const IOD: f32 = 80.0;

    const DARK: (u32, u32) = (120, 150);
    const LIGHT: (u32, u32) = (280, 150);

    /// Skin lit from the left (lighter by a tenth across the image), with a
    /// soft dark patch and a soft light one, and a dark "brow" band across
    /// y 40–60 that isn't skin.
    fn image() -> Raster {
        let pixels = (0..W * H)
            .map(|i| {
                let (x, y) = (i % W, i / W);
                if (40..60).contains(&y) {
                    return [8000, 6000, 5000, u16::MAX];
                }
                let patch = |(cx, cy): (u32, u32)| 3000.0 * (-((x as f32 - cx as f32).hypot(y as f32 - cy as f32) / 8.0).powi(2)).exp();
                let v = |base: f32| (base - x as f32 * 16.0 - patch(DARK) + patch(LIGHT)) as u16;
                [v(45000.0), v(36000.0), v(32000.0), u16::MAX]
            })
            .collect();
        Raster::new(W, H, pixels)
    }

    /// Everything below the brow.
    fn skin() -> Selection {
        Selection::rectangle(W, H, (0.0, 60.0), (W as f32, H as f32))
    }

    fn tone(image: &Raster) -> Tone {
        Tone::new(image, &skin(), IOD, Evening::default().radii(IOD)).unwrap()
    }

    #[test]
    fn at_fifty_the_large_blur_is_three_twentieths_of_the_eyes_distance() {
        let [small, large] = Evening::default().radii(100.0);
        assert!((small - 4.0).abs() < 0.01 && (large - 15.0).abs() < 0.01, "{small} {large}");
        let larger = Evening { size: 100.0, ..Default::default() }.radii(100.0);
        assert!((larger[0] - small).abs() < 0.01 && (larger[1] - 30.0).abs() < 0.01);
    }

    #[test]
    fn patches_come_level_and_the_light_across_the_face_stays() {
        let image = image();
        let mut doc = Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16);
        let group = add_layers(&mut doc, 0, &tone(&image), &skin(), 100.0);
        let group = doc.layer(group).unwrap();
        assert_eq!((group.name.as_str(), group.opacity), ("Dodge & Burn", 1.0));
        let after = doc.composite();
        let red = |image: &Raster, (x, y): (u32, u32)| f32::from(image.get(x, y)[0]);
        // Each patch is a good deal nearer the skin beside it, which
        // hardly moves.
        for (patch, sign) in [(DARK, -1.0), (LIGHT, 1.0)] {
            let beside = (patch.0, patch.1 + 60);
            let off = |image: &Raster| (red(image, patch) - red(image, beside)) * sign;
            assert!(off(&image) > 2900.0 && off(&after) < 0.6 * off(&image), "{} {}", off(&image), off(&after));
            assert!((red(&after, beside) - red(&image, beside)).abs() < 150.0);
        }
        // The light from the left is as it was.
        let across = |image: &Raster| red(image, (60, 250)) - red(image, (340, 250));
        assert!((across(&after) - across(&image)).abs() < 200.0, "{} {}", across(&after), across(&image));
        // The brow isn't skin: left alone.
        assert_eq!(after.get(200, 50), image.get(200, 50));
    }

    #[test]
    fn dodge_opens_on_dark_patches_and_burn_on_light_ones_within_the_skin() {
        let image = image();
        let mut doc = Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16);
        add_layers(&mut doc, 0, &tone(&image), &skin(), 60.0);
        let mask = |name: &str| &doc.layers.iter().find(|l| l.name == name).unwrap().mask.as_ref().unwrap().pixels;
        let (dodge, burn) = (mask("Dodge"), mask("Burn"));
        assert!(dodge.get(DARK.0, DARK.1) > 8000 && burn.get(DARK.0, DARK.1) == 0);
        assert!(burn.get(LIGHT.0, LIGHT.1) > 8000 && dodge.get(LIGHT.0, LIGHT.1) == 0);
        // Grey, to paint on: nowhere near fully open.
        assert!(dodge.get(DARK.0, DARK.1) < 40000);
        // Nothing off the skin, and no tiles where there's none.
        assert_eq!((dodge.get(200, 50), burn.get(200, 50)), (0, 0));
        let none = Selection::rectangle(W, H, (0.0, 0.0), (100.0, 100.0));
        let mut doc = Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16);
        let tone = Tone::new(&image, &none, IOD, Evening::default().radii(IOD)).unwrap();
        add_layers(&mut doc, 0, &tone, &none, 60.0);
        assert!(doc.layers.iter().filter_map(|l| l.mask.as_ref()).all(|m| m.pixels.tile(1, 1).is_none()));
        assert!(Tone::new(&image, &Selection::from_coverage(Tiled::new(W, H, 0)), IOD, [3.2, 12.0]).is_none());
    }

    #[test]
    fn light_and_shade_move_no_further_than_the_limit() {
        // A highlight far brighter than the skin round it.
        let pixels = (0..W * H)
            .map(|i| {
                let d = ((i % W) as f32 - 200.0).hypot((i / W) as f32 - 150.0);
                let v = 30000 + (25000.0 * (-(d / 8.0).powi(2)).exp()) as u16;
                [v, v, v, u16::MAX]
            })
            .collect();
        let image = Raster::new(W, H, pixels);
        let skin = Selection::all(W, H);
        let tone = Tone::new(&image, &skin, IOD, Evening::default().radii(IOD)).unwrap();
        let mut doc = Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16);
        add_layers(&mut doc, 0, &tone, &skin, 100.0);
        let moved = f32::from(image.get(200, 150)[0]) - f32::from(doc.composite().get(200, 150)[0]);
        assert!(moved > 0.5 * LIMIT * MAX && moved < 1.05 * LIMIT * MAX, "{moved}");
    }

    #[test]
    fn the_copy_evened_is_what_the_layers_show() {
        let image = image();
        let tone = tone(&image);
        // The skin's part of the image, at its own size.
        assert_eq!((tone.width, tone.height), (W, H - 60));
        assert_eq!(tone.at(120.0, 150.0), (120.0, 90.0));
        let mut doc = Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16);
        add_layers(&mut doc, 0, &tone, &skin(), 60.0);
        let (after, evened) = (doc.composite(), tone.evened([100, 50, 200, 100], 0.6));
        for (x, y) in [DARK, LIGHT, (200, 200), (100, 110)] {
            let (a, b) = (after.get(x, y), evened[((y - 110) * 200 + x - 100) as usize]);
            assert!((0..3).all(|c| a[c].abs_diff(b[c]) <= 24), "{a:?} {b:?} at {x}, {y}");
        }
        assert_ne!(evened[40 * 200 + 20], image.get(DARK.0, DARK.1));
        assert_eq!(tone.evened([100, 50, 200, 100], 0.0)[40 * 200 + 20], image.get(DARK.0, DARK.1));
        // Shrunk, for eyes further apart: three of the image's pixels to
        // each of its own.
        let small = Tone::new(&image, &skin(), 3.0 * IOD, Evening::default().radii(3.0 * IOD)).unwrap();
        assert_eq!((small.width, small.height), (W.div_ceil(3), (H - 60) / 3));
        assert_eq!(small.at(120.0, 150.0), (40.0, 30.0));
    }
}
