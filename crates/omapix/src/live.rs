//! Showing a Move drag, a Free Transform, or a slider dragged on one
//! layer, live on the GPU.
//!
//! Moving a layer on the CPU means translating it, compositing every tile
//! and rebuilding the display pyramid, many times a second; so does
//! dragging its opacity or an adjustment's settings. Instead, when the drag
//! starts, what's below the layer (the "subject") is composited once, and
//! it and the layers above are uploaded as textures at the pyramid level on
//! screen (`gpu.rs`). Each frame the GPU composites them with the subject
//! moved, or with its opacity, blend mode and adjustment as they are now.
//! When the drag ends the exact CPU render replaces the live one.
//!
//! Only stacks the shader handles are shown live: the subject is a pixel
//! layer (or for edits also an adjustment layer, or a Pass Through group),
//! and the layers above it are pixel or adjustment layers. They can be in
//! Pass Through groups, which blend as if their layers weren't grouped;
//! one with an opacity or a mask then fades what its layers made back
//! towards what was below them. Anything else (clipping, Blend If, other
//! groups) stays on the CPU as before.

use std::sync::Arc;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use omapix_engine::composite;
use omapix_engine::layer::Layer;
use omapix_engine::tiled::{TILE, Tiled};
use omapix_engine::transform::Affine;
use omapix_engine::{BlendMode, Document, Pixel};

/// Points per side of an adjustment's 3D lookup table.
pub const LUT_SIZE: usize = 33;

/// An adjustment's lookup table, [`LUT_SIZE`]³ points (see
/// `Prepared::lut`).
pub type Table = Arc<Vec<[f32; 3]>>;

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
    /// The end of a Pass Through group, which comes after its layers: what
    /// they made of what was below them is faded back towards that by the
    /// group's opacity and mask.
    Group(Before),
}

/// What was below a group's layers.
pub enum Before {
    /// Composited beforehand, for the subject and the groups it's in.
    Plane(Plane<Pixel>),
    /// Kept in this slot as the stack is blended (see [`LiveLayer::keep`]).
    Kept(usize),
}

/// One layer the GPU blends onto what's below it, as `composite.rs` does.
pub struct LiveLayer {
    pub source: Source,
    pub mask: Option<Plane<u16>>,
    pub mode: BlendMode,
    pub opacity: f32,
    /// The layer being moved or edited.
    pub subject: bool,
    /// The slots to keep what's below this layer in, for the ends of the
    /// groups that start with it.
    pub keep: Vec<usize>,
}

/// The subject's settings this frame, while they're being edited.
#[derive(Clone)]
pub struct Settings {
    pub opacity: f32,
    pub mode: BlendMode,
    pub lut: Option<Table>,
}

/// What the canvas draws live this frame.
#[derive(Clone)]
pub struct LiveFrame {
    pub stack: Arc<LiveStack>,
    /// How far the subject has moved, in image pixels.
    pub offset: (i32, i32),
    /// Free Transform's transform of the subject, in image pixels.
    pub transform: Option<Affine>,
    pub settings: Option<Settings>,
}

pub struct LiveStack {
    /// Tells stacks apart, so the GPU uploads each once.
    pub id: u64,
    /// The pyramid level, and the area of it on screen (x0, y0, w, h).
    pub level: usize,
    pub region: (u32, u32, u32, u32),
    /// Everything below the subject, composited, over `region`.
    pub below: Plane<Pixel>,
    /// The subject (unless it's hidden), then the layers above it, bottom
    /// first.
    pub layers: Vec<LiveLayer>,
    /// How many slots the layers keep what's below them in.
    pub slots: usize,
}

/// A layer of a live stack, by its place in the document.
struct Step {
    index: usize,
    end: Option<End>,
    keep: Vec<usize>,
}

/// For a group's end, what was below its layers: everything below this
/// place in the document, or what's kept in this slot.
enum End {
    Below(usize),
    Kept(usize),
}

