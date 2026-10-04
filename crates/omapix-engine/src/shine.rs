//! Reduce Shine (docs/AI.md, milestone 13): the highlights on skin taken
//! down, within a skin mask.
//!
//! ```text
//! here   = blur(image, fine)            the skin without its texture
//! around = blur(image, wide)            the lit skin round it
//! excess = luminance(here) − luminance(around)
//! ```
//!
//! `around` leaves out the darker half of the skin nearby, so a hot spot is
//! measured against the lit skin it sits on, not the shaded side of the
//! face. Skin a little lighter than that is the face's shape (a cheekbone,
//! the bridge of the nose) and is left alone. What's lighter by
//! more than `KNEE` is taken for shine, and only the part over the knee:
//! the layer is the image with `here` replaced by `around` (the texture
//! stays, and the paler colour of a highlight goes with its lightness), its
//! mask is the share of the excess that's over the knee, and its opacity is
//! how much of that goes. So at full opacity a hot spot is as light as the
//! knee allows and no darker than its edges. Both blurs only see skin, and
//! their sizes are fractions of the distance between the eyes.

use rayon::prelude::*;

use crate::adjust::luminance;
use crate::document::Document;
use crate::filters::blur_buffer;
use crate::layer::{Layer, Mask};
use crate::raster::{Pixel, Raster};
use crate::selection::Selection;
use crate::tiled::Tiled;

const MAX: f32 = u16::MAX as f32;

/// The layer's name.
pub const NAME: &str = "Reduce Shine";

/// The fine blur's standard deviation, as a fraction of the distance
/// between the eyes: Smooth Skin's, so pores stay.
const FINE: f32 = 0.02;
/// The wide blur's: the skin round a patch of shine. At 0.2 and at 0.35
/// the same hot spots are found, a little larger at 0.35.
const WIDE: f32 = 0.3;
/// Skin this much darker than the skin round it (of the whole range)
/// isn't counted as the lit skin round anything: it's in shade.
const SHADE: f32 = 0.03;
/// How much lighter than the lit skin round it (of the whole range) skin
/// can be before it's shine: what's under this is the face's shape.
const KNEE: f32 = 0.03;

/// 0 below `from`, 1 above `to`, and straight between.
fn ramp([from, to]: [f32; 2], v: f32) -> f32 {
    ((v - from) / (to - from)).clamp(0.0, 1.0)
}

/// What a Reduce Shine layer is made of: the part of the image with skin in
/// it, with the skin's colour evened to that round it, and how much of each
/// pixel is shine.
pub struct Matte {
    /// Left, top, width and height in the image.
    bounds: [u32; 4],
    pixels: Vec<Pixel>,
    shine: Vec<u16>,
}

