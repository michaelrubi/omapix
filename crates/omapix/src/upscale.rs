//! Image › Image Size's Enlarge with AI (docs/AI.md, milestone 11):
//! RealPLKSR enlarges the image as its layers make it, onto a new Upscale
//! layer on top of them, while they, their masks and the selection are
//! enlarged the ordinary way. So hiding the layer shows the ordinary
//! enlargement, and its opacity or a mask holds the model back.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::sync::Arc;

use omapix_ai::upscale::Upscaler;
use omapix_engine::{Layer, Raster, upscale};

use crate::editor::Editor;

/// The image being enlarged: the result, how many model squares are done
/// of how many, the document it was started on (its file and size), and the
/// size it's going to.
type Running = (Receiver<Result<Raster, String>>, Arc<(AtomicUsize, AtomicUsize)>, (PathBuf, u32, u32), (u32, u32));

#[derive(Default)]
pub struct Upscaling {
    running: Option<Running>,
}

/// How many times larger the model has to make an image going `from` one
/// size `to` another: it comes as ×2 and ×4, and what it gives is fitted
/// to the size.
fn factor(from: (u32, u32), to: (u32, u32)) -> usize {
    if to.0 <= from.0 * 2 && to.1 <= from.1 * 2 { 2 } else { 4 }
}

