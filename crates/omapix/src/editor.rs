//! One open document: its layers, undo history, selection, and the
//! background work that keeps the canvas up to date.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};

use omapix_engine::brush::{BrushSettings, Paint, Stroke, Surface};
use omapix_engine::layer::Layer;
use omapix_engine::moving::Lifted;
use omapix_engine::reduced::Reduced;
use omapix_engine::selection::{Combine, Selection};
use omapix_engine::tiled::{TILE, TILE_PIXELS, Tiled};
use omapix_engine::{
    BlendMode, DisplayTransform, Document, Pixel, Raster, composite, filters, ops,
};

use crate::canvas::{Canvas, Render};

/// Undo steps kept. Snapshots share unchanged tiles, so this mostly costs
/// memory for pixels that edits actually replaced.
const HISTORY_LIMIT: usize = 50;

/// Opacity of the mask overlay where the mask hides everything (50 %, as
/// in Photoshop), out of `u16::MAX`.
const OVERLAY_OPACITY: u32 = 32768;

/// What the canvas shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum View {
    /// The finished image.
    Image,
    /// One layer's mask, in greyscale (Photoshop's Alt+click on a mask).
    Mask(u64),
    /// The finished image with one layer's mask over it in translucent red
    /// where it hides (Photoshop's `\` "rubylith").
    MaskOverlay(u64),
    /// What frequency separation at `radius` would produce: the texture
    /// layer, or the colour/tone layer.
    Separation { radius: f32, texture: bool },
    /// What Gaussian blur at `radius` on `layer` would produce.
    GaussianBlur { layer: u64, radius: f32 },
}

/// A whole new render, and for separation previews the flattened image it
/// started from (reused while only the radius changes).
type Rendered = (Render, Option<Arc<Tiled<Pixel>>>);

/// Tiles made at a time when rendering a whole mask view.
const BATCH: usize = 32;

/// A background render in progress.
struct Rendering {
    /// Revision and view generation it shows.
    target: (u64, u64),
    /// Stops a render in place before its next batch of tiles.
    cancel: Arc<AtomicBool>,
    /// The part of the image on screen is done, or previewed.
    visible: bool,
    rx: Receiver<Progress>,
}

/// What a background render reports as it goes.
enum Progress {
    /// A whole new render, for the first one or a separation preview.
    Whole(Rendered),
    /// 256 px tiles of a pyramid level were redrawn in place.
    Tiles(usize, Vec<(u32, u32)>),
    /// The part of the image on screen is done, or previewed.
    Visible,
    /// Every tile is up to date.
    Done,
}

/// Which part of the active layer edits apply to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Pixels,
    Mask,
}

#[derive(Clone)]
struct Snapshot {
    label: String,
    doc: Document,
    active: u64,
}

/// Selected pixels or mask values lifted out by the Move tool.
enum Lift {
    Pixels(Lifted<Pixel>),
    Mask(Lifted<u16>),
}

/// A Move tool drag (or arrow-key nudge) in progress.
struct Moving {
    label: String,
    /// The layer being moved, as it was before the move.
    original: Layer,
    /// The other layers moving with it, as they were: those in it if it's
    /// a group, and the other selected layers and what's in them.
    contents: Vec<Layer>,
    /// The selected layers being moved, not counting those inside a
    /// selected group.
    roots: Vec<u64>,
    /// The selection before the move, which moves with the pixels.
    selection: Option<Selection>,
    target: Target,
    /// Move a copy (Alt+drag).
    copy: bool,
    /// The background colour's grey, left behind on a mask.
    background: u16,
    /// Made on the first real move: the lifted selection, if any.
    lift: Option<Option<Lift>>,
    /// Offset showing in the document, and the latest one asked for.
    applied: (i32, i32),
    wanted: (i32, i32),
}

struct Job {
    label: String,
    rx: Receiver<(Document, u64)>,
}

pub struct Editor {
    pub doc: Document,
    /// Id of the selected layer: the one painting and adjustments apply
    /// to (the last one clicked).
    pub active: u64,
    pub target: Target,
    /// Layers selected along with the active one (Ctrl/Shift+click in the
    /// Layers panel), and the active layer they go with. Selecting another
    /// layer any other way leaves just that one selected.
    selected: (u64, Vec<u64>),
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// Key of the continuous edit in progress (e.g. dragging the opacity
    /// slider), so a whole drag is one undo step.
    live: Option<String>,
    /// A dialog's edits, gathered into one undo step until it closes, and
    /// whether anything changed.
    group: Option<(String, bool)>,
    /// Incremented on every change to `doc`.
    revision: u64,
    /// What the canvas shows, and a counter bumped whenever that changes.
    view: View,
    view_generation: u64,
    rendering: Option<Rendering>,
    /// Revision and view generation the canvas is showing.
    rendered: (u64, u64),
    /// Layers shrunk to the zoomed-out level on screen, for previews.
    reduced: Arc<Mutex<Reduced>>,
    /// Flattened image a separation preview blurs, with its revision.
    separation_base: Option<(u64, Arc<Tiled<Pixel>>)>,
    /// A slow operation running in the background. Edits are refused until
    /// it finishes.
    job: Option<Job>,
    /// Changed since last saved.
    pub modified: bool,
    pub canvas: Canvas,
    /// The brush stroke in progress, and the layer it paints on.
    stroke: Option<(Stroke, u64)>,
    /// The Move tool drag in progress.
    moving: Option<Moving>,
    /// Last revision drawn straight into the canvas by a brush stroke.
    /// Background renders of older revisions are thrown away.
    painted: u64,
    /// The mask overlay's red, in the document's colour space.
    overlay_colour: Pixel,
}

impl Editor {
    pub fn new(doc: Document) -> Result<Self, String> {
        let transform = DisplayTransform::to_srgb(&doc.profile).map_err(|e| e.to_string())?;
        let canvas = Canvas::new(doc.width, doc.height, transform);
        let active = doc.layers.last().map_or(0, |l| l.id);
        let overlay_colour =
            doc.profile
                .from_srgb8([255, 0, 0])
                .unwrap_or([u16::MAX, 0, 0, u16::MAX]);
        Ok(Self {
            doc,
            active,
            target: Target::Pixels,
            selected: (active, Vec::new()),
            undo: Vec::new(),
            redo: Vec::new(),
            live: None,
            group: None,
            revision: 1,
            view: View::Image,
            view_generation: 0,
            rendering: None,
            rendered: (0, u64::MAX),
            reduced: Arc::default(),
            separation_base: None,
            job: None,
            modified: false,
            canvas,
            stroke: None,
            moving: None,
            painted: 0,
            overlay_colour,
        })
    }

    pub fn active_index(&self) -> Option<usize> {
        self.doc.index_of(self.active)
    }

    /// The selected layers that exist, bottom first, the active one
    /// always among them.
    pub fn selected(&self) -> Vec<u64> {
        let (active, others) = &self.selected;
        let mut ids = vec![self.active];
        if *active == self.active {
            ids.extend(others.iter().filter(|&&id| id != self.active));
        }
        ids.retain(|&id| self.doc.layer(id).is_some());
        ids.sort_by_key(|&id| self.doc.index_of(id));
        ids
    }

    pub fn is_selected(&self, id: u64) -> bool {
        id == self.active || self.selected.0 == self.active && self.selected.1.contains(&id)
    }

    /// Select several layers, `active` among them.
    pub fn select_layers(&mut self, active: u64, ids: Vec<u64>) {
        self.active = active;
        self.selected = (active, ids);
    }

    /// Whether more than one layer is selected, not counting those inside a
    /// selected group.
    pub fn several_selected(&self) -> bool {
        self.doc.outermost(&self.selected()).len() > 1
    }