/// The layers from `id` up that a live move (or edit) blends, bottom
/// first, or `None` if it can't be shown live.
fn stack(doc: &Document, id: u64, moving: bool) -> Option<Vec<Step>> {
    let index = doc.index_of(id)?;
    let subject = &doc.layers[index];
    let plain = |l: &Layer| {
        !l.clipped && !doc.is_clip_base(l.id) && l.blend_if.is_none_or(|b| b.is_neutral())
    };
    let through = |l: &Layer| l.is_group && l.blend == BlendMode::PassThrough;
    let kind = subject.has_pixels() || !moving && (subject.adjustment.is_some() || through(subject));
    let groups_shown = |l: &Layer| {
        std::iter::successors(l.parent, |&g| doc.layer(g)?.parent)
            .all(|g| doc.layer(g).is_some_and(|g| g.visible))
    };
    // What's below the subject is composited without the groups round it
    // (the loop below checks they're Pass Through). That's wrong if one is
    // hidden, or if a clipped layer in one has nothing there to clip to: it
    // shows unclipped, but would clip to what's below the group.
    let stray = |l: &Layer| l.clipped && l.parent.is_some() && doc.clip_base(l.id).is_none();
    let hidden_group = subject.is_group && !subject.visible;
    if !kind || !plain(subject) || !groups_shown(subject) || hidden_group {
        return None;
    }
    if (subject.parent.is_some() || subject.is_group) && doc.layers[..index].iter().any(stray) {
        return None;
    }
    let shown = |l: &Layer| l.visible && groups_shown(l);
    let layer = |index| Step {
        index,
        end: None,
        keep: Vec::new(),
    };
    let mut out: Vec<Step> = Vec::new();
    // The groups above the subject that fade: where each starts, and the
    // slot what's below it is kept in.
    let mut kept: Vec<(usize, usize)> = Vec::new();
    for (i, l) in doc.layers.iter().enumerate().skip(index) {
        if !plain(l) {
            return None;
        }
        if !shown(l) {
            continue;
        }
        if !l.is_group {
            out.push(layer(i));
            continue;
        }
        if !through(l) {
            return None;
        }
        let fades = l.opacity < 1.0 || l.mask.as_ref().is_some_and(|m| m.enabled);
        let start = doc.span(i).start;
        if start <= index {
            // The subject, or a group it's in: what's below the group
            // doesn't change.
            if fades || i == index {
                out.push(Step {
                    end: Some(End::Below(start)),
                    ..layer(i)
                });
            }
        } else if fades && let Some(first) = out.iter().position(|s| s.index >= start) {
            // A slot no group inside this one is using.
            let inside = kept.iter().filter(|(s, _)| *s >= start);
            let slot = inside.map(|(_, slot)| slot + 1).max().unwrap_or(0);
            kept.push((start, slot));
            out[first].keep.push(slot);
            out.push(Step {
                end: Some(End::Kept(slot)),
                ..layer(i)
            });
        }
    }
    // The subject itself may be hidden; there's still nothing to fall back
    // for.
    Some(out)
}

/// Whether a move (or with `moving` false, an edit) of layer `id` can be
/// shown live.
pub fn can_show(doc: &Document, id: u64, moving: bool) -> bool {
    stack(doc, id, moving).is_some()
}

/// Whether `after` differs from `before` only in layer `id`'s opacity,
/// blend mode and adjustment settings: what a live edit can show.
pub fn only_settings_changed(before: &[Layer], after: &[Layer], id: u64) -> bool {
    before.len() == after.len()
        && before.iter().zip(after).all(|(a, b)| {
            let same_pixels = a.pixels.same_tiles(&b.pixels)
                && match (&a.mask, &b.mask) {
                    (None, None) => true,
                    (Some(x), Some(y)) => x.enabled == y.enabled && x.pixels.same_tiles(&y.pixels),
                    _ => false,
                };
            let same_place = (a.id, a.parent, a.visible, a.clipped, a.is_group, a.blend_if)
                == (b.id, b.parent, b.visible, b.clipped, b.is_group, b.blend_if);
            let same_settings =
                (a.opacity, a.blend, &a.adjustment) == (b.opacity, b.blend, &b.adjustment);
            same_pixels && same_place && (same_settings || a.id == id)
        })
}

