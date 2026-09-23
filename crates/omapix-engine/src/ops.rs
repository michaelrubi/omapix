//! Document edits that build or combine layers.

use crate::blend::BlendMode;
use crate::composite::composite;
use crate::filters::gaussian_blur;
use crate::layer::Layer;
use crate::tiled::Tiled;
use crate::{Document, Raster};

/// Gamma that linear documents are converted to for editing. ProPhoto RGB's
/// own standard gamma.
pub const EDITING_GAMMA: f64 = 1.8;

/// Get a freshly opened flat image ready for editing. Linear colour spaces
/// (darktable's default export) make brushes, blurs and blend modes behave
/// unlike Photoshop, so their pixels are converted to the same colour space
/// with a normal gamma curve. Returns the original profile's name if it
/// converted anything.
pub fn prepare_for_editing(doc: &mut Document) -> crate::Result<Option<String>> {
    if doc.layers.len() != 1 || !doc.profile.is_linear() {
        return Ok(None);
    }
    let target = doc.profile.with_gamma(EDITING_GAMMA)?;
    let layer = &mut doc.layers[0];
    layer.pixels = crate::color::convert(&layer.pixels, &doc.profile, &target)?;
    let original = std::mem::replace(&mut doc.profile, target);
    Ok(Some(original.description().to_owned()))
}

/// Photoshop's "Stamp Visible": a new layer holding the flattened image,
/// placed above `above`. Returns the new layer's id.
pub fn stamp_visible(doc: &mut Document, above: usize) -> u64 {
    let pixels = Tiled::from_raster(&doc.composite());
    let id = doc.next_layer_id();
    let name = doc.unused_name("Stamp");
    doc.layers
        .insert(above + 1, Layer::from_pixels(id, name, pixels));
    id
}

/// Split the visible image into a blurred colour/tone layer and a texture
/// layer that recombine exactly to the original (Grain Extract / Grain
/// Merge), placed above layer index `above`. Returns (low id, high id).
pub fn frequency_separation(doc: &mut Document, above: usize, radius: f32) -> (u64, u64) {
    let visible = doc.composite();
    let (w, h) = (doc.width, doc.height);
    let visible_layer = Layer::from_raster(0, "", &visible);
    let low_pixels = gaussian_blur(&visible_layer.pixels, radius);

    let mut extract = Layer::from_pixels(0, "", low_pixels.clone());
    extract.blend = BlendMode::GrainExtract;
    let high_raster: Raster = composite(&[visible_layer, extract], w, h);

    let low_id = doc.next_layer_id();
    let high_id = doc.next_layer_id();
    let low = Layer::from_pixels(low_id, "Low - color/tone", low_pixels);
    let mut high = Layer::from_raster(high_id, "High - texture", &high_raster);
    high.blend = BlendMode::GrainMerge;
    doc.layers.insert(above + 1, low);
    doc.layers.insert(above + 2, high);
    (low_id, high_id)
}

/// Photoshop's dodge & burn setup: a layer filled with 50 % grey in Soft
/// Light mode, which leaves the image unchanged until painted lighter or
/// darker. Returns the new layer's id.
pub fn dodge_and_burn_layer(doc: &mut Document, above: usize) -> u64 {
    let (w, h) = (doc.width, doc.height);
    let grey = Raster::new(
        w,
        h,
        vec![[32768, 32768, 32768, 65535]; w as usize * h as usize],
    );
    let id = doc.next_layer_id();
    let mut layer = Layer::from_raster(id, "Dodge & Burn", &grey);
    layer.blend = BlendMode::SoftLight;
    doc.layers.insert(above + 1, layer);
    id
}

/// Merge the layer at `index` into the one below it (Ctrl+E). The result
/// keeps the lower layer's name, mode, opacity and mask. Returns the merged
/// layer's id, or `None` if there is no layer below.
pub fn merge_down(doc: &mut Document, index: usize) -> Option<u64> {
    if index == 0 || index >= doc.layers.len() {
        return None;
    }
    // Pixels can't be merged into an adjustment layer.
    if doc.layers[index - 1].adjustment.is_some() {
        return None;
    }
    let upper = doc.layers.remove(index);
    let lower = &mut doc.layers[index - 1];
    let mut base = lower.clone();
    base.visible = true;
    base.opacity = 1.0;
    base.blend = BlendMode::Normal;
    base.mask = None;
    let mut upper = upper;
    upper.visible = true;
    let merged = composite(&[base, upper], doc.width, doc.height);
    lower.pixels = Tiled::from_raster(&merged);
    Some(lower.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ColorProfile;

    fn doc_with(pixels: Vec<crate::Pixel>, w: u32, h: u32) -> Document {
        Document::from_image(
            "t.tif".into(),
            &Raster::new(w, h, pixels),
            ColorProfile::srgb(),
            16,
        )
    }

    #[test]
    fn frequency_separation_leaves_the_image_unchanged() {
        let (w, h) = (300, 280);
        let px = (0..w * h)
            .map(|i| {
                // Smooth gradients plus fine, skin-like texture.
                let (x, y) = (i % w, i / w);
                let grain = ((x * 7 + y * 3) % 11) * 300;
                [
                    (15000 + x * 60 + grain) as u16,
                    (20000 + y * 50 + grain) as u16,
                    (30000 + grain) as u16,
                    65535,
                ]
            })
            .collect();
        let mut doc = doc_with(px, w, h);
        let before = doc.composite();
        let (low, high) = frequency_separation(&mut doc, 0, 6.0);
        assert_eq!(doc.layers.len(), 3);
        assert_eq!(doc.layer(high).unwrap().blend, BlendMode::GrainMerge);
        assert!(doc.index_of(low).unwrap() < doc.index_of(high).unwrap());
        let after = doc.composite();
        let worst = before
            .pixels()
            .iter()
            .zip(after.pixels())
            .flat_map(|(a, b)| (0..3).map(move |c| a[c].abs_diff(b[c])))
            .max()
            .unwrap();
        // Only rounding differences, except where local contrast exceeds
        // half the range and the texture layer clips (same as Photoshop).
        assert!(worst <= 3, "worst channel difference {worst}");
    }

    #[test]
    fn dodge_and_burn_layer_is_invisible_until_painted() {
        let mut doc = doc_with(vec![[12000, 40000, 55000, 65535]; 100], 10, 10);
        let before = doc.composite();
        dodge_and_burn_layer(&mut doc, 0);
        let after = doc.composite();
        for (a, b) in before.pixels().iter().zip(after.pixels()) {
            for c in 0..3 {
                assert!(a[c].abs_diff(b[c]) <= 2);
            }
        }
    }

    #[test]
    fn merge_down_bakes_the_blend_mode() {
        let mut doc = doc_with(vec![[40000, 40000, 40000, 65535]; 100], 10, 10);
        let id = stamp_visible(&mut doc, 0);
        doc.layer_mut(id).unwrap().blend = BlendMode::Multiply;
        let before = doc.composite();
        merge_down(&mut doc, 1).unwrap();
        assert_eq!(doc.layers.len(), 1);
        assert_eq!(doc.composite().get(5, 5), before.get(5, 5));
    }
}