    pub fn busy(&self) -> Option<&str> {
        self.job.as_ref().map(|j| j.label.as_str())
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|s| s.label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|s| s.label.as_str())
    }

    fn snapshot(&self, label: &str) -> Snapshot {
        Snapshot {
            label: label.to_owned(),
            doc: self.doc.clone(),
            active: self.active,
        }
    }

    fn push_undo(&mut self, snapshot: Snapshot) {
        self.undo.push(snapshot);
        if self.undo.len() > HISTORY_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn changed(&mut self) {
        self.revision += 1;
        self.modified = true;
    }

    /// Apply an edit as one undo step. Returns false if a background job is
    /// running and the edit was refused.
    pub fn edit(&mut self, label: &str, f: impl FnOnce(&mut Document, &mut u64)) -> bool {
        if self.job.is_some() {
            return false;
        }
        self.end_gesture();
        self.live = None;
        let before = self.snapshot(label);
        f(&mut self.doc, &mut self.active);
        self.push_undo(before);
        self.changed();
        true
    }

    /// Apply part of a continuous edit. Consecutive calls with the same key
    /// form one undo step, until [`Self::end_live`].
    pub fn edit_live(&mut self, key: &str, f: impl FnOnce(&mut Document)) {
        if self.job.is_some() {
            return;
        }
        self.end_gesture();
        let key = match &mut self.group {
            Some((label, changed)) => {
                *changed = true;
                label.clone()
            }
            None => key.to_owned(),
        };
        let key = key.as_str();
        if self.live.as_deref() != Some(key) {
            let before = self.snapshot(key);
            self.push_undo(before);
            self.live = Some(key.to_owned());
        }
        f(&mut self.doc);
        self.changed();
    }

    pub fn end_live(&mut self) {
        if self.group.is_none() {
            self.live = None;
        }
    }

    /// Start gathering live edits into one undo step called `label`, for a
    /// dialog with OK and Cancel.
    pub fn begin_group(&mut self, label: &str) {
        self.end_gesture();
        self.live = None;
        self.group = Some((label.to_owned(), false));
    }

    /// Finish the group: keep its edits (OK), or revert them without a
    /// trace in the history (Cancel).
    pub fn end_group(&mut self, keep: bool) {
        let Some((_, changed)) = self.group.take() else {
            return;
        };
        self.live = None;
        if changed && !keep {
            self.undo();
            self.redo.pop();
        }
    }

    /// Run a slow edit on a background thread as one undo step.
    pub fn edit_in_background(
        &mut self,
        label: &str,
        f: impl FnOnce(&mut Document, &mut u64) + Send + 'static,
        ctx: &egui::Context,
    ) {
        if self.job.is_some() {
            return;
        }
        self.end_gesture();
        self.live = None;
        let (tx, rx) = channel();
        let mut doc = self.doc.clone();
        let mut active = self.active;
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            f(&mut doc, &mut active);
            let _ = tx.send((doc, active));
            ctx.request_repaint();
        });
        self.job = Some(Job {
            label: label.to_owned(),
            rx,
        });
    }

    pub fn undo(&mut self) {
        if self.job.is_some() {
            return;
        }
        self.end_gesture();
        self.live = None;
        if let Some(prev) = self.undo.pop() {
            let current = Snapshot {
                label: prev.label.clone(),
                doc: self.doc.clone(),
                active: self.active,
            };
            self.redo.push(current);
            self.restore(prev);
        }
    }

    pub fn redo(&mut self) {
        if self.job.is_some() {
            return;
        }
        self.end_gesture();
        self.live = None;
        if let Some(next) = self.redo.pop() {
            let current = Snapshot {
                label: next.label.clone(),
                doc: self.doc.clone(),
                active: self.active,
            };
            self.undo.push(current);
            self.restore(next);
        }
    }

    pub fn history_labels(&self) -> Vec<String> {
        let initial = self
            .doc
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Open".into());
        let mut labels = Vec::with_capacity(1 + self.undo.len() + self.redo.len());
        labels.push(initial);
        for s in &self.undo {
            labels.push(s.label.clone());
        }
        for s in self.redo.iter().rev() {
            labels.push(s.label.clone());
        }
        labels
    }

    pub fn history_count(&self) -> usize {
        1 + self.undo.len() + self.redo.len()
    }

    pub fn history_active_index(&self) -> usize {
        self.undo.len()
    }

    pub fn jump_to_history(&mut self, target: usize) {
        if self.job.is_some() || target >= self.history_count() {
            return;
        }
        self.end_gesture();
        self.live = None;
        let current = self.undo.len();
        if target == current {
            return;
        }
        let saved_path = self.doc.saved_path.clone();
        if target < current {
            let count = current - target;
            for _ in 0..count {
                if let Some(prev) = self.undo.pop() {
                    let cur = Snapshot {
                        label: prev.label.clone(),
                        doc: self.doc.clone(),
                        active: self.active,
                    };
                    self.redo.push(cur);
                    self.doc = prev.doc;
                    self.active = prev.active;
                }
            }
        } else {
            let count = (target - current).min(self.redo.len());
            for _ in 0..count {
                if let Some(next) = self.redo.pop() {
                    let cur = Snapshot {
                        label: next.label.clone(),
                        doc: self.doc.clone(),
                        active: self.active,
                    };
                    self.undo.push(cur);
                    self.doc = next.doc;
                    self.active = next.active;
                }
            }
        }
        self.doc.saved_path = saved_path;
        self.fix_selection();
        self.changed();
    }

    pub fn clear_history(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }

    fn restore(&mut self, snapshot: Snapshot) {
        // Keep the save location: undo shouldn't forget where the file lives.
        let saved_path = self.doc.saved_path.clone();
        self.doc = snapshot.doc;
        self.doc.saved_path = saved_path;
        self.active = snapshot.active;
        self.fix_selection();
        self.changed();
    }

    /// Keep the selection pointing at something that exists.
    pub fn fix_selection(&mut self) {
        if self.doc.index_of(self.active).is_none() {
            self.active = self.doc.layers.last().map_or(0, |l| l.id);
        }
        let has_mask = self
            .doc
            .layer(self.active)
            .is_some_and(|l| l.mask.is_some());
        if self.target == Target::Mask && !has_mask {
            self.target = Target::Pixels;
        }
    }

    /// Start a brush stroke on the active layer (or its mask). Clone and
    /// heal strokes copy from the active layer, or from the whole visible
    /// image if `sample_all`. Returns false if there is nothing to paint
    /// on or a background job is running.
    pub fn begin_stroke(
        &mut self,
        settings: BrushSettings,
        paint: Paint,
        sample_all: bool,
    ) -> bool {
        if self.job.is_some() {
            return false;
        }
        let target = self.target;
        let copying = matches!(
            paint,
            Paint::Clone { .. } | Paint::Heal { .. } | Paint::SpotHeal
        );
        let Some(layer) = self.doc.layer(self.active) else {
            return false;
        };
        let surface = match (target, &layer.mask) {
            // Cloning and healing work on pixels only.
            (Target::Mask, _) if copying => return false,
            (Target::Mask, Some(mask)) => Surface::Mask(mask.pixels.clone()),
            (Target::Mask, None) => return false,
            // Groups and adjustment layers have no pixels to paint.
            (Target::Pixels, _) if !layer.has_pixels() => return false,
            (Target::Pixels, _) => Surface::Pixels(layer.pixels.clone()),
        };
        let mut stroke = Stroke::new(settings, paint, surface);
        if let Some(selection) = &self.doc.selection {
            stroke = stroke.within(selection.coverage.clone());
        }
        if copying {
            let source = if sample_all {
                Tiled::from_raster(&self.doc.composite())
            } else {
                layer.pixels.clone()
            };
            stroke = stroke.sampling(source);
        }
        self.live = None;
        let label = match (target, paint) {
            (Target::Mask, _) => "Paint Mask",
            (_, Paint::Erase) => "Eraser",
            (_, Paint::Clone { .. }) => "Clone Stamp",
            (_, Paint::Heal { .. }) => "Healing Brush",
            (_, Paint::SpotHeal) => "Spot Healing Brush",
            _ => "Brush Stroke",
        };
        let before = self.snapshot(label);
        self.push_undo(before);
        self.stroke = Some((stroke, self.active));
        true
    }

    /// Continue the stroke to an image position.
    pub fn stroke_to(&mut self, x: f32, y: f32) {
        let Some((stroke, _)) = &mut self.stroke else {
            return;
        };
        let tiles = stroke.add_point(x, y);
        self.paint_tiles(&tiles, false);
    }

    /// Finish the stroke (healing happens here). Does nothing if no stroke
    /// is in progress.
    pub fn end_stroke(&mut self) {
        if self.stroke.is_some() {
            self.paint_tiles(&[], true);
            self.stroke = None;
        }
    }

    /// Write the stroke's result for `tiles` into the layer (or, when
    /// `finish`ing, whatever tiles finishing changes), and redraw them.
    fn paint_tiles(&mut self, tiles: &[(u32, u32)], finish: bool) {
        let Some((stroke, id)) = &mut self.stroke else {
            return;
        };
        if tiles.is_empty() && !finish {
            return;
        }
        let Some(layer) = self.doc.layer_mut(*id) else {
            return;
        };
        // Move the surface out of the layer, paint, and put it back, so
        // tiles are written in place rather than copied.
        let mut surface = match self.target {
            Target::Mask => {
                let Some(mask) = layer.mask.as_mut() else {
                    return;
                };
                Surface::Mask(std::mem::replace(&mut mask.pixels, Tiled::new(0, 0, 0)))
            }
            Target::Pixels => Surface::Pixels(std::mem::replace(
                &mut layer.pixels,
                Tiled::new(0, 0, [0; 4]),
            )),
        };
        let changed = if finish {
            stroke.finish(&mut surface)
        } else {
            stroke.apply(&mut surface, tiles);
            tiles.to_vec()
        };
        match surface {
            Surface::Mask(t) => {
                if let Some(mask) = layer.mask.as_mut() {
                    mask.pixels = t;
                }
            }
            Surface::Pixels(t) => layer.pixels = t,
        }
        if changed.is_empty() {
            return;
        }
        let up_to_date = self.rendered == (self.revision, self.view_generation);
        self.changed();
        // A render in place of an older revision would paint over this.
        if let Some(rendering) = &self.rendering {
            rendering.cancel.store(true, Ordering::Release);
        }
        let Some(render) = self.canvas.render() else {
            return;
        };
        if matches!(self.view, View::Separation { .. } | View::GaussianBlur { .. }) {
            return;
        }
        let data = draw_tiles(&self.doc.layers, self.view, self.overlay_colour, &changed);
        render.write_tiles(0, &changed, &data, None);
        self.canvas.invalidate_tiles(0, &changed);
        self.painted = self.revision;
        // If the canvas was current before this dab, it still is. If not, a
        // full render will catch up with the rest.
        if up_to_date {
            self.rendered = (self.revision, self.view_generation);
        }
    }

    /// Finish a brush stroke or move in progress, before another edit.
    fn end_gesture(&mut self) {
        self.end_stroke();
        self.end_move();
    }

    /// Start moving the active layer with the Move tool: the whole layer
    /// and its mask, along with the other selected layers, or with a
    /// selection, just the active layer's selected pixels (or mask values,
    /// when the mask is targeted) and the selection with them. A group
    /// moves with everything in it, but has no pixels to move within a
    /// selection. `copy` moves a copy (Alt+drag): of the whole layers as
    /// new layers, or of the selected pixels. `background` is the grey a
    /// mask is left with where selected values move away. Nothing changes
    /// until [`Self::move_to`] asks for a real move.
    pub fn begin_move(&mut self, label: &str, copy: bool, background: u16) -> bool {
        if self.job.is_some() {
            return false;
        }
        self.end_gesture();
        let Some(index) = self.active_index() else {
            return false;
        };
        let layer = &self.doc.layers[index];
        if layer.is_group && self.doc.selection.is_some() && self.target == Target::Pixels {
            return false;
        }
        let roots = if self.doc.selection.is_some() {
            vec![self.active]
        } else {
            self.doc.outermost(&self.selected())
        };
        let (original, contents) = moving_layers(&self.doc, self.active, &roots);
        self.moving = Some(Moving {
            label: label.to_owned(),
            original,
            contents,
            roots,
            selection: self.doc.selection.clone(),
            target: self.target,
            copy,
            background,
            lift: None,
            applied: (0, 0),
            wanted: (0, 0),
        });
        true
    }

    /// Move to an offset from where the move started, in image pixels.
    /// Applied at once if the canvas is up to date, or else when the
    /// render in progress has drawn what's on screen, so drags on large
    /// images skip intermediate positions rather than falling behind.
    pub fn move_to(&mut self, dx: i32, dy: i32) {
        let Some(moving) = &mut self.moving else {
            return;
        };
        moving.wanted = (dx, dy);
        if self.rendering.as_ref().is_none_or(|r| r.visible) {
            self.apply_move();
        }
    }

    /// Finish the move at the last offset asked for.
    pub fn end_move(&mut self) {
        self.apply_move();
        self.moving = None;
    }

    /// Magic Wand: click to select similar colours, combining with the
    /// current selection using `how`.
    pub fn magic_wand(
        &mut self,
        start: (u32, u32),
        tolerance: u16,
        contiguous: bool,
        anti_alias: bool,
        sample_all: bool,
        how: Combine,
    ) -> bool {
        let (w, h) = (self.doc.width, self.doc.height);
        if start.0 >= w || start.1 >= h {
            if how == Combine::Replace && self.doc.selection.is_some() {
                return self.edit("Deselect", |doc, _| doc.selection = None);
            }
            return false;
        }

        let selection = if sample_all {
            self.canvas
                .render()
                .and_then(|r| {
                    r.with_image(|img| {
                        Selection::magic_wand_raster(img, start, tolerance, contiguous, anti_alias)
                    })
                })
                .unwrap_or_else(|| {
                    let composite = self.doc.composite();
                    Selection::magic_wand_raster(&composite, start, tolerance, contiguous, anti_alias)
                })
        } else if self.target == Target::Mask {
            if let Some(mask) = self.doc.layer(self.active).and_then(|l| l.mask.as_ref()) {
                Selection::magic_wand(
                    w,
                    h,
                    start,
                    tolerance,
                    contiguous,
                    anti_alias,
                    |x, y| {
                        let v = mask.pixels.get(x, y);
                        [v, v, v, u16::MAX]
                    },
                )
            } else {
                Selection::from_coverage(Tiled::new(w, h, 0))
            }
        } else if let Some(layer) = self.doc.layer(self.active) {
            if layer.has_pixels() {
                Selection::magic_wand_tiled(&layer.pixels, start, tolerance, contiguous, anti_alias)
            } else {
                Selection::from_coverage(Tiled::new(w, h, 0))
            }
        } else {
            Selection::from_coverage(Tiled::new(w, h, 0))
        };

        self.edit("Magic Wand", |doc, _| {
            let combined = match (&doc.selection, how) {
                (None, Combine::Subtract) => return,
                (None, _) => selection,
                (Some(current), _) => current.combine(&selection, how),
            };
            doc.selection = (!combined.is_empty()).then_some(combined);
        })
    }

    /// Put the moving layer (or its selected part) at the offset asked for.
    fn apply_move(&mut self) {
        let Some(moving) = &self.moving else {
            return;
        };
        let (dx, dy) = moving.wanted;
        if moving.applied == (dx, dy) {
            return;
        }
        if moving.lift.is_none() {
            // The first real move: one undo step from here, taking the
            // copy (a new layer, with no selection) if Alt was held.
            let label = moving.label.clone();
            self.live = None;
            let before = self.snapshot(&label);
            self.push_undo(before);
            let moving = self.moving.as_mut().expect("still moving");
            let original = &moving.original;
            moving.lift = Some(match (&moving.selection, moving.target, &original.mask) {
                (None, ..) => None,
                (Some(sel), Target::Mask, Some(mask)) => Some(Lift::Mask(Lifted::mask(
                    &mask.pixels,
                    &sel.coverage,
                    moving.copy,
                    moving.background,
                ))),
                (Some(sel), ..) => Some(Lift::Pixels(Lifted::pixels(
                    &original.pixels,
                    &sel.coverage,
                    moving.copy,
                ))),
            });
            if moving.copy && moving.selection.is_none() {
                let copies = self.doc.duplicate_layers(&moving.roots);
                // The copy of the active layer, or if that's inside a
                // selected group, of the top one.
                let at = moving.roots.iter().position(|&id| id == self.active);
                let active = at.or(copies.len().checked_sub(1)).map(|i| copies[i]);
                let active = active.expect("something was copied");
                (moving.original, moving.contents) = moving_layers(&self.doc, active, &copies);
                moving.roots = copies.clone();
                self.select_layers(active, copies);
            }
        }
        let moving = self.moving.as_mut().expect("still moving");
        moving.applied = (dx, dy);
        if moving.lift.as_ref().expect("lifted").is_none() {
            for from in std::iter::once(&moving.original).chain(&moving.contents) {
                if let Some(layer) = self.doc.layer_mut(from.id) {
                    layer.pixels = from.pixels.translated(dx, dy, from.pixels.fill());
                    if let (Some(mask), Some(from)) = (&mut layer.mask, &from.mask) {
                        mask.pixels = from.pixels.translated(dx, dy, from.pixels.fill());
                    }
                }
            }
        }
        let Some(layer) = self.doc.layer_mut(moving.original.id) else {
            return;
        };
        let original = &moving.original;
        match moving.lift.as_ref().expect("lifted") {
            None => {}
            // Put back where it was, it's the original (soft edges would
            // otherwise lose a little opacity).
            Some(_) if (dx, dy) == (0, 0) && !moving.copy => {
                layer.pixels = original.pixels.clone();
                layer.mask = original.mask.clone();
            }
            Some(Lift::Pixels(lifted)) => layer.pixels = lifted.drop_at(dx, dy),
            Some(Lift::Mask(lifted)) => {
                if let Some(mask) = &mut layer.mask {
                    mask.pixels = lifted.drop_at(dx, dy);
                }
            }
        }
        if let Some(sel) = &moving.selection {
            self.doc.selection = Some(sel.translated(dx, dy)).filter(|s| !s.is_empty());
        }
        self.changed();
    }

    /// Pick up finished background work and start rendering if the document
    /// changed. Call once per frame.
    pub fn update(&mut self, ctx: &egui::Context) {
        if let Some(job) = &self.job
            && let Ok((doc, active)) = job.rx.try_recv()
        {
            let label = job.label.clone();
            self.job = None;
            let before = self.snapshot(&label);
            // A save may have finished while the job ran.
            let saved_path = self.doc.saved_path.clone();
            self.doc = doc;
            self.doc.saved_path = saved_path;
            self.active = active;
            self.push_undo(before);
            self.fix_selection();
            self.changed();
        }

        // Leave mask view or the mask overlay when their mask goes away.
        // The overlay also goes when another layer is selected, as in
        // Photoshop.
        if let View::Mask(id) | View::MaskOverlay(id) = self.view {
            let gone = self.doc.layer(id).is_none_or(|l| l.mask.is_none());
            if gone || self.view == View::MaskOverlay(id) && id != self.active {
                self.set_view(View::Image);
            }
        }
        if let View::GaussianBlur { layer, .. } = self.view
            && (self.doc.layer(layer).is_none() || layer != self.active)
        {
            self.set_view(View::Image);
        }

        if let Some(rendering) = &mut self.rendering {
            loop {
                match rendering.rx.try_recv() {
                    Ok(Progress::Whole((render, base))) => {
                        let (revision, generation) = rendering.target;
                        if let Some(base) = base {
                            self.separation_base = Some((revision, base));
                        }
                        // A render started before brush dabs were drawn in
                        // place would erase them from the screen, and one
                        // for a previous view is simply out of date; drop
                        // both and render again.
                        if revision >= self.painted && generation == self.view_generation {
                            self.rendered = rendering.target;
                            self.canvas.set_render(Arc::new(render));
                        }
                    }
                    Ok(Progress::Tiles(level, tiles)) => {
                        self.canvas.invalidate_tiles(level, &tiles)
                    }
                    Ok(Progress::Visible) => rendering.visible = true,
                    Ok(Progress::Done) => self.rendered = rendering.target,
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.rendering = None;
                        break;
                    }
                }
            }
        }
        // Catch up with a move dragged further while rendering.
        if self.rendering.as_ref().is_none_or(|r| r.visible) {
            self.apply_move();
        }
        // One render at a time. Once what's on screen is drawn, a render
        // that's out of date stops, and the latest state is rendered next,
        // so fast slider drags skip intermediate states.
        let current = (self.revision, self.view_generation);
        if let Some(rendering) = &self.rendering
            && rendering.visible
            && rendering.target != current
        {
            rendering.cancel.store(true, Ordering::Release);
        }
        if self.rendering.is_none() && self.rendered != current && self.stroke.is_none() {
            self.start_render(ctx);
        }
    }

    /// Render the current state in the background: in place over what the
    /// canvas shows if it can, or else as a whole new render.
    fn start_render(&mut self, ctx: &egui::Context) {
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let doc = self.doc.clone();
        let view = self.view;
        let colour = self.overlay_colour;
        let ctx = ctx.clone();
        let in_place = self
            .canvas
            .render()
            .filter(|_| !matches!(view, View::Separation { .. } | View::GaussianBlur { .. }));
        if let Some(render) = in_place {
            let job = InPlace {
                doc,
                view,
                colour,
                render: Arc::clone(render),
                visible: self.canvas.visible_area(),
                reduced: Arc::clone(&self.reduced),
                cancel: Arc::clone(&cancel),
                tx,
                ctx,
            };
            std::thread::spawn(move || job.run());
        } else {
            let base = self
                .separation_base
                .as_ref()
                .filter(|(r, _)| *r == self.revision)
                .map(|(_, b)| Arc::clone(b));
            std::thread::spawn(move || {
                let rendered = render_view(&doc, view, colour, base);
                let _ = tx.send(Progress::Whole(rendered));
                ctx.request_repaint();
            });
        }
        self.rendering = Some(Rendering {
            target: (self.revision, self.view_generation),
            cancel,
            visible: false,
            rx,
        });
    }

    pub fn view(&self) -> View {
        self.view
    }

    /// Change what the canvas shows. Old tiles stay up until the new view
    /// is rendered.
    pub fn set_view(&mut self, view: View) {
        if view != self.view {
            self.view = view;
            self.view_generation += 1;
        }
    }

    /// Identifies the current state of the document; changes on every edit.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Mark the document saved, unless it changed since `revision` (the
    /// state that was written).
    pub fn mark_saved(&mut self, revision: u64) {
        if revision == self.revision {
            self.modified = false;
        }
    }
}