impl Upscaling {
    /// Start enlarging what the document shows to `width` × `height`. The
    /// model is loaded for this and dropped after, giving its GPU memory
    /// back: it's rarely wanted twice.
    pub fn start(&mut self, ctx: &egui::Context, editor: &Editor, width: u32, height: u32) {
        let doc = editor.doc.clone();
        let from = (doc.path.clone(), doc.width, doc.height);
        let progress = Arc::new((AtomicUsize::new(0), AtomicUsize::new(0)));
        let (tx, rx) = channel();
        let (ctx, shared) = (ctx.clone(), Arc::clone(&progress));
        std::thread::spawn(move || {
            let result = (|| {
                let mut model = Upscaler::load(factor((doc.width, doc.height), (width, height)))?;
                let squares = (model.size, model.factor);
                upscale::upscale(&doc.composite(), &doc.profile, (width, height), squares, |s| model.upscale(s), |done, total| {
                    shared.0.store(done, Ordering::Relaxed);
                    shared.1.store(total, Ordering::Relaxed);
                    ctx.request_repaint();
                })
            })();
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        self.running = Some((rx, progress, from, (width, height)));
    }

    /// Once the enlargement is made, resize the document and add it as the
    /// Upscale layer on top, as one undo step. Returns what went wrong, if
    /// anything.
    pub fn poll(&mut self, editor: &mut Editor) -> Option<String> {
        let (rx, _, from, to) = self.running.as_ref()?;
        // An edit under way would refuse this one.
        if editor.busy().is_some() {
            return None;
        }
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err("Image Size stopped unexpectedly".into()),
        };
        let same = (&editor.doc.path, editor.doc.width, editor.doc.height) == (&from.0, from.1, from.2);
        let (width, height) = *to;
        self.running = None;
        let enlarged = match result {
            Ok(enlarged) if same => enlarged,
            Ok(_) => return Some("The image changed size or was closed while it was being enlarged, so nothing was done".into()),
            Err(e) => return Some(e),
        };
        editor.edit("Image Size", move |doc, active| {
            doc.resize_image(width, height);
            let id = doc.next_layer_id();
            doc.layers.push(Layer::from_raster(id, "Upscale", &enlarged));
            *active = id;
        });
        None
    }

    /// How many model squares are done, of how many, while it runs.
    pub fn busy(&self) -> Option<(usize, usize)> {
        let (_, progress, ..) = self.running.as_ref()?;
        Some((progress.0.load(Ordering::Relaxed), progress.1.load(Ordering::Relaxed)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omapix_engine::{ColorProfile, Document, Selection};

    fn editor() -> Editor {
        let image = Raster::new(300, 200, vec![[30000, 30000, 30000, 65535]; 300 * 200]);
        let mut doc = Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16);
        let id = doc.next_layer_id();
        doc.layers.push(Layer::empty(id, "Retouch", 300, 200));
        doc.selection = Some(Selection::rectangle(300, 200, (0.0, 0.0), (150.0, 200.0)));
        Editor::new(doc).unwrap()
    }

    fn running(from: (u32, u32), to: (u32, u32)) -> (std::sync::mpsc::Sender<Result<Raster, String>>, Upscaling) {
        let (tx, rx) = channel();
        let progress = Arc::new((AtomicUsize::new(3), AtomicUsize::new(8)));
        (tx, Upscaling { running: Some((rx, progress, ("t.tif".into(), from.0, from.1), to)) })
    }

    #[test]
    fn the_enlargement_arrives_as_a_layer_on_top_of_the_resized_document_in_one_undo_step() {
        let mut editor = editor();
        let (tx, mut job) = running((300, 200), (600, 400));
        assert_eq!(job.poll(&mut editor), None);
        assert_eq!(job.busy(), Some((3, 8)));
        tx.send(Ok(Raster::new(600, 400, vec![[31000, 30000, 29000, 65535]; 600 * 400]))).unwrap();
        assert_eq!(job.poll(&mut editor), None);
        assert_eq!(job.busy(), None);

        let doc = &editor.doc;
        assert_eq!((doc.width, doc.height, doc.layers.len()), (600, 400, 3));
        let top = doc.layers.last().unwrap();
        assert_eq!((top.name.as_str(), top.id, top.parent), ("Upscale", editor.active, None));
        assert_eq!(top.pixels.get(599, 399), [31000, 30000, 29000, 65535]);
        // The layers under it, and the selection, enlarged the ordinary way.
        assert_eq!(doc.layers[0].pixels.get(599, 399), [30000, 30000, 30000, 65535]);
        assert_eq!(doc.selection.as_ref().unwrap().bounds().map(|b| b[3]), Some(400));
        assert_eq!(editor.undo_label(), Some("Image Size"));
        editor.undo();
        assert_eq!((editor.doc.width, editor.doc.height, editor.doc.layers.len()), (300, 200, 2));
    }

    #[test]
    fn failures_and_an_image_that_changed_size_meanwhile_come_back_as_messages() {
        let mut editor = editor();
        let (tx, mut job) = running((300, 200), (600, 400));
        tx.send(Err("no model".into())).unwrap();
        assert_eq!(job.poll(&mut editor), Some("no model".into()));

        // Cropped while the model ran: what it made no longer fits.
        let (tx, mut job) = running((300, 200), (600, 400));
        editor.edit("Crop", |doc, _| doc.resize_canvas(200, 200, 0, 0, None));
        tx.send(Ok(Raster::new(600, 400, vec![[0; 4]; 600 * 400]))).unwrap();
        assert!(job.poll(&mut editor).is_some_and(|e| e.contains("changed size")));
        assert_eq!((editor.doc.width, editor.doc.height, editor.undo_label()), (200, 200, Some("Crop")));
    }

    #[test]
    fn the_models_factor_is_the_smaller_that_reaches_the_size() {
        assert_eq!(factor((300, 200), (450, 300)), 2);
        assert_eq!(factor((300, 200), (600, 400)), 2);
        assert_eq!(factor((300, 200), (601, 400)), 4);
        assert_eq!(factor((300, 200), (300, 900)), 4);
    }

    /// The whole job on a real photo, with the model (darktable installs
    /// it): `OMAPIX_UPSCALE_PHOTO=photo.jpg cargo test --release -p omapix
    /// upscale_a_photo -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn upscale_a_photo() {
        let Ok(photo) = std::env::var("OMAPIX_UPSCALE_PHOTO") else { return };
        let doc = omapix_engine::io::load(std::path::Path::new(&photo)).unwrap();
        let (w, h) = (doc.width, doc.height);
        let mut editor = Editor::new(doc).unwrap();
        let mut job = Upscaling::default();
        let t = std::time::Instant::now();
        job.start(&egui::Context::default(), &editor, w * 2, h * 2);
        while job.busy().is_some() {
            assert_eq!(job.poll(&mut editor), None);
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        eprintln!("{w} × {h} enlarged twice in {:?}", t.elapsed());
        assert_eq!((editor.doc.width, editor.doc.height), (w * 2, h * 2));
        assert_eq!(editor.doc.layers.last().unwrap().name, "Upscale");
    }
}
