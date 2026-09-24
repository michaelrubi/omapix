//! Flattening a layer stack into one image.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use rayon::prelude::*;

use crate::adjust::Prepared;
use crate::blend::BlendMode;
use crate::layer::{BlendIf, Layer};
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
    for (&(col, row), tile) in tiles.iter().zip(composite_tiles(layers, tiles, None)) {
        image.put_tile(col, row, &tile);
    }
}

/// Composite the given 256 px tiles (col, row) in parallel, each returned
/// as a whole tile of pixels, row-major, ready for [`Raster::put_tile`].
/// With `groups`, what isolated groups composite to is reused from earlier
/// calls where nothing in them has changed, and kept for later ones.
pub fn composite_tiles(
    layers: &[Layer],
    tiles: &[(u32, u32)],
    groups: Option<&GroupCache>,
) -> Vec<Vec<Pixel>> {
    let mut visible = prepare(layers);
    if let Some(groups) = groups {
        groups.attach(&mut visible);
    }
    tiles
        .par_iter()
        .map(|&(col, row)| {
            let mut acc = vec![[0f32; 4]; TILE_PIXELS];
            blend_all(&mut acc, &visible, col, row);
            acc.into_iter().map(to_u16).collect()
        })
        .collect()
}

/// A visible layer ready to blend: its adjustment made ready to apply,
/// for a group, the visible layers in it, and the visible layers clipped
/// to it.
struct Node<'a> {
    layer: &'a Layer,
    adjustment: Option<Prepared>,
    children: Vec<Node<'a>>,
    clipped: Vec<Node<'a>>,
    /// For a group composited on its own, where to keep what its contents
    /// composite to.
    cached: Option<Arc<Slots>>,
}

/// What a group's contents composite to, a slot per 256 px tile (row
/// major), each filled on first use: `None` where it's all transparent.
type Slots = Vec<OnceLock<Option<Arc<Vec<Pixel>>>>>;

/// What the contents of groups that are composited on their own (any mode
/// but Pass Through, or clipped, or with layers clipped to them) last
/// composited to, reused while nothing in them changes. Dragging a slider
/// on a layer above a big group then just blends the group's result
/// rather than compositing everything in it again.
///
/// Each group's result costs up to as much memory as a layer. One cache is
/// for one image size: a different size (such as shrunk layers for a
/// preview) needs its own, or each call throws away the other's results.
#[derive(Default)]
pub struct GroupCache {
    groups: Mutex<HashMap<u64, Cached>>,
}

struct Cached {
    /// The group's visible contents when last composited, in order, each
    /// with where it sits in the group (see [`contents`]). Holding on to
    /// them keeps their tiles alive, so a tile at the same address is
    /// known to be the same tile.
    contents: Vec<(u32, Layer)>,
    slots: Arc<Slots>,
}

impl GroupCache {
    /// Give each group among `nodes` that's composited on its own the
    /// results kept for it, less those of tiles where something in it has
    /// changed since. Groups that aren't there any more are forgotten.
    fn attach(&self, nodes: &mut [Node]) {
        let mut groups = self.groups.lock().expect("group cache");
        let mut seen = HashSet::new();
        attach(&mut groups, &mut seen, nodes, false);
        groups.retain(|id, _| seen.contains(id));
    }
}

fn attach(
    groups: &mut HashMap<u64, Cached>,
    seen: &mut HashSet<u64>,
    nodes: &mut [Node],
    atop: bool,
) {
    for node in nodes {
        let layer = node.layer;
        let own = layer.blend != BlendMode::PassThrough || atop || !node.clipped.is_empty();
        if layer.is_group && own {
            seen.insert(layer.id);
            node.cached = Some(slots(groups, node));
        }
        attach(groups, seen, &mut node.children, false);
        attach(groups, seen, &mut node.clipped, true);
    }
}