/// Layer `active` and the others that move with it: everything in the
/// layers `roots`.
fn moving_layers(doc: &Document, active: u64, roots: &[u64]) -> (Layer, Vec<Layer>) {
    let mut others = Vec::new();
    let mut original = None;
    for index in roots.iter().filter_map(|&id| doc.index_of(id)) {
        for layer in &doc.layers[doc.span(index)] {
            if layer.id == active {
                original = Some(layer.clone());
            } else {
                others.push(layer.clone());
            }
        }
    }
    let original = original.or_else(|| doc.layer(active).cloned());
    (original.expect("the active layer exists"), others)
}

/// Render what `view` shows, with `colour` for the mask overlay. For
/// separation previews, also return the flattened image used, so later
/// radius changes can reuse it.
pub(crate) fn render_view(
    doc: &Document,
    view: View,
    colour: Pixel,
    base: Option<Arc<Tiled<Pixel>>>,
) -> Rendered {
    let all_tiles = || -> Vec<(u32, u32)> {
        (0..doc.height.div_ceil(TILE))
            .flat_map(|r| (0..doc.width.div_ceil(TILE)).map(move |c| (c, r)))
            .collect()
    };
    match view {
        View::Image => (Render::new(doc.composite()), None),
        View::Mask(_) | View::MaskOverlay(_) => {
            let mut image = Raster::new(
                doc.width,
                doc.height,
                vec![[0; 4]; doc.width as usize * doc.height as usize],
            );
            for tiles in all_tiles().chunks(BATCH) {
                let data = draw_tiles(&doc.layers, view, colour, tiles);
                for (&(col, row), tile) in tiles.iter().zip(&data) {
                    image.put_tile(col, row, tile);
                }
            }
            (Render::new(image), None)
        }
        View::Separation { radius, texture } => {
            let base = base.unwrap_or_else(|| Arc::new(Tiled::from_raster(&doc.composite())));
            let low = filters::gaussian_blur(&base, radius);
            let image = if texture {
                // The texture layer on its own: image grain-extract blurred.
                let mut extract = Layer::from_pixels(0, "", low);
                extract.blend = BlendMode::GrainExtract;
                composite::composite(
                    &[Layer::from_pixels(0, "", (*base).clone()), extract],
                    doc.width,
                    doc.height,
                )
            } else {
                low.to_raster()
            };
            (Render::new(image), Some(base))
        }
        View::GaussianBlur { layer, radius } => {
            let Some(l) = doc.layer(layer) else {
                return (Render::new(doc.composite()), None);
            };
            let blurred = filters::gaussian_blur(&l.pixels, radius);
            let blurred = ops::within_selection(&l.pixels, blurred, doc.selection.as_ref());
            let mut layers = doc.layers.clone();
            if let Some(target) = layers.iter_mut().find(|l| l.id == layer) {
                target.pixels = blurred;
            }
            let image = composite::composite(&layers, doc.width, doc.height);
            (Render::new(image), None)
        }
    }
}

