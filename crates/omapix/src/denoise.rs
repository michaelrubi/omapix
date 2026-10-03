//! Filter › Noise › Denoise… (docs/AI.md, milestone 4): NIND takes the
//! noise out of the image as the active layer and those below it make it,
//! onto a new Denoise layer above, so its opacity is the strength and a
//! mask keeps it off what should stay grainy. The dialog shows a 100 %
//! box of the result, and Luminance and Color decide how much of each kind
//! of noise goes.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::sync::{Arc, Mutex};

use egui::{Pos2, RichText, Slider};
use omapix_ai::denoise::{Nind, SIZE};
use omapix_engine::denoise::{self, Amounts};
use omapix_engine::{ColorProfile, Layer, Raster};

use crate::editor::Editor;
use crate::preview_box::{PreviewBox, SIDE};
use crate::theme::Theme;

/// Loaded on first use, and kept until Generative Fill needs the card.
static MODEL: Mutex<Option<Nind>> = Mutex::new(None);

/// Drop the model, giving its GPU memory back. It loads again when next
/// used.
pub fn unload() {
    if let Ok(mut model) = MODEL.lock() {
        *model = None;
    }
}

/// NIND's answer for `square`, loading it the first time.
fn model(square: &[f32]) -> Result<Vec<f32>, String> {
    let mut model = MODEL.lock().map_err(|e| e.to_string())?;
    let nind = match &mut *model {
        Some(nind) => nind,
        None => model.insert(Nind::load()?),
    };
    nind.denoise(square)
}

/// What the preview box shows: the box at `corner` of the image, in the
/// document's colours, and before and after denoising in sRGB, ready for
/// the amounts.
struct Sample {
    corner: (u32, u32),
    image: Raster,
    before: Raster,
    after: Vec<[f32; 3]>,
}

pub struct Denoise {
    /// How much luminance and colour noise goes, 0–100.
    pub luminance: f32,
    pub color: f32,
    /// The layer whose Current & Below is denoised.
    layer: u64,
    profile: ColorProfile,
    /// Current & Below, once composited.
    source: Option<Arc<Raster>>,
    opening: Option<Receiver<Result<Arc<Raster>, String>>>,
    /// Where the preview box looks.
    centre: Pos2,
    sample: Option<Sample>,
    sampling: Option<Receiver<Result<Sample, String>>>,
    shown: Option<Shown>,
}

/// The preview box as shown, after and before, and what it was made from:
/// the corner, and the amounts (as bits), unset while the box isn't
/// denoised yet.
type Shown = (egui::TextureHandle, egui::TextureHandle, (u32, u32), [u32; 2]);

