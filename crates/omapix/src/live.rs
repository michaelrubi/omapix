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
//! layer (or for edits also an adjustment layer, or a group), and the
//! layers above it are pixel or adjustment layers, in groups or not. A Pass
//! Through group blends as if its layers weren't grouped; one with an
//! opacity or a mask then fades what its layers made back towards what was
//! below them. A group composited on its own (any other mode) has its
//! layers blended onto nothing, then what they made blended onto what was
//! below it, with the group's mode, opacity and mask. Anything else
//! (clipping, Blend If, a subject inside a group composited on its own)
//! stays on the CPU as before.

use std::sync::Arc;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use omapix_engine::composite;
use omapix_engine::layer::{BlendIf, Layer};
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
    /// The end of a group composited on its own: what its layers made (onto
    /// nothing, from the first of them) is blended onto what was below
    /// them, kept in this slot, with the group's mode, opacity and mask.
    /// Or of a clipping mask, with its base's mode and opacity.
    Isolated(usize),
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
    /// It's the first in a group composited on its own, so it blends onto
    /// nothing (after what's below is kept).
    pub fresh: bool,
    /// It's clipped: blended only where there's something below, which
    /// keeps its alpha.
    pub atop: bool,
    /// How much it shows by its colour and the colour below.
    pub blend_if: Option<BlendIf>,
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
    /// While a stroke is painted on the subject: its pixels now, at the
    /// stack's level, for the GPU to bring the tiles that changed up to
    /// date.
    pub painted: Option<Tiled<Pixel>>,
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
    /// The subject's pixels at this level, as its layer was cropped from.
    pub subject_pixels: Tiled<Pixel>,
}

/// A layer of a live stack, by its place in the document.
struct Step {
    index: usize,
    end: Option<End>,
    keep: Vec<usize>,
    fresh: bool,
    /// Clipped: blended only where there's something below, keeping its
    /// alpha (the W3C's source-atop).
    atop: bool,
    /// A clipping mask's base, drawn plainly (Normal, at full opacity)
    /// onto nothing for the layers clipped to it; its end blends the
    /// result with its own mode and opacity.
    base: bool,
}

/// For a Pass Through group's end, what was below its layers: everything
/// below this place in the document, or what's kept in this slot. For a
/// group composited on its own: the slot what was below it is kept in, or
/// for the subject itself, its layers from this place, composited. For a
/// clipping mask, the slot what was below its base is kept in.
#[derive(Clone, Copy)]
enum End {
    Below(usize),
    Kept(usize),
    Isolated(usize),
    Contents(usize),
    Clip(usize),
}

/// The slot of a group that starts onto nothing: one never kept into.
const NOTHING: usize = usize::MAX;

