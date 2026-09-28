//! One open document: its layers, undo history, selection, and the
//! background work that keeps the canvas up to date.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};

use omapix_engine::adjust::Adjustment;
use omapix_engine::brush::{BrushSettings, Paint, Stroke, Surface};
use omapix_engine::composite::GroupCache;
use omapix_engine::layer::Layer;
use omapix_engine::moving::Lifted;
use omapix_engine::transform::{Affine, Resampling, transformed};
use omapix_engine::reduced::Reduced;
use omapix_engine::selection::{Channel, Combine, Selection};
use omapix_engine::tiled::{TILE, TILE_PIXELS, Tiled};
use omapix_engine::{
    BlendMode, DisplayTransform, Document, NoiseOptions, Pixel, Raster, composite, filters, ops,
};

use crate::canvas::{Canvas, Render};
use crate::live::{self, LiveFrame, LiveStack, Settings, Table};
use crate::tools::{Sample, SampleSize};

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
    /// Photoshop's Quick Mask mode (`Q`), painting the selection directly
    /// with the red overlay.
    QuickMask,
    /// What frequency separation at `radius` would produce: the texture
    /// layer, or the colour/tone layer.
    Separation { radius: f32, texture: bool },
    /// What a Filter menu filter would do to `layer`, or with `mask` to
    /// its mask.
    Filter {
        layer: u64,
        filter: omapix_engine::filters::LayerFilter,
        mask: bool,
    },
    /// What adding noise with `options` above `layer` would produce.
    AddNoise { layer: u64, options: NoiseOptions },
    /// One channel of the finished image, in grey (the Channels panel).
    Channel(Channel),
    /// A saved alpha channel, in grey.
    Alpha(u64),
}

impl View {
    /// Whether it's rendered whole by [`render_view`], rather than tile by
    /// tile, in place, by [`draw_tiles`].
    fn whole(self) -> bool {
        matches!(
            self,
            View::Separation { .. } | View::Filter { .. } | View::AddNoise { .. } | View::Alpha(_)
        )
    }
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
    QuickMask,
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

/// A Free Transform (Ctrl+T) in progress: the layer, or its selected pixels
/// or mask values, scaled, rotated and moved until it's applied or
/// cancelled.
struct Transforming {
    /// The layer as it was.
    original: Layer,
    /// The selection as it was, which is transformed along with the pixels.
    selection: Option<Selection>,
    /// The selected part, lifted out; `None` transforms the whole layer and
    /// its mask.
    lift: Option<Lift>,
    /// What's being transformed, before it was: (x0, y0, x1, y1).
    bounds: [f64; 4],
    /// The transform showing in the document (`None` once it needs doing
    /// again at full quality), and the latest one asked for.
    applied: Option<Affine>,
    wanted: Affine,
    /// The document has changed, as one undo step.
    started: bool,
}

/// A Move drag, or a slider drag on one layer's settings, shown live on the
/// GPU (see live.rs), and once it ends, until the CPU render of it is on
/// screen.
struct LiveView {
    /// The layer moved or edited.
    subject: u64,
    /// The stack being made in the background, then made.
    rx: Option<Receiver<Option<LiveStack>>>,
    stack: Option<Arc<LiveStack>>,
    /// How far the drag has moved, in image pixels.
    offset: (i32, i32),
    /// For a Free Transform: the transform showing.
    transform: Option<Affine>,
    /// For an edit: the subject's adjustment and its lookup table, kept
    /// while the adjustment is unchanged.
    edit: Option<Option<(Adjustment, Table)>>,
    /// Once the move is made or the edit ends: the revision to wait for.
    until: Option<u64>,
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
    /// What isolated groups composite to, at full size and in previews.
    groups: Arc<GroupCache>,
    preview_groups: Arc<GroupCache>,
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
    transforming: Option<Transforming>,
    live_view: Option<LiveView>,
    /// The largest GPU texture side for live moves, or `None` to move on
    /// the CPU (see gpu.rs).
    pub live_limit: Option<u32>,
    /// Last revision drawn straight into the canvas by a brush stroke.
    /// Background renders of older revisions are thrown away.
    painted: u64,
    /// The mask overlay's red, in the document's colour space.
    overlay_colour: Pixel,
    /// The active layer when entering Quick Mask.
    quick_mask_layer: Option<u64>,
    /// Ctrl+H: the selection's marching ants are hidden. A new selection
    /// shows them again.
    pub hide_selection_edges: bool,
    /// While a refined selection is previewed: the real selection, and
    /// the view to go back to.
    previewing: Option<(Option<Selection>, View)>,
    /// Cached source images for sampling (Current & Below and All Layers).
    sample_cache: Mutex<SampleCache>,
}

#[derive(Default)]
struct SampleCache {
    all: Option<(u64, Arc<Tiled<Pixel>>)>,
    current_and_below: Option<(u64, u64, Arc<Tiled<Pixel>>)>,
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
            groups: Arc::default(),
            preview_groups: Arc::default(),
            separation_base: None,
            job: None,
            modified: false,
            canvas,
            stroke: None,
            moving: None,
            transforming: None,
            live_view: None,
            live_limit: crate::gpu::max_side(),
            painted: 0,
            overlay_colour,
            quick_mask_layer: None,
            hide_selection_edges: false,
            previewing: None,
            sample_cache: Mutex::new(SampleCache::default()),
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

    fn check_size_changed(&mut self) {
        if self.doc.width != self.canvas.width() || self.doc.height != self.canvas.height() {
            self.canvas.resize(self.doc.width, self.doc.height);
            self.reduced = Arc::default();
            self.groups = Arc::default();
            self.preview_groups = Arc::default();
            self.separation_base = None;
            if let Ok(mut cache) = self.sample_cache.lock() {
                *cache = SampleCache::default();
            }
            self.live_view = None;
            self.moving = None;
            self.transforming = None;
            if let Some(rendering) = self.rendering.take() {
                rendering.cancel.store(true, Ordering::Release);
            }
            self.rendered = (0, u64::MAX);
        }
    }

    fn changed(&mut self) {
        self.revision += 1;
        self.modified = true;
        self.check_size_changed();
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
        let before = self.doc.layers.clone();
        f(&mut self.doc);
        self.changed();
        self.show_edit_live(&before);
    }

    /// A continuous edit changed the document from `before`. If it only
    /// changed the active layer's opacity, blend mode or adjustment, show
    /// it live on the GPU until it ends; otherwise render it on the CPU.
    fn show_edit_live(&mut self, before: &[Layer]) {
        let fits = live::only_settings_changed(before, &self.doc.layers, self.active);
        let showing = self.live_view.as_ref().is_some_and(|v| {
            v.edit.is_some() && v.subject == self.active && v.until.is_none()
        });
        if !fits {
            self.live_view = None;
        } else if !showing {
            self.live_view = self.start_live_view(false);
        }
    }

    pub fn end_live(&mut self) {
        if self.group.is_none() {
            self.live = None;
            self.end_live_edit();
        }
    }