/// The slots for group `node`, keeping those of tiles where nothing in it
/// has changed since the last call.
fn slots(groups: &mut HashMap<u64, Cached>, node: &Node) -> Arc<Slots> {
    let mut now = Vec::new();
    contents(&node.children, 0, &mut now);
    let pixels = &node.layer.pixels;
    let (cols, rows) = (pixels.cols(), pixels.rows());
    let fresh = || (0..cols * rows).map(|_| OnceLock::new()).collect::<Slots>();
    let old = groups.remove(&node.layer.id).filter(|old| {
        old.contents.len() == now.len()
            && old
                .contents
                .iter()
                .zip(&now)
                .all(|((d0, a), (d1, b))| d0 == d1 && same_settings(a, b))
    });
    let slots = match old {
        Some(old) => {
            let same = |col: u32, row: u32| {
                old.contents.iter().zip(&now).all(|((_, a), (_, b))| {
                    let masks = match (&a.mask, &b.mask) {
                        (Some(m), Some(n)) => m.pixels.same_tile(&n.pixels, col, row),
                        _ => true,
                    };
                    masks && a.pixels.same_tile(&b.pixels, col, row)
                })
            };
            let kept = (0..rows).flat_map(|row| (0..cols).map(move |col| (col, row)));
            if kept.clone().all(|(col, row)| same(col, row)) {
                old.slots
            } else {
                // A new set, so a render still going with the old contents
                // can't fill in the new ones.
                let slots = kept
                    .zip(old.slots.iter())
                    .map(|((col, row), slot)| match slot.get() {
                        Some(tile) if same(col, row) => OnceLock::from(tile.clone()),
                        _ => OnceLock::new(),
                    })
                    .collect();
                Arc::new(slots)
            }
        }
        None => Arc::new(fresh()),
    };
    let contents = now.into_iter().map(|(d, l)| (d, l.clone())).collect();
    groups.insert(
        node.layer.id,
        Cached {
            contents,
            slots: Arc::clone(&slots),
        },
    );
    slots
}

/// Everything in `nodes`, depth first, each with its depth and whether it's
/// clipped (odd) or not (even), which together give the tree's shape.
fn contents<'a>(nodes: &[Node<'a>], depth: u32, out: &mut Vec<(u32, &'a Layer)>) {
    for node in nodes {
        out.push((depth * 2, node.layer));
        contents(&node.children, depth + 1, out);
        for c in &node.clipped {
            out.push((depth * 2 + 1, c.layer));
            contents(&c.children, depth + 1, out);
        }
    }
}

/// True if two layers composite the same where their tiles are the same:
/// everything but their pixels and name.
fn same_settings(a: &Layer, b: &Layer) -> bool {
    let size = |l: &Layer| (l.pixels.width(), l.pixels.height());
    let mask = |l: &Layer| {
        l.mask
            .as_ref()
            .map(|m| (m.enabled, m.pixels.fill(), m.pixels.width(), m.pixels.height()))
    };
    a.id == b.id
        && a.is_group == b.is_group
        && a.opacity == b.opacity
        && a.blend == b.blend
        && a.blend_if == b.blend_if
        && a.adjustment == b.adjustment
        && size(a) == size(b)
        && a.pixels.fill() == b.pixels.fill()
        && mask(a) == mask(b)
}

/// The visible layers as a tree of groups, bottom first. Layers whose group
/// isn't among `layers` (such as a pair being merged) count as top level.
fn prepare(layers: &[Layer]) -> Vec<Node<'_>> {
    let groups: HashSet<u64> = layers.iter().filter(|l| l.is_group).map(|l| l.id).collect();
    nodes(layers, &|l| l.parent.is_none_or(|p| !groups.contains(&p)))
}

fn nodes<'a>(layers: &'a [Layer], within: &dyn Fn(&Layer) -> bool) -> Vec<Node<'a>> {
    let mut out: Vec<Node> = Vec::new();
    // What clipped layers clip to: nothing yet (they show unclipped), a
    // hidden layer (they're hidden too), or the last node in `out`.
    #[derive(PartialEq)]
    enum Base {
        None,
        Hidden,
        Shown,
    }
    let mut base = Base::None;
    for l in layers.iter().filter(|l| within(l)) {
        let shown = l.visible && l.opacity > 0.0;
        let clipped = l.clipped && base != Base::None;
        if clipped && base == Base::Hidden {
            continue;
        }
        if !clipped && !l.clipped {
            base = if shown { Base::Shown } else { Base::Hidden };
        }
        if !shown {
            continue;
        }
        let node = Node {
            layer: l,
            adjustment: l.adjustment.as_ref().map(|a| a.prepare()),
            children: if l.is_group {
                nodes(layers, &|c| c.parent == Some(l.id))
            } else {
                Vec::new()
            },
            clipped: Vec::new(),
            cached: None,
        };
        match out.last_mut() {
            Some(b) if clipped => b.clipped.push(node),
            _ => out.push(node),
        }
    }
    out
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
    const FULL: Coverage<'static> = Coverage {
        tile: None,
        fill: 1.0,
    };

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

