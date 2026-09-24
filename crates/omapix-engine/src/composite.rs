//! Flattening a layer stack into one image.

use std::collections::HashSet;

use rayon::prelude::*;

use crate::adjust::Prepared;
use crate::blend::BlendMode;
use crate::layer::Layer;
use crate::tiled::{TILE, TILE_PIXELS};
use crate::{Pixel, Raster};

const MAX: f32 = u16::MAX as f32;

/// Composite layers (bottom first) into a single image.
///
/// Each layer is blended onto everything below it with its blend mode, then
/// combined by its alpha × opacity × mask using source-over, as in
/// Photoshop and the W3C compositing spec. Tiles are processed in parallel.
pub fn composite(layers: &[Layer], width: u32, height: u32) -> Raster {
    let visible = prepare(layers);
    let w = width as usize;
    let mut out = vec![[0u16; 4]; w * height as usize];
    let cols = width.div_ceil(TILE);

    out.par_chunks_mut(w * TILE as usize)
        .enumerate()
        .for_each(|(row, band)| {
            let row = row as u32;
            let mut acc = vec![[0f32; 4]; TILE_PIXELS];
            for col in 0..cols {
                acc.fill([0.0; 4]);
                blend_all(&mut acc, &visible, col, row);
                let x0 = (col * TILE) as usize;
                let tw = (TILE as usize).min(w - x0);
                for (ty, line) in band.chunks_mut(w).enumerate() {
                    let src = &acc[ty * TILE as usize..ty * TILE as usize + tw];
                    for (dst, px) in line[x0..x0 + tw].iter_mut().zip(src) {
                        *dst = to_u16(*px);
                    }
                }
            }
        });
    Raster::new(width, height, out)
}

/// Recomposite only the given 256 px tiles (col, row) into an existing
/// flattened image, e.g. the area a brush stroke just touched.
pub fn composite_into(layers: &[Layer], image: &mut Raster, tiles: &[(u32, u32)]) {
    let visible = prepare(layers);
    type Blended = ((u32, u32), Vec<[f32; 4]>);
    let results: Vec<Blended> = tiles
        .par_iter()
        .map(|&(col, row)| {
            let mut acc = vec![[0f32; 4]; TILE_PIXELS];
            blend_all(&mut acc, &visible, col, row);
            ((col, row), acc)
        })
        .collect();
    let (w, h) = (image.width(), image.height());
    for ((col, row), acc) in results {
        let (x0, y0) = (col * TILE, row * TILE);
        if x0 >= w || y0 >= h {
            continue;
        }
        let tw = TILE.min(w - x0) as usize;
        for ty in 0..TILE.min(h - y0) {
            let src = &acc[(ty * TILE) as usize..(ty * TILE) as usize + tw];
            let line = &mut image.row_mut(y0 + ty)[x0 as usize..x0 as usize + tw];
            for (dst, px) in line.iter_mut().zip(src) {
                *dst = to_u16(*px);
            }
        }
    }
}

/// A visible layer ready to blend: its adjustment made ready to apply,
/// and for a group, the visible layers in it.
struct Node<'a> {
    layer: &'a Layer,
    adjustment: Option<Prepared>,
    children: Vec<Node<'a>>,
}

/// The visible layers as a tree of groups, bottom first. Layers whose group
/// isn't among `layers` (such as a pair being merged) count as top level.
fn prepare(layers: &[Layer]) -> Vec<Node<'_>> {
    let groups: HashSet<u64> = layers.iter().filter(|l| l.is_group).map(|l| l.id).collect();
    nodes(layers, &|l| l.parent.is_none_or(|p| !groups.contains(&p)))
}

fn nodes<'a>(layers: &'a [Layer], within: &dyn Fn(&Layer) -> bool) -> Vec<Node<'a>> {
    layers
        .iter()
        .filter(|l| within(l) && l.visible && l.opacity > 0.0)
        .map(|l| Node {
            layer: l,
            adjustment: l.adjustment.as_ref().map(|a| a.prepare()),
            children: if l.is_group {
                nodes(layers, &|c| c.parent == Some(l.id))
            } else {
                Vec::new()
            },
        })
        .collect()
}

fn blend_all(acc: &mut [[f32; 4]], nodes: &[Node], col: u32, row: u32) {
    for node in nodes {
        blend_node(acc, node, col, row);
    }
}

/// A layer's mask over one tile.
struct Coverage<'a> {
    tile: Option<&'a [u16]>,
    fill: f32,
}