/// Draw 256 px tiles (col, row) of what `view` shows (anything but a
/// separation preview), with `colour` for the mask overlay.
fn draw_tiles(
    layers: &[Layer],
    view: View,
    colour: Pixel,
    tiles: &[(u32, u32)],
) -> Vec<Vec<Pixel>> {
    const MAX: u32 = u16::MAX as u32;
    let mask = |id: u64| {
        layers
            .iter()
            .find(|l| l.id == id)
            .and_then(|l| l.mask.as_ref())
    };
    match view {
        // The mask as opaque grey.
        View::Mask(id) => tiles
            .iter()
            .map(|&(col, row)| {
                let values = mask(id).map(|m| (m.pixels.tile(col, row), m.pixels.fill()));
                (0..TILE_PIXELS)
                    .map(|i| {
                        let v = values.map_or(0, |(t, fill)| t.map_or(fill, |t| t[i]));
                        [v, v, v, u16::MAX]
                    })
                    .collect()
            })
            .collect(),
        // Tinted with `colour` where the mask hides, fading to clear where
        // it reveals.
        View::MaskOverlay(id) => {
            let mut out = composite::composite_tiles(layers, tiles);
            let Some(mask) = mask(id) else {
                return out;
            };
            for (tile, &(col, row)) in out.iter_mut().zip(tiles) {
                let values = mask.pixels.tile(col, row);
                for (i, pixel) in tile.iter_mut().enumerate() {
                    let v = values.map_or(mask.pixels.fill(), |t| t[i]);
                    let a = (MAX - v as u32) * OVERLAY_OPACITY / MAX;
                    // Opaque colour over the pixel, so it shows on
                    // transparent areas too.
                    for (c, k) in pixel.iter_mut().zip(colour) {
                        *c = ((*c as u32 * (MAX - a) + k as u32 * a) / MAX) as u16;
                    }
                }
            }
            out
        }
        View::Image | View::Separation { .. } | View::GaussianBlur { .. } => {
            composite::composite_tiles(layers, tiles)
        }
    }
}