impl Denoise {
    /// Start compositing what the active layer and those below it show.
    pub fn open(ctx: &egui::Context, editor: &Editor, luminance: f32, color: f32) -> Result<Self, String> {
        let index = editor.active_index().ok_or("Select a layer first")?;
        let doc = editor.doc.clone();
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Ok(Arc::new(doc.composite_current_and_below(index))));
            ctx.request_repaint();
        });
        Ok(Self {
            luminance,
            color,
            layer: editor.active,
            profile: editor.doc.profile.clone(),
            source: None,
            opening: Some(rx),
            centre: editor.canvas.view_centre(),
            sample: None,
            sampling: None,
            shown: None,
        })
    }

    fn amounts(&self) -> Amounts {
        Amounts {
            luminance: self.luminance / 100.0,
            color: self.color / 100.0,
        }
    }

    /// Show the dialog. Returns `Some(true)` once OK'd, `Some(false)` if
    /// cancelled, and an error if something went wrong.
    pub fn show(&mut self, ctx: &egui::Context, editor: &Editor, theme: &Theme) -> Result<Option<bool>, String> {
        if let Some(rx) = &self.opening {
            match rx.try_recv() {
                Ok(source) => {
                    self.source = Some(source?);
                    self.opening = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => return Err("Denoise stopped unexpectedly".into()),
            }
        }
        if let Some(rx) = &self.sampling {
            match rx.try_recv() {
                Ok(sample) => {
                    self.sample = Some(sample?);
                    self.sampling = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => return Err("Denoise stopped unexpectedly".into()),
            }
        }
        let mut result = None;
        egui::Modal::new(egui::Id::new("denoise")).show(ctx, |ui| {
            ui.set_width(300.0);
            ui.heading("Denoise");
            ui.add_space(8.0);
            self.preview_box(ui, editor);
            ui.horizontal(|ui| {
                ui.label("Luminance");
                ui.add(Slider::new(&mut self.luminance, 0.0..=100.0).suffix(" %").fixed_decimals(0));
            });
            ui.horizontal(|ui| {
                ui.label("Color");
                ui.add(Slider::new(&mut self.color, 0.0..=100.0).suffix(" %").fixed_decimals(0));
            });
            ui.add_space(4.0);
            let hint = if self.sampling.is_some() { "Denoising the preview…" } else { "AI (NIND), onto a new Denoise layer." };
            ui.label(RichText::new(hint).color(theme.dark_foreground));
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                let ready = self.source.is_some();
                if ui.add_enabled(ready, egui::Button::new("OK")).clicked() || (ready && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                    result = Some(true);
                }
                if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    result = Some(false);
                }
            });
        });
        Ok(result)
    }

    /// The 100 % preview box: the result, and the image as it was while
    /// the button is held on it.
    fn preview_box(&mut self, ui: &mut egui::Ui, editor: &Editor) {
        let Some(source) = self.source.clone() else {
            ui.add_space(SIDE as f32 + 8.0);
            return;
        };
        let preview = PreviewBox::new(ui, &mut self.centre, (source.width(), source.height()));
        let (corner, (w, h)) = (preview.corner, preview.size);
        // Denoise round the box once it stops moving.
        if !preview.dragged() && self.sampling.is_none() && self.sample.as_ref().is_none_or(|s| s.corner != corner) {
            self.sampling = Some(sample(ui.ctx(), &source, &self.profile, corner));
        }
        let amounts = [self.luminance, self.color].map(f32::to_bits);
        let ready = self.sample.as_ref().filter(|s| s.corner == corner);
        let stale = self.shown.as_ref().is_none_or(|s| s.2 != corner || (ready.is_some() && s.3 != amounts));
        if stale {
            let before = crop(&source, corner.0, corner.1, w, h);
            let after = match ready.map(|s| denoise::finish(&s.image, &self.profile, &s.before, &s.after, self.amounts())) {
                Some(Ok(after)) => after,
                _ => before.clone(),
            };
            let key = if ready.is_some() { amounts } else { [u32::MAX; 2] };
            let texture = |name, pixels: &Raster| preview.texture(ui.ctx(), editor, name, pixels.pixels());
            self.shown = Some((texture("denoise-after", &after), texture("denoise-before", &before), corner, key));
        }
        if let Some((after, before, ..)) = &self.shown {
            preview.show(ui, after, before);
        }
        ui.add_space(8.0);
    }

    /// Start denoising the whole of Current & Below.
    pub fn apply(&self, ctx: &egui::Context, job: &mut Denoising) {
        let Some(source) = self.source.clone() else {
            return;
        };
        let (profile, amounts) = (self.profile.clone(), self.amounts());
        let progress = Arc::new((AtomicUsize::new(0), AtomicUsize::new(0)));
        let (tx, rx) = channel();
        let (ctx, shared) = (ctx.clone(), Arc::clone(&progress));
        std::thread::spawn(move || {
            let result = denoise::denoise(&source, &profile, SIZE, amounts, model, |done, total| {
                shared.0.store(done, Ordering::Relaxed);
                shared.1.store(total, Ordering::Relaxed);
                ctx.request_repaint();
            });
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        *job = Denoising {
            running: Some((rx, progress, self.layer)),
        };
    }
}

/// The preview box at `corner` of `source` denoised, on a thread, with a
/// model square's worth of the image round it for the model to see.
fn sample(ctx: &egui::Context, source: &Arc<Raster>, profile: &ColorProfile, corner: (u32, u32)) -> Receiver<Result<Sample, String>> {
    let (source, profile) = (Arc::clone(source), profile.clone());
    let (tx, rx) = channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let result = (|| {
            let (iw, ih) = (source.width(), source.height());
            let (w, h) = (SIDE.min(iw), SIDE.min(ih));
            let side = SIZE as u32;
            let around = |c: u32, box_side: u32, size: u32| {
                let s = side.min(size);
                ((c + box_side / 2).saturating_sub(s / 2).min(size - s), s)
            };
            let ((x0, cw), (y0, ch)) = (around(corner.0, w, iw), around(corner.1, h, ih));
            let image = crop(&source, x0, y0, cw, ch);
            let before = denoise::srgb(&image, &profile)?;
            let after = denoise::run(&before, SIZE, model, |_, _| {})?;
            // Just the box.
            let (bx, by) = (corner.0 - x0, corner.1 - y0);
            let at = |x: u32, y: u32| (y * cw + x) as usize;
            let boxed = (0..h).flat_map(|y| (0..w).map(move |x| at(bx + x, by + y)));
            Ok(Sample {
                corner,
                image: crop(&image, bx, by, w, h),
                before: crop(&before, bx, by, w, h),
                after: boxed.map(|i| after[i]).collect(),
            })
        })();
        let _ = tx.send(result);
        ctx.request_repaint();
    });
    rx
}

