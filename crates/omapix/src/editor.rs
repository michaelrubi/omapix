//! One open document: its layers, undo history, selection, and the
//! background work that keeps the canvas up to date.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};

use omapix_engine::brush::{BrushSettings, Paint, Stroke, Surface};
use omapix_engine::layer::Layer;
use omapix_engine::tiled::{TILE, Tiled};
use omapix_engine::{BlendMode, DisplayTransform, Document, Pixel, Raster, composite, filters};

use crate::canvas::{Canvas, Render};

/// Undo steps kept. Snapshots share unchanged tiles, so this mostly costs
/// memory for pixels that edits actually replaced.
const HISTORY_LIMIT: usize = 50;

/// What the canvas shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum View {
    /// The finished image.
    Image,
    /// One layer's mask, in greyscale (Photoshop's Alt+click on a mask).
    Mask(u64),
    /// What frequency separation at `radius` would produce: the texture
    /// layer, or the colour/tone layer.
    Separation { radius: f32, texture: bool },
}

/// A finished background render: the revision and view it shows, and for
/// separation previews the flattened image it started from (reused while
/// only the radius changes).
type Rendered = (Render, Option<Arc<Tiled<Pixel>>>);

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

struct Job {
    label: String,
    rx: Receiver<(Document, u64)>,
}

pub struct Editor {
    pub doc: Document,
    /// Id of the selected layer.
    pub active: u64,
    pub target: Target,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// Key of the continuous edit in progress (e.g. dragging the opacity
    /// slider), so a whole drag is one undo step.
    live: Option<String>,
    /// Incremented on every change to `doc`.
    revision: u64,
    /// What the canvas shows, and a counter bumped whenever that changes.
    view: View,
    view_generation: u64,
    /// Revision and view generation being rendered, and the result.
    rendering: Option<((u64, u64), Receiver<Rendered>)>,
    /// Revision and view generation the canvas is showing.
    rendered: (u64, u64),
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
    /// Last revision drawn straight into the canvas by a brush stroke.
    /// Background renders of older revisions are thrown away.
    painted: u64,
}

impl Editor {
    pub fn new(doc: Document) -> Result<Self, String> {
        let transform = DisplayTransform::to_srgb(&doc.profile).map_err(|e| e.to_string())?;
        let canvas = Canvas::new(doc.width, doc.height, transform);
        let active = doc.layers.last().map_or(0, |l| l.id);
        Ok(Self {
            doc,
            active,
            target: Target::Pixels,
            undo: Vec::new(),
            redo: Vec::new(),
            live: None,
            revision: 1,
            view: View::Image,
            view_generation: 0,
            rendering: None,
            rendered: (0, u64::MAX),
            separation_base: None,
            job: None,
            modified: false,
            canvas,
            stroke: None,
            painted: 0,
        })
    }

    pub fn active_index(&self) -> Option<usize> {
        self.doc.index_of(self.active)
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
        self.end_stroke();
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
        self.end_stroke();
        if self.live.as_deref() != Some(key) {
            let before = self.snapshot(key);
            self.push_undo(before);
            self.live = Some(key.to_owned());
        }
        f(&mut self.doc);
        self.changed();
    }

    pub fn end_live(&mut self) {
        self.live = None;
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
        self.end_stroke();
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
        self.end_stroke();
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
        self.end_stroke();
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
        let (layers, view) = (&self.doc.layers, self.view);
        let updated = self.canvas.render().cloned().and_then(|render| match view {
            View::Image => render.update_tiles(&changed, |image, tiles| {
                composite::composite_into(layers, image, tiles)
            }),
            View::Mask(id) => {
                render.update_tiles(&changed, |image, tiles| draw_mask(layers, id, image, tiles))
            }
            View::Separation { .. } => None,
        });
        if let Some(area) = updated {
            self.canvas.invalidate(area);
            self.painted = self.revision;
            // If the canvas was current before this dab, it still is. If
            // not, a full render will catch up with the rest.
            if up_to_date {
                self.rendered = (self.revision, self.view_generation);
            }
        }
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

        if let Some(((revision, generation), rx)) = &self.rendering
            && let Ok((render, base)) = rx.try_recv()
        {
            let (revision, generation) = (*revision, *generation);
            self.rendering = None;
            if let Some(base) = base {
                self.separation_base = Some((revision, base));
            }
            // A render started before brush dabs were drawn in place would
            // erase them from the screen, and one for a previous view is
            // simply out of date; drop both and render again.
            if revision >= self.painted && generation == self.view_generation {
                self.rendered = (revision, generation);
                self.canvas.set_render(Arc::new(render));
            }
        }
        // One render at a time; when it finishes, the latest state is
        // rendered next, so fast slider drags skip intermediate states.
        let current = (self.revision, self.view_generation);
        if self.rendering.is_none() && self.rendered != current && self.stroke.is_none() {
            let (tx, rx) = channel();
            let doc = self.doc.clone();
            let view = self.view;
            let base = self
                .separation_base
                .as_ref()
                .filter(|(r, _)| *r == self.revision)
                .map(|(_, b)| Arc::clone(b));
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let rendered = render_view(&doc, view, base);
                let _ = tx.send(rendered);
                ctx.request_repaint();
            });
            self.rendering = Some((current, rx));
        }
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

/// Render what `view` shows. For separation previews, also return the
/// flattened image used, so later radius changes can reuse it.
fn render_view(doc: &Document, view: View, base: Option<Arc<Tiled<Pixel>>>) -> Rendered {
    match view {
        View::Image => (Render::new(doc.composite()), None),
        View::Mask(id) => {
            let mut image = Raster::new(
                doc.width,
                doc.height,
                vec![[0; 4]; doc.width as usize * doc.height as usize],
            );
            let tiles: Vec<(u32, u32)> = (0..doc.height.div_ceil(TILE))
                .flat_map(|r| (0..doc.width.div_ceil(TILE)).map(move |c| (c, r)))
                .collect();
            draw_mask(&doc.layers, id, &mut image, &tiles);
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
    }
}

/// Write a layer's mask as opaque grey into the given tiles of `image`.
fn draw_mask(layers: &[Layer], id: u64, image: &mut Raster, tiles: &[(u32, u32)]) {
    let Some(mask) = layers
        .iter()
        .find(|l| l.id == id)
        .and_then(|l| l.mask.as_ref())
    else {
        return;
    };
    let (w, h) = (image.width(), image.height());
    for &(col, row) in tiles {
        let tile = mask.pixels.tile(col, row);
        for ty in 0..TILE.min(h.saturating_sub(row * TILE)) {
            let y = row * TILE + ty;
            let line = image.row_mut(y);
            for tx in 0..TILE.min(w.saturating_sub(col * TILE)) {
                let v = tile.map_or(mask.pixels.fill(), |t| t[(ty * TILE + tx) as usize]);
                line[(col * TILE + tx) as usize] = [v, v, v, u16::MAX];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omapix_engine::layer::Mask;
    use omapix_engine::{ColorProfile, Raster, ops};

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
        let (render, _) = render_view(&e.doc, View::Mask(e.active), None);
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
            None,
        );
        assert!(texture.sample_for_test(300, 200)[0].abs_diff(32768) <= 2);
        let (tone, _) = render_view(
            &e.doc,
            View::Separation {
                radius: 5.0,
                texture: false,
            },
            base,
        );
        assert!(tone.sample_for_test(300, 200)[0].abs_diff(30000) <= 2);
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
}