/// How a tile of colours is laid over what's below.
#[derive(Clone, Copy)]
struct Params<'a> {
    mode: BlendMode,
    opacity: f32,
    blend_if: Option<&'a BlendIf>,
    /// Keep what's below's alpha, as layers clipped to it do (the W3C's
    /// source-atop): it's painted on only where there's something already.
    atop: bool,
}

impl<'a> Params<'a> {
    /// A layer's own mode, opacity and Blend If. A group that passes through
    /// but is blended as one layer anyway (clipped, or with layers clipped
    /// to it) counts as Normal.
    fn of(layer: &'a Layer) -> Self {
        Self {
            mode: match layer.blend {
                BlendMode::PassThrough => BlendMode::Normal,
                mode => mode,
            },
            opacity: layer.opacity,
            blend_if: layer.blend_if.as_ref().filter(|b| !b.is_neutral()),
            atop: false,
        }
    }

    fn atop(self, atop: bool) -> Self {
        Self { atop, ..self }
    }
}

fn blend_node(acc: &mut [[f32; 4]], node: &Node, col: u32, row: u32) {
    if !node.clipped.is_empty() {
        blend_clipping(acc, node, col, row);
    } else {
        blend_one(acc, node, false, col, row);
    }
}

/// Blend a layer without what's clipped to it, `atop` what's below if it's
/// itself clipped.
fn blend_one(acc: &mut [[f32; 4]], node: &Node, atop: bool, col: u32, row: u32) {
    let layer = node.layer;
    if !layer.is_group {
        blend_tile(acc, layer, node.adjustment.as_ref(), atop, col, row);
        return;
    }
    let Some(coverage) = Coverage::of(layer, col, row) else {
        return;
    };
    if layer.blend != BlendMode::PassThrough || atop {
        // Composite the contents on their own, then blend the result like
        // a single layer.
        if let Some(inner) = group_tile(node, col, row) {
            blend_source(acc, Params::of(layer).atop(atop), &coverage, |i| {
                unpack(inner[i])
            });
        }
        return;
    }
    // Pass Through: the contents blend straight onto what's below. The
    // group's opacity, mask and Blend If then fade between that and what
    // was there before.
    let blend_if = layer.blend_if.as_ref().filter(|b| !b.is_neutral());
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

/// A clipping mask: a base layer and the layers clipped to it, which show
/// only where it does. As with Photoshop's default "Blend Clipped Layers
/// as Group", they're composited onto the base on their own, then the
/// result is blended like the base, with its mode, opacity and Blend If.
fn blend_clipping(acc: &mut [[f32; 4]], node: &Node, col: u32, row: u32) {
    let base = node.layer;
    let Some(coverage) = Coverage::of(base, col, row) else {
        return;
    };
    if base.adjustment.is_some() {
        // An adjustment layer has no pixels to clip to, so its mask is the
        // shape: the clipped layers blend onto what's below through it.
        blend_one(acc, node, false, col, row);
        let before = acc.to_vec();
        for c in &node.clipped {
            blend_one(acc, c, false, col, row);
        }
        for (i, (px, b)) in acc.iter_mut().zip(&before).enumerate() {
            *px = mix(*b, *px, coverage.at(i));
        }
        return;
    }
    let mut inner = vec![[0f32; 4]; TILE_PIXELS];
    if base.is_group {
        let Some(contents) = group_tile(node, col, row) else {
            return;
        };
        for (i, px) in inner.iter_mut().enumerate() {
            *px = unpack(contents[i]);
            px[3] *= coverage.at(i);
        }
    } else {
        let Some(src) = base.pixels.tile(col, row) else {
            // Transparent, so nothing clipped to it shows either.
            return;
        };
        let normal = Params {
            mode: BlendMode::Normal,
            opacity: 1.0,
            blend_if: None,
            atop: false,
        };
        blend_source(&mut inner, normal, &coverage, |i| unpack(src[i]));
    }
    for c in &node.clipped {
        blend_one(&mut inner, c, true, col, row);
    }
    blend_source(acc, Params::of(base), &Coverage::FULL, |i| inner[i]);
}

/// What's in group `node` composited on its own over one tile, or `None`
/// where that's all transparent. Rounded to 16 bits, as it's cached, so
/// that results are the same with the cache as without.
fn group_tile(node: &Node, col: u32, row: u32) -> Option<Arc<Vec<Pixel>>> {
    let make = || {
        let mut inner = vec![[0f32; 4]; TILE_PIXELS];
        blend_all(&mut inner, &node.children, col, row);
        let tile: Vec<Pixel> = inner.into_iter().map(to_u16).collect();
        tile.iter().any(|p| p[3] > 0).then(|| Arc::new(tile))
    };
    let Some(slots) = &node.cached else {
        return make();
    };
    let slot = &slots[(row * node.layer.pixels.cols() + col) as usize];
    if let Some(tile) = slot.get() {
        return tile.clone();
    }
    // Not `get_or_init`, so another render of the same tile never waits.
    let tile = make();
    let _ = slot.set(tile.clone());
    tile
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

#[inline]
fn unpack(s: Pixel) -> [f32; 4] {
    s.map(|v| f32::from(v) / MAX)
}

fn blend_tile(
    acc: &mut [[f32; 4]],
    layer: &Layer,
    adjustment: Option<&Prepared>,
    atop: bool,
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
    blend_source(acc, Params::of(layer).atop(atop), &coverage, |i| {
        unpack(src[i])
    });
}

/// Blend a tile of colours (0–1, straight alpha) onto `acc` with these
/// params and mask coverage.
fn blend_source(
    acc: &mut [[f32; 4]],
    params: Params,
    coverage: &Coverage,
    src: impl Fn(usize) -> [f32; 4],
) {
    let Params {
        mode,
        opacity,
        blend_if,
        atop,
    } = params;
    for (i, px) in acc.iter_mut().enumerate() {
        let [sr, sg, sb, sa] = src(i);
        let a_s = sa * opacity * coverage.at(i);
        if a_s <= 0.0 {
            continue;
        }
        let cs = [sr, sg, sb];
        let [br, bg, bb, a_b] = *px;
        if atop && a_b <= 0.0 {
            continue;
        }
        let cb = [br, bg, bb];
        let a_s = a_s * blend_if.map_or(1.0, |b| b.factor(cs, cb));
        if a_s <= 0.0 {
            continue;
        }
        let blended = if a_b > 0.0 { mode.apply(cb, cs) } else { cs };
        if atop {
            for c in 0..3 {
                px[c] = cb[c] + (blended[c] - cb[c]) * a_s;
            }
            continue;
        }
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

    /// A layer of `px` over the left-hand tile only, transparent elsewhere.
    fn left_tile(id: u64, w: u32, h: u32, px: Pixel) -> Layer {
        let mut layer = Layer::empty(id, "left", w, h);
        layer.pixels.tile_mut(0, 0).fill(px);
        layer
    }

    fn clipped(mut layer: Layer) -> Layer {
        layer.clipped = true;
        layer
    }

    #[test]
    fn clipped_layers_show_only_where_the_base_does() {
        let (w, h) = (300, 10);
        let red = [50000, 10000, 10000, 65535];
        let bottom = solid(1, w, h, red);
        let base = left_tile(2, w, h, [10000, 50000, 10000, 65535]);
        let blue = clipped(solid(3, w, h, [0, 0, 65535, 65535]));
        let layers = vec![bottom, base, blue];
        let out = composite(&layers, w, h);
        assert_eq!(out.get(10, 5), [0, 0, 65535, 65535]);
        assert_eq!(out.get(280, 5), red, "outside the base");

        // Partial updates agree with full ones.
        let mut image = Raster::new(w, h, vec![[0; 4]; (w * h) as usize]);
        composite_into(&layers, &mut image, &[(0, 0), (1, 0)]);
        assert_eq!(image.pixels(), out.pixels());
    }

    #[test]
    fn clipped_adjustments_change_only_the_base() {
        let (w, h) = (300, 10);
        let red = [50000, 10000, 10000, 65535];
        let bottom = solid(1, w, h, red);
        // Half transparent, so the red below would show the adjustment
        // too if it weren't clipped.
        let base = left_tile(2, w, h, [10000, 50000, 10000, 32768]);
        let layers = vec![bottom.clone(), base.clone(), clipped(desaturate(3, w, h))];
        let out = composite(&layers, w, h);
        assert_eq!(out.get(280, 5), red);
        // The base turned grey and was then laid over the red at half
        // strength, so the red still shows.
        let grey = composite(&[base, desaturate(3, w, h)], w, h).get(10, 5);
        assert!(grey[0].abs_diff(grey[1]) <= 2 && grey[3] == 32768, "{grey:?}");
        let grey_layer = solid(4, w, h, grey);
        let over = composite(&[bottom, grey_layer], w, h);
        assert_eq!(out.get(10, 5), over.get(10, 5));
    }

    #[test]
    fn a_clipped_run_takes_the_bases_mode_opacity_and_visibility() {
        let (w, h) = (10, 10);
        let grey = [30000, 30000, 30000, 65535];
        let bottom = solid(1, w, h, grey);
        // A dodge & burn layer: 50 % grey in Soft Light changes nothing
        // until painted. A clipped white layer paints it white.
        let mut dodge = solid(2, w, h, [32768, 32768, 32768, 65535]);
        dodge.blend = BlendMode::SoftLight;
        let white = clipped(solid(3, w, h, [65535; 4]));
        let mut layers = vec![bottom.clone(), dodge, white];
        let mut soft_white = solid(4, w, h, [65535; 4]);
        soft_white.blend = BlendMode::SoftLight;
        let expected = composite(&[bottom, soft_white], w, h).get(5, 5);
        assert!(expected[0] > grey[0] + 5000);
        assert_eq!(composite(&layers, w, h).get(5, 5), expected);

        // The base's opacity fades the whole run.
        layers[1].opacity = 0.0;
        assert_eq!(composite(&layers, w, h).get(5, 5), grey);
        layers[1].opacity = 1.0;
        layers[1].visible = false;
        assert_eq!(composite(&layers, w, h).get(5, 5), grey, "hidden base hides the run");
    }

    #[test]
    fn clipping_to_an_adjustment_layer_uses_its_mask() {
        let (w, h) = (300, 10);
        let red = [50000, 10000, 10000, 65535];
        let bottom = solid(1, w, h, red);
        let mut adj = desaturate(2, w, h);
        adj.opacity = 0.0001; // Barely there, so only the clipped layer shows.
        adj.mask.as_mut().unwrap().pixels.tile_mut(1, 0).fill(0);
        let blue = clipped(solid(3, w, h, [0, 0, 65535, 65535]));
        let out = composite(&[bottom, adj, blue], w, h);
        assert_eq!(out.get(10, 5), [0, 0, 65535, 65535]);
        assert_eq!(out.get(280, 5), red);
    }

    #[test]
    fn clipping_to_a_group_clips_to_its_contents() {
        let (w, h) = (300, 10);
        let red = [50000, 10000, 10000, 65535];
        let mut layers = vec![solid(1, w, h, red)];
        layers.extend(grouped(10, vec![left_tile(2, w, h, [0, 65535, 0, 65535])], w, h));
        layers.push(clipped(solid(3, w, h, [0, 0, 65535, 65535])));
        let out = composite(&layers, w, h);
        assert_eq!(out.get(10, 5), [0, 0, 65535, 65535]);
        assert_eq!(out.get(280, 5), red);
    }

    #[test]
    fn clipped_layers_with_nothing_below_show_unclipped() {
        let blue = clipped(solid(1, 10, 10, [0, 0, 65535, 65535]));
        assert_eq!(composite(&[blue], 10, 10).get(5, 5), [0, 0, 65535, 65535]);
    }

    fn all_tiles(w: u32, h: u32) -> Vec<(u32, u32)> {
        (0..h.div_ceil(TILE))
            .flat_map(|r| (0..w.div_ceil(TILE)).map(move |c| (c, r)))
            .collect()
    }

    /// The kept result for group `id` at its `n`th tile (row major): `None`
    /// if there's none, `Some(None)` if it's transparent.
    fn kept(cache: &GroupCache, id: u64, n: usize) -> Option<Option<Arc<Vec<Pixel>>>> {
        let groups = cache.groups.lock().unwrap();
        groups.get(&id)?.slots[n].get().cloned()
    }

    #[test]
    fn cached_groups_match_uncached_and_redo_only_what_changed() {
        let (w, h) = (600, 300);
        let bottom = solid(1, w, h, [40000, 30000, 20000, 65535]);
        let mut multiply = solid(2, w, h, [50000, 65535, 30000, 50000]);
        multiply.blend = BlendMode::Multiply;
        let mut layers = vec![bottom];
        layers.extend(grouped(10, vec![multiply, desaturate(3, w, h)], w, h));
        layers.last_mut().unwrap().blend = BlendMode::Screen;
        layers.push(solid(4, w, h, [0, 0, 65535, 65535]));
        layers[4].opacity = 0.3;
        let tiles = all_tiles(w, h);
        let cache = GroupCache::default();
        let check = |layers: &[Layer]| {
            let cached = composite_tiles(layers, &tiles, Some(&cache));
            assert_eq!(cached, composite_tiles(layers, &tiles, None));
        };
        check(&layers);
        let first = kept(&cache, 10, 1).flatten().expect("kept");

        // A change above the group reuses what's in it.
        layers[4].opacity = 0.8;
        check(&layers);
        assert!(Arc::ptr_eq(&first, &kept(&cache, 10, 1).flatten().unwrap()));

        // Painting in the group redoes just the tiles painted.
        let before = kept(&cache, 10, 0).flatten().unwrap();
        layers[1].pixels.tile_mut(1, 0)[0] = [0, 0, 0, 65535];
        check(&layers);
        assert!(Arc::ptr_eq(&before, &kept(&cache, 10, 0).flatten().unwrap()));
        assert!(!Arc::ptr_eq(&first, &kept(&cache, 10, 1).flatten().unwrap()));

        // Changing a layer's settings in the group, or hiding it, redoes
        // the lot.
        let before = kept(&cache, 10, 0).flatten().unwrap();
        layers[1].opacity = 0.5;
        check(&layers);
        assert!(!Arc::ptr_eq(&before, &kept(&cache, 10, 0).flatten().unwrap()));
        layers[2].visible = false;
        check(&layers);

        // Pass Through groups aren't composited on their own, so there's
        // nothing to keep.
        layers[3].blend = BlendMode::PassThrough;
        check(&layers);
        assert!(cache.groups.lock().unwrap().is_empty());
    }

    #[test]
    fn cached_nested_and_clipped_groups_match_uncached() {
        let (w, h) = (300, 300);
        let bottom = solid(1, w, h, [40000, 30000, 20000, 65535]);
        let mut inner = grouped(10, vec![left_tile(2, w, h, [10000, 60000, 0, 65535])], w, h);
        inner[1].blend = BlendMode::Overlay;
        inner[1].parent = Some(11);
        let mut layers = vec![bottom];
        layers.extend(inner);
        layers.push(Layer::group(11, "outer", w, h));
        layers.push(clipped(solid(3, w, h, [65535, 0, 0, 40000])));
        let tiles = all_tiles(w, h);
        let cache = GroupCache::default();
        for opacity in [1.0, 0.5] {
            layers[4].opacity = opacity;
            let cached = composite_tiles(&layers, &tiles, Some(&cache));
            assert_eq!(cached, composite_tiles(&layers, &tiles, None));
        }
        // The outer group is a clipping base and the inner one isolated,
        // so both are kept.
        assert!(kept(&cache, 10, 0).flatten().is_some());
        assert!(kept(&cache, 11, 0).flatten().is_some());
        // Transparent tiles are kept as nothing.
        assert!(matches!(kept(&cache, 10, 3), Some(None)));
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
