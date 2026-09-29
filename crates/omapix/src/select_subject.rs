//! Select › Subject: the person in the photo, matted by BiRefNet portrait
//! (see omapix-ai) on a thread of its own, and selected with soft edges
//! along hair, as in Photoshop.

use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::sync::{Arc, Mutex};

use omapix_ai::subject::{SIZE, Subject};
use omapix_engine::selection::Selection;
use omapix_engine::{ColorProfile, DisplayTransform, refine};

use crate::canvas::Render;
use crate::editor::Editor;

/// Loaded on first use and kept until Omapix quits: dropping CUDA sessions
/// can crash it as it quits (docs/AI.md).
static MODEL: Mutex<Option<Subject>> = Mutex::new(None);

#[derive(Default)]
pub struct SelectSubject {
    /// The selection being made.
    running: Option<Receiver<Result<Selection, String>>>,
}

impl SelectSubject {
    /// Select the subject of what `editor` shows.
    pub fn start(&mut self, ctx: &egui::Context, editor: &Editor) -> Result<(), String> {
        let image = Arc::clone(editor.canvas.render().ok_or("The image isn't ready yet")?);
        let profile = editor.doc.profile.clone();
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(select(&image, &profile));
            ctx.request_repaint();
        });
        self.running = Some(rx);
        Ok(())
    }

    /// Make it the selection once it's found, as one undo step. Returns
    /// what went wrong, if anything.
    pub fn poll(&mut self, editor: &mut Editor) -> Option<String> {
        let result = match self.running.as_ref()?.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err("Select Subject stopped unexpectedly".into()),
        };
        self.running = None;
        match result {
            Ok(selection) => {
                editor.edit("Select Subject", |doc, _| doc.selection = Some(selection));
                None
            }
            Err(e) => Some(e),
        }
    }

    pub fn busy(&self) -> bool {
        self.running.is_some()
    }
}

fn select(image: &Render, profile: &ColorProfile) -> Result<Selection, String> {
    let transform = DisplayTransform::to_srgb(profile).map_err(|e| e.to_string())?;
    let mut model = MODEL.lock().map_err(|e| e.to_string())?;
    let subject = match &mut *model {
        Some(subject) => subject,
        None => model.insert(Subject::load()?),
    };
    image
        .with_image(|image| {
            let mut srgb = vec![[0u8; 4]; image.pixels().len()];
            transform.convert(image.pixels(), &mut srgb);
            let matte = subject.matte(&srgb, image.width(), image.height())?;
            Ok(Selection::from_coverage(refine::matte_coverage(&matte, SIZE, SIZE, image)))
        })
        .ok_or("The image isn't ready yet")?
}

#[cfg(test)]
mod tests {
    use super::*;
    use omapix_engine::{Document, Raster};

    #[test]
    fn the_subject_becomes_the_selection_in_one_undo_step() {
        let image = Raster::new(600, 400, vec![[30000, 30000, 30000, 65535]; 600 * 400]);
        let doc = Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16);
        let mut editor = Editor::new(doc).unwrap();
        let (tx, rx) = channel();
        let mut subject = SelectSubject { running: Some(rx) };
        assert_eq!(subject.poll(&mut editor), None);
        assert!(subject.busy());

        tx.send(Ok(Selection::rectangle(600, 400, (100.0, 100.0), (200.0, 150.0)))).unwrap();
        assert_eq!(subject.poll(&mut editor), None);
        assert!(!subject.busy());
        assert_eq!(editor.doc.selection.as_ref().unwrap().at(150, 120), 1.0);
        assert_eq!(editor.undo_label(), Some("Select Subject"));

        // Failures come back as messages.
        let (tx, rx) = channel();
        subject.running = Some(rx);
        tx.send(Err("no model".into())).unwrap();
        assert_eq!(subject.poll(&mut editor), Some("no model".into()));
    }
}
