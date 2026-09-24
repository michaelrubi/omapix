//! Document edits that build or combine layers.

use crate::adjust::{Adjustment, Curve, Curves};
use crate::blend::BlendMode;
use crate::composite::composite;
use crate::filters::{self, NoiseOptions, gaussian_blur, high_pass};
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

/// `edited` with the alpha of `original`, for layers with Lock Transparent
/// Pixels: only the colour changes.
pub fn keep_alpha(
    original: &Tiled<crate::Pixel>,
    mut edited: Tiled<crate::Pixel>,
) -> Tiled<crate::Pixel> {
    let (fill, alpha_fill) = (edited.fill(), original.fill()[3]);
    edited.par_update(|col, row, tile| {
        let alpha = original.tile(col, row);
        if tile.is_none() && alpha.is_none() && fill[3] == alpha_fill {
            return None;
        }
        let tile = tile.map_or_else(|| vec![fill; crate::tiled::TILE_PIXELS], <[_]>::to_vec);
        Some(
            tile.into_iter()
                .enumerate()
                .map(|(i, [r, g, b, _])| [r, g, b, alpha.map_or(alpha_fill, |t| t[i][3])])
                .collect(),
        )
    });
    edited
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
/// Merge), in a "Frequency Separation" group placed above layer index
/// `above`, so hiding the group shows the image before. Returns (low id,
/// high id).
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
    let group = new_setup_group(doc, above, "Frequency Separation");
    let at = doc.insert_above(group, low);
    doc.insert_above(at, high);
    (low_id, high_id)
}

/// Layer `id`'s pixels after `filter`, within the selection if there is
/// one. `None` if there's no such layer.
pub fn filtered(doc: &Document, id: u64, filter: &crate::filters::LayerFilter) -> Option<Tiled<crate::Pixel>> {
    let layer = doc.layer(id)?;
    let out = filter.apply(&layer.pixels);
    Some(within_selection(&layer.pixels, out, doc.selection.as_ref()))
}

/// One-step High Pass sharpening: a "High Pass Sharpening" layer in
/// Overlay mode above layer index `above`, holding the High Pass of the
/// visible image's luminance, so it sharpens tone without colour fringes.
/// Its opacity sets the strength and a mask keeps it off areas. Returns
/// its id.
pub fn high_pass_sharpening(doc: &mut Document, above: usize, radius: f32) -> u64 {
    let visible = doc.composite();
    let luminance: Vec<crate::Pixel> = visible
        .pixels()
        .iter()
        .map(|p| {
            let y = 0.2126 * f32::from(p[0]) + 0.7152 * f32::from(p[1]) + 0.0722 * f32::from(p[2]);
            let y = y.round() as u16;
            [y, y, y, u16::MAX]
        })
        .collect();
    let luminance = Tiled::from_slice(doc.width, doc.height, [0; 4], &luminance);
    let id = doc.next_layer_id();
    let mut layer = Layer::from_pixels(id, "High Pass Sharpening", high_pass(&luminance, radius));
    layer.blend = BlendMode::Overlay;
    doc.insert_above(above, layer);
    id
}

