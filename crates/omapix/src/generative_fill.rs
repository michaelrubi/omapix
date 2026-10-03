//! Edit › Generative Fill… (docs/AI.md, feature 9): the selection changed
//! as a prompt asks, or what's in it removed, by FLUX.2 klein (see
//! omapix-ai) on a thread of its own. With nothing selected, the empty
//! canvas round the picture (as the Crop tool leaves when it's dragged
//! outwards) is filled, extending the picture. Three results at a time,
//! each shown on the canvas as a new layer masked to the selection; OK
//! keeps the one showing, as one undo step.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use egui::{Color32, RichText, TextEdit, vec2};
use omapix_ai::flux::{self, CELL, CELLS, Canvas, Progress, STEPS};
use omapix_engine::fill::{self, Patch};
use omapix_engine::selection::Selection;
use omapix_engine::{ColorProfile, Layer, Raster};

use crate::editor::Editor;
use crate::theme::Theme;

const NAME: &str = "Generative Fill";
const NOTHING: &str = "Select what to fill first, or make room round the picture with the Crop tool";
/// Results made each time.
const RESULTS: usize = 3;

enum Message {
    /// What the model sees, cut from the image, and what of it to fill.
    Patch(Box<(Patch, Selection)>),
    Status(String),
    Result(Box<Layer>),
    Done,
}

pub struct GenerativeFill {
    pub prompt: String,
    /// Generate once ready, one result, and OK it: for scripts.
    pub accept: bool,
    profile: ColorProfile,
    /// The layer the fill goes above.
    above: usize,
    /// What the model sees, cut from the image as it was when this opened,
    /// before any result was on it, and what's filled: the selection, or
    /// with none, the empty canvas round the picture.
    patch: Option<Arc<(Patch, Selection)>>,
    working: Option<Receiver<Result<Message, String>>>,
    /// Tells the thread at work to stop.
    stop: Arc<AtomicBool>,
    status: String,
    failed: bool,
    results: Vec<Layer>,
    shown: usize,
    /// The layer in the document showing a result.
    layer: Option<u64>,
    seed: u64,
    /// The next result to arrive is shown: the first of each lot.
    awaited: bool,
    focused: bool,
}