impl Matte {
    /// The shine on `skin` in `image`, for eyes `iod` apart. `None` if
    /// there's no skin.
    pub fn new(image: &Raster, skin: &Selection, iod: f32) -> Option<Self> {
        let bounds @ [x, y, w, h] = skin.bounds()?;
        // Enough round it for the blurs to see what they would in the
        // whole image.
        let pad = (WIDE * iod * 3.0).ceil() as u32;
        let (x0, y0) = (x.saturating_sub(pad), y.saturating_sub(pad));
        let (x1, y1) = ((x + w + pad).min(image.width()), (y + h + pad).min(image.height()));
        let (pw, ph) = ((x1 - x0) as usize, (y1 - y0) as usize);
        let mut weighted = vec![[0f32; 4]; pw * ph];
        weighted.par_chunks_mut(pw).enumerate().for_each(|(row, out)| {
            let py = y0 + row as u32;
            for (px, v) in (x0..x1).zip(out) {
                let p = image.get(px, py);
                let m = skin.at(px, py) * f32::from(p[3]) / MAX;
                *v = [f32::from(p[0]) / MAX * m, f32::from(p[1]) / MAX * m, f32::from(p[2]) / MAX * m, m];
            }
        });
        // A blurred colour, where there's skin to speak of.
        let colour = |v: &[f32; 4]| (v[3] > 1e-4).then(|| [v[0] / v[3], v[1] / v[3], v[2] / v[3]]);
        let here = blur_buffer(weighted.clone(), pw, ph, FINE * iod);
        let all_round = blur_buffer(weighted.clone(), pw, ph, WIDE * iod);
        // The skin round each pixel again, without what's darker than the
        // first says: a patch of shine is on lit skin, and it's that it
        // stands out from, not the shaded side of the face.
        weighted.par_iter_mut().zip(&here).zip(&all_round).for_each(|((v, here), round)| {
            if let (Some(here), Some(round)) = (colour(here), colour(round)) {
                let lit = ramp([-SHADE, 0.0], luminance(here) - luminance(round));
                *v = v.map(|c| c * lit);
            }
        });
        let around = blur_buffer(weighted, pw, ph, WIDE * iod);
        let made: Vec<(Pixel, u16)> = (0..w * h)
            .into_par_iter()
            .map(|i| {
                let (px, py) = (x + i % w, y + i / w);
                let p = image.get(px, py);
                let at = ((py - y0) as usize) * pw + (px - x0) as usize;
                let (Some(here), Some(all_round)) = (colour(&here[at]), colour(&all_round[at])) else {
                    return (p, 0);
                };
                let round = colour(&around[at]).unwrap_or(all_round);
                let excess = luminance(here) - luminance(round);
                let shine = if excess > KNEE { (excess - KNEE) / excess } else { 0.0 };
                let v = |c: usize| ((f32::from(p[c]) / MAX - here[c] + round[c]) * MAX).round().clamp(0.0, MAX) as u16;
                ([v(0), v(1), v(2), p[3]], (shine * skin.at(px, py) * MAX).round() as u16)
            })
            .collect();
        let (pixels, shine) = made.into_iter().unzip();
        Some(Self { bounds, pixels, shine })
    }

    /// Whether there's any shine to speak of.
    pub fn is_empty(&self) -> bool {
        self.shine.iter().all(|&s| s < u16::MAX / 50)
    }
}

