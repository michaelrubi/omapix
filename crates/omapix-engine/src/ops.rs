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
/// placed at the top of the stack, outside any group. Returns the new
/// layer's id.
pub fn stamp_visible(doc: &mut Document) -> u64 {
    let pixels = Tiled::from_raster(&doc.composite());
    let id = doc.next_layer_id();
    let name = doc.unused_name("Stamp");
    doc.layers.push(Layer::from_pixels(id, name, pixels));
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
    let at = doc.insert_above(above, low);
    doc.insert_above(at, high);
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
    doc.insert_above(above, layer);
    id
}

/// Whether the layer at `index` can be merged into the one below it: both
/// are in the same group, and the one below has pixels.
pub fn can_merge_down(doc: &Document, index: usize) -> bool {
    let (Some(upper), Some(lower)) = (doc.layers.get(index), index.checked_sub(1)) else {
        return false;
    };
    let lower = &doc.layers[lower];
    !upper.is_group && lower.parent == upper.parent && lower.has_pixels()
}

/// Merge the layer at `index` into the one below it (Ctrl+E). The result
/// keeps the lower layer's name, mode, opacity and mask. Returns the merged
/// layer's id, or `None` if it can't be merged (see [`can_merge_down`]).
pub fn merge_down(doc: &mut Document, index: usize) -> Option<u64> {
    if !can_merge_down(doc, index) {
        return None;
    }
    let upper = doc.layers.remove(index);
    let lower = &mut doc.layers[index - 1];
    let mut base = lower.clone();
    base.visible = true;
    base.opacity = 1.0;
    base.blend = BlendMode::Normal;
    base.mask = None;
    base.clipped = false;
    let mut upper = upper;
    upper.visible = true;
    // Clipped to the lower layer, it merges in clipped; clipped along with
    // it to something further down, the merged layer is clipped anyway.
    upper.clipped &= !lower.clipped;
    let merged = composite(&[base, upper], doc.width, doc.height);
    lower.pixels = Tiled::from_raster(&merged);
    Some(lower.id)
}

/// Flatten the group at `index` into one layer (Photoshop's Merge Group,
/// Ctrl+E on a group). The layer keeps the group's name, opacity, mask and
/// blend mode (Normal for Pass Through). Returns its id, or `None` if it
/// isn't a group.
pub fn merge_group(doc: &mut Document, index: usize) -> Option<u64> {
    let group = doc.layers.get(index).filter(|l| l.is_group)?.clone();
    let span = doc.span(index);
    let merged = composite(&doc.layers[span.start..index], doc.width, doc.height);
    let mut layer = group;
    layer.is_group = false;
    layer.pixels = Tiled::from_raster(&merged);
    if layer.blend == BlendMode::PassThrough {
        layer.blend = BlendMode::Normal;
    }
    let id = layer.id;
    doc.layers.splice(span, [layer]);
    Some(id)
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
    fn merging_stays_within_groups_and_merges_whole_groups() {
        let mut doc = doc_with(vec![[40000, 40000, 40000, 65535]; 100], 10, 10);
        let mut top = Layer::from_raster(
            doc.next_layer_id(),
            "top",
            &Raster::new(10, 10, vec![[65535, 0, 0, 65535]; 100]),
        );
        top.opacity = 0.5;
        let top_id = top.id;
        doc.insert_above(0, top);
        let group = doc.group_layer(1);
        doc.layer_mut(group).unwrap().opacity = 0.5;
        // "top" is the bottom of its group; the background is outside it.
        assert!(!can_merge_down(&doc, 1));
        assert!(!can_merge_down(&doc, 2), "a group isn't merged down");
        let before = doc.composite();

        assert_eq!(merge_group(&mut doc, 2), Some(group));
        assert_eq!(merge_group(&mut doc, 0), None);
        assert_eq!(doc.layers.len(), 2);
        let merged = doc.layer(group).unwrap();
        assert!(!merged.is_group && merged.blend == BlendMode::Normal);
        assert!(doc.layer(top_id).is_none());
        let after = doc.composite();
        for c in 0..3 {
            assert!(after.get(5, 5)[c].abs_diff(before.get(5, 5)[c]) <= 2);
        }
        // Now it's a plain layer above the background, it merges down.
        assert!(can_merge_down(&doc, 1));
    }

    #[test]
    fn merge_down_keeps_a_clipped_layer_inside_its_base() {
        let (w, h) = (300, 10);
        let mut doc = doc_with(vec![[50000, 10000, 10000, 65535]; (w * h) as usize], w, h);
        let base = doc.next_layer_id();
        let mut layer = Layer::empty(base, "base", w, h);
        layer.pixels.tile_mut(0, 0).fill([0, 65535, 0, 65535]);
        doc.layers.push(layer);
        let top = doc.next_layer_id();
        let mut blue = Layer::empty(top, "blue", w, h);
        blue.pixels.tile_mut(0, 0).fill([0, 0, 65535, 65535]);
        blue.pixels.tile_mut(1, 0).fill([0, 0, 65535, 65535]);
        blue.clipped = true;
        doc.layers.push(blue);
        let before = doc.composite();
        assert_eq!(merge_down(&mut doc, 2), Some(base));
        assert_eq!(doc.composite().pixels(), before.pixels());
        assert!(doc.layers[1].pixels.tile(1, 0).is_none_or(|t| t.iter().all(|p| p[3] == 0)));
    }

    #[test]
    fn merge_down_inside_a_group() {
        let mut doc = doc_with(vec![[40000, 40000, 40000, 65535]; 100], 10, 10);
        let group = doc.group_layer(0);
        dodge_and_burn_layer(&mut doc, 0);
        assert_eq!(doc.layers[1].parent, Some(group));
        let before = doc.composite();
        assert!(merge_down(&mut doc, 1).is_some());
        assert_eq!(doc.layers.len(), 2);
        assert_eq!(doc.composite().get(5, 5), before.get(5, 5));
    }

    #[test]
    fn merge_down_bakes_the_blend_mode() {
        let mut doc = doc_with(vec![[40000, 40000, 40000, 65535]; 100], 10, 10);
        let id = stamp_visible(&mut doc);
        doc.layer_mut(id).unwrap().blend = BlendMode::Multiply;
        let before = doc.composite();
        merge_down(&mut doc, 1).unwrap();
        assert_eq!(doc.layers.len(), 1);
        assert_eq!(doc.composite().get(5, 5), before.get(5, 5));
    }
}
