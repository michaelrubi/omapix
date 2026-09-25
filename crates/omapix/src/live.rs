//! Showing a Move drag live on the GPU.
//!
//! Moving a layer on the CPU means translating it, compositing every tile
//! and rebuilding the display pyramid, many times a second. Instead, when
//! a drag starts, what's below the moving layer is composited once, and it,
//! the moving layer and the layers above are uploaded as textures at the
//! pyramid level on screen (`gpu.rs`). Each frame the GPU composites them
//! with the moving layer offset. When the drag ends the move is made for
//! real and the exact CPU render replaces the live one.
//!
//! Only stacks the shader handles are shown live: the moving layer is a
//! pixel layer at the top level, and the layers above it are pixel or
//! adjustment layers, at the top level or in Pass Through groups with
//! default settings. Anything else (clipping, Blend If, other groups)
//! moves on the CPU as before.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use omapix_engine::composite;
use omapix_engine::layer::Layer;
use omapix_engine::tiled::{TILE, Tiled};
use omapix_engine::{BlendMode, Document, Pixel};

/// Points per side of an adjustment's 3D lookup table.
pub const LUT_SIZE: usize = 33;

/// An area of one pyramid level, row-major, in that level's pixels.
pub struct Plane<T> {
    pub x0: u32,
    pub y0: u32,
    pub w: u32,
    pub h: u32,
    pub data: Vec<T>,
    /// What reads outside the area.
    pub fill: T,
}

impl<T: Copy + PartialEq + Send + Sync> Plane<T> {
    fn crop(tiled: &Tiled<T>, (x0, y0, w, h): (u32, u32, u32, u32)) -> Self {
        Self {
            x0,
            y0,
            w,
            h,
            data: tiled.crop(x0, y0, w, h),
            fill: tiled.fill(),
        }
    }
}

pub enum Source {
    Pixels(Plane<Pixel>),
    /// An adjustment, as a [`LUT_SIZE`]³ lookup table.
    Adjustment(Vec<[f32; 3]>),
}

/// One layer the GPU blends onto what's below it, as `composite.rs` does.
pub struct LiveLayer {
    pub source: Source,
    pub mask: Option<Plane<u16>>,
    pub mode: BlendMode,
    pub opacity: f32,
    /// Drawn at the drag's offset.
    pub moves: bool,
}

pub struct LiveStack {
    /// Tells stacks apart, so the GPU uploads each once.
    pub id: u64,
    /// The pyramid level, and the area of it on screen (x0, y0, w, h).
    pub level: usize,
    pub region: (u32, u32, u32, u32),
    /// Everything below the moving layer, composited, over `region`.
    pub below: Plane<Pixel>,
    /// The moving layer, then those above it, bottom first.
    pub layers: Vec<LiveLayer>,
}

/// The layers from `id` up that a live move blends, bottom first, or
/// `None` if the move can't be shown live.
fn stack(doc: &Document, id: u64) -> Option<Vec<usize>> {
    let index = doc.index_of(id)?;
    let moving = &doc.layers[index];
    let plain = |l: &Layer| {
        !l.clipped && !doc.is_clip_base(l.id) && l.blend_if.is_none_or(|b| b.is_neutral())
    };
    if moving.parent.is_some() || !moving.has_pixels() || !plain(moving) {
        return None;
    }
    let shown = |l: &Layer| {
        l.visible
            && std::iter::successors(l.parent, |&g| doc.layer(g)?.parent)
                .all(|g| doc.layer(g).is_some_and(|g| g.visible))
    };
    let mut out = Vec::new();
    for (i, layer) in doc.layers.iter().enumerate().skip(index) {
        if !plain(layer) {
            return None;
        }
        if layer.is_group {
            let default = layer.blend == BlendMode::PassThrough
                && layer.opacity >= 1.0
                && layer.mask.as_ref().is_none_or(|m| !m.enabled);
            if !default {
                return None;
            }
        } else if shown(layer) {
            out.push(i);
        }
    }
    // The moving layer itself may be hidden; there's still nothing to fall
    // back for.
    Some(out)
}

/// Whether a move of layer `id` can be shown live.
pub fn can_show(doc: &Document, id: u64) -> bool {
    stack(doc, id).is_some()
}

