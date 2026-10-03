//! Whitening teeth and the whites of the eyes (docs/AI.md, milestone 13),
//! as it's done by hand: a Hue/Saturation layer that takes colour out and
//! lightens a little, masked to the teeth or the whites, its opacity the
//! amount. A face's points only say where the mouth and the eyes open; what's
//! a tooth or a white in there is what's light and not red ([`whites`]), so
//! gums, tongue, lashes and the dark of the mouth are left as they are.

use crate::adjust::{Adjustment, HueSaturation};
use crate::document::Document;
use crate::filters::blur_buffer;
use crate::layer::{Layer, Mask};
use crate::selection::Selection;
use crate::tiled::Tiled;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Whiten {
    Teeth,
    Eyes,
}

impl Whiten {
    /// Its layer's name.
    pub fn name(self) -> &'static str {
        match self {
            Whiten::Teeth => "Whiten Teeth",
            Whiten::Eyes => "Whiten Eyes",
        }
    }

    /// What its layer does at full opacity.
    fn adjustment(self) -> HueSaturation {
        let (saturation, lightness) = match self {
            Whiten::Teeth => (-70.0, 12.0),
            Whiten::Eyes => (-60.0, 10.0),
        };
        HueSaturation {
            hue: 0.0,
            saturation,
            lightness,
        }
    }
}

/// How red a pixel is, as (red − green) / red: teeth and the whites of eyes
/// are under the first, gums, tongue, lips and skin over the second.
const RED: [f32; 2] = [0.2, 0.32];
/// How light, against the lightest there: under the first is the dark of
/// the mouth, lashes and the lid's shadow.
const LIGHT: [f32; 2] = [0.35, 0.6];