impl GenerativeFill {
    /// Open on what `editor` shows and has selected. Results are previewed
    /// as edits to the document, gathered into one undo step.
    pub fn open(ctx: &egui::Context, editor: &mut Editor) -> Result<Self, String> {
        if omapix_ai::find_model(flux::MODEL).is_none() {
            return Err(format!("Generative Fill needs the FLUX.2 klein model: run scripts/fetch-models.sh {}", flux::MODEL));
        }
        let image = Arc::clone(editor.canvas.render().ok_or("The image isn't ready yet")?);
        let (selection, profile) = (editor.doc.selection.clone(), editor.doc.profile.clone());
        let (tx, rx) = channel();
        {
            let (profile, ctx) = (profile.clone(), ctx.clone());
            std::thread::spawn(move || {
                let cut = image.with_image(|image| cut(image, selection, &profile));
                let _ = tx.send(cut.unwrap_or(Err("The image isn't ready yet".into())));
                ctx.request_repaint();
            });
        }
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos() as u64);
        let above = editor.active_index().unwrap_or(0);
        editor.begin_group(NAME);
        Ok(Self {
            prompt: String::new(),
            accept: false,
            profile,
            above,
            patch: None,
            working: Some(rx),
            stop: Arc::default(),
            status: String::new(),
            failed: false,
            results: Vec::new(),
            shown: 0,
            layer: None,
            seed,
            awaited: true,
            focused: false,
        })
    }

    /// Start making results for the prompt.
    fn generate(&mut self, ctx: &egui::Context) {
        let Some(cut) = self.patch.clone() else {
            return;
        };
        let count = if self.accept { 1 } else { RESULTS };
        let seeds: Vec<u64> = (0..count as u64).map(|i| self.seed.wrapping_add(i)).collect();
        self.seed = self.seed.wrapping_add(count as u64);
        self.stop = Arc::default();
        self.awaited = true;
        self.failed = false;
        self.status = "Starting…".into();
        let (tx, rx) = channel();
        let (prompt, profile) = (self.prompt.clone(), self.profile.clone());
        let (stop, ctx) = (Arc::clone(&self.stop), ctx.clone());
        std::thread::spawn(move || {
            let (patch, selection) = &*cut;
            let tell = |progress| {
                let message = match progress {
                    Progress::Prompt => Ok(Message::Status("Reading the prompt…".into())),
                    Progress::Loading => Ok(Message::Status("Loading the model…".into())),
                    Progress::Step(result, step) => {
                        Ok(Message::Status(format!("Result {} of {count}, step {} of {STEPS}…", result + 1, step + 1)))
                    }
                    Progress::Result(image) => fill::layer(0, NAME, patch, &image, &profile, selection)
                        .map(|layer| Message::Result(Box::new(layer)))
                        .map_err(|e| e.to_string()),
                };
                let sent = message.is_ok() && tx.send(message).is_ok();
                ctx.request_repaint();
                sent && !stop.load(Ordering::Relaxed)
            };
            free_the_card();
            let canvas = Canvas {
                image: &patch.image,
                width: patch.width,
                height: patch.height,
                anew: &patch.cells(CELL),
                empty: &patch.empty_cells(CELL),
            };
            let done = flux::fill(&prompt, &canvas, &seeds, tell);
            let _ = tx.send(done.map(|()| Message::Done));
            ctx.request_repaint();
        });
        self.working = Some(rx);
    }

    /// Take in what the thread at work has to say. The first result of
    /// each lot is shown as it arrives.
    fn poll(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        loop {
            let message = match self.working.as_ref().map(Receiver::try_recv) {
                Some(Ok(message)) => message,
                Some(Err(TryRecvError::Disconnected)) => Err("Generative Fill stopped unexpectedly".into()),
                Some(Err(TryRecvError::Empty)) | None => return,
            };
            match message {
                Ok(Message::Patch(cut)) => {
                    self.patch = Some(Arc::from(cut));
                    self.working = None;
                    if self.accept {
                        self.generate(ctx);
                    }
                }
                Ok(Message::Status(status)) => self.status = status,
                Ok(Message::Result(layer)) => {
                    self.results.push(*layer);
                    if std::mem::take(&mut self.awaited) {
                        self.shown = self.results.len() - 1;
                        self.put(editor);
                    }
                }
                Ok(Message::Done) => {
                    self.working = None;
                    self.status.clear();
                }
                Err(e) => {
                    self.working = None;
                    self.status = e;
                    self.failed = true;
                }
            }
        }
    }

    /// Put the result showing into the document: as a new layer above the
    /// one that was selected, or in place of the result there.
    fn put(&mut self, editor: &mut Editor) {
        let Some(mut result) = self.results.get(self.shown).cloned() else {
            return;
        };
        let (above, mut id) = (self.above, self.layer);
        editor.edit_live(NAME, |doc| match id.and_then(|id| doc.layer_mut(id)) {
            Some(layer) => layer.pixels = result.pixels,
            None => {
                result.id = doc.next_layer_id();
                id = Some(result.id);
                doc.insert_above(above, result);
            }
        });
        self.layer = id;
        if let Some(id) = id {
            editor.select_layers(id, vec![id]);
        }
    }

    /// Show the result `by` after (or before) the one showing.
    fn flip(&mut self, editor: &mut Editor, by: isize) {
        if self.results.is_empty() {
            return;
        }
        self.shown = (self.shown as isize + by).rem_euclid(self.results.len() as isize) as usize;
        self.put(editor);
    }

    /// Close: keep the result showing as one undo step, or leave the
    /// document as it was. Whatever's still being made is stopped.
    fn finish(&mut self, editor: &mut Editor, keep: bool) {
        self.stop.store(true, Ordering::Relaxed);
        editor.end_group(keep);
    }

    /// Show the dialog. Returns `Some(true)` once OK'd and `Some(false)`
    /// if cancelled; either way it has finished with the document.
    pub fn show(&mut self, ctx: &egui::Context, editor: &mut Editor, theme: &Theme) -> Option<bool> {
        self.poll(ctx, editor);
        let (ready, working) = (self.patch.is_some(), self.working.is_some());
        let mut result = (self.accept && self.layer.is_some()).then_some(true);
        if self.accept && self.failed {
            result = Some(false);
        }
        let mut generate = false;
        let area = egui::Modal::default_area(egui::Id::new("generative-fill"))
            .anchor(egui::Align2::RIGHT_TOP, vec2(-320.0, 90.0));
        egui::Modal::new(egui::Id::new("generative-fill"))
            .area(area)
            .backdrop_color(Color32::TRANSPARENT)
            .show(ctx, |ui| {
                ui.set_width(340.0);
                ui.heading(NAME);
                ui.add_space(8.0);
                let prompt = ui.add(
                    TextEdit::singleline(&mut self.prompt)
                        .hint_text("What to change (empty: remove what's there)")
                        .desired_width(f32::INFINITY),
                );
                if !self.focused {
                    prompt.request_focus();
                    self.focused = true;
                }
                let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                let typed = prompt.lost_focus() && enter;
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let label = if self.results.is_empty() { "Generate" } else { "Generate Again" };
                    if ui.add_enabled(ready && !working, egui::Button::new(label)).clicked() || (typed && ready && !working) {
                        generate = true;
                    }
                    if !self.results.is_empty() {
                        if ui.button("◀").clicked() {
                            self.flip(editor, -1);
                        }
                        ui.label(format!("{} of {}", self.shown + 1, self.results.len()));
                        if ui.button("▶").clicked() {
                            self.flip(editor, 1);
                        }
                    }
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if working && ready {
                        ui.spinner();
                    }
                    let color = if self.failed { theme.red } else { theme.dark_foreground };
                    let status = match self.status.as_str() {
                        "" if self.results.is_empty() => "The first result takes about 15 seconds",
                        status => status,
                    };
                    ui.label(RichText::new(status).color(color));
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let shown = self.layer.is_some();
                    if ui.add_enabled(shown, egui::Button::new("OK")).clicked() || (enter && !typed && !prompt.has_focus() && shown) {
                        result = Some(true);
                    }
                    if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        result = Some(false);
                    }
                });
            });
        if generate {
            self.generate(ctx);
        }
        if let Some(keep) = result {
            self.finish(editor, keep);
        }
        result
    }
}

