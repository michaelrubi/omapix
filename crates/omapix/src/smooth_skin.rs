//! Retouch › Smooth Skin… (docs/AI.md, feature 3): the skin the face
//! models find in what the active layer and those below it show, with its
//! blotches evened out and its pores kept (`omapix_engine::skin`), onto a
//! new Smooth Skin layer above, masked by the skin. The dialog shows a
//! 100 % box of the result, on a cheek to start with.

use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::sync::Arc;

use egui::{Pos2, RichText, Slider};
use omapix_engine::skin::{self, Smoothing};
use omapix_engine::Pixel;

use crate::editor::{Editor, Target};
use crate::face_selection::{FoundSkin, find_skin};
use crate::preview_box::{PreviewBox, SIDE};
use crate::theme::Theme;

/// Which part of the image a sample is, and at which blurs (as bits).
type Key = ((u32, u32), [u32; 2]);

/// The preview box smoothed: before and after, and how much of each pixel
/// is skin.
struct Sample {
    key: Key,
    before: Vec<Pixel>,
    after: Vec<Pixel>,
    skin: Vec<f32>,
}

pub struct SmoothSkin {
    pub smoothing: Smoothing,
    /// The layer it goes above.
    layer: u64,
    finding: Option<Receiver<Result<FoundSkin, String>>>,
    found: Option<Arc<FoundSkin>>,
    /// Where the preview box looks.
    centre: Pos2,
    sample: Option<Sample>,
    sampling: Option<Receiver<Sample>>,
    /// The preview box as shown, after and before, and what it was made
    /// from: the sample (none while there's none for this part of the
    /// image) and the amount (as bits).
    shown: Option<(egui::TextureHandle, egui::TextureHandle, (Option<Key>, u32))>,
}