impl<'a> Coverage<'a> {
    /// `None` where the mask hides the whole tile.
    fn of(layer: &'a Layer, col: u32, row: u32) -> Option<Self> {
        let mask = layer.mask.as_ref().filter(|m| m.enabled).map(|m| &m.pixels);
        let tile = mask.and_then(|m| m.tile(col, row));
        let fill = mask.map_or(1.0, |m| f32::from(m.fill()) / MAX);
        (tile.is_some() || fill > 0.0).then_some(Self { tile, fill })
    }

    #[inline]
    fn at(&self, i: usize) -> f32 {
        self.tile.map_or(self.fill, |t| f32::from(t[i]) / MAX)
    }

    fn is_full(&self) -> bool {
        self.tile.is_none() && self.fill >= 1.0
    }
}

fn blend_node(acc: &mut [[f32; 4]], node: &Node, col: u32, row: u32) {
    let layer = node.layer;
    if !layer.is_group {
        blend_tile(acc, layer, node.adjustment.as_ref(), col, row);
        return;
    }
    let Some(coverage) = Coverage::of(layer, col, row) else {
        return;
    };
    let blend_if = layer.blend_if.as_ref().filter(|b| !b.is_neutral());
    if layer.blend != BlendMode::PassThrough {
        // Composite the contents on their own, then blend the result like
        // a single layer.
        let mut inner = vec![[0f32; 4]; TILE_PIXELS];
        blend_all(&mut inner, &node.children, col, row);
        blend_source(acc, layer, &coverage, |i| inner[i]);
        return;
    }
    // Pass Through: the contents blend straight onto what's below. The
    // group's opacity, mask and Blend If then fade between that and what
    // was there before.
    if layer.opacity >= 1.0 && coverage.is_full() && blend_if.is_none() {
        blend_all(acc, &node.children, col, row);
        return;
    }
    let before = acc.to_vec();
    blend_all(acc, &node.children, col, row);
    for (i, (px, b)) in acc.iter_mut().zip(&before).enumerate() {
        let (cs, cb) = ([px[0], px[1], px[2]], [b[0], b[1], b[2]]);
        let t = layer.opacity * coverage.at(i) * blend_if.map_or(1.0, |f| f.factor(cs, cb));
        *px = mix(*b, *px, t);
    }
}

/// `a` faded towards `b` by `t`, with premultiplied alpha, so a colour
/// with no alpha doesn't bleed in.
#[inline]
fn mix(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    if t <= 0.0 {
        return a;
    }
    if t >= 1.0 {
        return b;
    }
    let alpha = a[3] + (b[3] - a[3]) * t;
    if alpha <= 0.0 {
        return [0.0; 4];
    }
    let mut out = [0.0, 0.0, 0.0, alpha];
    for c in 0..3 {
        out[c] = (a[c] * a[3] + (b[c] * b[3] - a[c] * a[3]) * t) / alpha;
    }
    out
}

fn blend_tile(
    acc: &mut [[f32; 4]],
    layer: &Layer,
    adjustment: Option<&Prepared>,
    col: u32,
    row: u32,
) {
    let Some(coverage) = Coverage::of(layer, col, row) else {
        return;
    };
    let opacity = layer.opacity;
    let mode = layer.blend;
    let blend_if = layer.blend_if.as_ref().filter(|b| !b.is_neutral());

    // An adjustment layer changes what's below it: the adjusted colour is
    // blended onto the original with the layer's mode, opacity and mask.
    // Transparency below is left as it is.
    if let Some(adjustment) = adjustment {
        for (i, px) in acc.iter_mut().enumerate() {
            let [br, bg, bb, a_b] = *px;
            let a = opacity * coverage.at(i);
            if a_b <= 0.0 || a <= 0.0 {
                continue;
            }
            let cb = [br, bg, bb];
            let adjusted = adjustment.apply(cb);
            let a = a * blend_if.map_or(1.0, |b| b.factor(adjusted, cb));
            let blended = mode.apply(cb, adjusted);
            for c in 0..3 {
                px[c] = cb[c] + (blended[c] - cb[c]) * a;
            }
        }
        return;
    }

    // An empty tile is transparent and changes nothing.
    let Some(src) = layer.pixels.tile(col, row) else {
        return;
    };
    blend_source(acc, layer, &coverage, |i| {
        let s = src[i];
        [
            f32::from(s[0]) / MAX,
            f32::from(s[1]) / MAX,
            f32::from(s[2]) / MAX,
            f32::from(s[3]) / MAX,
        ]
    });
}