/// The layers that a live move (or edit) of layer `id` blends, bottom
/// first, from the place returned (below which everything's composited
/// beforehand), or `None` if it can't be shown live.
fn stack(doc: &Document, id: u64, moving: bool) -> Option<(usize, Vec<Step>)> {
    let index = doc.index_of(id)?;
    let subject = &doc.layers[index];
    // Clipping masks are of plain pixel layers, with no groups in them.
    let base_of = |l: &Layer| doc.clip_base(l.id).and_then(|b| doc.layer(b));
    let clips = |l: &Layer| {
        let pixels = |l: &Layer| !l.is_group && l.adjustment.is_none();
        if l.clipped {
            !l.is_group && base_of(l).is_some_and(pixels)
        } else {
            !doc.is_clip_base(l.id) || pixels(l)
        }
    };
    let plain = clips;
    let through = |l: &Layer| l.is_group && l.blend == BlendMode::PassThrough;
    let kind = subject.has_pixels() || !moving && (subject.adjustment.is_some() || subject.is_group);
    let groups_shown = |l: &Layer| std::iter::successors(l.parent, |&g| doc.layer(g)?.parent).all(|g| doc.layer(g).is_some_and(|g| g.visible));
    // What's below the subject is composited without the groups round it
    // (the loop below checks they're Pass Through). That's wrong if one is
    // hidden, or if a clipped layer in one has nothing there to clip to: it
    // shows unclipped, but would clip to what's below the group.
    let stray = |l: &Layer| l.clipped && l.parent.is_some() && doc.clip_base(l.id).is_none();
    let hidden_group = subject.is_group && !subject.visible;
    // A base's own opacity and mode are blended after what's clipped to it.
    let edited_base = !moving && doc.is_clip_base(subject.id);
    if !kind || !plain(subject) || !groups_shown(subject) || hidden_group || edited_base {
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
        fresh: false,
        atop: false,
        base: false,
    };
    // A clipped subject's stack starts at its base.
    let from = match base_of(subject) {
        Some(base) if subject.clipped => doc.index_of(base.id)?,
        _ => index,
    };
    let mut out: Vec<Step> = Vec::new();
    // The groups above the subject that fade: where each starts, and the
    // slot what's below it is kept in.
    let mut kept: Vec<(usize, usize)> = Vec::new();
    // The clipping mask being blended: its base, its last layer, and the
    // slot what was below its base is kept in.
    let mut clip: Option<(usize, usize, usize)> = None;
    for (i, l) in doc.layers.iter().enumerate().skip(from) {
        if let Some((base, last, slot)) = clip
            && i > last
        {
            out.push(Step {
                end: Some(End::Clip(slot)),
                ..layer(base)
            });
            clip = None;
        }
        if !plain(l) {
            return None;
        }
        if !shown(l) || l.clipped && !base_of(l).is_some_and(shown) {
            continue;
        }
        if l.clipped {
            out.push(Step { atop: true, ..layer(i) });
            continue;
        }
        if !l.is_group && doc.is_clip_base(l.id) {
            // The layers clipped to it come straight after it.
            let last = (i + 1..doc.layers.len())
                .take_while(|&j| doc.layers[j].clipped && doc.layers[j].parent == l.parent)
                .last()?;
            let inside = kept.iter().filter(|(s, _)| *s >= i);
            let slot = inside.map(|(_, slot)| slot + 1).max().unwrap_or(0);
            kept.push((i, slot));
            clip = Some((i, last, slot));
            out.push(Step {
                keep: vec![slot],
                fresh: true,
                base: true,
                ..layer(i)
            });
            continue;
        }
        if !l.is_group {
            out.push(layer(i));
            continue;
        }
        let start = doc.span(i).start;
        if !through(l) {
            if i == index {
                // The subject: its layers, composited, stand in for it.
                out.push(Step {
                    end: Some(End::Contents(start)),
                    ..layer(i)
                });
            } else if start <= index {
                // Round the subject: what's below it would be what's below
                // it in the group, onto nothing.
                return None;
            } else if let Some(first) = out.iter().position(|s| s.index >= start) {
                // Its layers start onto nothing, with what's below them
                // kept, then what they made is blended onto that.
                // Groups inside it that start with the same layer start
                // onto nothing too, not onto what's below it.
                let starts = |g: usize| out.iter().position(|s| s.index >= doc.span(g).start);
                let same: Vec<usize> = (first + 1..out.len())
                    .filter(|&j| out[j].end.is_some() && starts(out[j].index) == Some(first))
                    .collect();
                for j in same {
                    out[j].end = match out[j].end {
                        Some(End::Isolated(_)) => Some(End::Isolated(NOTHING)),
                        Some(End::Kept(_)) => Some(End::Kept(NOTHING)),
                        Some(End::Clip(_)) => Some(End::Clip(NOTHING)),
                        ref end => *end,
                    };
                }
                out[first].keep.clear();
                kept.retain(|&(s, _)| out.iter().position(|step| step.index >= s) != Some(first));
                let inside = kept.iter().filter(|(s, _)| *s >= start);
                let slot = inside.map(|(_, slot)| slot + 1).max().unwrap_or(0);
                kept.push((start, slot));
                out[first].keep.push(slot);
                out[first].fresh = true;
                out.push(Step {
                    end: Some(End::Isolated(slot)),
                    ..layer(i)
                });
            }
            continue;
        }
        let fades = l.opacity < 1.0 || l.mask.as_ref().is_some_and(|m| m.enabled) || l.blend_if.is_some_and(|b| !b.is_neutral());
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
    if let Some((base, _, slot)) = clip {
        out.push(Step {
            end: Some(End::Clip(slot)),
            ..layer(base)
        });
    }
    // The subject itself may be hidden; there's still nothing to fall back
    // for.
    Some((from, out))
}

/// Whether a move (or with `moving` false, an edit) of layer `id` can be
/// shown live.
pub fn can_show(doc: &Document, id: u64, moving: bool) -> bool {
    stack(doc, id, moving).is_some()
}

/// Whether `after` differs from `before` only in layer `id`'s opacity,
/// blend mode and adjustment settings: what a live edit can show. A group
/// can't start or stop passing through.
pub fn only_settings_changed(before: &[Layer], after: &[Layer], id: u64) -> bool {
    before.len() == after.len()
        && before.iter().zip(after).all(|(a, b)| {
            let same_pixels = a.pixels.same_tiles(&b.pixels)
                && match (&a.mask, &b.mask) {
                    (None, None) => true,
                    (Some(x), Some(y)) => x.enabled == y.enabled && x.pixels.same_tiles(&y.pixels),
                    _ => false,
                };
            let same_place = (a.id, a.parent, a.visible, a.clipped, a.is_group, a.blend_if) == (b.id, b.parent, b.visible, b.clipped, b.is_group, b.blend_if);
            let same_settings = (a.opacity, a.blend, &a.adjustment) == (b.opacity, b.blend, &b.adjustment);
            // A group that starts or stops passing through is made of other passes.
            let through = |l: &Layer| l.is_group && l.blend == BlendMode::PassThrough;
            same_pixels && same_place && (same_settings || a.id == id && through(a) == through(b))
        })
}

/// Everything needed to show a move (or with `moving` false, an edit) of
/// layer `id` live: `layers` are the document's layers at pyramid level
/// `level` (shrunk, or the originals at level 0), and `region` the area of
/// that level on screen.
pub fn build(doc: &Document, layers: &[Layer], id: u64, moving: bool, level: usize, region: (u32, u32, u32, u32)) -> Option<LiveStack> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let (from, steps) = stack(doc, id, moving)?;
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
    // What the group at `group`, from place `start`, makes of its layers
    // on its own, over `region`: the group alone, plainly.
    let contents = |start: usize, group: usize| {
        let mut alone = layers[start..=group].to_vec();
        let g = alone.last_mut().expect("the group");
        (g.parent, g.blend, g.opacity, g.mask, g.visible, g.blend_if) = (None, BlendMode::Normal, 1.0, None, true, None);
        let composited = composite::composite_tiles(&alone, &tiles, None);
        let by_tile: HashMap<_, _> = tiles.iter().zip(composited).collect();
        Plane::crop(&Tiled::from_tiles(lw, lh, [0; 4], |col, row| by_tile.get(&(col, row)).cloned()), region)
    };
    let whole = (0, 0, lw, lh);
    let mut slots = steps.iter().flat_map(|s| &s.keep).map(|slot| slot + 1).max().unwrap_or(0);
    let nothing = slots;
    if steps
        .iter()
        .any(|s| matches!(s.end, Some(End::Isolated(NOTHING) | End::Kept(NOTHING) | End::Clip(NOTHING))))
    {
        slots += 1;
    }
    let slot = |s: usize| if s == NOTHING { nothing } else { s };
    // Below a group composited on its own is below its layers.
    let from = match steps.first().and_then(|s| s.end) {
        Some(End::Contents(start)) => start,
        _ => from,
    };
    let live = steps
        .into_iter()
        .map(|step| {
            let layer = &layers[step.index];
            let clip_end = matches!(step.end, Some(End::Clip(_)));
            let subject = step.index == index && !clip_end;
            // A moving layer is uploaded whole, as any of it may be dragged
            // into view; the rest only where they're seen.
            let area = if subject && moving { whole } else { region };
            let source = match (step.end, &layer.adjustment) {
                (Some(End::Below(start)), _) => Source::Group(Before::Plane(below(start))),
                (Some(End::Kept(s)), _) => Source::Group(Before::Kept(slot(s))),
                (Some(End::Isolated(s)), _) => Source::Isolated(slot(s)),
                (Some(End::Contents(start)), _) => Source::Pixels(contents(start, step.index)),
                (Some(End::Clip(s)), _) => Source::Isolated(slot(s)),
                (None, Some(a)) => Source::Adjustment(a.prepare().lut(LUT_SIZE)),
                (None, None) => Source::Pixels(Plane::crop(&layer.pixels, area)),
            };
            let mask = layer.mask.as_ref().filter(|m| m.enabled && !clip_end);
            LiveLayer {
                source,
                mask: mask.map(|m| Plane::crop(&m.pixels, area)),
                mode: if step.base { BlendMode::Normal } else { layer.blend },
                blend_if: layer.blend_if.filter(|b| !b.is_neutral() && !step.base),
                opacity: if step.base { 1.0 } else { layer.opacity },
                subject,
                keep: step.keep,
                fresh: step.fresh,
                atop: step.atop,
            }
        })
        .collect();
    Some(LiveStack {
        id: NEXT.fetch_add(1, Ordering::Relaxed),
        level,
        region,
        below: below(from),
        layers: live,
        slots,
        subject_pixels: layers[index].pixels.clone(),
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
        // Clipping masks of pixel layers are fine; to an adjustment, or with
        // a group in them, they're not.
        doc.layers[2].clipped = true;
        assert!(can_show(&doc, patch, true), "clipped layer above");
        assert!(can_show(&doc, doc.layers[2].id, true), "moving the clipped layer");
        assert!(!can_show(&doc, patch, false), "but the base's own settings");
        doc.layers[3].clipped = true;
        assert!(can_show(&doc, curves, false));
        doc.layers[2].clipped = false;
        doc.layers[3].clipped = false;
        let id = doc.next_layer_id();
        let mut more = Layer::adjustment(id, Adjustment::Curves(Curves::default()), 300, 200);
        more.clipped = true;
        doc.layers.push(more);
        assert!(!can_show(&doc, id, false), "clipped to an adjustment");
        doc.layers.pop();
        let inner = doc.group_layer(1);
        doc.layer_mut(inner).unwrap().clipped = true;
        assert!(!can_show(&doc, background, true), "a clipped group");
        doc.ungroup(doc.index_of(inner).unwrap());
        // Groups above are fine, Pass Through or composited on their own.
        let group = doc.group_layer(2);
        assert!(can_show(&doc, patch, true));
        doc.layer_mut(group).unwrap().blend = BlendMode::Normal;
        assert!(can_show(&doc, patch, true));
        assert!(can_show(&doc, group, false), "and edited itself");
    }

    #[test]
    fn clipping_masks_start_onto_nothing_from_their_base() {
        let mut doc = doc();
        let (patch, soft) = (doc.layers[1].id, doc.layers[2].id);
        // Background, Patch, Soft Light clipped to it, Curves.
        doc.layers[2].clipped = true;
        doc.layers[1].opacity = 0.5;
        let all = (0, 0, 300, 200);
        let live = build(&doc, &doc.layers, doc.layers[0].id, true, 0, all).unwrap();
        assert_eq!(live.layers.len(), 5);
        // The base, plainly, onto nothing, with what's below it kept.
        let base = &live.layers[1];
        assert!(base.fresh && base.keep == [0] && base.opacity == 1.0 && base.mode == BlendMode::Normal);
        assert!(live.layers[2].atop);
        // Then what they made, with the base's opacity.
        assert!(matches!(live.layers[3].source, Source::Isolated(0)) && live.layers[3].opacity == 0.5);
        assert!(!live.layers[4].atop && live.slots == 1);

        // Moving the clipped layer, the stack starts at the base.
        let live = build(&doc, &doc.layers, soft, true, 0, all).unwrap();
        assert_eq!(live.layers.len(), 4);
        assert!(!live.layers[0].subject && live.layers[1].subject && !live.layers[2].subject);
        assert_eq!(live.below.data[0], [30000, 30000, 30000, 65535]);
        // Hidden, its base hides it too.
        doc.layer_mut(patch).unwrap().visible = false;
        let live = build(&doc, &doc.layers, soft, true, 0, all).unwrap();
        assert_eq!(live.layers.len(), 1);
    }

    #[test]
    fn blend_if_goes_with_each_layer_and_makes_groups_fade() {
        use omapix_engine::layer::BlendIf;
        let mut doc = doc();
        let (patch, soft) = (doc.layers[1].id, doc.layers[2].id);
        let split = BlendIf {
            this: [0.0, 0.3, 1.0, 1.0],
            ..BlendIf::default()
        };
        doc.layers[2].blend_if = Some(split);
        doc.layers[3].blend_if = Some(BlendIf::default());
        let all = (0, 0, 300, 200);
        let live = build(&doc, &doc.layers, patch, true, 0, all).unwrap();
        assert_eq!(live.layers[1].blend_if, Some(split));
        assert_eq!(live.layers[2].blend_if, None, "it hides nothing");
        // A Pass Through group with only Blend If still fades.
        doc.layers[2].blend_if = None;
        let group = doc.group_layer(2);
        doc.layer_mut(group).unwrap().blend_if = Some(split);
        let live = build(&doc, &doc.layers, patch, true, 0, all).unwrap();
        assert!(matches!(live.layers[2].source, Source::Group(Before::Kept(0))) && live.layers[2].blend_if.is_some());
        // A clipping mask's base has it at the end, not drawn plainly.
        doc.ungroup(doc.index_of(group).unwrap());
        doc.layer_mut(soft).unwrap().clipped = true;
        doc.layers[1].blend_if = Some(split);
        let live = build(&doc, &doc.layers, doc.layers[0].id, true, 0, all).unwrap();
        assert_eq!(live.layers[1].blend_if, None);
        assert_eq!(live.layers[3].blend_if, Some(split));
    }

    #[test]
    fn isolated_groups_start_onto_nothing() {
        let mut doc = doc();
        let (patch, soft, curves) = (doc.layers[1].id, doc.layers[2].id, doc.layers[3].id);
        // Background, [[Patch] inner, Soft Light] outer, Curves, both groups
        // composited on their own.
        let inner = doc.group_layer(1);
        let outer = doc.group_layers(&[inner, soft]).unwrap();
        for g in [inner, outer] {
            doc.layer_mut(g).unwrap().blend = BlendMode::Multiply;
        }
        let all = (0, 0, 300, 200);
        let ends = |live: &LiveStack| -> Vec<Option<usize>> {
            live.layers
                .iter()
                .map(|l| match l.source {
                    Source::Isolated(slot) => Some(slot),
                    _ => None,
                })
                .collect()
        };

        // From the background, the patch starts onto nothing with what's
        // below it kept for the outer group; the inner one, which starts
        // there too, ends onto a slot nothing's kept in.
        let live = build(&doc, &doc.layers, doc.layers[0].id, true, 0, all).unwrap();
        assert_eq!(ends(&live), [None, None, Some(1), None, Some(0), None]);
        assert!(live.layers[1].fresh && !live.layers[3].fresh);
        assert_eq!(live.layers[1].keep, [0]);
        assert_eq!(live.slots, 2);

        // Inside one, a layer moves on the CPU.
        assert!(!can_show(&doc, patch, true));
        // Editing the outer group, its layers stand in for it, and what's
        // below it is below them.
        let live = build(&doc, &doc.layers, outer, false, 0, all).unwrap();
        assert_eq!(live.layers.len(), 2);
        assert!(live.layers[0].subject && matches!(live.layers[0].source, Source::Pixels(_)));
        assert_eq!(live.below.data[0], [30000, 30000, 30000, 65535]);
        assert!(can_show(&doc, curves, false));
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
            let Source::Group(Before::Plane(before)) = &live.layers[end].source else {
                panic!()
            };
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
