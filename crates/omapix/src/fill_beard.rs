//! Retouch › Fill Beard: the gaps selected (painted in Quick Mask, say)
//! filled with hair copied from the beard found on the same image
//! (`crate::face_selection::find_beard`), by `crate::content_fill`, as a
//! masked Beard layer.

use std::sync::mpsc::{Receiver, TryRecvError, channel};

use omapix_engine::Selection;

use crate::editor::Editor;
use crate::face_selection::find_beard;
use crate::theme::Theme;

pub struct FillBeard {
    finding: Receiver<Result<Selection, String>>,
    /// The beard, once it's found.
    pub beard: Option<Selection>,
}

impl FillBeard {
    /// Start finding the beard in what the active layer and those below it
    /// show.
    pub fn open(ctx: &egui::Context, editor: &Editor) -> Result<Self, String> {
        let index = editor.active_index().ok_or("Select a layer first")?;
        let doc = editor.doc.clone();
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(find_beard(&doc.composite_current_and_below(index), &doc.profile));
            ctx.request_repaint();
        });
        Ok(Self { finding: rx, beard: None })
    }

    /// Show that it's looking. Returns `Some(true)` once the beard's found,
    /// `Some(false)` if cancelled, and an error if it couldn't be found.
    pub fn show(&mut self, ctx: &egui::Context, theme: &Theme) -> Result<Option<bool>, String> {
        match self.finding.try_recv() {
            Ok(beard) => {
                self.beard = Some(beard?);
                return Ok(Some(true));
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => return Err("Fill Beard stopped unexpectedly".into()),
        }
        let mut cancelled = false;
        egui::Modal::new(egui::Id::new("fill-beard")).show(ctx, |ui| {
            ui.set_width(300.0);
            ui.heading("Fill Beard");
            ui.add_space(8.0);
            ui.label(egui::RichText::new("Finding the beard…").color(theme.dark_foreground));
            ui.add_space(12.0);
            cancelled = ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape));
        });
        Ok(cancelled.then_some(false))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use omapix_engine::{ColorProfile, Document, Raster};

    use super::*;
    use crate::canvas::Render;
    use crate::content_fill::ContentFill;

    #[test]
    fn a_gap_is_filled_with_the_beard_found_or_says_why_not() {
        let frame = |dialog: &mut FillBeard| {
            let (ctx, mut done) = (egui::Context::default(), Err(String::new()));
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| done = dialog.show(ui.ctx(), &Theme::default()));
            out.textures_delta.clear();
            done
        };
        let (tx, rx) = channel();
        let mut dialog = FillBeard { finding: rx, beard: None };
        assert_eq!(frame(&mut dialog), Ok(None));
        tx.send(Err("Found no beard to copy from".into())).unwrap();
        assert_eq!(frame(&mut dialog), Err("Found no beard to copy from".into()));

        // Skin with a dark beard at the bottom, and a gap selected.
        let dark = |i: usize| i / 600 >= 200;
        let pixels = (0..600 * 400)
            .map(|i| if dark(i) { [8000, 6000, 5000, 65535] } else { [45000, 35000, 30000, 65535] })
            .collect();
        let image = Raster::new(600, 400, pixels);
        let doc = Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16);
        let mut editor = Editor::new(doc).unwrap();
        editor.doc.selection = Some(Selection::rectangle(600, 400, (250.0, 80.0), (300.0, 130.0)));
        editor.canvas.set_render(Arc::new(Render::new(image)));
        let (tx, rx) = channel();
        let mut dialog = FillBeard { finding: rx, beard: None };
        tx.send(Ok(Selection::rectangle(600, 400, (0.0, 200.0), (600.0, 400.0)))).unwrap();
        assert_eq!(frame(&mut dialog), Ok(Some(true)));

        let mut content = ContentFill::default();
        content.copy(&egui::Context::default(), &editor, "Beard", dialog.beard.take().unwrap()).unwrap();
        while content.busy() {
            std::thread::sleep(std::time::Duration::from_millis(5));
            assert_eq!(content.poll(&mut editor), None);
        }
        let added = editor.doc.layer(editor.active).unwrap();
        assert_eq!(added.name, "Beard");
        assert!(added.pixels.get(275, 105)[0] < 12000, "{:?}", added.pixels.get(275, 105));
        assert_eq!(editor.undo_label(), Some("Beard"));
    }
}