/// 0 below `from`, 1 above `to`, and smooth between.
fn step([from, to]: [f32; 2], v: f32) -> f32 {
    let t = ((v - from) / (to - from)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The part of `within` that's light and not red in `srgb` (8-bit sRGB, the
/// same size), its edges softened by `soften` pixels: the teeth in a mouth,
/// or the whites in an eye without its iris.
pub fn whites(srgb: &[[u8; 4]], within: &Selection, soften: f32) -> Selection {
    let (w, h) = (within.width(), within.height());
    let none = || Selection::from_coverage(Tiled::new(w, h, 0));
    // Only the box round it is looked at.
    let Some([bx, by, bw, bh]) = within.bounds() else {
        return none();
    };
    let inside = within.coverage.crop(bx, by, bw, bh);
    // Each pixel's lightness, and how far it is from red.
    let seen: Vec<(f32, f32)> = (0..bw * bh)
        .map(|i| {
            let [r, g, b, _] = srgb[((by + i / bw) * w + bx + i % bw) as usize].map(f32::from);
            (0.3 * r + 0.59 * g + 0.11 * b, 1.0 - step(RED, (r - g) / r.max(1.0)))
        })
        .collect();
    // The lightest: nine tenths of the way up what isn't red.
    let mut lights: Vec<f32> = (seen.iter().zip(&inside))
        .filter(|((_, pale), inside)| *pale > 0.5 && **inside > u16::MAX / 2)
        .map(|((light, _), _)| *light)
        .collect();
    if lights.is_empty() {
        return none();
    }
    let nth = lights.len() * 9 / 10;
    let lightest = *lights.select_nth_unstable_by(nth, f32::total_cmp).1;
    let white: Vec<_> = seen.iter().map(|&(light, pale)| [pale * step(LIGHT.map(|k| k * lightest), light), 0.0, 0.0, 0.0]).collect();
    let white = blur_buffer(white, bw as usize, bh as usize, soften);
    let coverage: Vec<u16> = white.iter().zip(&inside).map(|(white, &inside)| (white[0].clamp(0.0, 1.0) * f32::from(inside)).round() as u16).collect();
    Selection::from_coverage(Tiled::from_slice(bw, bh, 0, &coverage).reframed(w, h, bx as i32, by as i32, 0))
}

/// A "Whiten Teeth" or "Whiten Eyes" layer above the layer at `above`,
/// masked to `whites`, at `amount` (0–100, its opacity). Returns the new
/// layer's id, or `None` if there's nothing to whiten.
pub fn add_layer(doc: &mut Document, above: usize, what: Whiten, whites: &Selection, amount: f32) -> Option<u64> {
    if whites.is_empty() {
        return None;
    }
    let id = doc.next_layer_id();
    let mut layer = Layer::adjustment(id, Adjustment::HueSaturation(what.adjustment()), doc.width, doc.height);
    layer.name = what.name().into();
    layer.mask = Some(Mask {
        pixels: whites.coverage.clone(),
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
    use crate::raster::Raster;

    const W: u32 = 300;
    const H: u32 = 200;

    const TOOTH: [u8; 4] = [215, 195, 160, 255];
    const SHADED_TOOTH: [u8; 4] = [150, 135, 110, 255];
    const GUM: [u8; 4] = [190, 105, 110, 255];
    const DARK: [u8; 4] = [35, 18, 18, 255];
    const LIP: [u8; 4] = [180, 100, 95, 255];

    /// A mouth open from (100, 80) to (200, 120) in lips: gums along the
    /// top 10 px, the dark of the mouth along the bottom 10, and teeth
    /// between, in shade for the last 20 px on the right.
    fn mouth() -> Vec<[u8; 4]> {
        (0..W * H)
            .map(|i| match (i % W, i / W) {
                (100..200, 80..90) => GUM,
                (100..180, 90..110) => TOOTH,
                (180..200, 90..110) => SHADED_TOOTH,
                (100..200, 110..120) => DARK,
                _ => LIP,
            })
            .collect()
    }

    fn opening() -> Selection {
        Selection::rectangle(W, H, (100.0, 80.0), (200.0, 120.0))
    }

    #[test]
    fn the_whites_of_a_mouth_are_its_teeth_not_its_gums_or_the_dark() {
        let teeth = whites(&mouth(), &opening(), 0.0);
        assert!(teeth.at(140, 100) > 0.99 && teeth.at(190, 100) > 0.99, "{} {}", teeth.at(140, 100), teeth.at(190, 100));
        assert!(teeth.at(140, 84) < 0.01 && teeth.at(140, 115) < 0.01, "{} {}", teeth.at(140, 84), teeth.at(140, 115));
        // Softened, the edge between tooth and gum is a ramp.
        let soft = whites(&mouth(), &opening(), 2.0);
        assert!((0.3..0.7).contains(&soft.at(140, 90)), "{}", soft.at(140, 90));
        assert!(soft.at(140, 100) > 0.99 && soft.at(140, 82) < 0.02);
        // Only tiles the mouth is in are made.
        assert!(teeth.coverage.tile(0, 0).is_some());
        let far = Selection::rectangle(600, 600, (10.0, 10.0), (50.0, 50.0));
        let image = vec![TOOTH; 600 * 600];
        assert!(whites(&image, &far, 0.0).coverage.tile(1, 1).is_none());
    }

    #[test]
    fn nothing_outside_the_opening_is_white_however_light() {
        let mut image = mouth();
        image[(50 * W + 50) as usize] = [250, 250, 250, 255];
        let teeth = whites(&image, &opening(), 2.0);
        assert!(teeth.at(50, 50) < 0.01 && teeth.at(99, 100) < 0.01);
        // A soft opening's edge is the mask's.
        let soft = opening().feather(3.0);
        let teeth = whites(&image, &soft, 0.0);
        assert!((0.3..0.7).contains(&teeth.at(100, 100)), "{}", teeth.at(100, 100));
        // A mouth with no teeth showing, or nowhere to look: nothing.
        assert!(whites(&vec![GUM; (W * H) as usize], &opening(), 2.0).is_empty());
        assert!(whites(&image, &Selection::from_coverage(Tiled::new(W, H, 0)), 2.0).is_empty());
    }

    #[test]
    fn the_layer_takes_colour_out_of_the_teeth_and_lightens_them_by_its_opacity() {
        let pixels = mouth().iter().map(|p| p.map(|v| u16::from(v) * 257)).collect();
        let mut doc = Document::from_image("t.tif".into(), &Raster::new(W, H, pixels), ColorProfile::srgb(), 16);
        let before = doc.composite();
        let teeth = whites(&mouth(), &opening(), 0.0);
        let id = add_layer(&mut doc, 0, Whiten::Teeth, &teeth, 50.0).unwrap();
        let layer = doc.layer(id).unwrap();
        assert_eq!((layer.name.as_str(), layer.opacity), ("Whiten Teeth", 0.5));
        assert!(matches!(layer.adjustment, Some(Adjustment::HueSaturation(_))));

        let colour = |p: [u16; 4]| i32::from(p[0]) - i32::from(p[2]);
        let (half, was) = (doc.composite(), before.get(140, 100));
        assert!(colour(half.get(140, 100)) < colour(was) * 3 / 4 && half.get(140, 100)[2] > was[2]);
        // Gums, the dark and the lips are as they were.
        for (x, y) in [(140, 84), (140, 115), (50, 50)] {
            assert_eq!(half.get(x, y), before.get(x, y));
        }
        // More at full opacity.
        doc.layer_mut(id).unwrap().opacity = 1.0;
        assert!(colour(doc.composite().get(140, 100)) < colour(half.get(140, 100)));

        // With nothing to whiten there's no layer.
        let none = Selection::from_coverage(Tiled::new(W, H, 0));
        assert_eq!(add_layer(&mut doc, 0, Whiten::Eyes, &none, 50.0), None);
        assert_eq!(doc.layers.len(), 2);
    }
}