/// `w` × `h` of `raster` from (`x`, `y`).
fn crop(raster: &Raster, x: u32, y: u32, w: u32, h: u32) -> Raster {
    let pixels = (0..h).flat_map(|py| (0..w).map(move |px| (px, py))).map(|(px, py)| raster.get(x + px, y + py)).collect();
    Raster::new(w, h, pixels)
}

/// The whole image being denoised: the result, how many model squares are
/// done of how many, and the layer it goes above.
type Running = (Receiver<Result<Raster, String>>, Arc<(AtomicUsize, AtomicUsize)>, u64);

#[derive(Default)]
pub struct Denoising {
    running: Option<Running>,
}

impl Denoising {
    /// Add the Denoise layer once it's made, above the layer it was made
    /// for (or the active one, if that's gone), as one undo step. Returns
    /// what went wrong, if anything.
    pub fn poll(&mut self, editor: &mut Editor) -> Option<String> {
        let (rx, _, layer) = self.running.as_ref()?;
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err("Denoise stopped unexpectedly".into()),
        };
        let layer = *layer;
        self.running = None;
        let denoised = match result {
            Ok(denoised) => denoised,
            Err(e) => return Some(e),
        };
        let above = editor.doc.index_of(layer).or(editor.active_index()).unwrap_or(0);
        editor.edit("Denoise", move |doc, active| {
            let id = doc.next_layer_id();
            doc.insert_above(above, Layer::from_raster(id, "Denoise", &denoised));
            *active = id;
        });
        None
    }

    /// How many model squares are done, of how many, while it runs.
    pub fn busy(&self) -> Option<(usize, usize)> {
        let (_, progress, _) = self.running.as_ref()?;
        Some((progress.0.load(Ordering::Relaxed), progress.1.load(Ordering::Relaxed)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::vec2;
    use omapix_engine::Document;

    #[test]
    fn the_denoised_image_arrives_as_a_layer_above_the_one_it_was_made_for_in_one_undo_step() {
        let image = Raster::new(300, 200, vec![[30000, 30000, 30000, 65535]; 300 * 200]);
        let doc = Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16);
        let mut editor = Editor::new(doc).unwrap();
        let below = editor.active;
        let layers = editor.doc.layers.len();

        let (tx, rx) = channel();
        let progress = Arc::new((AtomicUsize::new(3), AtomicUsize::new(8)));
        let mut job = Denoising {
            running: Some((rx, progress, below)),
        };
        assert_eq!(job.poll(&mut editor), None);
        assert_eq!(job.busy(), Some((3, 8)));
        let denoised = Raster::new(300, 200, vec![[31000, 30000, 29000, 65535]; 300 * 200]);
        tx.send(Ok(denoised)).unwrap();
        assert_eq!(job.poll(&mut editor), None);
        assert_eq!(job.busy(), None);
        assert_eq!(editor.doc.layers.len(), layers + 1);
        let added = editor.doc.layer(editor.active).unwrap();
        assert_eq!(added.name, "Denoise");
        assert_eq!(added.pixels.get(10, 10), [31000, 30000, 29000, 65535]);
        assert_eq!(editor.doc.index_of(editor.active), editor.doc.index_of(below).map(|i| i + 1));
        assert_eq!(editor.undo_label(), Some("Denoise"));

        // Failures come back as messages.
        let (tx, rx) = channel();
        job.running = Some((rx, Arc::new((AtomicUsize::new(0), AtomicUsize::new(0))), below));
        tx.send(Err("no model".into())).unwrap();
        assert_eq!(job.poll(&mut editor), Some("no model".into()));
    }

    #[test]
    fn the_dialog_previews_at_the_amounts_and_ok_is_enter() {
        // Noisy grey, and a preview already denoised to flat grey (as if
        // by the model), so no model is needed.
        let pixels = (0..400 * 300).map(|i: u32| {
            let v = 30000 + (i.wrapping_mul(2654435761) >> 20) as u16;
            [v, v, v, 65535]
        });
        let image = Raster::new(400, 300, pixels.collect());
        let doc = Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16);
        let editor = Editor::new(doc).unwrap();
        let ctx = egui::Context::default();
        let mut dialog = Denoise::open(&ctx, &editor, 80.0, 100.0).unwrap();
        dialog.opening.take().unwrap().recv().unwrap().unwrap();
        let source = Arc::new(image);
        // Where the box goes round the middle of the view.
        let at = |c: f32, size: u32| (c - SIDE as f32 / 2.0).clamp(0.0, (size - SIDE) as f32).round() as u32;
        let corner = (at(dialog.centre.x, 400), at(dialog.centre.y, 300));
        let boxed = crop(&source, corner.0, corner.1, SIDE, SIDE);
        dialog.sample = Some(Sample {
            corner,
            before: denoise::srgb(&boxed, &ColorProfile::srgb()).unwrap(),
            after: vec![[30000.0 / 65535.0; 3]; (SIDE * SIDE) as usize],
            image: boxed,
        });
        dialog.source = Some(source);
        let theme = Theme::default();
        let frame = |dialog: &mut Denoise, events: Vec<egui::Event>| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 800.0))),
                events,
                ..Default::default()
            };
            let mut result = Ok(None);
            let mut out = ctx.run_ui(input, |ui| result = dialog.show(ui.ctx(), &editor, &theme));
            out.textures_delta.clear();
            result
        };
        assert_eq!(frame(&mut dialog, vec![]), Ok(None));
        assert_eq!(frame(&mut dialog, vec![]), Ok(None));
        assert!(dialog.sampling.is_none(), "the preview was ready");
        assert_eq!(dialog.shown.as_ref().map(|s| s.3), Some([80f32, 100.0].map(f32::to_bits)));
        dialog.luminance = 50.0;
        frame(&mut dialog, vec![]).unwrap();
        assert_eq!(dialog.shown.as_ref().map(|s| s.3), Some([50f32, 100.0].map(f32::to_bits)));
        let enter = egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        assert_eq!(frame(&mut dialog, vec![enter]), Ok(Some(true)));
        assert_eq!(dialog.amounts(), Amounts { luminance: 0.5, color: 1.0 });
    }

    /// The dialog and the job on a real photo, with the model (darktable
    /// installs it): `OMAPIX_DENOISE_PHOTO=photo.jpg cargo test --release
    /// -p omapix denoise_a_photo -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn denoise_a_photo() {
        let Ok(photo) = std::env::var("OMAPIX_DENOISE_PHOTO") else { return };
        let doc = omapix_engine::io::load(std::path::Path::new(&photo)).unwrap();
        let mut editor = Editor::new(doc).unwrap();
        let ctx = egui::Context::default();
        let wait = |ready: &dyn Fn() -> bool| {
            let t = std::time::Instant::now();
            while !ready() {
                std::thread::sleep(std::time::Duration::from_millis(20));
                assert!(t.elapsed().as_secs() < 120);
            }
            t.elapsed()
        };
        let mut dialog = Denoise::open(&ctx, &editor, 80.0, 100.0).unwrap();
        let rx = dialog.opening.take().unwrap();
        let source = rx.recv().unwrap().unwrap();
        let t = std::time::Instant::now();
        let sample = sample(&ctx, &source, &dialog.profile, ((source.width() - SIDE) / 2, (source.height() - SIDE) / 2)).recv().unwrap().unwrap();
        eprintln!("preview in {:?} (loading the model too)", t.elapsed());
        let t = std::time::Instant::now();
        let again = super::sample(&ctx, &source, &dialog.profile, (0, 0)).recv().unwrap().unwrap();
        eprintln!("preview elsewhere in {:?}", t.elapsed());
        assert_eq!(sample.image.width(), SIDE.min(source.width()));
        assert_eq!(again.after.len(), again.image.pixels().len());
        dialog.source = Some(source);
        let mut job = Denoising::default();
        dialog.apply(&ctx, &mut job);
        let layers = editor.doc.layers.len();
        let took = wait(&|| job.busy().is_some_and(|(done, total)| total > 0 && done == total));
        while job.busy().is_some() {
            assert_eq!(job.poll(&mut editor), None);
        }
        eprintln!("denoised in {took:?}");
        assert_eq!(editor.doc.layers.len(), layers + 1);
        assert_eq!(editor.doc.layer(editor.active).unwrap().name, "Denoise");
    }
}