/// Everything needed to show a move (or with `moving` false, an edit) of
/// layer `id` live: `layers` are the document's layers at pyramid level
/// `level` (shrunk, or the originals at level 0), and `region` the area of
/// that level on screen.
pub fn build(
    doc: &Document,
    layers: &[Layer],
    id: u64,
    moving: bool,
    level: usize,
    region: (u32, u32, u32, u32),
) -> Option<LiveStack> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let steps = stack(doc, id, moving)?;
    let index = doc.index_of(id)?;
    let (lw, lh) = (layers[index].pixels.width(), layers[index].pixels.height());
    let (x0, y0, w, h) = region;
    let tiles: Vec<(u32, u32)> = (y0 / TILE..(y0 + h).div_ceil(TILE))
        .flat_map(|row| (x0 / TILE..(x0 + w).div_ceil(TILE)).map(move |col| (col, row)))
        .collect();
    // Everything below place `end` in the stack, over `region`.
    let below = |end: usize| {
        let composited = composite::composite_tiles(&layers[..end], &tiles, None);
        let by_tile: HashMap<_, _> = tiles.iter().zip(composited).collect();
        let all = Tiled::from_tiles(lw, lh, [0; 4], |col, row| by_tile.get(&(col, row)).cloned());
        Plane::crop(&all, region)
    };
    let whole = (0, 0, lw, lh);
    let slots = steps.iter().flat_map(|s| &s.keep).map(|slot| slot + 1).max().unwrap_or(0);
    let live = steps
        .into_iter()
        .map(|step| {
            let layer = &layers[step.index];
            let subject = step.index == index;
            // A moving layer is uploaded whole, as any of it may be dragged
            // into view; the rest only where they're seen.
            let area = if subject && moving { whole } else { region };
            let source = match (step.end, &layer.adjustment) {
                (Some(End::Below(start)), _) => Source::Group(Before::Plane(below(start))),
                (Some(End::Kept(slot)), _) => Source::Group(Before::Kept(slot)),
                (None, Some(a)) => Source::Adjustment(a.prepare().lut(LUT_SIZE)),
                (None, None) => Source::Pixels(Plane::crop(&layer.pixels, area)),
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
                subject,
                keep: step.keep,
            }
        })
        .collect();
    Some(LiveStack {
        id: NEXT.fetch_add(1, Ordering::Relaxed),
        level,
        region,
        below: below(index),
        layers: live,
        slots,
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
        let live = build(&doc, &doc.layers, patch, true, 0, (10, 20, 280, 170)).unwrap();
        assert_eq!(live.layers.len(), 3);
        assert!(live.layers[0].subject && !live.layers[1].subject);
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
        assert!(!can_show(&doc, curves, true), "an adjustment layer");
        assert!(can_show(&doc, background, true));
        doc.layers[2].clipped = true;
        assert!(!can_show(&doc, patch, true), "clipped layer above");
        doc.layers[2].clipped = false;
        // A Pass Through group above is fine; one composited on its own isn't,
        // unless it's hidden.
        let group = doc.group_layer(2);
        assert!(can_show(&doc, patch, true));
        doc.layer_mut(group).unwrap().blend = BlendMode::Normal;
        assert!(!can_show(&doc, patch, true));
        assert!(!can_show(&doc, group, false), "nor edited itself");
        doc.layer_mut(group).unwrap().visible = false;
        assert!(can_show(&doc, patch, true));
    }

    #[test]
    fn groups_that_fade_end_with_what_was_below_them() {
        let mut doc = doc();
        let (patch, soft, curves) = (doc.layers[1].id, doc.layers[2].id, doc.layers[3].id);
        // Background, [[Patch] inner, Soft Light] outer, Curves.
        let inner = doc.group_layer(1);
        let outer = doc.group_layers(&[inner, soft]).unwrap();
        doc.layer_mut(inner).unwrap().opacity = 0.5;
        doc.layer_mut(outer).unwrap().mask = Some(omapix_engine::layer::Mask::white(300, 200));
        let all = (0, 0, 300, 200);
        let ends = |live: &LiveStack| -> Vec<Option<usize>> {
            live.layers
                .iter()
                .map(|l| match l.source {
                    Source::Group(Before::Kept(slot)) => Some(slot),
                    _ => None,
                })
                .collect()
        };

        // From the background, both groups start with the patch: what's
        // below it is kept for each, the outer one's where the inner one
        // doesn't use it.
        let live = build(&doc, &doc.layers, doc.layers[0].id, true, 0, all).unwrap();
        assert_eq!(ends(&live), [None, None, Some(0), None, Some(1), None]);
        assert_eq!(live.layers[1].keep, [0, 1]);
        assert_eq!(live.slots, 2);
        assert!(live.layers[4].mask.is_some() && live.layers[2].opacity == 0.5);

        // From inside them, what was below each is composited beforehand.
        let live = build(&doc, &doc.layers, patch, true, 0, all).unwrap();
        assert_eq!(live.layers.len(), 5);
        assert_eq!(live.slots, 0);
        for end in [1, 3] {
            let Source::Group(Before::Plane(before)) = &live.layers[end].source else { panic!() };
            assert_eq!(before.data, live.below.data);
        }

        // A group can be the subject of an edit, if not of a move: its
        // layers are part of what's below, and its end fades them.
        assert!(!can_show(&doc, inner, true));
        doc.layer_mut(inner).unwrap().opacity = 1.0;
        let live = build(&doc, &doc.layers, inner, false, 0, all).unwrap();
        assert!(live.layers[0].subject && matches!(live.layers[0].source, Source::Group(Before::Plane(_))));
        assert_eq!(live.layers.len(), 4);
        doc.layer_mut(inner).unwrap().visible = false;
        assert!(!can_show(&doc, inner, false), "hidden, its layers aren't below");
        assert!(can_show(&doc, curves, false));
    }

    #[test]
    fn a_layer_in_pass_through_groups_moves_live() {
        let mut doc = doc();
        let (patch, soft, curves) = (doc.layers[1].id, doc.layers[2].id, doc.layers[3].id);
        // A layer below the patch in its group, to be part of what's below.
        let id = doc.next_layer_id();
        let red = Raster::new(300, 200, vec![[60000, 0, 0, 65535]; 300 * 200]);
        let mut under = Layer::from_raster(id, "Under", &red);
        under.opacity = 0.5;
        doc.layers.insert(1, under);
        let inner = doc.group_layers(&[id, patch, soft]).unwrap();
        let outer = doc.group_layer(doc.index_of(inner).unwrap());
        assert!(can_show(&doc, patch, true));
        assert!(can_show(&doc, soft, false), "an edit too");
        let index = doc.index_of(patch).unwrap();
        let live = build(&doc, &doc.layers, patch, true, 0, (0, 0, 300, 200)).unwrap();
        // The patch, the layer above it in the group, and the Curves on top.
        assert_eq!(live.layers.len(), 3);
        assert!(live.layers[0].subject);
        assert!(matches!(live.layers[2].source, Source::Adjustment(_)));
        // Below it: the background and the layer under it in the group.
        let mut hidden = doc.clone();
        hidden.layers[index..].iter_mut().filter(|l| !l.is_group).for_each(|l| l.visible = false);
        assert_eq!(live.below.data, composite::composite(&hidden.layers, 300, 200).pixels());
        assert_eq!(live.below.data[0], [45000, 15000, 15000, 65535]);

        // A group round it that's composited on its own or hidden, on the CPU.
        for group in [inner, outer] {
            let set = |doc: &mut Document, f: &dyn Fn(&mut Layer)| f(doc.layer_mut(group).unwrap());
            set(&mut doc, &|g| g.blend = BlendMode::Normal);
            assert!(!can_show(&doc, patch, true), "an isolated group");
            set(&mut doc, &|g| g.blend = BlendMode::PassThrough);
            set(&mut doc, &|g| g.visible = false);
            assert!(!can_show(&doc, patch, true), "a hidden group");
            set(&mut doc, &|g| g.visible = true);
            assert!(can_show(&doc, patch, true));
        }
        // A clipped layer with nothing in the group to clip to shows
        // unclipped there, which what's below wouldn't.
        doc.layer_mut(id).unwrap().clipped = true;
        assert!(!can_show(&doc, patch, true), "a stray clipped layer below");
        assert!(can_show(&doc, curves, false), "but not below a layer outside the group");
    }

    #[test]
    fn edits_show_adjustment_layers_live_and_only_settings_changes_count() {
        let doc = doc();
        let curves = doc.layers[3].id;
        assert!(can_show(&doc, curves, false), "an adjustment's settings");
        let live = build(&doc, &doc.layers, curves, false, 0, (0, 0, 300, 200)).unwrap();
        assert!(live.layers[0].subject && matches!(live.layers[0].source, Source::Adjustment(_)));
        let mut after = doc.layers.clone();
        after[3].opacity = 0.4;
        assert!(only_settings_changed(&doc.layers, &after, curves));
        assert!(!only_settings_changed(&doc.layers, &after, doc.layers[0].id), "another layer");
        after[3].visible = false;
        assert!(!only_settings_changed(&doc.layers, &after, curves), "hiding it");
    }
}