/// Blend a tile of colours (0–1, straight alpha) onto `acc` with `layer`'s
/// blend mode, opacity, Blend If and mask coverage.
fn blend_source(
    acc: &mut [[f32; 4]],
    layer: &Layer,
    coverage: &Coverage,
    src: impl Fn(usize) -> [f32; 4],
) {
    let opacity = layer.opacity;
    let mode = layer.blend;
    let blend_if = layer.blend_if.as_ref().filter(|b| !b.is_neutral());
    for (i, px) in acc.iter_mut().enumerate() {
        let [sr, sg, sb, sa] = src(i);
        let a_s = sa * opacity * coverage.at(i);
        if a_s <= 0.0 {
            continue;
        }
        let cs = [sr, sg, sb];
        let [br, bg, bb, a_b] = *px;
        let cb = [br, bg, bb];
        let a_s = a_s * blend_if.map_or(1.0, |b| b.factor(cs, cb));
        if a_s <= 0.0 {
            continue;
        }
        let blended = if a_b > 0.0 { mode.apply(cb, cs) } else { cs };
        let a_o = a_s + a_b * (1.0 - a_s);
        let mut out = [0.0; 4];
        for c in 0..3 {
            let co = a_s * (1.0 - a_b) * cs[c] + a_s * a_b * blended[c] + (1.0 - a_s) * a_b * cb[c];
            out[c] = co / a_o;
        }
        out[3] = a_o;
        *px = out;
    }
}