impl SmoothSkin {
    /// Start finding the skin in what the active layer and those below it
    /// show.
    pub fn open(ctx: &egui::Context, editor: &Editor, smoothing: Smoothing) -> Result<Self, String> {
        let index = editor.active_index().ok_or("Select a layer first")?;
        let doc = editor.doc.clone();
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(find_skin(doc.composite_current_and_below(index), &doc.profile));
            ctx.request_repaint();
        });
        Ok(Self {
            smoothing,
            layer: editor.active,
            finding: Some(rx),
            found: None,
            centre: editor.canvas.view_centre(),
            sample: None,
            sampling: None,
            shown: None,
        })
    }

    /// Show the dialog. Returns `Some(true)` once OK'd, `Some(false)` if
    /// cancelled, and an error if the skin couldn't be found.
    pub fn show(&mut self, ctx: &egui::Context, editor: &Editor, theme: &Theme) -> Result<Option<bool>, String> {
        if let Some(rx) = &self.finding {
            match rx.try_recv() {
                Ok(found) => {
                    let found = found?;
                    self.centre = found.cheek;
                    self.found = Some(Arc::new(found));
                    self.finding = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => return Err("Smooth Skin stopped unexpectedly".into()),
            }
        }
        if let Some(rx) = &self.sampling {
            match rx.try_recv() {
                Ok(sample) => {
                    self.sample = Some(sample);
                    self.sampling = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => return Err("Smooth Skin stopped unexpectedly".into()),
            }
        }
        let mut result = None;
        egui::Modal::new(egui::Id::new("smooth-skin")).show(ctx, |ui| {
            ui.set_width(300.0);
            ui.heading("Smooth Skin");
            ui.add_space(8.0);
            self.preview_box(ui, editor);
            let s = &mut self.smoothing;
            for (label, value) in [("Amount", &mut s.amount), ("Smoothness", &mut s.smoothness), ("Detail", &mut s.detail)] {
                ui.horizontal(|ui| {
                    ui.label(label);
                    let slider = Slider::new(value, 0.0..=100.0).fixed_decimals(0);
                    ui.add(if label == "Amount" { slider.suffix(" %") } else { slider });
                });
            }
            ui.add_space(4.0);
            let ready = self.found.is_some();
            let hint = if ready { "Onto a new Smooth Skin layer, masked to the skin." } else { "Finding skin…" };
            ui.label(RichText::new(hint).color(theme.dark_foreground));
            ui.add_space(12.0);
            ui.horizontal(|ui| {
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

    /// The 100 % preview box: the result at the amount, and the image as it
    /// was while the button is held on it.
    fn preview_box(&mut self, ui: &mut egui::Ui, editor: &Editor) {
        let Some(found) = self.found.clone() else {
            ui.add_space(SIDE as f32 + 8.0);
            return;
        };
        let image = &found.image;
        let preview = PreviewBox::new(ui, &mut self.centre, (image.width(), image.height()));
        let radii = self.smoothing.radii(found.iod);
        let key = (preview.corner, radii.map(f32::to_bits));
        // Smooth the box once it stops moving, and again as the sliders
        // change, the latest settings each time the last is done.
        if !preview.dragged() && self.sampling.is_none() && self.sample.as_ref().is_none_or(|s| s.key != key) {
            self.sampling = Some(sample(ui.ctx(), &found, key, preview.size, radii));
        }
        // Until then, the last one done here, or the image as it is.
        let sample = self.sample.as_ref().filter(|s| s.key.0 == preview.corner);
        let amount = self.smoothing.amount / 100.0;
        let made = (sample.map(|s| s.key), amount.to_bits());
        if self.shown.as_ref().is_none_or(|s| s.2 != made) {
            let ((x, y), (w, h)) = (preview.corner, preview.size);
            let before: Vec<Pixel> = match sample {
                Some(s) => s.before.clone(),
                None => (0..h).flat_map(|py| (0..w).map(move |px| image.get(x + px, y + py))).collect(),
            };
            let after: Vec<Pixel> = match sample {
                Some(s) => (s.before.iter().zip(&s.after).zip(&s.skin))
                    .map(|((b, a), &k)| {
                        let mix = |c: usize| (f32::from(b[c]) + (f32::from(a[c]) - f32::from(b[c])) * k * amount).round() as u16;
                        [mix(0), mix(1), mix(2), b[3]]
                    })
                    .collect(),
                None => before.clone(),
            };
            let texture = |name, pixels: &[Pixel]| preview.texture(ui.ctx(), editor, name, pixels);
            self.shown = Some((texture("smooth-skin-after", &after), texture("smooth-skin-before", &before), made));
        }
        if let Some((after, before, _)) = &self.shown {
            preview.show(ui, after, before);
        }
        ui.add_space(8.0);
    }

    /// Add the Smooth Skin layer above the layer it was opened on (or the
    /// active one, if that's gone), as one undo step.
    pub fn apply(&self, ctx: &egui::Context, editor: &mut Editor) {
        let (Some(found), Some(index)) = (self.found.clone(), editor.doc.index_of(self.layer).or(editor.active_index())) else {
            return;
        };
        let smoothing = self.smoothing;
        editor.target = Target::Pixels;
        editor.edit_in_background(
            "Smooth Skin",
            move |doc, active| {
                if let Some(id) = skin::add_layer(doc, index, &found.image, &found.skin, found.iod, &smoothing) {
                    *active = id;
                }
            },
            ctx,
        );
    }
}

/// The `size` box at `key`'s corner smoothed with blurs of `radii`, on a
/// thread.
fn sample(ctx: &egui::Context, found: &Arc<FoundSkin>, key: Key, (w, h): (u32, u32), radii: [f32; 2]) -> Receiver<Sample> {
    let (found, ctx) = (Arc::clone(found), ctx.clone());
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let (x, y) = key.0;
        let at = |i: usize| (x + i as u32 % w, y + i as u32 / w);
        let n = (w * h) as usize;
        let _ = tx.send(Sample {
            key,
            before: (0..n).map(at).map(|(px, py)| found.image.get(px, py)).collect(),
            after: skin::smooth(&found.image, &found.skin, [x, y, w, h], radii),
            skin: (0..n).map(at).map(|(px, py)| found.skin.at(px, py)).collect(),
        });
        ctx.request_repaint();
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, vec2};
    use omapix_engine::{ColorProfile, Document, Raster, Selection};

    /// Skin-coloured with a soft dark blotch round (200, 150), all skin,
    /// eyes 300 px apart, and the dialog open on it with the skin found.
    fn dialog() -> (Editor, SmoothSkin) {
        let pixels = (0..400 * 300)
            .map(|i| {
                let d = ((i % 400) as f32 - 200.0).hypot((i / 400) as f32 - 150.0);
                let v = 45000 - (6000.0 * (-(d / 12.0).powi(2)).exp()) as u16;
                [v, v - 8000, v - 12000, 65535]
            })
            .collect();
        let image = Raster::new(400, 300, pixels);
        let doc = Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16);
        let editor = Editor::new(doc).unwrap();
        let dialog = SmoothSkin {
            smoothing: Smoothing::default(),
            layer: editor.active,
            finding: None,
            found: Some(Arc::new(FoundSkin {
                skin: Selection::all(400, 300),
                image,
                iod: 300.0,
                cheek: pos2(200.0, 150.0),
                nose: pos2(200.0, 150.0),
            })),
            centre: pos2(200.0, 150.0),
            sample: None,
            sampling: None,
            shown: None,
        };
        (editor, dialog)
    }

    #[test]
    fn the_dialog_previews_the_box_at_the_amount_and_ok_is_enter() {
        let (editor, mut dialog) = dialog();
        let (ctx, theme) = (egui::Context::default(), Theme::default());
        let frame = |dialog: &mut SmoothSkin, events: Vec<egui::Event>| {
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
        let wait = |dialog: &mut SmoothSkin| {
            while dialog.sampling.is_some() || dialog.sample.is_none() {
                assert_eq!(frame(dialog, vec![]), Ok(None));
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            frame(dialog, vec![]).unwrap();
        };
        wait(&mut dialog);
        // The box round the blotch, smoothed, and shown at 70 %.
        let sample = dialog.sample.as_ref().unwrap();
        assert_eq!(sample.key.0, (80, 30));
        let middle = (120 * 240 + 120) as usize;
        assert!(sample.after[middle][0] > sample.before[middle][0] + 1500);
        assert_eq!(dialog.shown.as_ref().unwrap().2, (Some(sample.key), 0.7f32.to_bits()));
        // The amount only mixes; the blurs smooth again.
        dialog.smoothing.amount = 40.0;
        frame(&mut dialog, vec![]).unwrap();
        assert!(dialog.sampling.is_none());
        assert_eq!(dialog.shown.as_ref().unwrap().2.1, 0.4f32.to_bits());
        dialog.smoothing.smoothness = 80.0;
        frame(&mut dialog, vec![]).unwrap();
        assert!(dialog.sampling.is_some());
        wait(&mut dialog);

        let enter = egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        assert_eq!(frame(&mut dialog, vec![enter]), Ok(Some(true)));
    }

    #[test]
    fn ok_adds_a_smooth_skin_layer_above_in_one_undo_step() {
        let (mut editor, dialog) = dialog();
        let ctx = egui::Context::default();
        let before = editor.doc.composite().get(200, 150);
        dialog.apply(&ctx, &mut editor);
        while editor.busy().is_some() {
            std::thread::sleep(std::time::Duration::from_millis(1));
            editor.update(&ctx);
        }
        assert_eq!(editor.doc.layers.len(), 2);
        let layer = editor.doc.layer(editor.active).unwrap();
        assert_eq!((layer.name.as_str(), layer.opacity), ("Smooth Skin", 0.7));
        assert!(layer.mask.is_some());
        assert!(editor.doc.composite().get(200, 150)[0] > before[0] + 1000);
        assert_eq!(editor.undo_label(), Some("Smooth Skin"));
        editor.undo();
        assert_eq!(editor.doc.layers.len(), 1);
    }

    /// A look at real photos: for `OMAPIX_FACE_PHOTO` (a photo, or a
    /// folder of them), before and after at 100 % amount, the largest face
    /// as `<photo>-face.png` and 600 px round its cheek, at 100 %, as
    /// `<photo>-100.png`, in `OMAPIX_FACE_OUT`. `SMOOTHNESS` and `DETAIL`
    /// set the sliders. Needs the models (scripts/fetch-models.sh) and
    /// ImageMagick.
    #[test]
    #[ignore]
    fn smooth_skin_in_photos() {
        let (Ok(photos), Ok(out)) = (std::env::var("OMAPIX_FACE_PHOTO"), std::env::var("OMAPIX_FACE_OUT")) else {
            return;
        };
        let photos = std::path::Path::new(&photos);
        let mut paths: Vec<_> = match std::fs::read_dir(photos) {
            Ok(dir) => dir.map(|e| e.unwrap().path()).collect(),
            Err(_) => vec![photos.to_owned()],
        };
        paths.sort();
        let slider = |name: &str| std::env::var(name).map_or(50.0, |v| v.parse().unwrap());
        let smoothing = Smoothing {
            amount: 100.0,
            smoothness: slider("SMOOTHNESS"),
            detail: slider("DETAIL"),
        };
        for path in paths {
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            let Ok(mut doc) = omapix_engine::io::load(&path) else { continue };
            let t = std::time::Instant::now();
            let found = match find_skin(doc.composite(), &doc.profile) {
                Ok(found) => found,
                Err(e) => {
                    eprintln!("{name}: {e}");
                    continue;
                }
            };
            let found_in = t.elapsed();
            let t = std::time::Instant::now();
            let top = doc.layers.len() - 1;
            skin::add_layer(&mut doc, top, &found.image, &found.skin, found.iod, &smoothing);
            let after = doc.composite();
            eprintln!("{name}: iod {:.0}, radii {:?}, found in {found_in:?}, smoothed in {:?}", found.iod, smoothing.radii(found.iod), t.elapsed());
            let before = &found.image;
            let (iw, ih) = (before.width() as f32, before.height() as f32);
            // The face: about 3 × the distance between the eyes each way
            // round the cheek, at most 900 px across.
            let r = found.iod * 1.6;
            let (cx, cy) = (found.cheek.x, found.cheek.y);
            let face = [(cx - r).max(0.0), (cy - r).max(0.0), (cx + r).min(iw), (cy + r).min(ih)];
            let close = [(cx - 300.0).max(0.0), (cy - 300.0).max(0.0), (cx + 300.0).min(iw), (cy + 300.0).min(ih)];
            for (suffix, [x0, y0, x1, y1]) in [("face", face), ("100", close)] {
                let step = ((x1 - x0) / 900.0).max(1.0);
                let (ow, oh) = (((x1 - x0) / step) as u32, ((y1 - y0) / step) as u32);
                let mut ppm = format!("P6 {} {} 255\n", ow * 2, oh).into_bytes();
                for y in 0..oh {
                    for half in [before, &after] {
                        for x in 0..ow {
                            let p = half.get((x0 + x as f32 * step) as u32, (y0 + y as f32 * step) as u32);
                            ppm.extend([0, 1, 2].map(|c| (p[c] >> 8) as u8));
                        }
                    }
                }
                let ppm_path = format!("{out}/{name}-{suffix}.ppm");
                std::fs::write(&ppm_path, ppm).unwrap();
                std::process::Command::new("magick").args([&ppm_path, &format!("{out}/{name}-{suffix}.png")]).status().unwrap();
                std::fs::remove_file(ppm_path).unwrap();
            }
        }
    }
}