/// Everything needed to show a move of layer `id` live: `layers` are the
/// document's layers at pyramid level `level` (shrunk, or the originals at
/// level 0), and `region` the area of that level on screen.
pub fn build(
    doc: &Document,
    layers: &[Layer],
    id: u64,
    level: usize,
    region: (u32, u32, u32, u32),
) -> Option<LiveStack> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let order = stack(doc, id)?;
    let index = doc.index_of(id)?;
    let (lw, lh) = (layers[index].pixels.width(), layers[index].pixels.height());
    let (x0, y0, w, h) = region;
    let tiles: Vec<(u32, u32)> = (y0 / TILE..(y0 + h).div_ceil(TILE))
        .flat_map(|row| (x0 / TILE..(x0 + w).div_ceil(TILE)).map(move |col| (col, row)))
        .collect();
    let composited = composite::composite_tiles(&layers[..index], &tiles, None);
    let by_tile: HashMap<_, _> = tiles.into_iter().zip(composited).collect();
    let below = Tiled::from_tiles(lw, lh, [0; 4], |col, row| by_tile.get(&(col, row)).cloned());
    let whole = (0, 0, lw, lh);
    let live = order
        .into_iter()
        .map(|i| {
            let layer = &layers[i];
            let moves = i == index;
            // The moving layer is uploaded whole, as any of it may be
            // dragged into view; the rest only where they're seen.
            let area = if moves { whole } else { region };
            let source = match &layer.adjustment {
                Some(a) => Source::Adjustment(a.prepare().lut(LUT_SIZE)),
                None => Source::Pixels(Plane::crop(&layer.pixels, area)),
            };
            LiveLayer {
                source,
                mask: layer
                    .mask
                    .as_ref()
                    .filter(|m| m.enabled)
                    .map(|m| Plane::crop(&m.pixels, area)),
                mode: layer.blend,
                opacity: layer.opacity,
                moves,
            }
        })
        .collect();
    Some(LiveStack {
        id: NEXT.fetch_add(1, Ordering::Relaxed),
        level,
        region,
        below: Plane::crop(&below, region),
        layers: live,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use omapix_engine::adjust::{Adjustment, Curves};
    use omapix_engine::{ColorProfile, Raster, ops};

    fn doc() -> Document {
        let (w, h) = (300, 200);
        let image = Raster::new(w, h, vec![[30000, 30000, 30000, 65535]; (w * h) as usize]);
        let mut doc = Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16);
        let id = doc.next_layer_id();
        doc.layers.push(Layer::empty(id, "Patch", w, h));
        ops::dodge_and_burn_layer(&mut doc, 1);
        let id = doc.next_layer_id();
        doc.layers.push(Layer::adjustment(id, Adjustment::Curves(Curves::default()), w, h));
        doc
    }

    #[test]
    fn a_top_level_pixel_layer_with_plain_layers_above_moves_live() {
        let doc = doc();
        let patch = doc.layers[1].id;
        let live = build(&doc, &doc.layers, patch, 0, (10, 20, 280, 170)).unwrap();
        assert_eq!(live.layers.len(), 3);
        assert!(live.layers[0].moves && !live.layers[1].moves);
        assert!(matches!(live.layers[2].source, Source::Adjustment(ref l) if l.len() == LUT_SIZE.pow(3)));
        // The moving layer comes whole; the others and what's below, as seen.
        let Source::Pixels(moving) = &live.layers[0].source else { panic!() };
        assert_eq!((moving.w, moving.h), (300, 200));
        let Source::Pixels(above) = &live.layers[1].source else { panic!() };
        assert_eq!((above.x0, above.y0, above.w, above.h), (10, 20, 280, 170));
        assert_eq!(live.below.data[0], [30000, 30000, 30000, 65535]);
        assert_eq!(live.below.data.len(), 280 * 170);
    }

    #[test]
    fn clipping_blend_if_groups_and_adjustments_move_on_the_cpu() {
        let mut doc = doc();
        let (background, patch, curves) = (doc.layers[0].id, doc.layers[1].id, doc.layers[3].id);
        assert!(!can_show(&doc, curves), "an adjustment layer");
        assert!(can_show(&doc, background));
        doc.layers[2].clipped = true;
        assert!(!can_show(&doc, patch), "clipped layer above");
        doc.layers[2].clipped = false;
        // A default Pass Through group above is fine; one with an opacity isn't.
        let group = doc.group_layer(2);
        assert!(can_show(&doc, patch));
        doc.layer_mut(group).unwrap().opacity = 0.5;
        assert!(!can_show(&doc, patch));
        doc.layer_mut(group).unwrap().opacity = 1.0;
        // Inside a group, the moving layer stays on the CPU.
        let inner = doc.group_layer(doc.index_of(patch).unwrap());
        assert!(!can_show(&doc, patch));
        assert!(doc.layer(inner).is_some());
    }
}