fn to_u16(px: [f32; 4]) -> Pixel {
    px.map(|v| (v.clamp(0.0, 1.0) * MAX).round() as u16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blend::BlendMode;
    use crate::layer::Mask;
    use crate::tiled::Tiled;

    fn solid(id: u64, w: u32, h: u32, px: Pixel) -> Layer {
        let raster = Raster::new(w, h, vec![px; (w * h) as usize]);
        Layer::from_raster(id, "solid", &raster)
    }

    #[test]
    fn single_opaque_layer_is_unchanged() {
        let layer = solid(1, 300, 260, [1000, 30000, 65535, 65535]);
        let out = composite(&[layer], 300, 260);
        assert_eq!(out.get(299, 259), [1000, 30000, 65535, 65535]);
    }

    #[test]
    fn half_opacity_normal_mixes_evenly() {
        let bottom = solid(1, 10, 10, [0, 0, 0, 65535]);
        let mut top = solid(2, 10, 10, [65535, 65535, 65535, 65535]);
        top.opacity = 0.5;
        let out = composite(&[bottom, top], 10, 10);
        assert!(out.get(5, 5)[0].abs_diff(32768) <= 1);
        assert_eq!(out.get(5, 5)[3], 65535);
    }

    #[test]
    fn hidden_layers_and_black_masks_are_ignored() {
        let bottom = solid(1, 10, 10, [100, 200, 300, 65535]);
        let mut hidden = solid(2, 10, 10, [65535; 4]);
        hidden.visible = false;
        let mut masked = solid(3, 10, 10, [65535; 4]);
        let mut mask = Mask::white(10, 10);
        mask.invert();
        masked.mask = Some(mask);
        let out = composite(&[bottom, hidden, masked], 10, 10);
        assert_eq!(out.get(3, 3), [100, 200, 300, 65535]);
    }

    #[test]
    fn frequency_separation_reconstructs_the_image() {
        let (w, h) = (64, 64);
        let image: Vec<Pixel> = (0..w * h)
            .map(|i| {
                [
                    (i * 13 % 60000) as u16 + 2000,
                    (i * 7 % 50000) as u16 + 5000,
                    30000,
                    65535,
                ]
            })
            .collect();
        let low: Vec<Pixel> = image
            .iter()
            .map(|p| [p[0] / 2 + 15000, p[1] / 2 + 12000, 29000, 65535])
            .collect();
        let image = Raster::new(w, h, image);
        let low_layer = Layer::from_raster(1, "low", &Raster::new(w, h, low.clone()));

        // High = image grain-extract low.
        let mut extract = Layer::from_raster(2, "x", &Raster::new(w, h, low));
        extract.blend = BlendMode::GrainExtract;
        let high = composite(&[Layer::from_raster(3, "img", &image), extract], w, h);

        let mut high_layer = Layer::from_raster(4, "high", &high);
        high_layer.blend = BlendMode::GrainMerge;
        let rebuilt = composite(&[low_layer, high_layer], w, h);
        for (a, b) in rebuilt.pixels().iter().zip(image.pixels()) {
            for c in 0..3 {
                assert!(a[c].abs_diff(b[c]) <= 2, "{a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn adjustment_layers_change_what_is_below_through_their_mask() {
        use crate::adjust::{Adjustment, HueSaturation};
        let (w, h) = (300, 10);
        let bottom = solid(1, w, h, [50000, 20000, 20000, 65535]);
        let grey = Adjustment::HueSaturation(HueSaturation {
            saturation: -100.0,
            ..Default::default()
        });
        let mut adj = Layer::adjustment(2, grey, w, h);
        // Hide the adjustment on the right-hand tile.
        adj.mask.as_mut().unwrap().pixels.tile_mut(1, 0).fill(0);
        let out = composite(&[bottom, adj], w, h);
        let left = out.get(10, 5);
        assert!(left[0].abs_diff(left[1]) <= 2, "desaturated: {left:?}");
        assert_eq!(out.get(280, 5), [50000, 20000, 20000, 65535]);
    }

    #[test]
    fn blend_if_hides_the_layer_over_dark_underlying_pixels() {
        use crate::layer::BlendIf;
        let (w, h) = (256, 1);
        // Underlying: a black-to-white ramp. Top: solid red.
        let ramp: Vec<Pixel> = (0..256)
            .map(|x| {
                let v = (x * 257) as u16;
                [v, v, v, 65535]
            })
            .collect();
        let bottom = Layer::from_raster(1, "ramp", &Raster::new(w, h, ramp));
        let mut top = solid(2, w, h, [65535, 0, 0, 65535]);
        // Show only over underlying values above ~50 %, fading in from 40 %.
        top.blend_if = Some(BlendIf {
            underlying: [0.4, 0.5, 1.0, 1.0],
            ..BlendIf::default()
        });
        let out = composite(&[bottom, top], w, h);
        assert_eq!(
            out.get(50, 0),
            [50 * 257, 50 * 257, 50 * 257, 65535],
            "dark areas untouched"
        );
        assert_eq!(
            out.get(200, 0),
            [65535, 0, 0, 65535],
            "light areas fully covered"
        );
        let mid = out.get(115, 0); // ~45 %: half-way through the fade
        assert!(mid[0] > mid[1] && mid[1] > 0, "partially covered: {mid:?}");
    }

    #[test]
    fn partial_update_matches_full_composite() {
        let (w, h) = (600, 400);
        let bottom = solid(1, w, h, [10000, 20000, 30000, 65535]);
        let mut top = solid(2, w, h, [65535, 0, 0, 40000]);
        top.blend = BlendMode::Multiply;
        let mut layers = vec![bottom, top];
        let mut image = composite(&layers, w, h);
        layers[1].pixels.tile_mut(1, 1)[5] = [0, 65535, 0, 65535];
        composite_into(&layers, &mut image, &[(1, 1)]);
        assert_eq!(image.pixels(), composite(&layers, w, h).pixels());
    }

    /// Put `layers` in a new group with `id`, at the top of the list.
    fn grouped(id: u64, mut layers: Vec<Layer>, w: u32, h: u32) -> Vec<Layer> {
        for l in &mut layers {
            l.parent = Some(id);
        }
        layers.push(Layer::group(id, "group", w, h));
        layers
    }

    fn desaturate(id: u64, w: u32, h: u32) -> Layer {
        use crate::adjust::{Adjustment, HueSaturation};
        let grey = Adjustment::HueSaturation(HueSaturation {
            saturation: -100.0,
            ..Default::default()
        });
        Layer::adjustment(id, grey, w, h)
    }

    #[test]
    fn pass_through_groups_look_ungrouped() {
        let (w, h) = (300, 10);
        let bottom = solid(1, w, h, [50000, 20000, 20000, 65535]);
        let mut multiply = solid(2, w, h, [40000, 65535, 30000, 50000]);
        multiply.blend = BlendMode::Multiply;
        let contents = vec![multiply, desaturate(3, w, h)];
        let flat = composite(&[vec![bottom.clone()], contents.clone()].concat(), w, h);
        let mut layers = vec![bottom];
        layers.extend(grouped(10, contents, w, h));
        assert_eq!(composite(&layers, w, h).pixels(), flat.pixels());
    }

    #[test]
    fn pass_through_opacity_and_mask_fade_the_contents() {
        let (w, h) = (300, 10);
        let bottom = solid(1, w, h, [0, 0, 0, 65535]);
        let top = solid(2, w, h, [65535, 65535, 65535, 65535]);
        let mut layers = vec![bottom];
        layers.extend(grouped(10, vec![top], w, h));
        let group = layers.last_mut().unwrap();
        group.opacity = 0.5;
        // Hide the group on the right-hand tile.
        let mut mask = Mask::white(w, h);
        mask.pixels.tile_mut(1, 0).fill(0);
        group.mask = Some(mask);
        let out = composite(&layers, w, h);
        assert!(out.get(10, 5)[0].abs_diff(32768) <= 1, "{:?}", out.get(10, 5));
        assert_eq!(out.get(280, 5), [0, 0, 0, 65535]);
    }

    #[test]
    fn isolated_groups_keep_adjustments_to_their_contents() {
        let (w, h) = (10, 10);
        let red = [50000, 20000, 20000, 65535];
        let bottom = solid(1, w, h, red);
        let half = solid(2, w, h, [20000, 50000, 20000, 32768]);
        let mut layers = vec![bottom];
        layers.extend(grouped(10, vec![half, desaturate(3, w, h)], w, h));

        // Pass Through: the adjustment greys everything below it.
        let out = composite(&layers, w, h).get(5, 5);
        assert!(out[0].abs_diff(out[1]) <= 2, "all grey: {out:?}");

        // Normal: only the group's own layer turns grey, then it's laid
        // over the red at half strength.
        layers.last_mut().unwrap().blend = BlendMode::Normal;
        let out = composite(&layers, w, h).get(5, 5);
        assert!(out[0] > out[1] + 5000, "red shows through: {out:?}");
        assert!(out[1].abs_diff(out[2]) <= 2, "{out:?}");
    }

    #[test]
    fn isolated_groups_blend_as_one_layer() {
        let (w, h) = (10, 10);
        let bottom = solid(1, w, h, [40000, 40000, 40000, 65535]);
        // Two layers that together are opaque mid grey, multiplied in as
        // one: half of what's below.
        let white = solid(2, w, h, [65535, 65535, 65535, 65535]);
        let mut darken = solid(3, w, h, [32768, 32768, 32768, 65535]);
        darken.blend = BlendMode::Multiply;
        let mut layers = vec![bottom];
        layers.extend(grouped(10, vec![white, darken], w, h));
        layers.last_mut().unwrap().blend = BlendMode::Multiply;
        assert!(composite(&layers, w, h).get(5, 5)[0].abs_diff(20000) <= 2);
    }

    #[test]
    fn hidden_groups_hide_their_contents_and_nest() {
        let (w, h) = (300, 300);
        let bottom = solid(1, w, h, [100, 200, 300, 65535]);
        let top = solid(2, w, h, [65535; 4]);
        let inner = grouped(10, vec![top], w, h);
        let mut layers = vec![bottom];
        layers.extend(grouped(11, inner, w, h));
        let white = composite(&layers, w, h);
        assert_eq!(white.get(5, 5), [65535; 4]);
        // The nested group is inside the outer one, so hiding the outer
        // hides both.
        layers.last_mut().unwrap().visible = false;
        assert_eq!(composite(&layers, w, h).get(5, 5), [100, 200, 300, 65535]);

        // Partial updates agree with full ones.
        layers.last_mut().unwrap().visible = true;
        layers[3].opacity = 0.5;
        layers[3].blend = BlendMode::Screen;
        let mut image = white;
        composite_into(&layers, &mut image, &[(0, 0), (1, 1)]);
        let full = composite(&layers, w, h);
        assert_eq!(image.get(5, 5), full.get(5, 5));
        assert_eq!(image.get(290, 290), full.get(290, 290));
    }

    #[test]
    fn pass_through_fades_onto_transparency_without_dark_fringes() {
        let (w, h) = (10, 10);
        let white = solid(2, w, h, [65535, 65535, 65535, 65535]);
        let mut layers = grouped(10, vec![white], w, h);
        layers.last_mut().unwrap().opacity = 0.5;
        let out = composite(&layers, w, h).get(5, 5);
        assert_eq!(out[0], 65535, "colour stays white: {out:?}");
        assert!(out[3].abs_diff(32768) <= 1);
    }

    #[test]
    fn empty_layer_costs_nothing_and_changes_nothing() {
        let bottom = solid(1, 10, 10, [5, 6, 7, 65535]);
        let empty = Layer::from_pixels(2, "empty", Tiled::new(10, 10, [0; 4]));
        assert_eq!(
            composite(&[bottom, empty], 10, 10).get(0, 0),
            [5, 6, 7, 65535]
        );
    }
}
