//! Lightening the shadows under the eyes (docs/AI.md, milestone 13), as
//! it's done by hand: a brightening Curves layer (the Dodge & Burn setup's
//! Dodge curve), its mask opened under each eye by as much as brings the
//! skin there level with the cheek below it ([`shadows`]), and its opacity
//! the amount. Skin under an eye that's no darker than the cheek is left
//! alone.

use crate::adjust::luminance;
use crate::document::Document;
use crate::filters::blur_buffer;
use crate::layer::{Layer, Mask};
use crate::ops;
use crate::raster::Raster;
use crate::selection::Selection;
use crate::tiled::Tiled;

const MAX: f32 = u16::MAX as f32;

/// The layer's name.
pub const NAME: &str = "Under Eyes";

/// The blur the skin under the eye is seen through, as a fraction of the
/// distance between the eyes: Even Tone's small one, so fine lines stay.
const SMALL: f32 = 0.04;
/// The blur that carries the cheek's level up under the eye.
const LARGE: f32 = 0.2;
/// How dark against the cheek is too dark for skin in shadow: under the
/// first (of the cheek's level) it counts less, and under the second not at
/// all. Lashes, eyeliner and the frames of glasses are, and are left as
/// they are.
const TOO_DARK: [f32; 2] = [0.6, 0.45];

/// How far to open the Dodge curve on `under` (the skin under the eyes) to
/// bring it level with `cheek` (the skin below that) in `image`, for eyes
/// `iod` apart: nothing where it's as light already.
pub fn shadows(image: &Raster, under: &Selection, cheek: &Selection, iod: f32) -> Selection {
    let (w, h) = (under.width(), under.height());
    let none = || Selection::from_coverage(Tiled::new(w, h, 0));
    // Only the box round both is looked at.
    let (Some(u), Some(c)) = (under.bounds(), cheek.bounds()) else {
        return none();
    };
    let (x0, y0) = (u[0].min(c[0]), u[1].min(c[1]));
    let (bw, bh) = ((u[0] + u[2]).max(c[0] + c[2]) - x0, (u[1] + u[3]).max(c[1] + c[3]) - y0);
    let [under, cheek] = [under, cheek].map(|s| s.coverage.crop(x0, y0, bw, bh));
    let colour = |i: u32| {
        let p = image.get(x0 + i % bw, y0 + i / bw);
        [p[0], p[1], p[2]].map(|v| f32::from(v) / MAX)
    };
    // The cheek's level, blurred only over the cheek.
    let level: Vec<[f32; 4]> = (0..bw * bh)
        .map(|i| {
            let m = f32::from(cheek[i as usize]) / MAX;
            [luminance(colour(i)) * m, m, 0.0, 0.0]
        })
        .collect();
    let level = blur_buffer(level, bw as usize, bh as usize, LARGE * iod);
    // The skin under the eye, blurred only over itself, less what's too
    // dark to be skin in shadow.
    let here: Vec<[f32; 4]> = (0..bw * bh)
        .map(|i| {
            let (c, l) = (colour(i), level[i as usize]);
            if l[1] < 1e-3 {
                return [0.0; 4];
            }
            let against = luminance(c) / (l[0] / l[1]).max(1e-4);
            let shadow = ((against - TOO_DARK[1]) / (TOO_DARK[0] - TOO_DARK[1])).clamp(0.0, 1.0);
            let m = f32::from(under[i as usize]) / MAX * shadow;
            [c[0] * m, c[1] * m, c[2] * m, m]
        })
        .collect();
    // How much of each pixel is skin in shadow, before it's blurred.
    let skin: Vec<f32> = here.iter().map(|p| p[3]).collect();
    let here = blur_buffer(here, bw as usize, bh as usize, SMALL * iod);
    let dodge = ops::dodge_and_burn_adjustments()[0].prepare();
    let open: Vec<u16> = (here.iter().zip(&level).zip(&skin))
        .map(|((s, l), &skin)| {
            if s[3] < 1e-4 || l[1] < 1e-3 {
                return 0;
            }
            let around = [s[0] / s[3], s[1] / s[3], s[2] / s[3]];
            let darker = l[0] / l[1] - luminance(around);
            // How far the curve lifts this skin at full strength.
            let reach = (luminance(dodge.apply(around)) - luminance(around)).max(1e-4);
            ((darker / reach).clamp(0.0, 1.0) * skin * MAX).round() as u16
        })
        .collect();
    Selection::from_coverage(Tiled::from_slice(bw, bh, 0, &open).reframed(w, h, x0 as i32, y0 as i32, 0))
}