    /// A live edit is over: render it on the CPU, showing it live until
    /// that's on screen.
    fn end_live_edit(&mut self) {
        if let Some(view) = self.live_view.as_mut().filter(|v| v.edit.is_some()) {
            view.until.get_or_insert(self.revision);
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
        self.end_live_edit();
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
    /// heal strokes copy from the source image determined by `sample`.
    /// Returns false if there is nothing to paint on or a background job is running.
    pub fn begin_stroke(
        &mut self,
        settings: BrushSettings,
        paint: Paint,
        sample: Sample,
    ) -> bool {
        if self.job.is_some() {
            return false;
        }
        let target = self.target;
        let copying = matches!(
            paint,
            Paint::Clone { .. } | Paint::Heal { .. } | Paint::SpotHeal
        );
        let (w, h) = (self.doc.width, self.doc.height);
        let surface = match target {
            // Cloning and healing work on pixels only.
            Target::Mask | Target::QuickMask if copying => return false,
            Target::QuickMask => {
                let sel = self
                    .doc
                    .selection
                    .get_or_insert_with(|| Selection::all(w, h));
                Surface::Mask(sel.coverage.clone())
            }
            Target::Mask => {
                let Some(layer) = self.doc.layer(self.active) else {
                    return false;
                };
                let Some(mask) = &layer.mask else {
                    return false;
                };
                Surface::Mask(mask.pixels.clone())
            }
            // Groups, adjustment layers and image-locked layers cannot paint pixels.
            Target::Pixels => {
                let Some(layer) = self.doc.layer(self.active) else {
                    return false;
                };
                if !layer.can_paint_pixels() {
                    return false;
                }
                Surface::Pixels(layer.pixels.clone())
            }
        };
        let locked = target == Target::Pixels
            && self.doc.layer(self.active).is_some_and(|l| l.lock_alpha());
        let mut stroke = Stroke::new(settings, paint, surface);
        if locked {
            stroke = stroke.keeping_alpha();
        }
        if target != Target::QuickMask
            && let Some(selection) = &self.doc.selection
        {
            stroke = stroke.within(selection.coverage.clone());
        }
        if copying {
            let source = self.sample_source(sample);
            stroke = stroke.sampling(source);
        }
        self.live = None;
        let label = match (target, paint) {
            (Target::QuickMask, _) => "Quick Mask",
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

    /// Continue the stroke to an image position, with a pen's `pressure`
    /// there (1 for a mouse).
    pub fn stroke_to(&mut self, x: f32, y: f32, pressure: f32) {
        let Some((stroke, _)) = &mut self.stroke else {
            return;
        };
        let tiles = stroke.add_point(x, y, pressure);
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
        // Move the surface out of the layer, paint, and put it back, so
        // tiles are written in place rather than copied.
        let mut surface = match self.target {
            Target::QuickMask => {
                let (w, h) = (self.doc.width, self.doc.height);
                let sel = self
                    .doc
                    .selection
                    .get_or_insert_with(|| Selection::all(w, h));
                Surface::Mask(std::mem::replace(&mut sel.coverage, Tiled::new(0, 0, 0)))
            }
            Target::Mask => {
                let Some(layer) = self.doc.layer_mut(*id) else {
                    return;
                };
                let Some(mask) = layer.mask.as_mut() else {
                    return;
                };
                Surface::Mask(std::mem::replace(&mut mask.pixels, Tiled::new(0, 0, 0)))
            }
            Target::Pixels => {
                let Some(layer) = self.doc.layer_mut(*id) else {
                    return;
                };
                Surface::Pixels(std::mem::replace(
                    &mut layer.pixels,
                    Tiled::new(0, 0, [0; 4]),
                ))
            }
        };
        let changed = if finish {
            stroke.finish(&mut surface)
        } else {
            stroke.apply(&mut surface, tiles);
            tiles.to_vec()
        };
        match (surface, self.target) {
            (Surface::Mask(t), Target::QuickMask) => {
                if let Some(sel) = self.doc.selection.as_mut() {
                    sel.coverage = t;
                }
            }
            (Surface::Mask(t), Target::Mask) => {
                if let Some(layer) = self.doc.layer_mut(*id)
                    && let Some(mask) = layer.mask.as_mut()
                {
                    mask.pixels = t;
                }
            }
            (Surface::Pixels(t), Target::Pixels) => {
                if let Some(layer) = self.doc.layer_mut(*id) {
                    layer.pixels = t;
                }
            }
            _ => {}
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
        if self.view.whole() {
            return;
        }
        let data = draw_tiles(
            &self.doc.layers,
            self.view,
            self.overlay_colour,
            &changed,
            Some(&self.groups),
            self.doc.selection.as_ref(),
        );
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
        self.commit_transform();
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
        if self.job.is_some() || self.target == Target::QuickMask {
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
        if !original.can_move() || contents.iter().any(|l| !l.can_move()) {
            return false;
        }
        let simple = !copy && self.doc.selection.is_none() && roots == [self.active];
        self.live_view = simple.then(|| self.start_live_view(true)).flatten();
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
        // Shown live, it doesn't wait for renders.
        let live = self.live_view.as_ref().is_some_and(|l| l.until.is_none());
        if live || self.rendering.as_ref().is_none_or(|r| r.visible) {
            self.apply_move();
        }
    }

    /// Finish the move at the last offset asked for.
    pub fn end_move(&mut self) {
        if self.moving.is_none() {
            return;
        }
        // Made for real now; the live view stays up until it's on screen.
        let until = self.revision + 1;
        if let Some(live) = &mut self.live_view {
            live.until = Some(until);
            if let (Some(moving), Some(_)) = (&mut self.moving, &live.stack) {
                moving.wanted = live.offset;
            }
        }
        self.apply_move();
        self.moving = None;
        if self.revision < until {
            // Nothing moved.
            self.live_view = None;
        }
    }

    /// Start Free Transform on the active layer: its selected pixels (or
    /// mask values, when the mask is targeted) with a selection, or else
    /// the whole layer with its mask. Nothing changes until
    /// [`Self::transform_to`] asks for a transform. `background` is the grey a
    /// mask is left with where selected values move away. Returns why it
    /// can't start, if it can't.
    pub fn begin_transform(&mut self, background: u16) -> Result<(), &'static str> {
        if self.job.is_some() {
            return Err("Omapix is busy");
        }
        if self.target == Target::QuickMask {
            return Err("Leave Quick Mask (Q) to transform");
        }
        self.end_gesture();
        let layer = self.doc.layer(self.active).ok_or("There's no layer to transform")?;
        if !layer.can_move() {
            return Err("Could not transform because the layer is locked");
        }
        let lift = match (&self.doc.selection, self.target, &layer.mask) {
            (None, ..) => None,
            (Some(sel), Target::Mask, Some(mask)) => {
                Some(Lift::Mask(Lifted::mask(&mask.pixels, &sel.coverage, false, background)))
            }
            (Some(_), ..) if !layer.has_pixels() => return Err("Could not transform because the layer has no pixels"),
            (Some(sel), ..) => Some(Lift::Pixels(Lifted::pixels(&layer.pixels, &sel.coverage, false))),
        };
        if lift.is_none() && !layer.has_pixels() {
            return Err("Could not transform because the layer has no pixels");
        }
        let bounds = match &self.doc.selection {
            Some(sel) => sel.bounds(),
            None => Selection::from_alpha(&layer.pixels).bounds(),
        };
        let [x, y, w, h] = bounds.ok_or("Could not transform because the selected area is empty")?;
        let (x, y, w, h) = (f64::from(x), f64::from(y), f64::from(w), f64::from(h));
        let whole = lift.is_none();
        self.transforming = Some(Transforming {
            original: layer.clone(),
            selection: self.doc.selection.clone(),
            lift,
            bounds: [x, y, x + w, y + h],
            applied: Some(Affine::IDENTITY),
            wanted: Affine::IDENTITY,
            started: false,
        });
        if whole {
            self.show_transform_live();
        }
        Ok(())
    }

    /// Show the whole-layer Free Transform in progress live on the GPU, if
    /// it can be.
    fn show_transform_live(&mut self) {
        let wanted = self.transforming.as_ref().map(|t| t.wanted);
        self.live_view = self.start_live_view(true).map(|live| LiveView {
            transform: wanted,
            ..live
        });
    }

    /// The Free Transform in progress: what's transformed, before it was
    /// (x0, y0, x1, y1), and the transform showing.
    pub fn transform(&self) -> Option<([f64; 4], Affine)> {
        self.transforming.as_ref().map(|t| (t.bounds, t.wanted))
    }

    /// Show the Free Transform with `t`. Applied at once if the canvas is up
    /// to date, or else when the render in progress has drawn what's on
    /// screen, as moves are.
    pub fn transform_to(&mut self, t: Affine) {
        let Some(transforming) = &mut self.transforming else {
            return;
        };
        transforming.wanted = t;
        // Shown live, it doesn't wait for renders.
        let live = self.live_view.as_ref().is_some_and(|l| l.transform.is_some() && l.until.is_none());
        if live || self.rendering.as_ref().is_none_or(|r| r.visible) {
            self.apply_transform(Resampling::Bilinear);
        }
    }

    /// Apply the Free Transform (Enter), resampled at full quality, as one
    /// undo step.
    pub fn commit_transform(&mut self) {
        let Some(transforming) = &mut self.transforming else {
            return;
        };
        if transforming.wanted == Affine::IDENTITY {
            self.cancel_transform();
            return;
        }
        transforming.applied = None;
        // Shown live until the CPU's render of it is on screen.
        let until = self.revision + 1;
        if let Some(live) = self.live_view.as_mut().filter(|l| l.transform.is_some()) {
            live.until = Some(until);
        }
        self.apply_transform(Resampling::Bicubic);
        self.transforming = None;
    }

    /// Put the layer and selection back as they were (Esc).
    pub fn cancel_transform(&mut self) {
        let Some(transforming) = self.transforming.take() else {
            return;
        };
        if self.live_view.as_ref().is_some_and(|l| l.transform.is_some()) {
            self.live_view = None;
        }
        if !transforming.started {
            return;
        }
        // Undo the step the first change made, without offering to redo it.
        if let Some(before) = self.undo.pop() {
            self.restore(before);
        }
    }

    fn apply_transform(&mut self, resampling: Resampling) {
        let Some(transforming) = &self.transforming else {
            return;
        };
        let t = transforming.wanted;
        if transforming.applied == Some(t) {
            return;
        }
        // Shown live, it's made only when it's applied.
        if resampling == Resampling::Bilinear
            && let Some(live) = self.live_view.as_mut().filter(|l| l.transform.is_some() && l.until.is_none())
        {
            live.transform = Some(t);
            return;
        }
        if !transforming.started {
            // The first change: one undo step from here.
            self.live = None;
            let before = self.snapshot("Free Transform");
            self.push_undo(before);
        }
        let transforming = self.transforming.as_mut().expect("still transforming");
        (transforming.applied, transforming.started) = (Some(t), true);
        let Some(layer) = self.doc.layer_mut(transforming.original.id) else {
            return;
        };
        let original = &transforming.original;
        match &transforming.lift {
            None => {
                layer.pixels = transformed(&original.pixels, &t, [0; 4], resampling);
                if let (Some(mask), Some(from)) = (&mut layer.mask, &original.mask) {
                    mask.pixels = transformed(&from.pixels, &t, from.pixels.fill(), resampling);
                }
            }
            Some(Lift::Pixels(lifted)) => layer.pixels = lifted.drop_transformed(&t, resampling),
            Some(Lift::Mask(lifted)) => {
                if let Some(mask) = &mut layer.mask {
                    mask.pixels = lifted.drop_transformed(&t, resampling);
                }
            }
        }
        if let Some(sel) = &transforming.selection {
            self.doc.selection = Some(sel.transformed(&t)).filter(|s| !s.is_empty());
        }
        self.changed();
    }

    /// Magic Wand: click to select similar colours, combining with the
    /// current selection using `how`.
    pub fn magic_wand(
        &mut self,
        start: (u32, u32),
        tolerance: u16,
        contiguous: bool,
        anti_alias: bool,
        sample: Sample,
        how: Combine,
    ) -> bool {
        let (w, h) = (self.doc.width, self.doc.height);
        if start.0 >= w || start.1 >= h {
            if how == Combine::Replace && self.doc.selection.is_some() {
                return self.edit("Deselect", |doc, _| doc.selection = None);
            }
            return false;
        }

        let selection = if sample != Sample::All && self.target == Target::Mask {
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
        } else if sample == Sample::Current
            && !self.doc.layer(self.active).is_some_and(|l| l.has_pixels())
        {
            Selection::from_coverage(Tiled::new(w, h, 0))
        } else if let Some(selection) = (sample == Sample::All)
            .then(|| self.canvas.render())
            .flatten()
            .and_then(|r| {
                // All layers are already flattened on screen.
                r.with_image(|img| Selection::magic_wand_raster(img, start, tolerance, contiguous, anti_alias))
            })
        {
            selection
        } else {
            let source = self.sample_source(sample);
            Selection::magic_wand_tiled(&source, start, tolerance, contiguous, anti_alias)
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

    /// Combine a new selection with the current one (a marquee, lasso or
    /// Load Selection), as one undo step. Also shows the selection edges
    /// again if Ctrl+H hid them.
    /// Load a channel of the finished image as a selection, combined `how`.
    pub fn load_channel(&mut self, channel: Channel, how: Combine) {
        let composite = self.doc.composite();
        self.set_selection("Load Selection", Selection::from_channel(&composite, channel), how);
    }

    pub fn set_selection(&mut self, label: &str, selection: Selection, how: Combine) {
        if self.edit(label, |doc, _| {
            let combined = match (&doc.selection, how) {
                (Some(current), how) if how != Combine::Replace => {
                    current.combine(&selection, how)
                }
                (None, Combine::Subtract | Combine::Intersect) => return,
                _ => selection,
            };
            doc.selection = (!combined.is_empty()).then_some(combined);
        }) {
            self.hide_selection_edges = false;
        }
    }

    /// Show a move (or with `moving` false, an edit) of the active layer
    /// live on the GPU, if it can be: the stack is made in the background.
    fn start_live_view(&self, moving: bool) -> Option<LiveView> {
        let limit = self.live_limit?;
        let (level, (x0, y0, x1, y1)) = self.canvas.visible_area()?;
        if self.view != View::Image || !live::can_show(&self.doc, self.active, moving) {
            return None;
        }
        let scale = 1u32 << level;
        if self.doc.width.div_ceil(scale) > limit || self.doc.height.div_ceil(scale) > limit {
            return None;
        }
        let region = (
            x0 / scale,
            y0 / scale,
            x1.div_ceil(scale) - x0 / scale,
            y1.div_ceil(scale) - y0 / scale,
        );
        let (doc, id, reduced) = (self.doc.clone(), self.active, Arc::clone(&self.reduced));
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let layers = if level == 0 {
                doc.layers.clone()
            } else {
                reduced.lock().expect("reduced layers").layers(&doc.layers, level as u32)
            };
            let _ = tx.send(live::build(&doc, &layers, id, moving, level, region));
        });
        Some(LiveView {
            subject: self.active,
            rx: Some(rx),
            stack: None,
            offset: (0, 0),
            transform: None,
            edit: (!moving).then_some(None),
            until: None,
        })
    }

    /// Put the moving layer (or its selected part) at the offset asked for.
    fn apply_move(&mut self) {
        let Some(moving) = &self.moving else {
            return;
        };
        let (dx, dy) = moving.wanted;
        // Shown live, the move is made only when it ends. Zoomed out, it
        // goes in whole pixels of the level on screen, so the move made
        // lands exactly where the live view showed it.
        let live = match &mut self.live_view {
            Some(live) if live.until.is_none() => {
                let k = live.stack.as_ref().map_or(1, |s| 1i32 << s.level);
                let snap = |d: i32| (d as f32 / k as f32).round() as i32 * k;
                live.offset = (snap(dx), snap(dy));
                true
            }
            _ => false,
        };
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
        if live {
            return;
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
        // Selecting another layer or targeting a layer mask leaves Quick Mask.
        if self.view == View::QuickMask
            && self.previewing.is_none()
            && (self.target != Target::QuickMask || self.quick_mask_layer != Some(self.active))
        {
            self.exit_quick_mask();
        }
        if let View::Filter { layer, .. } = self.view
            && (self.doc.layer(layer).is_none() || layer != self.active)
        {
            self.set_view(View::Image);
        }
        if let View::AddNoise { layer, .. } = self.view
            && (self.doc.layer(layer).is_none() || layer != self.active)
        {
            self.set_view(View::Image);
        }
        if let View::Alpha(id) = self.view
            && self.doc.channel(id).is_none()
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
        // Catch up with a move or transform dragged further while rendering.
        if self.rendering.as_ref().is_none_or(|r| r.visible) {
            self.apply_move();
            self.apply_transform(Resampling::Bilinear);
        }
        self.update_live_view();
        // A live transform whose view went (zoomed or scrolled away) is
        // shown live again from where it's on screen now.
        if self
            .transforming
            .as_ref()
            .is_some_and(|t| t.lift.is_none() && !t.started)
            && self.live_view.is_none()
        {
            self.show_transform_live();
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
        // A live edit on screen renders on the CPU once it ends.
        let editing_live = self
            .live_view
            .as_ref()
            .is_some_and(|v| v.edit.is_some() && v.stack.is_some() && v.until.is_none());
        if self.rendering.is_none() && self.rendered != current && self.stroke.is_none() && !editing_live {
            self.start_render(ctx);
        }
    }

    /// Pick up a live move's stack once made, drop it once the move is on
    /// screen, and show it on the canvas meanwhile. If it can't be shown
    /// (or the view changes under it), the move goes on on the CPU.
    fn update_live_view(&mut self) {
        let Some(live) = &mut self.live_view else {
            self.canvas.live = None;
            return;
        };
        if let Some(rx) = &live.rx {
            match rx.try_recv() {
                Ok(stack) => {
                    live.rx = None;
                    live.stack = stack.map(Arc::new);
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => live.rx = None,
            }
        }
        let area = self.canvas.visible_area();
        let (level, (x0, y0, x1, y1)) = area.unwrap_or((usize::MAX, (0, 0, 0, 0)));
        let covers = live.stack.as_ref().is_some_and(|s| {
            let (rx, ry, rw, rh) = s.region;
            let k = 1u32 << s.level;
            s.level == level && x0 / k >= rx && y0 / k >= ry && x1.div_ceil(k) <= rx + rw && y1.div_ceil(k) <= ry + rh
        });
        // Not made (or not in time for a gesture that's already over).
        let failed = live.rx.is_none() && !covers || live.until.is_some() && live.stack.is_none();
        // Once the move's render is on screen and the canvas's tiles show it.
        let caught_up = live.until.is_some_and(|until| {
            let rendered = self.rendered.0 >= until
                || self
                    .rendering
                    .as_ref()
                    .is_some_and(|r| r.visible && r.target.0 >= until);
            rendered && self.canvas.fresh
        });
        if failed || caught_up {
            self.live_view = None;
            self.canvas.live = None;
            return;
        }
        // An edit's settings as they are now; its adjustment's table is
        // made again only when the adjustment changes.
        let layer = self.doc.layer(live.subject);
        let settings = match (&mut live.edit, layer) {
            (Some(cached), Some(layer)) => {
                let lut = layer.adjustment.as_ref().map(|a| match cached {
                    Some((made_from, table)) if made_from == a => Arc::clone(table),
                    _ => {
                        let table = Arc::new(a.prepare().lut(live::LUT_SIZE));
                        *cached = Some((a.clone(), Arc::clone(&table)));
                        table
                    }
                });
                Some(Settings {
                    opacity: layer.opacity,
                    mode: layer.blend,
                    lut,
                })
            }
            _ => None,
        };
        self.canvas.live = live.stack.as_ref().map(|s| LiveFrame {
            stack: Arc::clone(s),
            offset: live.offset,
            transform: live.transform,
            settings,
        });
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
            .filter(|_| !view.whole());
        if let Some(render) = in_place {
            let job = InPlace {
                doc,
                view,
                colour,
                render: Arc::clone(render),
                visible: self.canvas.visible_area(),
                reduced: Arc::clone(&self.reduced),
                groups: Arc::clone(&self.groups),
                preview_groups: Arc::clone(&self.preview_groups),
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

    /// Enter Photoshop's Quick Mask mode (`Q`), painting the selection directly.
    pub fn enter_quick_mask(&mut self) {
        if self.view == View::QuickMask {
            return;
        }
        if self.doc.selection.is_none() {
            self.doc.selection = Some(Selection::all(self.doc.width, self.doc.height));
        }
        self.target = Target::QuickMask;
        self.quick_mask_layer = Some(self.active);
        self.set_view(View::QuickMask);
    }

    /// Leave Quick Mask mode, turning the painted coverage into a selection.
    pub fn exit_quick_mask(&mut self) {
        if self.view != View::QuickMask && self.target != Target::QuickMask {
            return;
        }
        self.set_view(View::Image);
        if self.target == Target::QuickMask {
            self.target = Target::Pixels;
        }
        self.quick_mask_layer = None;
        if let Some(sel) = self.doc.selection.take()
            && !sel.is_all()
            && !sel.is_empty()
        {
            self.doc.selection = Some(Selection::from_coverage(sel.coverage));
        }
        self.hide_selection_edges = false;
        self.changed();
    }

    /// Show `selection` over the image in Quick Mask's red, in place of the
    /// real one, until [`Editor::end_selection_preview`] (Select and Mask).
    pub fn preview_selection(&mut self, selection: Selection) {
        if self.previewing.is_none() {
            self.previewing = Some((self.doc.selection.clone(), self.view));
            self.set_view(View::QuickMask);
        }
        self.doc.selection = Some(selection);
        self.view_generation += 1;
    }

    /// Whether a refined selection is being previewed.
    pub fn previewing_selection(&self) -> bool {
        self.previewing.is_some()
    }

    /// Put back the real selection and the view after a preview.
    pub fn end_selection_preview(&mut self) {
        if let Some((selection, view)) = self.previewing.take() {
            self.doc.selection = selection;
            self.set_view(view);
            self.view_generation += 1;
        }
    }

    /// Toggle Quick Mask mode (`Q`).
    pub fn toggle_quick_mask(&mut self) {
        if self.view == View::QuickMask || self.target == Target::QuickMask {
            self.exit_quick_mask();
        } else {
            self.enter_quick_mask();
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

    /// The source image for clone/heal strokes, eyedropper sampling, and the magic wand.
    pub fn sample_source(&self, sample: Sample) -> Tiled<Pixel> {
        match sample {
            Sample::Current => self
                .doc
                .layer(self.active)
                .map(|l| l.pixels.clone())
                .unwrap_or_else(|| Tiled::new(self.doc.width, self.doc.height, [0, 0, 0, 0])),
            Sample::CurrentAndBelow => {
                let mut cache = self.sample_cache.lock().unwrap();
                if let Some((rev, act, tiled)) = &cache.current_and_below
                    && *rev == self.revision
                    && *act == self.active
                {
                    return tiled.as_ref().clone();
                }
                let mut doc = self.doc.clone();
                if let Some(idx) = doc.index_of(self.active) {
                    for l in &mut doc.layers[idx + 1..] {
                        if !self.doc.is_inside(self.active, l.id) {
                            l.visible = false;
                        }
                    }
                }
                let tiled = Arc::new(Tiled::from_raster(&doc.composite()));
                cache.current_and_below = Some((self.revision, self.active, Arc::clone(&tiled)));
                (*tiled).clone()
            }
            Sample::All => {
                let mut cache = self.sample_cache.lock().unwrap();
                if let Some((rev, tiled)) = &cache.all
                    && *rev == self.revision
                {
                    return tiled.as_ref().clone();
                }
                let tiled = Arc::new(Tiled::from_raster(&self.doc.composite()));
                cache.all = Some((self.revision, Arc::clone(&tiled)));
                (*tiled).clone()
            }
        }
    }

    /// The colour at (x, y) for the eyedropper: the average over `size`
    /// (ignoring pixels outside the image) of the chosen `sample` source.
    /// `None` outside the image.
    pub fn sample(&self, x: u32, y: u32, size: SampleSize, sample: Sample) -> Option<Pixel> {
        let (w, h) = (self.doc.width, self.doc.height);
        if x >= w || y >= h {
            return None;
        }
        if sample == Sample::Current && self.doc.layer(self.active).is_none() {
            return None;
        }
        let r = size.radius();
        // All layers are already flattened on screen; read that rather than
        // compositing again.
        let source = (sample != Sample::All).then(|| self.sample_source(sample));
        let at = |x, y| match &source {
            Some(source) => Some(source.get(x, y)),
            None => self.canvas.sample(x, y),
        };
        let (x0, x1) = (x.saturating_sub(r), (x + r).min(w - 1));
        let (y0, y1) = (y.saturating_sub(r), (y + r).min(h - 1));
        let (mut sum, mut count) = ([0u64; 4], 0u64);
        for p in (y0..=y1).flat_map(|y| (x0..=x1).filter_map(move |x| at(x, y))) {
            for c in 0..4 {
                sum[c] += u64::from(p[c]);
            }
            count += 1;
        }
        (count > 0).then(|| sum.map(|v| ((v + count / 2) / count) as u16))
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
        View::Alpha(id) => {
            let image = match doc.channel(id) {
                Some(c) => c.pixels.map(|v| [v, v, v, u16::MAX]).to_raster(),
                None => doc.composite(),
            };
            (Render::new(image), None)
        }
        View::Mask(_) | View::MaskOverlay(_) | View::QuickMask | View::Channel(_) => {
            let mut image = Raster::new(
                doc.width,
                doc.height,
                vec![[0; 4]; doc.width as usize * doc.height as usize],
            );
            for tiles in all_tiles().chunks(BATCH) {
                let data = draw_tiles(&doc.layers, view, colour, tiles, None, doc.selection.as_ref());
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
        View::Filter {
            layer,
            filter,
            mask,
        } => {
            let mut layers = doc.layers.clone();
            let target = layers.iter_mut().find(|l| l.id == layer);
            match (target, mask) {
                (Some(target), false) => target.pixels = ops::filtered(doc, layer, &filter).unwrap(),
                (Some(target), true) => {
                    if let (Some(m), Some(filtered)) = (&mut target.mask, ops::filtered_mask(doc, layer, &filter)) {
                        m.pixels = filtered;
                    }
                }
                (None, _) => {}
            }
            let image = composite::composite(&layers, doc.width, doc.height);
            (Render::new(image), None)
        }
        View::AddNoise { layer, options } => {
            let mut preview_doc = doc.clone();
            let base_tiled = match base {
                Some(b) => b,
                None => Arc::new(Tiled::from_raster(&doc.composite())),
            };
            let grain = ops::grain_layer(0, doc.width, doc.height, &options, Some(&base_tiled));
            let index = doc
                .index_of(layer)
                .unwrap_or(preview_doc.layers.len().saturating_sub(1));
            preview_doc.insert_above(index, grain);
            let image =
                composite::composite(&preview_doc.layers, preview_doc.width, preview_doc.height);
            (Render::new(image), Some(base_tiled))
        }
    }
}

/// Tint tiles with `colour` where `coverage` is less than MAX (masked / unselected),
/// fading to clear where revealed / selected.
fn apply_overlay(
    out: &mut [Vec<Pixel>],
    tiles: &[(u32, u32)],
    coverage: &Tiled<u16>,
    colour: Pixel,
) {
    const MAX: u32 = u16::MAX as u32;
    for (tile, &(col, row)) in out.iter_mut().zip(tiles) {
        let values = coverage.tile(col, row);
        for (i, pixel) in tile.iter_mut().enumerate() {
            let v = values.map_or(coverage.fill(), |t| t[i]);
            let a = (MAX - v as u32) * OVERLAY_OPACITY / MAX;
            // Opaque colour over the pixel, so it shows on
            // transparent areas too.
            for (c, k) in pixel.iter_mut().zip(colour) {
                *c = ((*c as u32 * (MAX - a) + k as u32 * a) / MAX) as u16;
            }
        }
    }
}

/// Draw 256 px tiles (col, row) of what `view` shows (anything but a
/// separation preview), with `colour` for the mask overlay, reusing and
/// keeping what isolated groups composite to in `groups`.
fn draw_tiles(
    layers: &[Layer],
    view: View,
    colour: Pixel,
    tiles: &[(u32, u32)],
    groups: Option<&GroupCache>,
    selection: Option<&Selection>,
) -> Vec<Vec<Pixel>> {
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
            let mut out = composite::composite_tiles(layers, tiles, groups);
            if let Some(mask) = mask(id) {
                apply_overlay(&mut out, tiles, &mask.pixels, colour);
            }
            out
        }
        View::QuickMask => {
            let mut out = composite::composite_tiles(layers, tiles, groups);
            if let Some(sel) = selection {
                apply_overlay(&mut out, tiles, &sel.coverage, colour);
            }
            out
        }
        View::Channel(channel) => {
            let mut out = composite::composite_tiles(layers, tiles, groups);
            for p in out.iter_mut().flatten() {
                let v = channel.value(*p);
                *p = [v, v, v, u16::MAX];
            }
            out
        }
        // The rest are drawn whole by `render_view`.
        View::Image
        | View::Separation { .. }
        | View::Filter { .. }
        | View::AddNoise { .. }
        | View::Alpha(_) => composite::composite_tiles(layers, tiles, groups),
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
    groups: Arc<GroupCache>,
    preview_groups: Arc<GroupCache>,
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
        if let Some((level, (x0, y0, x1, y1))) = self
            .visible
            .filter(|(level, _)| *level > 0 && self.view != View::QuickMask)
        {
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
            let groups = if level == 0 {
                &self.groups
            } else {
                &self.preview_groups
            };
            let data = draw_tiles(
                layers,
                self.view,
                self.colour,
                batch,
                Some(groups),
                self.doc.selection.as_ref(),
            );
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
            ..Default::default()
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
        assert!(e.begin_stroke(hard(40.0), Paint::Mask(0), Sample::Current));
        e.stroke_to(100.0, 100.0, 1.0);
        e.stroke_to(300.0, 100.0, 1.0);
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
        assert!(e.begin_stroke(hard(40.0), Paint::Color([0, 0, 0, 65535]), Sample::Current));
        e.stroke_to(100.0, 100.0, 1.0);
        e.undo();
        // Later moves of the abandoned stroke change nothing.
        e.stroke_to(300.0, 100.0, 1.0);
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
            View::Filter {
                layer: e.active,
                filter: omapix_engine::filters::LayerFilter::GaussianBlur { radius: 5.0 },
                mask: false,
            },
            RED,
            None,
        );
        // The dot was spread out: pixel at (100, 100) is no longer pure black.
        assert!(blurred.sample_for_test(100, 100)[0] > 0);

        // Add Noise preview adds grain on top.
        let (noise_render, _) = render_view(
            &e.doc,
            View::AddNoise {
                layer: e.active,
                options: omapix_engine::NoiseOptions {
                    amount: 50.0,
                    ..Default::default()
                },
            },
            RED,
            None,
        );
        let sample = noise_render.sample_for_test(300, 200);
        assert_ne!(sample, [30000, 30000, 30000, 65535]);
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
        assert!(e.begin_stroke(hard(40.0), Paint::Mask(0), Sample::Current));
        e.stroke_to(300.0, 300.0, 1.0);
        e.stroke_to(500.0, 300.0, 1.0);
        e.end_stroke();
        assert_eq!(e.rendered, (e.revision, e.view_generation));
        let (full, _) = render_view(&e.doc, e.view, e.overlay_colour, None);
        for (x, y) in [(400, 300), (400, 380), (100, 100)] {
            assert_eq!(e.canvas.sample(x, y), Some(full.sample_for_test(x, y)));
        }
        assert_ne!(e.canvas.sample(400, 300), e.canvas.sample(400, 380));
    }

    #[test]
    fn channel_views_show_one_channel_or_a_saved_selection_in_grey() {
        let ctx = egui::Context::default();
        let mut e = editor();
        let image = Raster::new(600, 400, vec![[10000, 20000, 40000, 65535]; 600 * 400]);
        e.edit("Fill", |doc, _| doc.layers[0].pixels = Tiled::from_raster(&image));
        let (render, _) = render_view(&e.doc, View::Channel(Channel::Green), RED, None);
        assert_eq!(render.sample_for_test(5, 5), [20000, 20000, 20000, 65535]);

        // Painting red while viewing Red updates the grey in place.
        e.set_view(View::Channel(Channel::Red));
        let (render, _) = render_view(&e.doc, e.view, e.overlay_colour, None);
        e.canvas.set_render(Arc::new(render));
        e.rendered = (e.revision, e.view_generation);
        assert!(e.begin_stroke(hard(40.0), Paint::Color([65535, 0, 0, 65535]), Sample::Current));
        e.stroke_to(300.0, 200.0, 1.0);
        e.end_stroke();
        assert_eq!(e.canvas.sample(300, 200), Some([65535, 65535, 65535, 65535]));
        assert_eq!(e.canvas.sample(10, 10), Some([10000, 10000, 10000, 65535]));

        e.edit("Save Selection", |doc, _| {
            doc.selection = Some(Selection::rectangle(600, 400, (0.0, 0.0), (10.0, 10.0)));
            doc.save_selection();
        });
        let id = e.doc.channels[0].id;
        let (render, _) = render_view(&e.doc, View::Alpha(id), RED, None);
        assert_eq!(render.sample_for_test(5, 5), [65535; 4]);
        assert_eq!(render.sample_for_test(50, 5), [0, 0, 0, 65535]);
        // Undoing the save leaves the channel's view.
        e.set_view(View::Alpha(id));
        e.update(&ctx);
        assert_eq!(e.view(), View::Alpha(id));
        e.undo();
        e.update(&ctx);
        assert_eq!(e.view(), View::Image);
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

    #[test]
    fn quick_mask_overlay_painting_and_undo() {
        let mut e = editor();
        e.doc.selection = Some(Selection::rectangle(600, 400, (200.0, 200.0), (400.0, 400.0)));
        e.enter_quick_mask();
        assert_eq!(e.view(), View::QuickMask);
        assert_eq!(e.target, Target::QuickMask);
        assert!(e.begin_transform(0).is_err());

        // Outside rectangle (unselected) shows red overlay; inside (selected) shows clear image.
        let (render, _) = render_view(&e.doc, View::QuickMask, RED, None);
        let near = |a: [u16; 4], b: [u16; 4]| a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 2);
        let outside = render.sample_for_test(100, 100);
        assert!(near(outside, [47767, 15000, 15000, 65535]), "{outside:?}");
        assert_eq!(render.sample_for_test(300, 300), [30000, 30000, 30000, 65535]);

        // White stroke outside adds to the selection (clearing red overlay).
        assert!(e.begin_stroke(hard(40.0), Paint::Mask(u16::MAX), Sample::Current));
        e.stroke_to(100.0, 100.0, 1.0);
        e.end_stroke();
        assert_eq!(e.doc.selection.as_ref().unwrap().coverage.get(100, 100), u16::MAX);

        // Black stroke inside removes from the selection (adding red overlay).
        assert!(e.begin_stroke(hard(40.0), Paint::Mask(0), Sample::Current));
        e.stroke_to(300.0, 300.0, 1.0);
        e.end_stroke();
        assert_eq!(e.doc.selection.as_ref().unwrap().coverage.get(300, 300), 0);

        // Undo the black stroke restores the selection inside.
        e.undo();
        assert_eq!(e.doc.selection.as_ref().unwrap().coverage.get(300, 300), u16::MAX);

        // Re-paint black stroke inside to test leaving with combined selection.
        assert!(e.begin_stroke(hard(40.0), Paint::Mask(0), Sample::Current));
        e.stroke_to(300.0, 300.0, 1.0);
        e.end_stroke();
        assert_eq!(e.doc.selection.as_ref().unwrap().coverage.get(300, 300), 0);

        // Leaving Quick Mask gives the combined selection.
        e.exit_quick_mask();
        assert_eq!(e.view(), View::Image);
        assert_eq!(e.target, Target::Pixels);
        let sel = e.doc.selection.as_ref().expect("selection present");
        assert_eq!(sel.coverage.get(100, 100), u16::MAX); // added
        assert_eq!(sel.coverage.get(300, 300), 0);        // removed
        assert_eq!(sel.coverage.get(250, 250), u16::MAX); // retained from original rect
        assert_eq!(sel.coverage.get(50, 50), 0);          // retained unselected
        assert!(!sel.outlines.is_empty());
    }

    #[test]
    fn quick_mask_empty_entry_and_exit_gives_no_selection() {
        let mut e = editor();
        assert!(e.doc.selection.is_none());
        e.enter_quick_mask();
        assert_eq!(e.view(), View::QuickMask);
        assert_eq!(e.target, Target::QuickMask);

        // With no initial selection, everything counts as selected (nothing is red).
        let (render, _) = render_view(&e.doc, View::QuickMask, RED, None);
        assert_eq!(render.sample_for_test(100, 100), [30000, 30000, 30000, 65535]);
        assert_eq!(render.sample_for_test(300, 300), [30000, 30000, 30000, 65535]);

        // Leaving without painting drops the all-white coverage, giving no selection.
        e.exit_quick_mask();
        assert_eq!(e.view(), View::Image);
        assert_eq!(e.target, Target::Pixels);
        assert!(e.doc.selection.is_none());
    }

    #[test]
    fn quick_mask_leaves_when_layer_changes_or_mask_targeted() {
        let ctx = egui::Context::default();
        let mut e = masked_editor();
        let top = e.active;
        e.enter_quick_mask();
        assert_eq!(e.view(), View::QuickMask);
        assert_eq!(e.target, Target::QuickMask);

        // Selecting another layer leaves Quick Mask.
        e.active = e.doc.layers[0].id;
        e.update(&ctx);
        assert_eq!(e.view(), View::Image);
        assert_ne!(e.target, Target::QuickMask);

        // Targeting a mask leaves Quick Mask.
        e.active = top;
        e.enter_quick_mask();
        assert_eq!(e.view(), View::QuickMask);
        e.target = Target::Mask;
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
    fn free_transform_scales_the_layer_and_mask_as_one_undo_step() {
        let mut e = square_editor();
        e.begin_transform(0).unwrap();
        // The box fits the red square.
        assert_eq!(e.transform().unwrap().0, [0.0, 0.0, 100.0, 100.0]);
        e.transform_to(Affine::scale_about(1.5, 1.5, (0.0, 0.0)));
        e.transform_to(Affine::scale_about(2.0, 2.0, (0.0, 0.0)));
        e.commit_transform();
        assert!(e.transform().is_none());
        let layer = e.doc.layer(e.active).unwrap();
        assert_eq!(layer.pixels.get(190, 190), RED);
        assert_eq!(layer.pixels.get(210, 100)[3], 0);
        let mask = &layer.mask.as_ref().unwrap().pixels;
        assert_eq!((mask.get(90, 90), mask.get(110, 110)), (0, u16::MAX));
        assert_eq!(e.undo_label(), Some("Free Transform"));
        e.undo();
        assert_eq!(e.doc.layer(e.active).unwrap().pixels.get(150, 150)[3], 0);
        assert_eq!(e.undo_label(), Some("Setup"));
    }

    #[test]
    fn cancelling_free_transform_leaves_no_trace() {
        let mut e = square_editor();
        let before = e.doc.layer(e.active).unwrap().pixels.clone();
        e.begin_transform(0).unwrap();
        e.transform_to(Affine::rotate_about(0.3, (50.0, 50.0)));
        e.cancel_transform();
        assert!(e.doc.layer(e.active).unwrap().pixels.same_tiles(&before));
        assert_eq!(e.undo_label(), Some("Setup"));
        assert_eq!(e.redo_label(), None);
        // Nor does applying it unchanged.
        e.begin_transform(0).unwrap();
        e.commit_transform();
        assert_eq!(e.undo_label(), Some("Setup"));
    }

    #[test]
    fn free_transform_with_a_selection_transforms_just_the_selected_pixels() {
        let mut e = square_editor();
        e.edit("Marquee", |doc, _| {
            doc.selection = Some(Selection::rectangle(600, 400, (0.0, 0.0), (50.0, 100.0)))
        });
        e.begin_transform(0).unwrap();
        assert_eq!(e.transform().unwrap().0, [0.0, 0.0, 50.0, 100.0]);
        // Twice as wide, moved right by 200.
        let t = Affine::scale_about(2.0, 1.0, (0.0, 0.0)).then(&Affine::translate(200.0, 0.0));
        e.transform_to(t);
        e.commit_transform();
        let layer = e.doc.layer(e.active).unwrap();
        assert_eq!(layer.pixels.get(25, 50)[3], 0);
        assert_eq!(layer.pixels.get(75, 50), RED);
        assert_eq!(layer.pixels.get(290, 50), RED);
        let sel = e.doc.selection.as_ref().unwrap();
        assert_eq!((sel.at(25, 50), sel.at(290, 50)), (0.0, 1.0));
        // A layer with nothing on it can't be transformed.
        e.edit("Clear", |doc, active| {
            doc.selection = None;
            doc.layer_mut(*active).unwrap().pixels = Tiled::new(600, 400, [0; 4]);
        });
        assert!(e.begin_transform(0).is_err());
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
        assert!(e.begin_stroke(hard(60.0), Paint::Color([0, 0, 65535, 65535]), Sample::Current));
        e.stroke_to(250.0, 250.0, 1.0);
        e.stroke_to(290.0, 270.0, 1.0);
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
            groups: Arc::default(),
            preview_groups: Arc::default(),
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
        assert!(!e.begin_stroke(hard(40.0), Paint::Color(RED), Sample::Current));

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
        assert!(e.magic_wand((75, 75), 0, true, false, Sample::Current, Combine::Replace));
        assert!(e.doc.selection.is_some());
        let sel = e.doc.selection.as_ref().unwrap();
        assert_eq!(sel.at(75, 75), 1.0);
        assert_eq!(sel.at(20, 20), 1.0);
        assert_eq!(sel.at(150, 150), 0.0);

        // Click outside image deselects
        assert!(e.magic_wand((700, 700), 0, true, false, Sample::Current, Combine::Replace));
        assert!(e.doc.selection.is_none());

        // Undo brings selection back
        e.undo();
        assert!(e.doc.selection.is_some());
    }

    #[test]
    fn sample_averaging_and_image_edges() {
        let (w, h) = (10, 10);
        let mut pixels = Vec::with_capacity((w * h) as usize);
        for y in 0..h {
            for x in 0..w {
                pixels.push([(x * 1000) as u16, (y * 1000) as u16, 0, 65535]);
            }
        }
        let image = Raster::new(w, h, pixels);
        let doc = Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16);
        let mut e = Editor::new(doc).unwrap();
        let render = Arc::new(Render::new(e.doc.composite()));
        e.canvas.set_render(Arc::clone(&render));

        // 1. Point sample at (5, 5) returns exact pixel [5000, 5000, 0, 65535]
        let point = e.sample(5, 5, SampleSize::Point, Sample::Current).unwrap();
        assert_eq!(point, [5000, 5000, 0, 65535]);

        let point_all = e.sample(5, 5, SampleSize::Point, Sample::All).unwrap();
        assert_eq!(point_all, [5000, 5000, 0, 65535]);

        // 2. 3x3 sample centered at (5, 5): interior averages to 5000
        let avg_3x3 = e.sample(5, 5, SampleSize::ThreeByThree, Sample::Current).unwrap();
        assert_eq!(avg_3x3, [5000, 5000, 0, 65535]);

        // 3. Top-left corner (0, 0) with 3x3:
        // Window extends to x: -1..=1, y: -1..=1. Out-of-bounds pixels are ignored.
        // Valid: (0,0), (1,0), (0,1), (1,1). Count = 4.
        // x values: 0, 1000, 0, 1000 -> avg = 500
        // y values: 0, 0, 1000, 1000 -> avg = 500
        let corner_3x3 = e.sample(0, 0, SampleSize::ThreeByThree, Sample::Current).unwrap();
        assert_eq!(corner_3x3, [500, 500, 0, 65535]);

        // 4. Bottom-right corner (9, 9) with 3x3:
        // Valid: (8,8), (9,8), (8,9), (9,9).
        // x values: 8000, 9000, 8000, 9000 -> avg = 8500
        // y values: 8000, 8000, 9000, 9000 -> avg = 8500
        let corner_br = e.sample(9, 9, SampleSize::ThreeByThree, Sample::Current).unwrap();
        assert_eq!(corner_br, [8500, 8500, 0, 65535]);

        // 5. Out of bounds returns None
        assert!(e.sample(10, 10, SampleSize::Point, Sample::Current).is_none());
        assert!(e.sample(15, 0, SampleSize::ThreeByThree, Sample::Current).is_none());

        // 6. Current layer vs Current & Below vs All layers
        e.edit("Add layer", |doc, active| {
            let mut top = Layer::empty(2, "top", w, h);
            for y in 0..h {
                for x in 0..w {
                    top.pixels.tile_mut(0, 0)[(y * 256 + x) as usize] = [65535, 65535, 0, 65535];
                }
            }
            doc.layers.push(top);
            *active = 2;
        });
        // All Layers reads the image on screen, as rendered after an edit.
        e.canvas.set_render(Arc::new(Render::new(e.doc.composite())));
        // With top layer active:
        assert_eq!(
            e.sample(0, 0, SampleSize::Point, Sample::Current),
            Some([65535, 65535, 0, 65535])
        );
        assert_eq!(
            e.sample(0, 0, SampleSize::Point, Sample::CurrentAndBelow),
            Some([65535, 65535, 0, 65535])
        );
        assert_eq!(
            e.sample(0, 0, SampleSize::Point, Sample::All),
            Some([65535, 65535, 0, 65535])
        );

        // With bottom layer active:
        e.select_layers(1, Vec::new());
        assert_eq!(
            e.sample(0, 0, SampleSize::Point, Sample::Current),
            Some([0, 0, 0, 65535])
        );
        assert_eq!(
            e.sample(0, 0, SampleSize::Point, Sample::CurrentAndBelow),
            Some([0, 0, 0, 65535])
        );
        assert_eq!(
            e.sample(0, 0, SampleSize::Point, Sample::All),
            Some([65535, 65535, 0, 65535])
        );
    }

    #[test]
    fn sample_source_modes_respect_layers_above_and_groups() {
        let (w, h) = (10, 10);
        let red: Pixel = [60000, 0, 0, 65535];
        let green: Pixel = [0, 60000, 0, 65535];
        let blue: Pixel = [0, 0, 60000, 65535];
        let white: Pixel = [60000, 60000, 60000, 65535];
        let transparent: Pixel = [0, 0, 0, 0];

        // Layer 1: Background (Red everywhere)
        let bg_image = Raster::new(w, h, vec![red; (w * h) as usize]);
        let mut doc = Document::from_image("t.tif".into(), &bg_image, ColorProfile::srgb(), 16);

        // Group 10 containing Layer 2 and Layer 3:
        // Flat stack order (bottom-first):
        // Layer 1 (Background)
        // Layer 2 (Active, Green on left x < 5, transparent on right x >= 5, parent: Some(10))
        // Layer 3 (Above active in group, Blue everywhere, parent: Some(10))
        // Layer 10 (Group 1, is_group: true, parent: None)
        // Layer 20 (Above group, White on right x >= 5, transparent on left x < 5, parent: None)

        let mut active_layer = Layer::empty(2, "Active", w, h);
        active_layer.parent = Some(10);
        for y in 0..h {
            for x in 0..5 {
                active_layer.pixels.tile_mut(0, 0)[(y * 256 + x) as usize] = green;
            }
        }
        doc.layers.push(active_layer);

        let mut above_in_group = Layer::empty(3, "Above In Group", w, h);
        above_in_group.parent = Some(10);
        for y in 0..h {
            for x in 0..w {
                above_in_group.pixels.tile_mut(0, 0)[(y * 256 + x) as usize] = blue;
            }
        }
        doc.layers.push(above_in_group);

        let mut group = Layer::empty(10, "Group 1", w, h);
        group.is_group = true;
        doc.layers.push(group);

        let mut top_layer = Layer::empty(20, "Top", w, h);
        for y in 0..h {
            for x in 5..w {
                top_layer.pixels.tile_mut(0, 0)[(y * 256 + x) as usize] = white;
            }
        }
        doc.layers.push(top_layer);

        let mut e = Editor::new(doc).unwrap();
        e.select_layers(2, Vec::new());
        assert_eq!(e.active, 2);

        // Current mode: only the active layer's pixels
        let src_current = e.sample_source(Sample::Current);
        assert_eq!(src_current.get(2, 2), green);
        assert_eq!(src_current.get(7, 2), transparent);

        // Current & Below:
        // - Layers above (Layer 3 inside group and Layer 20 above group) are hidden
        // - Group 10 containing active layer (row after active layer) is kept visible
        // - Layer 1 below active layer shows through where active layer is transparent
        let src_below = e.sample_source(Sample::CurrentAndBelow);
        assert_eq!(src_below.get(2, 2), green, "active layer on left");
        assert_eq!(src_below.get(7, 2), red, "layer below shows where active is transparent");

        // All mode: includes all layers above (Layer 3 in group and Layer 20 above group)
        let src_all = e.sample_source(Sample::All);
        assert_eq!(src_all.get(2, 2), blue, "layer above inside group covers left");
        assert_eq!(src_all.get(7, 2), white, "top layer covers right");
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;
    use omapix_engine::{ColorProfile, Raster};

    /// A 600 × 400 grey image and a red patch layer above it, laid out on
    /// screen as if a GPU could show moves live.
    fn editor() -> Editor {
        let (w, h) = (600, 400);
        let image = Raster::new(w, h, vec![[30000, 30000, 30000, 65535]; (w * h) as usize]);
        let mut doc = Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16);
        let id = doc.next_layer_id();
        let mut patch = Layer::empty(id, "Patch", w, h);
        patch.pixels.tile_mut(0, 0)[0] = [65535, 0, 0, 65535];
        doc.layers.push(patch);
        let mut e = Editor::new(doc).unwrap();
        e.live_limit = Some(16384);
        e.canvas.lay_out_for_test(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 400.0)), 1.0);
        e
    }

    fn settle(e: &mut Editor, ctx: &egui::Context) {
        for _ in 0..500 {
            e.update(ctx);
            if e.live_view.as_ref().is_none_or(|l| l.rx.is_none()) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    #[test]
    fn a_live_view_is_made_once_when_it_ends() {
        let ctx = egui::Context::default();
        let mut e = editor();
        let patch = e.active;
        assert!(e.begin_move("Move", false, 0));
        e.move_to(10, 5);
        settle(&mut e, &ctx);
        e.move_to(20, 7);
        e.update(&ctx);
        // Shown live: the layer hasn't moved yet, the canvas shows it moved.
        assert_eq!(e.doc.layer(patch).unwrap().pixels.get(0, 0), [65535, 0, 0, 65535]);
        let LiveFrame { stack, offset, .. } = e.canvas.live.clone().expect("shown live");
        assert_eq!((stack.level, offset), (0, (20, 7)));
        e.end_move();
        // Made for real, as one undo step, while the live view stays up
        // until the canvas has drawn it.
        let layer = e.doc.layer(patch).unwrap();
        assert_eq!(layer.pixels.get(20, 7), [65535, 0, 0, 65535]);
        assert_eq!(layer.pixels.get(0, 0)[3], 0);
        assert_eq!(e.undo_label(), Some("Move"));
        e.update(&ctx);
        assert!(e.canvas.live.is_some());
        e.undo();
        assert_eq!(e.doc.layer(patch).unwrap().pixels.get(0, 0), [65535, 0, 0, 65535]);
    }

    #[test]
    fn free_transform_shows_live_and_is_made_once_applied() {
        let ctx = egui::Context::default();
        let mut e = editor();
        let patch = e.active;
        e.begin_transform(0).unwrap();
        settle(&mut e, &ctx);
        let t = Affine::scale_about(4.0, 4.0, (0.0, 0.0));
        e.transform_to(Affine::scale_about(2.0, 2.0, (0.0, 0.0)));
        e.transform_to(t);
        e.update(&ctx);
        // Shown live: the layer hasn't changed, the canvas shows it transformed.
        assert_eq!(e.doc.layer(patch).unwrap().pixels.get(2, 2)[3], 0);
        assert_eq!(e.undo_label(), None);
        let frame = e.canvas.live.clone().expect("shown live");
        assert_eq!(frame.transform, Some(t));
        e.commit_transform();
        // Made for real, as one undo step, while the live view stays up
        // until the canvas has drawn it.
        assert!(e.doc.layer(patch).unwrap().pixels.get(2, 2)[3] > 0);
        assert_eq!(e.undo_label(), Some("Free Transform"));
        e.update(&ctx);
        assert!(e.canvas.live.is_some());
    }

    #[test]
    fn cancelling_a_live_free_transform_changes_nothing() {
        let ctx = egui::Context::default();
        let mut e = editor();
        let before = e.doc.layer(e.active).unwrap().pixels.clone();
        e.begin_transform(0).unwrap();
        settle(&mut e, &ctx);
        e.transform_to(Affine::rotate_about(0.5, (0.0, 0.0)));
        e.update(&ctx);
        e.cancel_transform();
        e.update(&ctx);
        assert!(e.canvas.live.is_none());
        assert!(e.doc.layer(e.active).unwrap().pixels.same_tiles(&before));
        assert_eq!(e.undo_label(), None);
    }

    #[test]
    fn a_live_free_transform_follows_the_zoom() {
        let ctx = egui::Context::default();
        let mut e = editor();
        e.begin_transform(0).unwrap();
        settle(&mut e, &ctx);
        let t = Affine::translate(3.5, 1.0);
        e.transform_to(t);
        // Zoomed out to 50 %, it's shown live again from level 1.
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(300.0, 200.0));
        e.canvas.lay_out_for_test(rect, 0.5);
        e.update(&ctx);
        settle(&mut e, &ctx);
        e.update(&ctx);
        let frame = e.canvas.live.clone().expect("shown live");
        assert_eq!((frame.stack.level, frame.transform), (1, Some(t)));
        assert_eq!(e.undo_label(), None);
        // With a selection, it's transformed on the CPU as before.
        e.cancel_transform();
        e.edit("Marquee", |doc, _| doc.selection = Some(Selection::rectangle(600, 400, (0.0, 0.0), (8.0, 8.0))));
        e.begin_transform(0).unwrap();
        assert!(e.live_view.is_none());
    }

    #[test]
    fn the_live_view_waits_for_the_canvas_to_draw_the_move() {
        let ctx = egui::Context::default();
        let mut e = editor();
        assert!(e.begin_move("Move", false, 0));
        e.move_to(10, 5);
        settle(&mut e, &ctx);
        e.end_move();
        // The canvas's tiles were fresh before the move's render changed
        // them; that mustn't count.
        e.canvas.fresh = true;
        e.canvas.invalidate_tiles(0, &[(0, 0)]);
        assert!(!e.canvas.fresh);
        e.update(&ctx);
        assert!(e.canvas.live.is_some(), "still shown live");
    }

    #[test]
    fn zoomed_out_live_views_land_on_the_levels_pixels() {
        let ctx = egui::Context::default();
        let mut e = editor();
        let patch = e.active;
        // At 50 %, level 1 is shown: 2 image pixels to a screen pixel.
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(300.0, 200.0));
        e.canvas.lay_out_for_test(rect, 0.5);
        assert!(e.begin_move("Move", false, 0));
        e.move_to(1, 1);
        settle(&mut e, &ctx);
        e.move_to(5, 3);
        e.update(&ctx);
        let LiveFrame { stack, offset, .. } = e.canvas.live.clone().expect("shown live");
        assert_eq!((stack.level, offset), (1, (6, 4)));
        e.end_move();
        assert_eq!(e.doc.layer(patch).unwrap().pixels.get(6, 4), [65535, 0, 0, 65535]);
    }

    #[test]
    fn slider_drags_on_a_layers_settings_show_live_then_render_once() {
        let ctx = egui::Context::default();
        let mut e = editor();
        let patch = e.active;
        e.update(&ctx);
        let opacity = |e: &mut Editor, v: f32| e.edit_live("Opacity", |doc| doc.layers[1].opacity = v);
        opacity(&mut e, 0.8);
        settle(&mut e, &ctx);
        opacity(&mut e, 0.5);
        e.update(&ctx);
        let frame = e.canvas.live.clone().expect("shown live");
        let settings = frame.settings.expect("an edit");
        assert_eq!((settings.opacity, settings.mode), (0.5, BlendMode::Normal));
        assert!(frame.stack.layers[0].subject);
        // No CPU render of the drag while it's shown live.
        for _ in 0..20 {
            e.update(&ctx);
        }
        assert_ne!(e.rendered.0, e.revision);
        assert!(e.rendering.as_ref().is_none_or(|r| r.target.0 < e.revision));
        // Once it ends it's rendered (after any render already under way),
        // and it's one undo step.
        e.end_live();
        let rendering_it = |e: &Editor| {
            e.rendered.0 == e.revision || e.rendering.as_ref().is_some_and(|r| r.target.0 == e.revision)
        };
        for _ in 0..500 {
            e.update(&ctx);
            if rendering_it(&e) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(rendering_it(&e));
        assert_eq!(e.undo_label(), Some("Opacity"));
        e.undo();
        assert_eq!(e.doc.layer(patch).unwrap().opacity, 1.0);
    }

    #[test]
    fn adjustment_edits_show_live_with_their_lookup_table() {
        use omapix_engine::adjust::{Adjustment, Curves};
        let ctx = egui::Context::default();
        let mut e = editor();
        let (w, h) = (e.doc.width, e.doc.height);
        let id = e.doc.next_layer_id();
        e.doc.layers.push(Layer::adjustment(id, Adjustment::Curves(Curves::default()), w, h));
        e.active = id;
        let bend = |e: &mut Editor, y: f32| {
            e.edit_live("Curves", |doc| {
                let Some(Adjustment::Curves(c)) = &mut doc.layers[2].adjustment else { unreachable!() };
                c.master.points = vec![(0.0, 0.0), (0.5, y), (1.0, 1.0)];
            })
        };
        bend(&mut e, 0.6);
        settle(&mut e, &ctx);
        bend(&mut e, 0.7);
        e.update(&ctx);
        let first = e.canvas.live.clone().unwrap().settings.unwrap().lut.expect("a table");
        let mid = first[16 + 16 * 33 + 16 * 33 * 33];
        assert!((mid[0] - 0.7).abs() < 0.01, "the middle of the new curve, {mid:?}");
        // Unchanged, the table isn't made again.
        e.update(&ctx);
        let again = e.canvas.live.clone().unwrap().settings.unwrap().lut.unwrap();
        assert!(Arc::ptr_eq(&first, &again));
    }

    #[test]
    fn edits_to_pixels_are_not_shown_live() {
        let ctx = egui::Context::default();
        let mut e = editor();
        e.edit_live("Paint", |doc| doc.layers[1].pixels.tile_mut(0, 0)[5] = [0, 0, 0, 65535]);
        settle(&mut e, &ctx);
        assert!(e.canvas.live.is_none());
    }

    #[test]
    fn moves_the_gpu_cant_show_are_made_as_they_go() {
        let ctx = egui::Context::default();
        let mut e = editor();
        let patch = e.active;
        e.doc.layer_mut(patch).unwrap().clipped = true;
        assert!(e.begin_move("Move", false, 0));
        e.move_to(10, 5);
        e.update(&ctx);
        assert!(e.canvas.live.is_none());
        assert_eq!(e.doc.layer(patch).unwrap().pixels.get(10, 5), [65535, 0, 0, 65535]);
        e.end_move();
    }
}

#[cfg(test)]
mod bench {
    //! `cargo test --release -p omapix bench -- --ignored --nocapture`
    use super::*;
    use omapix_engine::adjust::{Adjustment, Curves};
    use omapix_engine::{ColorProfile, Raster, ops, tiles};
    use std::time::Instant;

    #[test]
    #[ignore]
    fn slider_drag_stages_24mp() {
        let (w, h) = (6000u32, 4000u32);
        let px: Vec<Pixel> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                [(x * 10) as u16, (y * 15) as u16, ((x + y) * 5) as u16, 65535]
            })
            .collect();
        let doc = Document::from_image("b.tif".into(), &Raster::new(w, h, px), ColorProfile::srgb(), 16);
        let mut doc = doc;
        ops::dodge_and_burn_layer(&mut doc, 0);
        let id = doc.next_layer_id();
        doc.layers.push(Layer::adjustment(id, Adjustment::Curves(Curves::default()), w, h));
        // Best of three, so first-touch costs don't count.
        let time = |name: &str, f: &mut dyn FnMut()| {
            let best = (0..3)
                .map(|_| {
                    let t = Instant::now();
                    f();
                    t.elapsed().as_secs_f64() * 1e3
                })
                .fold(f64::MAX, f64::min);
            println!("{name:<44} {best:>7.1} ms");
        };
        // What a 2560×1440 window shows at 100 %: 10×6 tiles of 256.
        let tiles: Vec<(u32, u32)> = (0..6).flat_map(|r| (0..10).map(move |c| (c, r))).collect();
        let groups = GroupCache::default();
        let mut data = Vec::new();
        time("composite visible tiles at 100 %", &mut || {
            data = draw_tiles(&doc.layers, View::Image, [0; 4], &tiles, Some(&groups), None)
        });
        let render = Render::new(Raster::new(w, h, vec![[0; 4]; (w * h) as usize]));
        time("write tiles + update pyramid", &mut || {
            render.write_tiles(0, &tiles, &data, None);
        });
        let transform = DisplayTransform::to_srgb(&doc.profile).unwrap();
        render.with_image(|image| {
            time("convert display tiles to sRGB (15 × 512, par)", &mut || {
                use rayon::prelude::*;
                let keys: Vec<(u32, u32)> = (0..3).flat_map(|r| (0..5).map(move |c| (c, r))).collect();
                keys.par_iter().for_each(|&(c, r)| {
                    let b = tiles::bounds(image.width(), image.height(), c, r);
                    std::hint::black_box(tiles::render(image, &transform, b));
                });
            });
        });
        let base = &doc.layers[0].pixels;
        time("translate a 24 MP layer by (7, 3) (Move step)", &mut || {
            std::hint::black_box(base.translated(7, 3, base.fill()));
        });
        let patch = doc.layers[0].id;
        time("live stack for a move at 100 % (2560×1440)", &mut || {
            std::hint::black_box(live::build(&doc, &doc.layers, patch, true, 0, (1000, 1000, 2560, 1440)));
        });
        // Zoomed out to fit: level 2 (1500×1000), whole image on screen.
        let mut reduced = Reduced::default();
        let mut shrunk = Vec::new();
        time("shrink layers to level 2 (first time)", &mut || shrunk = reduced.layers(&doc.layers, 2));
        time("shrink layers to level 2 (kept)", &mut || shrunk = reduced.layers(&doc.layers, 2));
        let small: Vec<(u32, u32)> = (0..4).flat_map(|r| (0..6).map(move |c| (c, r))).collect();
        time("composite level 2 (fit), all tiles", &mut || {
            data = draw_tiles(&shrunk, View::Image, [0; 4], &small, Some(&groups), None)
        });
    }
}