/// A "Reduce Shine" layer above the layer at `above`, made from `matte`, at
/// `amount` (0–100, its opacity). Returns the new layer's id, or `None` if
/// there's no shine.
pub fn add_layer(doc: &mut Document, above: usize, matte: &Matte, amount: f32) -> Option<u64> {
    if matte.is_empty() {
        return None;
    }
    let [x, y, w, h] = matte.bounds;
    let (dx, dy) = (x as i32, y as i32);
    let id = doc.next_layer_id();
    let pixels = Tiled::from_slice(w, h, [0; 4], &matte.pixels).reframed(doc.width, doc.height, dx, dy, [0; 4]);
    let mut layer = Layer::from_pixels(id, NAME, pixels);
    layer.mask = Some(Mask {
        pixels: Tiled::from_slice(w, h, 0, &matte.shine).reframed(doc.width, doc.height, dx, dy, 0),
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
    const IOD: f32 = 100.0;

    const HOT: (u32, u32) = (120, 150);
    const GENTLE: (u32, u32) = (280, 150);

    /// Skin lit on the left 300 px and in shade on the right 100, with a
    /// hot spot (a fifth of the range lighter, and paler) and a gentle
    /// highlight (a fiftieth lighter) on the lit side, and a fine texture
    /// all over.
    fn image() -> Raster {
        let pixels = (0..W * H)
            .map(|i| {
                let (x, y) = (i % W, i / W);
                let spot = |(cx, cy): (u32, u32), size: f32| (-((x as f32 - cx as f32).hypot(y as f32 - cy as f32) / size).powi(2)).exp();
                let (hot, gentle) = (13000.0 * spot(HOT, 12.0), 1300.0 * spot(GENTLE, 12.0));
                let lit = if x < 300 { 1.0 } else { 0.6 };
                let texture = if (x + y) % 2 == 0 { 600.0 } else { -600.0 };
                let v = |base: f32, pale: f32| ((base + gentle) * lit + hot * pale + texture) as u16;
                [v(42000.0, 1.0), v(33000.0, 1.3), v(28000.0, 1.5), u16::MAX]
            })
            .collect();
        Raster::new(W, H, pixels)
    }

    fn document() -> (Document, Matte) {
        let image = image();
        let matte = Matte::new(&image, &Selection::all(W, H), IOD).unwrap();
        (Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16), matte)
    }

    #[test]
    fn the_mask_opens_on_a_hot_spot_and_not_on_the_faces_shape() {
        let (mut doc, matte) = document();
        let id = add_layer(&mut doc, 0, &matte, 100.0).unwrap();
        let mask = Selection::from_mask(&doc.layer(id).unwrap().mask.as_ref().unwrap().pixels);
        assert!(mask.at(HOT.0, HOT.1) > 0.7, "{}", mask.at(HOT.0, HOT.1));
        // Less towards its edge, and nothing on the gentle highlight, the
        // lit skin, the shade, or the lit skin beside the shade.
        assert!(mask.at(HOT.0 + 10, HOT.1) < mask.at(HOT.0, HOT.1) && mask.at(HOT.0 + 30, HOT.1) < 0.01);
        for (x, y) in [GENTLE, (200, 60), (350, 150), (290, 250)] {
            assert!(mask.at(x, y) < 0.01, "{x} {y}: {}", mask.at(x, y));
        }
    }

    #[test]
    fn the_layer_takes_a_hot_spot_down_to_the_knee_and_no_darker_than_its_edge() {
        let (mut doc, matte) = document();
        let before = doc.composite();
        let id = add_layer(&mut doc, 0, &matte, 50.0).unwrap();
        let layer = doc.layer(id).unwrap();
        assert_eq!((layer.name.as_str(), layer.opacity), ("Reduce Shine", 0.5));
        // Averaged over the texture's two pixels.
        let level = |image: &Raster, (x, y): (u32, u32), c: usize| (i32::from(image.get(x, y)[c]) + i32::from(image.get(x + 1, y)[c])) / 2;
        let (was, skin) = (level(&before, HOT, 0), level(&before, (200, 60), 0));
        let half = level(&doc.composite(), HOT, 0);
        assert!(half < was - 3000 && half > skin, "{was} {half} {skin}");
        // The lit skin and the shade are as they were.
        for at in [(200, 60), (350, 150)] {
            assert_eq!(doc.composite().get(at.0, at.1), before.get(at.0, at.1));
        }
        doc.layer_mut(id).unwrap().opacity = 1.0;
        let after = doc.composite();
        let full = level(&after, HOT, 0);
        // Still a little lighter than the skin round it, and than its own
        // edge: a highlight, not a hole.
        assert!(full < half && full > skin + 500 && full < skin + 4000, "{full} {skin}");
        assert!(full >= level(&after, (HOT.0 + 14, HOT.1), 0), "{full} {}", level(&after, (HOT.0 + 14, HOT.1), 0));
        // Its paleness goes with it: blue comes down by more of what it
        // was over the skin than red does.
        let over = |image: &Raster, c: usize| (level(image, HOT, c) - level(image, (200, 60), c)) as f32;
        assert!(over(&after, 2) / over(&before, 2) < 0.5, "{} {}", over(&after, 2), over(&before, 2));
        // The texture stays.
        let texture = |image: &Raster| i32::from(image.get(HOT.0, HOT.1)[0]) - i32::from(image.get(HOT.0 + 1, HOT.1)[0]);
        assert!((texture(&after) - texture(&before)).abs() < 300, "{} {}", texture(&after), texture(&before));
    }

    #[test]
    fn skin_with_no_hot_spots_or_no_skin_gets_no_layer() {
        let flat = Raster::new(W, H, vec![[42000, 33000, 28000, u16::MAX]; (W * H) as usize]);
        let matte = Matte::new(&flat, &Selection::all(W, H), IOD).unwrap();
        assert!(matte.is_empty());
        let mut doc = Document::from_image("t.tif".into(), &flat, ColorProfile::srgb(), 16);
        assert_eq!(add_layer(&mut doc, 0, &matte, 50.0), None);
        assert_eq!(doc.layers.len(), 1);
        assert!(Matte::new(&flat, &Selection::from_coverage(Tiled::new(W, H, 0)), IOD).is_none());
        // Only skin is looked at: a hot spot off it is left alone.
        let skin = Selection::rectangle(W, H, (200.0, 0.0), (W as f32, H as f32));
        assert!(Matte::new(&image(), &skin, IOD).unwrap().is_empty());
    }
}