/// What the model sees of `image`, and what's filled: `selection`, or with
/// none, the empty canvas round the picture and a soft overlap with it, a
/// few of the model's pixels wide.
fn cut(image: &Raster, selection: Option<Selection>, profile: &ColorProfile) -> Result<Message, String> {
    let overlap = image.width().max(image.height()) as f32 / 200.0;
    let selection = selection.or_else(|| fill::empty(image, overlap)).ok_or(NOTHING)?;
    let patch = fill::patch_of_cells(image, profile, &selection, CELL, CELLS).map_err(|e| e.to_string())?;
    Ok(Message::Patch(Box::new((patch.ok_or(NOTHING)?, selection))))
}

/// The other models give the card up, since the transformer needs nearly
/// all of it. They load again when next used.
fn free_the_card() {
    crate::content_fill::unload();
    crate::denoise::unload();
    crate::face_selection::unload();
    crate::object_selection::unload();
    crate::select_subject::unload();
}

#[cfg(test)]
mod tests {
    use super::*;
    use omapix_engine::Document;

    /// The dialog's thread's end of the channel.
    type Thread = std::sync::mpsc::Sender<Result<Message, String>>;
    type Cut = (Patch, Selection);

    /// A grey document with a box selected, the dialog open on it, and the
    /// other end of the channel its thread would talk down.
    fn open() -> (Editor, GenerativeFill, Thread, Cut) {
        let image = Raster::new(600, 400, vec![[30000, 30000, 30000, 65535]; 600 * 400]);
        let doc = Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16);
        let mut editor = Editor::new(doc).unwrap();
        let selection = Selection::rectangle(600, 400, (100.0, 100.0), (200.0, 150.0));
        let patch = fill::patch_of_cells(&image, &ColorProfile::srgb(), &selection, CELL, CELLS).unwrap().unwrap();
        let (tx, rx) = channel();
        editor.begin_group(NAME);
        let dialog = GenerativeFill {
            prompt: String::new(),
            accept: false,
            profile: ColorProfile::srgb(),
            above: 0,
            patch: None,
            working: Some(rx),
            stop: Arc::default(),
            status: String::new(),
            failed: false,
            results: Vec::new(),
            shown: 0,
            layer: None,
            seed: 1,
            awaited: true,
            focused: false,
        };
        (editor, dialog, tx, (patch, selection))
    }

    /// A result all of one grey.
    fn result(dialog: &GenerativeFill, (patch, selection): &Cut, grey: f32) -> Result<Message, String> {
        let image = vec![grey; 3 * patch.width * patch.height];
        Ok(Message::Result(Box::new(fill::layer(0, NAME, patch, &image, &dialog.profile, selection).unwrap())))
    }

    fn shown(editor: &Editor) -> u16 {
        editor.doc.layer(editor.active).unwrap().pixels.get(150, 120)[0]
    }

    #[test]
    fn the_first_result_shows_as_a_masked_layer_and_the_arrows_swap_the_others_in() {
        let (mut editor, mut dialog, tx, patch) = open();
        let ctx = egui::Context::default();
        tx.send(Ok(Message::Status("Loading the model…".into()))).unwrap();
        dialog.poll(&ctx, &mut editor);
        assert_eq!((dialog.status.as_str(), editor.doc.layers.len()), ("Loading the model…", 1));

        // The first result goes above the layer that was selected.
        tx.send(result(&dialog, &patch, 0.25)).unwrap();
        dialog.poll(&ctx, &mut editor);
        assert_eq!(editor.doc.layers.len(), 2);
        let layer = editor.doc.layer(editor.active).unwrap();
        assert_eq!((layer.name.as_str(), Some(layer.id)), (NAME, dialog.layer));
        let mask = &layer.mask.as_ref().unwrap().pixels;
        assert_eq!((mask.get(150, 120), mask.get(50, 50)), (65535, 0));
        let first = shown(&editor);

        // The rest wait for the arrows, which go round.
        tx.send(result(&dialog, &patch, 0.5)).unwrap();
        tx.send(result(&dialog, &patch, 0.75)).unwrap();
        tx.send(Ok(Message::Done)).unwrap();
        dialog.poll(&ctx, &mut editor);
        assert!(dialog.working.is_none() && dialog.status.is_empty());
        assert_eq!((dialog.results.len(), dialog.shown, shown(&editor), editor.doc.layers.len()), (3, 0, first, 2));
        dialog.flip(&mut editor, 1);
        assert!(shown(&editor) > first && editor.doc.layers.len() == 2);
        dialog.flip(&mut editor, -2);
        assert_eq!(dialog.shown, 2);
        dialog.flip(&mut editor, 1);
        assert_eq!((dialog.shown, shown(&editor)), (0, first));

        // The first of the next lot is shown as it arrives.
        let (tx, rx) = channel();
        (dialog.working, dialog.awaited) = (Some(rx), true);
        tx.send(result(&dialog, &patch, 1.0)).unwrap();
        dialog.poll(&ctx, &mut editor);
        assert_eq!(dialog.shown, 3);
    }

    #[test]
    fn ok_keeps_the_result_showing_as_one_undo_step_and_cancel_leaves_no_trace() {
        let (mut editor, mut dialog, tx, patch) = open();
        let ctx = egui::Context::default();
        for grey in [0.25, 0.5] {
            tx.send(result(&dialog, &patch, grey)).unwrap();
        }
        dialog.poll(&ctx, &mut editor);
        dialog.flip(&mut editor, 1);
        let second = shown(&editor);
        dialog.finish(&mut editor, true);
        assert!(dialog.stop.load(Ordering::Relaxed));
        assert_eq!((editor.doc.layers.len(), shown(&editor), editor.undo_label()), (2, second, Some(NAME)));
        editor.undo();
        assert_eq!((editor.doc.layers.len(), editor.undo_label()), (1, None));

        let (mut editor, mut dialog, tx, patch) = open();
        let background = editor.active;
        tx.send(result(&dialog, &patch, 0.25)).unwrap();
        dialog.poll(&ctx, &mut editor);
        dialog.finish(&mut editor, false);
        assert_eq!((editor.doc.layers.len(), editor.active, editor.undo_label()), (1, background, None));
    }

    #[test]
    fn typing_goes_to_the_prompt_and_escape_cancels() {
        let (mut editor, mut dialog, tx, patch) = open();
        let (ctx, theme) = (egui::Context::default(), Theme::default());
        let frame = |dialog: &mut GenerativeFill, editor: &mut Editor, events: Vec<egui::Event>| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(1200.0, 800.0))),
                events,
                ..Default::default()
            };
            let mut result = None;
            let mut out = ctx.run_ui(input, |ui| result = dialog.show(ui.ctx(), editor, &theme));
            out.textures_delta.clear();
            result
        };
        let key = |key| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        // The prompt has the keyboard from the start.
        assert_eq!(frame(&mut dialog, &mut editor, vec![]), None);
        assert_eq!(frame(&mut dialog, &mut editor, vec![egui::Event::Text("a bare wall".into())]), None);
        assert_eq!(dialog.prompt, "a bare wall");
        // Enter with nothing made yet, and the patch not cut, does nothing.
        assert_eq!(frame(&mut dialog, &mut editor, vec![key(egui::Key::Enter)]), None);
        assert!(dialog.results.is_empty() && !dialog.failed);

        tx.send(result(&dialog, &patch, 0.25)).unwrap();
        assert_eq!(frame(&mut dialog, &mut editor, vec![]), None);
        assert_eq!(editor.doc.layers.len(), 2);
        assert_eq!(frame(&mut dialog, &mut editor, vec![key(egui::Key::Escape)]), Some(false));
        assert_eq!((editor.doc.layers.len(), editor.undo_label()), (1, None));
    }

    #[test]
    fn with_nothing_selected_the_empty_canvas_is_filled() {
        // A picture with room made on its right, as the Crop tool leaves.
        let pixels = (0..600 * 400).map(|i| if i % 600 < 456 { [30000, 30000, 30000, 65535] } else { [0; 4] }).collect();
        let image = Raster::new(600, 400, pixels);
        let Ok(Message::Patch(room)) = cut(&image, None, &ColorProfile::srgb()) else {
            panic!("nothing to fill");
        };
        let (patch, selection) = *room;
        let at = |x| selection.coverage.get(x, 200);
        assert_eq!((at(599), at(456), at(400)), (65535, 65535, 0));
        assert!(at(454) > 0 && at(454) < 65535, "{}", at(454));
        // The model's told what's missing, and makes that and the overlap.
        let (anew, empty) = (patch.cells(CELL), patch.empty_cells(CELL));
        assert!(empty.iter().any(|&e| e > 0.0) && empty.iter().zip(&anew).all(|(e, a)| a >= e));
        assert!(anew.iter().sum::<f32>() > empty.iter().sum::<f32>());

        // A selection is used as it is, room or no room.
        let chosen = Selection::rectangle(600, 400, (100.0, 100.0), (200.0, 150.0));
        let Ok(Message::Patch(boxed)) = cut(&image, Some(chosen.clone()), &ColorProfile::srgb()) else {
            panic!("nothing to fill");
        };
        assert_eq!(boxed.1.bounds(), chosen.bounds());

        // With neither, there's nothing to do.
        let solid = Raster::new(60, 40, vec![[30000, 30000, 30000, 65535]; 60 * 40]);
        assert!(matches!(cut(&solid, None, &ColorProfile::srgb()), Err(e) if e == NOTHING));
    }

    #[test]
    fn a_failure_is_said_in_the_dialog_and_generate_can_be_tried_again() {
        let (mut editor, mut dialog, tx, patch) = open();
        let ctx = egui::Context::default();
        tx.send(Ok(Message::Patch(Box::new(patch)))).unwrap();
        dialog.poll(&ctx, &mut editor);
        assert!(dialog.patch.is_some() && dialog.working.is_none());

        let (tx, rx) = channel();
        dialog.working = Some(rx);
        tx.send(Err("Not enough free GPU memory".into())).unwrap();
        dialog.poll(&ctx, &mut editor);
        assert!(dialog.failed && dialog.working.is_none());
        assert_eq!((dialog.status.as_str(), editor.doc.layers.len()), ("Not enough free GPU memory", 1));

        // A thread that dies says so too.
        let (tx, rx) = channel::<Result<Message, String>>();
        dialog.working = Some(rx);
        drop(tx);
        dialog.poll(&ctx, &mut editor);
        assert_eq!(dialog.status, "Generative Fill stopped unexpectedly");
        dialog.finish(&mut editor, true);
        assert_eq!(editor.undo_label(), None);
    }
}
