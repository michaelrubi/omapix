//! Retouch › Soften Scars: the skin the face models find in what the
//! active layer and those below it show, with its mid band taken out
//! (`omapix_engine::skin`), onto a new Scars layer above whose mask hides it
//! all. Painting the mask softens a scar there.

use std::sync::mpsc::{Receiver, TryRecvError, channel};

use omapix_engine::skin;

use crate::editor::{Editor, Target};
use crate::face_selection::{FoundSkin, find_skin};
use crate::theme::Theme;

pub struct SoftenScars {
    /// The layer it goes above.
    layer: u64,
    finding: Receiver<Result<FoundSkin, String>>,
}

impl SoftenScars {
    /// Start finding the skin in what the active layer and those below it
    /// show.
    pub fn open(ctx: &egui::Context, editor: &Editor) -> Result<Self, String> {
        let index = editor.active_index().ok_or("Select a layer first")?;
        let doc = editor.doc.clone();
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(find_skin(doc.composite_current_and_below(index), &doc.profile));
            ctx.request_repaint();
        });
        Ok(Self {
            layer: editor.active,
            finding: rx,
        })
    }

    /// Show that it's looking. Returns `Some(true)` once the skin's found
    /// and the layer's on its way, `Some(false)` if cancelled, and an error
    /// if the skin couldn't be found.
    pub fn show(&mut self, ctx: &egui::Context, editor: &mut Editor, theme: &Theme) -> Result<Option<bool>, String> {
        match self.finding.try_recv() {
            Ok(found) => {
                self.apply(ctx, editor, found?);
                return Ok(Some(true));
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => return Err("Soften Scars stopped unexpectedly".into()),
        }
        let mut cancelled = false;
        egui::Modal::new(egui::Id::new("soften-scars")).show(ctx, |ui| {
            ui.set_width(300.0);
            ui.heading("Soften Scars");
            ui.add_space(8.0);
            ui.label(egui::RichText::new("Finding skin…").color(theme.dark_foreground));
            ui.add_space(12.0);
            cancelled = ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape));
        });
        Ok(cancelled.then_some(false))
    }

    /// Add the Scars layer above the layer it was opened on (or the active
    /// one, if that's gone), as one undo step, with its mask ready to paint.
    fn apply(&self, ctx: &egui::Context, editor: &mut Editor, found: FoundSkin) {
        let Some(index) = editor.doc.index_of(self.layer).or(editor.active_index()) else {
            return;
        };
        editor.target = Target::Mask;
        editor.edit_in_background(
            "Soften Scars",
            move |doc, active| {
                if let Some(id) = skin::add_scars_layer(doc, index, &found.image, &found.skin, found.iod) {
                    *active = id;
                }
            },
            ctx,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omapix_engine::{ColorProfile, Document, Raster, Selection};

    #[test]
    fn the_scars_layer_arrives_with_its_mask_ready_to_paint() {
        let image = Raster::new(600, 400, vec![[40000, 30000, 28000, 65535]; 600 * 400]);
        let doc = Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16);
        let mut editor = Editor::new(doc).unwrap();
        let (tx, rx) = channel();
        let mut dialog = SoftenScars {
            layer: editor.active,
            finding: rx,
        };
        let (ctx, theme) = (egui::Context::default(), Theme::default());
        let frame = |dialog: &mut SoftenScars, editor: &mut Editor| {
            let mut done = Err(String::new());
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| done = dialog.show(ui.ctx(), editor, &theme));
            out.textures_delta.clear();
            done
        };
        assert_eq!(frame(&mut dialog, &mut editor), Ok(None));
        tx.send(Ok(FoundSkin {
            image,
            skin: Selection::all(600, 400),
            iod: 100.0,
            cheek: egui::Pos2::ZERO,
            nose: egui::Pos2::ZERO,
        }))
        .unwrap();
        assert_eq!(frame(&mut dialog, &mut editor), Ok(Some(true)));
        while editor.busy().is_some() {
            std::thread::sleep(std::time::Duration::from_millis(1));
            editor.update(&ctx);
        }
        assert_eq!(editor.doc.layer(editor.active).unwrap().name, "Scars");
        assert_eq!(editor.target, Target::Mask);
        assert_eq!(editor.undo_label(), Some("Soften Scars"));
    }
}
