//! Edit › Content-Aware Fill: the selection filled from its surroundings by
//! LaMa (see omapix-ai), on a thread of its own, as a new layer masked to
//! the selection, so it can be painted back or thrown away.

use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::sync::{Arc, Mutex};

use omapix_ai::lama::{Lama, SIZE};
use omapix_engine::fill;
use omapix_engine::{ColorProfile, Layer};
use omapix_engine::selection::Selection;

use crate::canvas::Render;
use crate::editor::Editor;

/// Loaded on first use, and kept until Generative Fill needs the card.
static MODEL: Mutex<Option<Lama>> = Mutex::new(None);

/// Drop the model, giving its GPU memory back. It loads again when next
/// used.
pub fn unload() {
    if let Ok(mut model) = MODEL.lock() {
        *model = None;
    }
}

#[derive(Default)]
pub struct ContentFill {
    /// The layer being made.
    running: Option<Receiver<Result<Layer, String>>>,
}

impl ContentFill {
    /// Fill the selection in what `editor` shows.
    pub fn start(&mut self, ctx: &egui::Context, editor: &Editor) -> Result<(), String> {
        if omapix_ai::find_model(omapix_ai::lama::MODEL).is_none() {
            return Err("Content-Aware Fill needs the LaMa model: run scripts/fetch-models.sh".into());
        }
        let (Some(image), Some(selection)) = (editor.canvas.render(), &editor.doc.selection) else {
            return Err("Select what to fill first".into());
        };
        let (image, selection, profile) = (Arc::clone(image), selection.clone(), editor.doc.profile.clone());
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(layer(&image, &selection, &profile));
            ctx.request_repaint();
        });
        self.running = Some(rx);
        Ok(())
    }

    /// Add the layer once it's made, above the selected one. Returns what
    /// went wrong, if anything.
    pub fn poll(&mut self, editor: &mut Editor) -> Option<String> {
        let result = match self.running.as_ref()?.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err("Content-Aware Fill stopped unexpectedly".into()),
        };
        self.running = None;
        let mut layer = match result {
            Ok(layer) => layer,
            Err(e) => return Some(e),
        };
        let above = editor.active_index().unwrap_or(0);
        editor.edit("Content-Aware Fill", move |doc, active| {
            layer.id = doc.next_layer_id();
            *active = layer.id;
            doc.insert_above(above, layer);
        });
        None
    }

    pub fn busy(&self) -> bool {
        self.running.is_some()
    }
}

fn layer(image: &Render, selection: &Selection, profile: &ColorProfile) -> Result<Layer, String> {
    let patch = image
        .with_image(|image| fill::patch(image, profile, selection, SIZE))
        .ok_or("The image isn't ready yet")?
        .map_err(|e| e.to_string())?
        .ok_or("Select what to fill first")?;
    let filled = {
        let mut model = MODEL.lock().map_err(|e| e.to_string())?;
        let lama = match &mut *model {
            Some(lama) => lama,
            None => model.insert(Lama::load()?),
        };
        lama.fill(&patch.image, &patch.mask)?
    };
    fill::layer(0, "Content-Aware Fill", &patch, &filled, profile, selection).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use omapix_engine::{Document, Raster};

    #[test]
    fn the_fill_arrives_as_a_masked_layer_above_the_selected_one_in_one_undo_step() {
        let image = Raster::new(600, 400, vec![[30000, 30000, 30000, 65535]; 600 * 400]);
        let doc = Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16);
        let mut editor = Editor::new(doc).unwrap();
        let selection = Selection::rectangle(600, 400, (100.0, 100.0), (200.0, 150.0));
        let patch = fill::patch(&image, &ColorProfile::srgb(), &selection, 64).unwrap().unwrap();
        let layer = fill::layer(0, "Content-Aware Fill", &patch, &vec![0.5; 3 * 64 * 64], &ColorProfile::srgb(), &selection);
        let layers = editor.doc.layers.len();

        let (tx, rx) = channel();
        let mut content = ContentFill { running: Some(rx) };
        assert_eq!(content.poll(&mut editor), None);
        assert!(content.busy());
        tx.send(layer.map_err(|e| e.to_string())).unwrap();
        assert_eq!(content.poll(&mut editor), None);
        assert!(!content.busy());
        assert_eq!(editor.doc.layers.len(), layers + 1);
        let added = editor.doc.layer(editor.active).unwrap();
        assert_eq!(added.name, "Content-Aware Fill");
        assert!(added.mask.is_some());
        assert_eq!(editor.undo_label(), Some("Content-Aware Fill"));

        // Failures come back as messages.
        let (tx, rx) = channel();
        content.running = Some(rx);
        tx.send(Err("no model".into())).unwrap();
        assert_eq!(content.poll(&mut editor), Some("no model".into()));
    }
}