/// Brings a render up to date in place, in the background.
struct InPlace {
    doc: Document,
    view: View,
    colour: Pixel,
    render: Arc<Render>,
    /// The pyramid level on screen and the part of the image showing.
    visible: Option<(usize, (u32, u32, u32, u32))>,
    reduced: Arc<Mutex<Reduced>>,
    cancel: Arc<AtomicBool>,
    tx: Sender<Progress>,
    ctx: egui::Context,
}

impl InPlace {
    /// Draw what's on screen first: zoomed out, a preview at the level
    /// shown, composited from shrunk layers. Then draw every tile at full
    /// size, nearest the middle of the view first. Stops early if
    /// cancelled.
    fn run(self) {
        let (w, h) = (self.doc.width, self.doc.height);
        let mut previewed = false;
        if let Some((level, (x0, y0, x1, y1))) = self.visible.filter(|(level, _)| *level > 0) {
            let layers = self
                .reduced
                .lock()
                .expect("reduced layers")
                .layers(&self.doc.layers, level as u32);
            let scale = 1 << level;
            let size = (w.div_ceil(scale), h.div_ceil(scale));
            let area = (
                x0 / scale,
                y0 / scale,
                x1.div_ceil(scale),
                y1.div_ceil(scale),
            );
            let (mut tiles, on_screen) = tile_order(size, area, 2);
            tiles.truncate(on_screen);
            if !self.draw(&layers, level, &tiles, Some(on_screen)) {
                return;
            }
            previewed = true;
        }
        let (tiles, on_screen) = match self.visible {
            Some((level, area)) => tile_order((w, h), area, 2 << level),
            None => tile_order((w, h), (0, 0, w, h), 2),
        };
        let visible = (!previewed).then_some(on_screen);
        if self.draw(&self.doc.layers, 0, &tiles, visible) {
            let _ = self.tx.send(Progress::Done);
            self.ctx.request_repaint();
        }
    }

    /// Draw `tiles` of pyramid level `level` from `layers`, reporting when
    /// the first `visible` of them are done. Those are drawn in one go, as
    /// they're never cancelled for an edit; the rest a tile per core at a
    /// time, to stop soon when cancelled. Returns false if cancelled.
    fn draw(
        &self,
        layers: &[Layer],
        level: usize,
        tiles: &[(u32, u32)],
        mut visible: Option<usize>,
    ) -> bool {
        let (first, rest) = tiles.split_at(visible.unwrap_or(0).min(tiles.len()));
        let batches = rest.chunks(rayon::current_num_threads());
        let mut done = 0;
        for batch in std::iter::once(first)
            .filter(|b| !b.is_empty())
            .chain(batches)
        {
            if self.cancel.load(Ordering::Acquire) {
                return false;
            }
            let data = draw_tiles(layers, self.view, self.colour, batch);
            if !self
                .render
                .write_tiles(level, batch, &data, Some(&self.cancel))
            {
                return false;
            }
            done += batch.len();
            let _ = self.tx.send(Progress::Tiles(level, batch.to_vec()));
            if visible.is_some_and(|n| done >= n) {
                visible = None;
                let _ = self.tx.send(Progress::Visible);
            }
            self.ctx.request_repaint();
        }
        true
    }
}

/// The 256 px tiles of an image of `size`, those overlapping `area` (x0,
/// y0, x1, y1) first, each nearest the middle of `area` first, and how many
/// overlap. Tiles are kept together in blocks of `block` × `block`, the
/// tiles under one display tile, so each display tile is redrawn once.
fn tile_order(
    (w, h): (u32, u32),
    (x0, y0, x1, y1): (u32, u32, u32, u32),
    block: u32,
) -> (Vec<(u32, u32)>, usize) {
    let t = TILE as f32;
    let middle = ((x0 + x1) as f32 / 2.0 / t, (y0 + y1) as f32 / 2.0 / t);
    let on_screen = |col: u32, row: u32| {
        col * TILE < x1 && (col + 1) * TILE > x0 && row * TILE < y1 && (row + 1) * TILE > y0
    };
    let mut tiles: Vec<_> = (0..h.div_ceil(TILE))
        .flat_map(|row| (0..w.div_ceil(TILE)).map(move |col| (col, row)))
        .map(|(col, row)| {
            let (bc, br) = (col / block, row / block);
            let centre = (
                (bc as f32 + 0.5) * block as f32,
                (br as f32 + 0.5) * block as f32,
            );
            let distance = (centre.0 - middle.0).hypot(centre.1 - middle.1);
            (!on_screen(col, row), distance, (br, bc), (col, row))
        })
        .collect();
    tiles.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)).then(a.2.cmp(&b.2)));
    let visible = tiles.iter().filter(|t| !t.0).count();
    (tiles.into_iter().map(|t| t.3).collect(), visible)
}

#[cfg(test)]
mod tests {
    use super::*;
    use omapix_engine::layer::Mask;
    use omapix_engine::{ColorProfile, Raster, ops};

    const RED: Pixel = [65535, 0, 0, 65535];

