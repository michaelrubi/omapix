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

/// Keep `changed` only where `selection` covers, and `original` elsewhere
/// (blending partially selected pixels), so filters respect the selection.
pub fn within_selection(
    original: &Tiled<crate::Pixel>,
    changed: Tiled<crate::Pixel>,
    selection: Option<&crate::selection::Selection>,
) -> Tiled<crate::Pixel> {
    let Some(sel) = selection else { return changed };
    let sel = &sel.coverage;
    let mut out = changed;
    out.par_update(|col, row, tile| {
        let s = sel.tile(col, row);
        if s.is_none() && sel.fill() == u16::MAX {
            return None;
        }
        let o = original.tile(col, row);
        let c = tile;
        Some(
            (0..crate::tiled::TILE_PIXELS)
                .map(|i| {
                    let k = f32::from(s.map_or(sel.fill(), |t| t[i])) / 65535.0;
                    let a = o.map_or(original.fill(), |t| t[i]);
                    let b = c.map_or(original.fill(), |t| t[i]);
                    std::array::from_fn(|ch| {
                        (f32::from(a[ch]) + (f32::from(b[ch]) - f32::from(a[ch])) * k).round()
                            as u16
                    })
                })
                .collect(),
        )
    });
    out
}

/// Fill a layer with a colour where selected (everywhere without a
/// selection), like Photoshop's Alt+Backspace. `None` clears to
/// transparency instead (Delete).
pub fn fill_pixels(
    pixels: &Tiled<crate::Pixel>,
    colour: Option<crate::Pixel>,
    selection: Option<&crate::selection::Selection>,
) -> Tiled<crate::Pixel> {
    let filled = match colour {
        Some(c) => Tiled::from_tiles(pixels.width(), pixels.height(), [0; 4], |_, _| {
            Some(vec![c; crate::tiled::TILE_PIXELS])
        }),
        None => Tiled::new(pixels.width(), pixels.height(), [0; 4]),
    };
    within_selection(pixels, filled, selection)
}

/// Fill a mask with a grey level where selected.
pub fn fill_mask(
    mask: &Tiled<u16>,
    value: u16,
    selection: Option<&crate::selection::Selection>,
) -> Tiled<u16> {
    let Some(sel) = selection else {
        return Tiled::new(mask.width(), mask.height(), value);
    };
    let sel = &sel.coverage;
    let mut out = mask.clone();
    out.par_update(|col, row, tile| {
        let s = sel.tile(col, row);
        if s.is_none() && sel.fill() == 0 {
            return None;
        }
        Some(
            (0..crate::tiled::TILE_PIXELS)
                .map(|i| {
                    let k = f32::from(s.map_or(sel.fill(), |t| t[i])) / 65535.0;
                    let a = f32::from(tile.map_or(mask.fill(), |t| t[i]));
                    (a + (f32::from(value) - a) * k).round() as u16
                })
                .collect(),
        )
    });
    out
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
    fn fills_and_filters_respect_the_selection() {
        use crate::selection::Selection;
        let (w, h) = (300, 100);
        let doc = doc_with(vec![[40000, 40000, 40000, 65535]; (w * h) as usize], w, h);
        let pixels = &doc.layers[0].pixels;
        let sel = Selection::rectangle(w, h, (0.0, 0.0), (150.0, 100.0));
        let filled = fill_pixels(pixels, Some([0, 0, 0, 65535]), Some(&sel));
        assert_eq!(filled.get(10, 10), [0, 0, 0, 65535]);
        assert_eq!(filled.get(200, 10), [40000, 40000, 40000, 65535]);
        let cleared = fill_pixels(pixels, None, Some(&sel));
        assert_eq!(cleared.get(10, 10)[3], 0);
        assert_eq!(cleared.get(200, 10)[3], 65535);
        let mask = fill_mask(&Tiled::new(w, h, u16::MAX), 0, Some(&sel));
        assert_eq!((mask.get(10, 10), mask.get(200, 10)), (0, u16::MAX));
        let everywhere = fill_pixels(pixels, Some([1, 2, 3, 65535]), None);
        assert_eq!(everywhere.get(299, 99), [1, 2, 3, 65535]);
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