/// A new Pass Through group above layer index `above`, for a retouching
/// setup to fill. Returns its index; layers inserted above it go in at the
/// top of it.
fn new_setup_group(doc: &mut Document, above: usize, name: &str) -> usize {
    let id = doc.next_layer_id();
    let group = Layer::group(id, name, doc.width, doc.height);
    doc.insert_above(above, group);
    doc.index_of(id).expect("just inserted")
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

/// A new Grain layer filled with 50 % grey in Overlay mode carrying film grain,
/// placed above layer index `above`. Layer opacity is set to Amount.
pub fn add_noise_layer(doc: &mut Document, above: usize, options: &NoiseOptions) -> u64 {
    let visible = doc.composite();
    let base = Tiled::from_raster(&visible);
    let id = doc.next_layer_id();
    let layer = grain_layer(id, doc.width, doc.height, options, Some(&base));
    doc.insert_above(above, layer);
    id
}

/// Create a Grain layer filled with 50 % grey in Overlay mode carrying grain.
pub fn grain_layer(
    id: u64,
    width: u32,
    height: u32,
    options: &NoiseOptions,
    base: Option<&Tiled<crate::Pixel>>,
) -> Layer {
    let pixels = filters::generate_grain(width, height, options, base);
    let mut layer = Layer::from_pixels(id, "Grain", pixels);
    layer.blend = BlendMode::Overlay;
    layer.opacity = (options.amount / 100.0).clamp(0.0, 1.0);
    layer
}

/// The curves-based dodge & burn setup retouchers use: a "Dodge & Burn"
/// group holding a brightening and a darkening Curves layer, each with a
/// black mask, so painting white on a mask lightens or darkens there.
/// Placed above layer index `above`. Returns (dodge id, burn id).
pub fn dodge_and_burn_curves(doc: &mut Document, above: usize) -> (u64, u64) {
    let (w, h) = (doc.width, doc.height);
    let mut curves = |name: &str, mid: f32| {
        let adjustment = Adjustment::Curves(Curves {
            master: Curve {
                points: vec![(0.0, 0.0), (0.5, mid), (1.0, 1.0)],
            },
            ..Default::default()
        });
        let mut layer = Layer::adjustment(doc.next_layer_id(), adjustment, w, h);
        layer.name = name.into();
        if let Some(mask) = &mut layer.mask {
            mask.invert();
        }
        layer
    };
    let burn = curves("Burn", 0.35);
    let dodge = curves("Dodge", 0.65);
    let (dodge_id, burn_id) = (dodge.id, burn.id);
    let group = new_setup_group(doc, above, "Dodge & Burn");
    let at = doc.insert_above(group, burn);
    doc.insert_above(at, dodge);
    (dodge_id, burn_id)
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

/// Merge several layers (see [`Document::outermost`]) into one, where the
/// top one was, with its name (Photoshop's Merge Layers, Ctrl+E with several
/// selected). They're flattened on their own, as if nothing else were
/// there. Returns the merged layer's id, or `None` for fewer than two.
pub fn merge_layers(doc: &mut Document, ids: &[u64]) -> Option<u64> {
    let ids = doc.outermost(ids);
    if ids.len() < 2 {
        return None;
    }
    let mut layers = Vec::new();
    for &id in &ids {
        let index = doc.index_of(id)?;
        let span = doc.span(index);
        let base = doc.clip_base(id);
        layers.extend_from_slice(&doc.layers[span]);
        // Clipped to a layer that isn't merged, it shows unclipped.
        if base.is_some_and(|b| !ids.contains(&b)) {
            layers.last_mut().expect("just added").clipped = false;
        }
    }
    let merged = composite(&layers, doc.width, doc.height);
    let (&top, rest) = ids.split_last()?;
    let index = doc.index_of(top)?;
    let old = &doc.layers[index];
    let mut layer = Layer::from_raster(old.id, old.name.clone(), &merged);
    layer.parent = old.parent;
    doc.layers.splice(doc.span(index), [layer]);
    doc.remove_layers(rest);
    Some(top)
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
        assert_eq!(doc.layers.len(), 4);
        assert_eq!(doc.layer(high).unwrap().blend, BlendMode::GrainMerge);
        assert!(doc.index_of(low).unwrap() < doc.index_of(high).unwrap());
        // Both in one Pass Through group, on top.
        let group = doc.layers.last().unwrap();
        assert!(group.is_group && group.blend == BlendMode::PassThrough);
        assert_eq!(group.name, "Frequency Separation");
        assert_eq!(doc.layer(low).unwrap().parent, Some(group.id));
        assert_eq!(doc.layer(high).unwrap().parent, Some(group.id));
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
    fn high_pass_sharpening_steepens_edges_and_leaves_flat_areas() {
        // A soft vertical edge from dark to light grey.
        let (w, h) = (60u32, 10u32);
        let px = (0..w * h)
            .map(|i| {
                let v = (20000 + (i % w).saturating_sub(20).min(20) * 1000) as u16;
                [v, v, v, 65535]
            })
            .collect();
        let mut doc = doc_with(px, w, h);
        let before = doc.composite();
        let id = high_pass_sharpening(&mut doc, 0, 3.0);
        assert_eq!(doc.layer(id).unwrap().blend, BlendMode::Overlay);
        let after = doc.composite();
        let at = |r: &Raster, x: u32| r.pixels()[(5 * w + x) as usize][0];
        // Flat areas far from the edge stay (almost) the same.
        assert!(at(&before, 2).abs_diff(at(&after, 2)) <= 2);
        assert!(at(&before, 57).abs_diff(at(&after, 57)) <= 2);
        // Either side of the edge moves apart: darker below, lighter above.
        assert!(at(&after, 21) < at(&before, 21));
        assert!(at(&after, 39) > at(&before, 39));
    }

    #[test]
    fn keep_alpha_fills_only_what_is_there() {
        let (w, h) = (300, 10);
        // Opaque on the left, transparent on the right (a whole empty tile).
        let px = (0..w * h)
            .map(|i| if i % w < 100 { [65535; 4] } else { [0; 4] })
            .collect();
        let pixels = doc_with(px, w, h).layers.remove(0).pixels;
        let filled = fill_pixels(&pixels, Some([0, 0, 0, 65535]), None);
        let locked = keep_alpha(&pixels, filled);
        assert_eq!(locked.get(10, 5), [0, 0, 0, 65535]);
        assert_eq!(locked.get(150, 5)[3], 0);
        assert_eq!(locked.get(280, 5)[3], 0);
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
    fn grain_layer_leaves_image_unchanged_at_amount_zero() {
        let (w, h) = (300, 200);
        let px: Vec<crate::Pixel> = (0..w * h)
            .map(|i| {
                [
                    ((i * 17) % 65535) as u16,
                    ((i * 31) % 65535) as u16,
                    ((i * 53) % 65535) as u16,
                    65535,
                ]
            })
            .collect();
        let mut doc = doc_with(px, w, h);
        let before = doc.composite();

        let opts = NoiseOptions {
            amount: 0.0,
            ..Default::default()
        };
        let layer_id = add_noise_layer(&mut doc, 0, &opts);
        let after = doc.composite();

        assert_eq!(doc.layer(layer_id).unwrap().blend, BlendMode::Overlay);
        assert_eq!(doc.layer(layer_id).unwrap().opacity, 0.0);
        assert_eq!(before.pixels(), after.pixels());
    }

    #[test]
    fn dodge_and_burn_curves_lighten_and_darken_where_painted() {
        let grey = [30000, 30000, 30000, 65535];
        let mut doc = doc_with(vec![grey; 100], 10, 10);
        let (dodge, burn) = dodge_and_burn_curves(&mut doc, 0);
        let group = doc.layers.last().unwrap();
        assert!(group.is_group && group.blend == BlendMode::PassThrough);
        assert_eq!(group.name, "Dodge & Burn");
        assert_eq!(doc.layer(dodge).unwrap().parent, Some(group.id));
        assert_eq!(doc.layer(burn).unwrap().parent, Some(group.id));
        assert!(doc.index_of(burn).unwrap() < doc.index_of(dodge).unwrap());
        // Black masks: no change until painted.
        assert_eq!(doc.composite().pixels()[0], grey);
        let paint = |doc: &mut Document, id| {
            doc.layer_mut(id).unwrap().mask.as_mut().unwrap().pixels =
                Tiled::new(10, 10, crate::layer::MASK_WHITE);
        };
        paint(&mut doc, dodge);
        let lighter = doc.composite().pixels()[0][0];
        assert!(lighter > grey[0] + 5000, "dodged to {lighter}");
        doc.layer_mut(dodge).unwrap().visible = false;
        paint(&mut doc, burn);
        let darker = doc.composite().pixels()[0][0];
        assert!(darker < grey[0] - 5000, "burned to {darker}");
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

    #[test]
    fn merge_layers_flattens_several_where_the_top_one_was() {
        let (w, h) = (10, 10);
        let mut doc = doc_with(vec![[40000, 40000, 40000, 65535]; 100], w, h);
        let solid = |doc: &mut Document, name: &str, px: crate::Pixel| {
            let id = doc.next_layer_id();
            doc.layers.push(Layer::from_raster(id, name, &Raster::new(w, h, vec![px; 100])));
            id
        };
        let red = solid(&mut doc, "red", [65535, 0, 0, 65535]);
        let skip = solid(&mut doc, "skip", [0, 65535, 0, 65535]);
        let blue = solid(&mut doc, "blue", [0, 0, 65535, 32768]);
        doc.layer_mut(red).unwrap().opacity = 0.5;
        let skip_index = doc.index_of(skip).unwrap();
        let group = doc.group_layer(skip_index);

        assert_eq!(merge_layers(&mut doc, &[blue]), None, "one isn't enough");
        assert_eq!(merge_layers(&mut doc, &[red, blue]), Some(blue));
        // Where "blue" was, with its name, above the untouched group.
        let names: Vec<&str> = doc.layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["Background", "skip", "Group 1", "blue"]);
        assert_eq!(doc.layers[1].parent, Some(group));
        let merged = doc.layer(blue).unwrap();
        assert_eq!((merged.opacity, merged.blend), (1.0, BlendMode::Normal));
        // Half blue over half red over transparency, not over the rest.
        let px = merged.pixels.get(5, 5);
        assert!(px[3].abs_diff(49152) <= 2, "{px:?}");
        assert!((px[2] - 2 * px[0], px[1]) == (0, 0), "{px:?}");
    }
}