    fn editor() -> Editor {
        let (w, h) = (600, 400);
        let image = Raster::new(w, h, vec![[30000, 30000, 30000, 65535]; (w * h) as usize]);
        let doc = Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16);
        Editor::new(doc).unwrap()
    }

    fn hard(size: f32) -> BrushSettings {
        BrushSettings {
            size,
            hardness: 1.0,
            opacity: 1.0,
            flow: 1.0,
        }
    }

    #[test]
    fn stroke_paints_the_mask_when_it_is_targeted_and_undoes_in_one_step() {
        let mut e = editor();
        e.edit("D&B", |doc, active| {
            *active = ops::dodge_and_burn_layer(doc, 0)
        });
        e.edit("Mask", |doc, active| {
            doc.layer_mut(*active).unwrap().mask = Some(Mask::white(600, 400))
        });
        e.target = Target::Mask;
        assert!(e.begin_stroke(hard(40.0), Paint::Mask(0), false));
        e.stroke_to(100.0, 100.0);
        e.stroke_to(300.0, 100.0);
        e.end_stroke();
        let layer = e.doc.layer(e.active).unwrap();
        assert_eq!(layer.mask.as_ref().unwrap().pixels.get(200, 100), 0);
        assert_eq!(layer.mask.as_ref().unwrap().pixels.get(200, 300), u16::MAX);
        // Pixels untouched.
        assert_eq!(layer.pixels.get(200, 100), [32768, 32768, 32768, 65535]);
        e.undo();
        assert!(
            e.doc
                .layer(e.active)
                .unwrap()
                .mask
                .as_ref()
                .unwrap()
                .pixels
                .tile(0, 0)
                .is_none()
        );
        assert_eq!(e.undo_label(), Some("Mask"));
    }

    #[test]
    fn undo_mid_stroke_finishes_the_stroke_first() {
        let mut e = editor();
        assert!(e.begin_stroke(hard(40.0), Paint::Color([0, 0, 0, 65535]), false));
        e.stroke_to(100.0, 100.0);
        e.undo();
        // Later moves of the abandoned stroke change nothing.
        e.stroke_to(300.0, 100.0);
        assert_eq!(
            e.doc.layers[0].pixels.get(300, 100),
            [30000, 30000, 30000, 65535]
        );
        assert_eq!(
            e.doc.layers[0].pixels.get(100, 100),
            [30000, 30000, 30000, 65535]
        );
        e.redo();
        assert_eq!(e.doc.layers[0].pixels.get(100, 100), [0, 0, 0, 65535]);
    }

    #[test]
    fn views_render_masks_and_separation_previews() {
        let mut e = editor();
        e.edit("Mask", |doc, active| {
            let mut mask = Mask::white(600, 400);
            mask.pixels.tile_mut(0, 0)[0] = 1234;
            doc.layer_mut(*active).unwrap().mask = Some(mask);
        });
        let (render, _) = render_view(&e.doc, View::Mask(e.active), RED, None);
        let data = render.sample_for_test(0, 0);
        assert_eq!(data, [1234, 1234, 1234, 65535]);

        // A flat image has no texture: the preview is mid-grey everywhere,
        // and the colour/tone preview is the image itself.
        let (texture, base) = render_view(
            &e.doc,
            View::Separation {
                radius: 5.0,
                texture: true,
            },
            RED,
            None,
        );
        assert!(texture.sample_for_test(300, 200)[0].abs_diff(32768) <= 2);
        let (tone, _) = render_view(
            &e.doc,
            View::Separation {
                radius: 5.0,
                texture: false,
            },
            RED,
            base,
        );
        assert!(tone.sample_for_test(300, 200)[0].abs_diff(30000) <= 2);

        // Gaussian blur preview blurs the layer pixels.
        e.edit("Dot", |doc, active| {
            doc.layer_mut(*active).unwrap().pixels.tile_mut(0, 0)[0] = [0, 0, 0, 65535];
        });
        let (blurred, _) = render_view(
            &e.doc,
            View::GaussianBlur {
                layer: e.active,
                radius: 5.0,
            },
            RED,
            None,
        );
        // The dot was spread out: pixel at (100, 100) is no longer pure black.
        assert!(blurred.sample_for_test(100, 100)[0] > 0);
    }

    /// An editor with a neutral dodge & burn layer on top, selected, whose
    /// mask hides tile (0, 0), half hides tile (1, 0), and reveals the rest.
    fn masked_editor() -> Editor {
        let mut e = editor();
        e.edit("D&B", |doc, active| {
            *active = ops::dodge_and_burn_layer(doc, 0)
        });
        e.edit("Mask", |doc, active| {
            let mut mask = Mask::white(600, 400);
            mask.pixels.tile_mut(0, 0).fill(0);
            mask.pixels.tile_mut(1, 0).fill(32768);
            doc.layer_mut(*active).unwrap().mask = Some(mask);
        });
        e
    }

    #[test]
    fn mask_overlay_tints_hidden_areas_red() {
        let e = masked_editor();
        let (render, _) = render_view(&e.doc, View::MaskOverlay(e.active), RED, None);
        let near = |a: [u16; 4], b: [u16; 4]| a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 2);
        // Hidden: half way to red.
        let hidden = render.sample_for_test(100, 100);
        assert!(near(hidden, [47767, 15000, 15000, 65535]), "{hidden:?}");
        // Half hidden: a quarter of the way.
        let half = render.sample_for_test(300, 100);
        assert!(near(half, [38883, 22500, 22500, 65535]), "{half:?}");
        // Revealed: the image as it is.
        assert_eq!(
            render.sample_for_test(300, 300),
            [30000, 30000, 30000, 65535]
        );
    }

    #[test]
    fn mask_overlay_updates_in_place_while_painting_the_mask() {
        let mut e = masked_editor();
        e.target = Target::Mask;
        e.set_view(View::MaskOverlay(e.active));
        let (render, _) = render_view(&e.doc, e.view, e.overlay_colour, None);
        e.canvas.set_render(Arc::new(render));
        e.rendered = (e.revision, e.view_generation);
        assert!(e.begin_stroke(hard(40.0), Paint::Mask(0), false));
        e.stroke_to(300.0, 300.0);
        e.stroke_to(500.0, 300.0);
        e.end_stroke();
        assert_eq!(e.rendered, (e.revision, e.view_generation));
        let (full, _) = render_view(&e.doc, e.view, e.overlay_colour, None);
        for (x, y) in [(400, 300), (400, 380), (100, 100)] {
            assert_eq!(e.canvas.sample(x, y), Some(full.sample_for_test(x, y)));
        }
        assert_ne!(e.canvas.sample(400, 300), e.canvas.sample(400, 380));
    }

    #[test]
    fn mask_overlay_goes_when_another_layer_is_selected() {
        let ctx = egui::Context::default();
        let mut e = masked_editor();
        let top = e.active;
        e.set_view(View::MaskOverlay(top));
        e.update(&ctx);
        assert_eq!(e.view(), View::MaskOverlay(top));
        e.active = e.doc.layers[0].id;
        e.update(&ctx);
        assert_eq!(e.view(), View::Image);

        // Mask view stays on other layers, but goes with its mask.
        e.set_view(View::Mask(top));
        e.update(&ctx);
        assert_eq!(e.view(), View::Mask(top));
        e.edit("Delete Layer Mask", |doc, _| {
            doc.layer_mut(top).unwrap().mask = None
        });
        e.update(&ctx);
        assert_eq!(e.view(), View::Image);
    }

    /// An editor whose layer is red in the top-left 100 × 100 and
    /// transparent elsewhere, with a mask hiding the top-left 50 × 50.
    fn square_editor() -> Editor {
        let mut e = editor();
        e.edit("Setup", |doc, active| {
            let layer = doc.layer_mut(*active).unwrap();
            layer.pixels = Tiled::new(600, 400, [0; 4]);
            let mut mask = Mask::white(600, 400);
            for y in 0..100 {
                for x in 0..100 {
                    layer.pixels.tile_mut(0, 0)[y * 256 + x] = RED;
                    if x < 50 && y < 50 {
                        mask.pixels.tile_mut(0, 0)[y * 256 + x] = 0;
                    }
                }
            }
            layer.mask = Some(mask);
        });
        e
    }

    #[test]
    fn move_carries_the_layer_and_its_mask_as_one_undo_step() {
        let mut e = square_editor();
        // A click without dragging changes nothing.
        assert!(e.begin_move("Move", false, 0));
        e.end_move();
        assert_eq!(e.undo_label(), Some("Setup"));

        assert!(e.begin_move("Move", false, 0));
        e.move_to(100, 50);
        e.move_to(300, 200);
        e.end_move();
        let layer = e.doc.layer(e.active).unwrap();
        assert_eq!(layer.pixels.get(50, 50)[3], 0);
        assert_eq!(layer.pixels.get(350, 250), RED);
        let mask = &layer.mask.as_ref().unwrap().pixels;
        assert_eq!((mask.get(310, 210), mask.get(360, 260)), (0, u16::MAX));
        // Uncovered mask reads as the mask's fill (white).
        assert_eq!(mask.get(10, 10), u16::MAX);
        assert_eq!(e.undo_label(), Some("Move"));
        e.undo();
        assert_eq!(e.doc.layer(e.active).unwrap().pixels.get(50, 50), RED);
        assert_eq!(e.undo_label(), Some("Setup"));
    }

    #[test]
    fn move_with_a_selection_moves_just_the_selected_pixels() {
        let mut e = square_editor();
        e.edit("Marquee", |doc, _| {
            doc.selection = Some(Selection::rectangle(600, 400, (0.0, 0.0), (50.0, 100.0)))
        });
        assert!(e.begin_move("Move", false, 0));
        e.move_to(200, 0);
        e.end_move();
        let layer = e.doc.layer(e.active).unwrap();
        assert_eq!(layer.pixels.get(25, 50)[3], 0);
        assert_eq!(layer.pixels.get(75, 50), RED);
        assert_eq!(layer.pixels.get(225, 50), RED);
        // The mask stays put, and the selection moves with the pixels.
        assert_eq!(layer.mask.as_ref().unwrap().pixels.get(25, 25), 0);
        let sel = e.doc.selection.as_ref().unwrap();
        assert_eq!((sel.at(25, 50), sel.at(225, 50)), (0.0, 1.0));
        let xs = sel.outlines.iter().flatten().map(|p| p.0);
        assert_eq!(xs.fold((f32::MAX, 0f32), |(l, r), x| (l.min(x), r.max(x))), (200.0, 250.0));

        // With the mask targeted, the selected mask values move instead.
        e.target = Target::Mask;
        assert!(e.begin_move("Move", false, 1234));
        e.move_to(-200, 0);
        e.end_move();
        let layer = e.doc.layer(e.active).unwrap();
        let mask = &layer.mask.as_ref().unwrap().pixels;
        assert_eq!((mask.get(25, 25), mask.get(225, 25)), (u16::MAX, 1234));
        assert_eq!(layer.pixels.get(225, 50), RED);
    }

    #[test]
    fn alt_move_copies_the_layer_or_the_selected_pixels() {
        let mut e = square_editor();
        let original = e.active;
        assert!(e.begin_move("Move", true, 0));
        e.move_to(0, 200);
        e.end_move();
        assert_eq!(e.doc.layers.len(), 2);
        assert_ne!(e.active, original);
        assert_eq!(e.doc.layer(e.active).unwrap().name, "Background copy");
        assert_eq!(e.doc.layer(e.active).unwrap().pixels.get(50, 250), RED);
        assert_eq!(e.doc.layer(original).unwrap().pixels.get(50, 50), RED);
        e.undo();
        assert_eq!((e.doc.layers.len(), e.active), (1, original));

        e.edit("Marquee", |doc, _| {
            doc.selection = Some(Selection::rectangle(600, 400, (0.0, 0.0), (50.0, 100.0)))
        });
        assert!(e.begin_move("Move", true, 0));
        e.move_to(300, 0);
        e.end_move();
        let layer = e.doc.layer(e.active).unwrap();
        assert_eq!(
            (layer.pixels.get(25, 50), layer.pixels.get(325, 50)),
            (RED, RED)
        );
    }

    #[test]
    fn moves_wait_for_the_render_and_undo_finishes_them() {
        let ctx = egui::Context::default();
        let mut e = square_editor();
        e.update(&ctx);
        assert!(e.rendering.is_some());
        assert!(e.begin_move("Move", false, 0));
        e.move_to(10, 0);
        e.move_to(20, 0);
        // Still rendering: the document waits for the latest offset.
        assert_eq!(e.doc.layer(e.active).unwrap().pixels.get(5, 5), RED);
        while e.rendering.is_some() {
            std::thread::sleep(std::time::Duration::from_millis(5));
            e.update(&ctx);
        }
        let layer = e.doc.layer(e.active).unwrap();
        assert_eq!(
            (layer.pixels.get(19, 5)[3], layer.pixels.get(20, 5)),
            (0, RED)
        );
        // Undo in the middle of a drag finishes it first, then undoes it.
        e.move_to(40, 0);
        e.undo();
        e.move_to(80, 0);
        assert_eq!(e.doc.layer(e.active).unwrap().pixels.get(5, 5), RED);
        assert_eq!(e.undo_label(), Some("Setup"));
    }

    #[test]
    fn dialog_groups_are_one_undo_step_and_cancel_reverts() {
        let mut e = editor();
        e.begin_group("Blending Options");
        e.edit_live("a", |doc| doc.layers[0].opacity = 0.5);
        e.end_live(); // pointer released between drags
        e.edit_live("b", |doc| doc.layers[0].opacity = 0.2);
        e.end_group(true);
        assert_eq!(e.undo_label(), Some("Blending Options"));
        e.undo();
        assert_eq!(e.doc.layers[0].opacity, 1.0);
        assert_eq!(e.undo_label(), None);

        e.begin_group("Blending Options");
        e.edit_live("a", |doc| doc.layers[0].opacity = 0.3);
        e.end_group(false);
        assert_eq!(e.doc.layers[0].opacity, 1.0);
        // Cancelling leaves no trace in the history.
        assert_eq!((e.undo_label(), e.redo_label()), (None, None));
    }

    #[test]
    fn history_jumps_back_and_forward_and_branches() {
        let mut e = editor();
        e.edit("Step 1", |doc, _| doc.layers[0].opacity = 0.8);
        e.edit("Step 2", |doc, _| doc.layers[0].opacity = 0.6);
        e.edit("Step 3", |doc, _| doc.layers[0].opacity = 0.4);

        assert_eq!(e.history_count(), 4);
        assert_eq!(e.history_active_index(), 3);
        assert_eq!(
            e.history_labels(),
            ["t.tif", "Step 1", "Step 2", "Step 3"]
        );
        assert_eq!(e.doc.layers[0].opacity, 0.4);

        // Jump back to Step 1 (index 1).
        e.jump_to_history(1);
        assert_eq!(e.history_active_index(), 1);
        assert_eq!(e.doc.layers[0].opacity, 0.8);
        assert_eq!(
            e.history_labels(),
            ["t.tif", "Step 1", "Step 2", "Step 3"]
        );

        // Jump all the way back to initial state (index 0).
        e.jump_to_history(0);
        assert_eq!(e.history_active_index(), 0);
        assert_eq!(e.doc.layers[0].opacity, 1.0);

        // Jump forward to Step 2 (index 2).
        e.jump_to_history(2);
        assert_eq!(e.history_active_index(), 2);
        assert_eq!(e.doc.layers[0].opacity, 0.6);

        // Branch history with a new edit while at Step 2: Step 3 is discarded.
        e.edit("Step 2B", |doc, _| doc.layers[0].opacity = 0.2);
        assert_eq!(e.history_count(), 4);
        assert_eq!(e.history_active_index(), 3);
        assert_eq!(
            e.history_labels(),
            ["t.tif", "Step 1", "Step 2", "Step 2B"]
        );
        assert_eq!(e.doc.layers[0].opacity, 0.2);

        // Clear history resets to just current state.
        e.clear_history();
        assert_eq!(e.history_count(), 1);
        assert_eq!(e.history_active_index(), 0);
    }

    #[test]
    fn in_place_render_updates_match_a_full_composite() {
        let mut e = editor();
        let render = Arc::new(Render::new(e.doc.composite()));
        e.canvas.set_render(Arc::clone(&render));
        e.rendered = (e.revision, e.view_generation);
        assert!(e.begin_stroke(hard(60.0), Paint::Color([0, 0, 65535, 65535]), false));
        e.stroke_to(250.0, 250.0);
        e.stroke_to(290.0, 270.0);
        e.end_stroke();
        assert_eq!(
            e.rendered,
            (e.revision, e.view_generation),
            "canvas kept current without a full render"
        );
        assert_eq!(
            e.canvas.sample(270, 260),
            Some(e.doc.composite().get(270, 260))
        );
        assert_eq!(e.canvas.sample(270, 260), Some([0, 0, 65535, 65535]));
    }

    /// Call `update` until the background render is finished.
    fn settle(e: &mut Editor, ctx: &egui::Context) {
        e.update(ctx);
        while e.rendering.is_some() {
            std::thread::sleep(std::time::Duration::from_millis(2));
            e.update(ctx);
        }
    }

    #[test]
    fn slider_drags_render_in_place_and_end_up_exact() {
        let ctx = egui::Context::default();
        let mut e = square_editor();
        e.edit("Layer", |doc, active| {
            let mut top = Layer::from_raster(
                9,
                "top",
                &Raster::new(600, 400, vec![[0, 40000, 0, 65535]; 600 * 400]),
            );
            top.blend = BlendMode::Multiply;
            doc.layers.push(top);
            *active = 9;
        });
        settle(&mut e, &ctx);
        let render = Arc::clone(e.canvas.render().unwrap());
        for step in 0..20 {
            e.edit_live("Opacity", |doc| {
                doc.layers[1].opacity = 1.0 - step as f32 / 25.0
            });
            e.update(&ctx);
        }
        e.end_live();
        settle(&mut e, &ctx);
        assert!(
            Arc::ptr_eq(&render, e.canvas.render().unwrap()),
            "drawn in place"
        );
        assert_eq!(e.rendered, (e.revision, e.view_generation));
        let full = e.doc.composite();
        for (x, y) in [(0, 0), (50, 50), (599, 399), (300, 200)] {
            assert_eq!(e.canvas.sample(x, y), Some(full.get(x, y)));
        }
        let exact = Render::new(full);
        assert_eq!(
            render.level_for_test(2).pixels(),
            exact.level_for_test(2).pixels()
        );
    }

    #[test]
    fn zoomed_out_the_view_is_previewed_before_the_full_size_render() {
        let e = square_editor();
        let render = Arc::new(Render::new(Raster::new(600, 400, vec![[0; 4]; 600 * 400])));
        let (tx, rx) = channel();
        let job = InPlace {
            doc: e.doc.clone(),
            view: View::Image,
            colour: RED,
            render: Arc::clone(&render),
            // Half size, looking at the top left.
            visible: Some((1, (0, 0, 300, 200))),
            reduced: Arc::default(),
            cancel: Arc::default(),
            tx,
            ctx: egui::Context::default(),
        };
        job.run();
        let progress: Vec<Progress> = rx.try_iter().collect();
        let steps: Vec<String> = progress
            .iter()
            .map(|p| match p {
                Progress::Tiles(level, tiles) => format!("{level}:{tiles:?}"),
                Progress::Visible => "visible".into(),
                Progress::Done => "done".into(),
                Progress::Whole(_) => "whole".into(),
            })
            .collect();
        // The half-size level has 2 × 1 tiles, one of them on screen. The
        // full size image has 3 × 2, the two on screen first.
        assert_eq!(
            steps,
            [
                "1:[(0, 0)]",
                "visible",
                "0:[(0, 0), (1, 0), (2, 0), (0, 1), (1, 1), (2, 1)]",
                "done"
            ]
        );
        let exact = Render::new(e.doc.composite());
        for level in 0..3 {
            assert_eq!(
                render.level_for_test(level).pixels(),
                exact.level_for_test(level).pixels()
            );
        }
    }

    #[test]
    fn tiles_on_screen_come_first_from_the_middle_out() {
        // 5 × 3 tiles, looking at the middle three columns of the top row.
        let (order, on_screen) = tile_order((1280, 768), (256, 0, 1024, 256), 1);
        assert_eq!(on_screen, 3);
        assert_eq!(&order[..3], [(2, 0), (1, 0), (3, 0)]);
        assert_eq!(order.len(), 15);
        // In blocks of 2 × 2, a block's tiles stay together.
        let (order, _) = tile_order((1024, 512), (0, 0, 1024, 512), 2);
        assert_eq!(&order[..4], [(0, 0), (1, 0), (0, 1), (1, 1)]);
    }

    #[test]
    fn groups_move_with_their_contents_and_have_no_pixels_to_paint() {
        let mut e = square_editor();
        let layer = e.active;
        e.edit("Group Layers", |doc, active| *active = doc.group_layer(0));
        let group = e.active;
        assert!(!e.begin_stroke(hard(40.0), Paint::Color(RED), false));

        assert!(e.begin_move("Move", false, 0));
        e.move_to(200, 100);
        e.end_move();
        let moved = e.doc.layer(layer).unwrap();
        assert_eq!(moved.pixels.get(250, 150), RED);
        assert_eq!(moved.mask.as_ref().unwrap().pixels.get(210, 110), 0);

        // Alt+drag copies the group and everything in it.
        assert!(e.begin_move("Move", true, 0));
        e.move_to(0, 200);
        e.end_move();
        assert_eq!(e.doc.layers.len(), 4);
        let copy = e.doc.layer(e.active).unwrap();
        assert_eq!((copy.name.as_str(), copy.is_group), ("Group 1 copy", true));
        let inside = &e.doc.layers[2];
        assert_eq!(inside.parent, Some(e.active));
        assert_eq!(inside.pixels.get(250, 350), RED);
        assert_eq!(e.doc.layer(layer).unwrap().pixels.get(250, 150), RED);
        assert_eq!(e.doc.layers[1].id, group);

        // A group has no pixels to move within a selection.
        e.active = group;
        e.edit("Marquee", |doc, _| {
            doc.selection = Some(Selection::rectangle(600, 400, (0.0, 0.0), (50.0, 100.0)))
        });
        assert!(!e.begin_move("Move", false, 0));
    }

    #[test]
    fn several_selected_layers_move_together() {
        let mut e = square_editor();
        let bottom = e.active;
        e.edit("Layer", |doc, active| {
            let mut top = Layer::empty(9, "top", 600, 400);
            top.pixels.tile_mut(0, 0)[0] = RED;
            doc.layers.push(top);
            *active = 9;
        });
        e.select_layers(9, vec![bottom]);
        assert!(e.begin_move("Move", false, 0));
        e.move_to(300, 200);
        e.end_move();
        assert_eq!(e.doc.layer(9).unwrap().pixels.get(300, 200), RED);
        assert_eq!(e.doc.layer(bottom).unwrap().pixels.get(350, 250), RED);
        e.undo();

        // Alt+drag copies them all, and selects the copies.
        assert!(e.begin_move("Move", true, 0));
        e.move_to(0, 200);
        e.end_move();
        assert_eq!(e.doc.layers.len(), 4);
        let copies = e.selected();
        assert_eq!(copies.len(), 2);
        assert_eq!(e.doc.layer(e.active).unwrap().name, "top copy");
        assert_eq!(e.doc.layer(copies[0]).unwrap().pixels.get(50, 250), RED);
        assert_eq!(e.doc.layer(bottom).unwrap().pixels.get(50, 50), RED);

        // With a selection, just the active layer's selected pixels move.
        e.select_layers(9, vec![bottom]);
        e.edit("Marquee", |doc, _| {
            doc.selection = Some(Selection::rectangle(600, 400, (0.0, 0.0), (100.0, 100.0)))
        });
        assert!(e.begin_move("Move", false, 0));
        e.move_to(200, 0);
        e.end_move();
        assert_eq!(e.doc.layer(bottom).unwrap().pixels.get(50, 50), RED);
        assert_eq!(e.doc.layer(9).unwrap().pixels.get(200, 0), RED);
    }

    #[test]
    fn magic_wand_selects_red_square_and_combines() {
        let mut e = square_editor();
        // Magic wand on the red square at (75, 75)
        assert!(e.magic_wand((75, 75), 0, true, false, false, Combine::Replace));
        assert!(e.doc.selection.is_some());
        let sel = e.doc.selection.as_ref().unwrap();
        assert_eq!(sel.at(75, 75), 1.0);
        assert_eq!(sel.at(20, 20), 1.0);
        assert_eq!(sel.at(150, 150), 0.0);

        // Click outside image deselects
        assert!(e.magic_wand((700, 700), 0, true, false, false, Combine::Replace));
        assert!(e.doc.selection.is_none());

        // Undo brings selection back
        e.undo();
        assert!(e.doc.selection.is_some());
    }
}
