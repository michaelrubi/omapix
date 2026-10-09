//! Edit › Content-Aware Fill and Texture Fill: the selection filled from
//! its surroundings, by LaMa (see omapix-ai) or by copying texture
//! (`omapix_engine::inpaint`), on a thread of its own, as a new layer
//! masked to the selection, so it can be painted back or thrown away.

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
    /// The layer being made, and the name of its undo step.
    running: Option<(Receiver<Result<Layer, String>>, &'static str)>,
}

impl ContentFill {
    /// Fill the selection in what `editor` shows: with texture copied from
    /// round it if that's what's asked for (`texture`), or if the model
    /// isn't there, and by the model otherwise.
    pub fn start(&mut self, ctx: &egui::Context, editor: &Editor, texture: bool) -> Result<(), String> {
        let name = if texture { "Texture Fill" } else { "Content-Aware Fill" };
        let texture = texture || omapix_ai::find_model(omapix_ai::lama::MODEL).is_none();
        let (Some(image), Some(selection)) = (editor.canvas.render(), &editor.doc.selection) else {
            return Err("Select what to fill first".into());
        };
        let (image, selection, profile) = (Arc::clone(image), selection.clone(), editor.doc.profile.clone());
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let layer = if texture { copied(name, &image, &selection) } else { layer(&image, &selection, &profile) };
            let _ = tx.send(layer);
            ctx.request_repaint();
        });
        self.running = Some((rx, name));
        Ok(())
    }

    /// Add the layer once it's made, above the selected one. Returns what
    /// went wrong, if anything.
    pub fn poll(&mut self, editor: &mut Editor) -> Option<String> {
        let (running, name) = self.running.as_ref()?;
        let name = *name;
        let result = match running.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err(format!("{name} stopped unexpectedly")),
        };
        self.running = None;
        let mut layer = match result {
            Ok(layer) => layer,
            Err(e) => return Some(e),
        };
        let above = editor.active_index().unwrap_or(0);
        editor.edit(name, move |doc, active| {
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

/// The selection filled with texture copied from round it.
fn copied(name: &str, image: &Render, selection: &Selection) -> Result<Layer, String> {
    image
        .with_image(|image| fill::copied(0, name, image, selection))
        .ok_or("The image isn't ready yet")?
        .ok_or("Select what to fill first".into())
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
        let mut content = ContentFill { running: Some((rx, "Content-Aware Fill")) };
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
        content.running = Some((rx, "Content-Aware Fill"));
        tx.send(Err("no model".into())).unwrap();
        assert_eq!(content.poll(&mut editor), Some("no model".into()));
    }

    #[test]
    fn texture_fill_needs_no_model_and_covers_the_selection_with_what_is_round_it() {
        // Grey with a red square, which is selected.
        let red = |i: usize| (250..300).contains(&(i % 600)) && (150..200).contains(&(i / 600));
        let pixels = (0..600 * 400).map(|i| if red(i) { [65535, 0, 0, 65535] } else { [30000, 30000, 30000, 65535] }).collect();
        let image = Raster::new(600, 400, pixels);
        let doc = Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16);
        let mut editor = Editor::new(doc).unwrap();
        let mut content = ContentFill::default();
        let ctx = egui::Context::default();
        assert_eq!(content.start(&ctx, &editor, true), Err("Select what to fill first".into()));
        editor.doc.selection = Some(Selection::rectangle(600, 400, (250.0, 150.0), (300.0, 200.0)));
        editor.canvas.set_render(Arc::new(Render::new(image)));
        content.start(&ctx, &editor, true).unwrap();
        while content.busy() {
            std::thread::sleep(std::time::Duration::from_millis(5));
            assert_eq!(content.poll(&mut editor), None);
        }
        let added = editor.doc.layer(editor.active).unwrap();
        assert_eq!(added.name, "Texture Fill");
        let p = added.pixels.get(275, 175);
        assert!(p[0].abs_diff(30000) < 300 && p[1].abs_diff(30000) < 300 && p[3] == 65535, "{p:?}");
        assert_eq!(added.pixels.get(240, 175)[3], 0);
        assert_eq!(editor.undo_label(), Some("Texture Fill"));
    }
}