/// An "Under Eyes" layer above the layer at `above`, its mask `shadows`,
/// at `amount` (0–100, its opacity). Returns the new layer's id, or `None`
/// if there are no shadows to lift.
pub fn add_layer(doc: &mut Document, above: usize, shadows: &Selection, amount: f32) -> Option<u64> {
    if shadows.is_empty() {
        return None;
    }
    let [dodge, _] = ops::dodge_and_burn_adjustments();
    let id = doc.next_layer_id();
    let mut layer = Layer::adjustment(id, dodge, doc.width, doc.height);
    layer.name = NAME.into();
    layer.mask = Some(Mask {
        pixels: shadows.coverage.clone(),
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

    const W: u32 = 300;
    const H: u32 = 200;
    const IOD: f32 = 100.0;

    /// Skin with a soft shadow round (150, 70), a tenth of the range darker
    /// at its middle, and a black "lash" across it at y = 60.
    fn image() -> Raster {
        let pixels = (0..W * H)
            .map(|i| {
                let (x, y) = ((i % W) as f32, (i / W) as f32);
                if y == 60.0 && (120.0..180.0).contains(&x) {
                    return [3000, 2500, 2000, u16::MAX];
                }
                let shade = 6500.0 * (-((x - 150.0).hypot(y - 70.0) / 20.0).powi(2)).exp();
                let v = |base: f32| (base - shade) as u16;
                [v(45000.0), v(37000.0), v(33000.0), u16::MAX]
            })
            .collect();
        Raster::new(W, H, pixels)
    }

    /// Under the eye, and the cheek below it.
    fn regions() -> [Selection; 2] {
        [(50.0, 100.0), (100.0, 150.0)].map(|(top, bottom)| Selection::rectangle(W, H, (100.0, top), (200.0, bottom)))
    }

    #[test]
    fn the_mask_opens_where_the_skin_under_the_eye_is_darker_than_the_cheek() {
        let [under, cheek] = regions();
        let mask = shadows(&image(), &under, &cheek, IOD);
        assert!(mask.at(150, 72) > 0.5, "{}", mask.at(150, 72));
        // Less towards the shadow's edge, nothing where the skin is as
        // light as the cheek, and nothing outside.
        assert!(mask.at(150, 72) > mask.at(170, 80) && mask.at(105, 95) < 0.02, "{} {}", mask.at(170, 80), mask.at(105, 95));
        assert!(mask.at(150, 120) < 0.01 && mask.at(150, 40) < 0.01);
        // The lash is too dark to be skin in shadow.
        assert!(mask.at(150, 60) < 0.01, "{}", mask.at(150, 60));
        assert!(mask.coverage.tile(1, 0).is_none());
    }

    #[test]
    fn skin_as_light_as_the_cheek_or_with_no_cheek_to_go_by_has_no_shadows() {
        let [under, cheek] = regions();
        let flat = Raster::new(W, H, vec![[45000, 37000, 33000, u16::MAX]; (W * H) as usize]);
        assert!(shadows(&flat, &under, &cheek, IOD).is_empty());
        let none = Selection::from_coverage(Tiled::new(W, H, 0));
        assert!(shadows(&image(), &under, &none, IOD).is_empty());
        assert!(shadows(&image(), &none, &cheek, IOD).is_empty());
    }

    #[test]
    fn the_layer_lifts_the_shadow_to_the_cheeks_level_at_full_opacity() {
        let mut doc = Document::from_image("t.tif".into(), &image(), ColorProfile::srgb(), 16);
        let before = doc.composite();
        let [under, cheek] = regions();
        let mask = shadows(&image(), &under, &cheek, IOD);
        let id = add_layer(&mut doc, 0, &mask, 50.0).unwrap();
        let layer = doc.layer(id).unwrap();
        assert_eq!((layer.name.as_str(), layer.opacity), ("Under Eyes", 0.5));

        let red = |doc: &Document, x: u32, y: u32| i32::from(doc.composite().get(x, y)[0]);
        let (was, cheek_level) = (i32::from(before.get(150, 75)[0]), i32::from(before.get(150, 130)[0]));
        let half = red(&doc, 150, 75);
        assert!(half > was + 1500 && half < cheek_level, "{was} {half} {cheek_level}");
        // The cheek and the lash are as they were.
        assert_eq!(doc.composite().get(150, 130), before.get(150, 130));
        assert_eq!(doc.composite().get(150, 60), before.get(150, 60));
        // At full opacity the shadow's nearly gone, and not past the cheek.
        doc.layer_mut(id).unwrap().opacity = 1.0;
        let full = red(&doc, 150, 75);
        assert!(full > half && (cheek_level - full) < (cheek_level - was) / 3 && full <= cheek_level + 300, "{was} {full} {cheek_level}");

        let none = Selection::from_coverage(Tiled::new(W, H, 0));
        assert_eq!(add_layer(&mut doc, 0, &none, 50.0), None);
    }
}
