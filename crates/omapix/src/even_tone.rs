//! Retouch › Even Tone… (docs/AI.md, feature 5): the skin the face models
//! find in what the active layer and those below it show, with its darker
//! patches dodged and its lighter ones burned (`omapix_engine::tone`), as a
//! Dodge & Burn group above with its masks filled in. The dialog's box
//! shows the face small, evened, since evenness is judged from a distance.

use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::sync::Arc;

use egui::{Pos2, RichText, Slider, pos2};
use omapix_engine::tone::{self, Evening, Tone};
use omapix_engine::{ColorProfile, Raster, Selection};

use crate::editor::{Editor, Target};
use crate::face_selection::find_skin;
use crate::preview_box::{PreviewBox, SIDE};
use crate::theme::Theme;

/// What the group is made from: the skin, the distance between the eyes of
/// its largest face, the small copy of the image evened, and that face's
/// nose in the copy.
struct Found {
    skin: Selection,
    iod: f32,
    tone: Tone,
    nose: Pos2,
}

/// The blurs a copy was evened with, as bits.
type Key = [u32; 2];

/// What a preview was made from: the box's corner, the blurs and the
/// amount (as bits).
type Made = ((u32, u32), Key, u32);

pub struct EvenTone {
    pub evening: Evening,
    /// The layer it goes above.
    layer: u64,
    finding: Option<Receiver<Result<Found, String>>>,
    /// The skin and the distance between the eyes, once found.
    skin: Option<(Arc<Selection>, f32)>,
    /// The copy and the blurs it was evened with: away while it's evened
    /// again with others.
    tone: Option<(Tone, Key)>,
    redoing: Option<Receiver<(Tone, Key)>>,
    /// The copy's size, and where in it the preview box looks.
    size: (u32, u32),
    centre: Pos2,
    /// The preview box as shown, after and before, and what it was made
    /// from.
    shown: Option<(egui::TextureHandle, egui::TextureHandle, Made)>,
}

impl EvenTone {
    /// Start finding the skin in what the active layer and those below it
    /// show.
    pub fn open(ctx: &egui::Context, editor: &Editor, evening: Evening) -> Result<Self, String> {
        let index = editor.active_index().ok_or("Select a layer first")?;
        let doc = editor.doc.clone();
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(find(doc.composite_current_and_below(index), &doc.profile, &evening));
            ctx.request_repaint();
        });
        Ok(Self {
            evening,
            layer: editor.active,
            finding: Some(rx),
            skin: None,
            tone: None,
            redoing: None,
            size: (0, 0),
            centre: Pos2::ZERO,
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
                    self.size = (found.tone.width, found.tone.height);
                    self.centre = found.nose;
                    self.tone = Some((found.tone, self.evening.radii(found.iod).map(f32::to_bits)));
                    self.skin = Some((Arc::new(found.skin), found.iod));
                    self.finding = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => return Err("Even Tone stopped unexpectedly".into()),
            }
        }
        if let Some(rx) = &self.redoing {
            match rx.try_recv() {
                Ok(tone) => {
                    self.tone = Some(tone);
                    self.redoing = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => return Err("Even Tone stopped unexpectedly".into()),
            }
        }
        // Even the copy again as Size changes, the latest setting each time
        // the last is done.
        let radii = self.skin.as_ref().map(|(_, iod)| self.evening.radii(*iod));
        let key = radii.map(|r| r.map(f32::to_bits));
        if let (Some(radii), Some((tone, _))) = (radii, self.tone.take_if(|(_, with)| Some(*with) != key)) {
            self.redoing = Some(even(ctx, tone, radii));
        }
        let mut result = None;
        egui::Modal::new(egui::Id::new("even-tone")).show(ctx, |ui| {
            ui.set_width(300.0);
            ui.heading("Even Tone");
            ui.add_space(8.0);
            self.preview_box(ui, editor);
            let e = &mut self.evening;
            for (label, value) in [("Amount", &mut e.amount), ("Size", &mut e.size)] {
                ui.horizontal(|ui| {
                    ui.label(label);
                    let slider = Slider::new(value, 0.0..=100.0).fixed_decimals(0);
                    ui.add(if label == "Amount" { slider.suffix(" %") } else { slider });
                });
            }
            ui.add_space(4.0);
            let hint = if self.skin.is_some() { "As a Dodge & Burn group, its masks filled in on the skin." } else { "Finding skin…" };
            ui.label(RichText::new(hint).color(theme.dark_foreground));
            ui.add_space(12.0);
            // Not while the copy's away being evened again.
            let ready = self.tone.is_some();
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

    /// The preview box: the face small, evened at the amount, and as it
    /// was while the button is held on it.
    fn preview_box(&mut self, ui: &mut egui::Ui, editor: &Editor) {
        if self.skin.is_none() {
            ui.add_space(SIDE as f32 + 8.0);
            return;
        }
        let preview = PreviewBox::new(ui, &mut self.centre, self.size);
        let amount = self.evening.amount / 100.0;
        // While the copy's away, the last one shown.
        if let Some((tone, key)) = &self.tone {
            let made = (preview.corner, *key, amount.to_bits());
            if self.shown.as_ref().is_none_or(|s| s.2 != made) {
                let part = [preview.corner.0, preview.corner.1, preview.size.0, preview.size.1];
                let texture = |name, amount| preview.texture(ui.ctx(), editor, name, &tone.evened(part, amount));
                self.shown = Some((texture("even-tone-after", amount), texture("even-tone-before", 0.0), made));
            }
        }
        match &self.shown {
            Some((after, before, _)) => preview.show(ui, after, before),
            None => preview.frame(ui),
        }
        ui.add_space(8.0);
    }

    /// Add the Dodge & Burn group above the layer it was opened on (or the
    /// active one, if that's gone), as one undo step, and select it: its
    /// opacity is the amount.
    pub fn apply(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        let index = editor.doc.index_of(self.layer).or(editor.active_index());
        let (Some((skin, _)), Some((tone, _)), Some(index)) = (self.skin.clone(), self.tone.take(), index) else {
            return;
        };
        let amount = self.evening.amount;
        editor.target = Target::Pixels;
        editor.edit_in_background(
            "Even Tone",
            move |doc, active| *active = tone::add_layers(doc, index, &tone, &skin, amount),
            ctx,
        );
    }
}

/// The skin in `image`, its faces and body, measured by its largest face,
/// and a small copy of it evened as `evening` asks.
fn find(image: Raster, profile: &ColorProfile, evening: &Evening) -> Result<Found, String> {
    let found = find_skin(image, profile)?;
    let tone = Tone::new(&found.image, &found.skin, found.iod, evening.radii(found.iod)).ok_or("Found no skin")?;
    let (x, y) = tone.at(found.nose.x, found.nose.y);
    Ok(Found {
        skin: found.skin,
        iod: found.iod,
        tone,
        nose: pos2(x, y),
    })
}

/// `tone` evened again with blurs of `radii`, on a thread.
fn even(ctx: &egui::Context, mut tone: Tone, radii: [f32; 2]) -> Receiver<(Tone, Key)> {
    let ctx = ctx.clone();
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        tone.even(radii);
        let _ = tx.send((tone, radii.map(f32::to_bits)));
        ctx.request_repaint();
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::vec2;
    use omapix_engine::Document;

    /// Skin-coloured with a soft dark patch round (200, 150), all skin,
    /// eyes 80 px apart, and the dialog open on it with the skin found.
    fn dialog() -> (Editor, EvenTone) {
        let pixels = (0..400 * 300)
            .map(|i| {
                let d = ((i % 400) as f32 - 200.0).hypot((i / 400) as f32 - 150.0);
                let v = 45000 - (3000.0 * (-(d / 8.0).powi(2)).exp()) as u16;
                [v, v - 8000, v - 12000, 65535]
            })
            .collect();
        let image = Raster::new(400, 300, pixels);
        let doc = Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16);
        let editor = Editor::new(doc).unwrap();
        let (skin, evening) = (Selection::all(400, 300), Evening::default());
        let radii = evening.radii(80.0);
        let dialog = EvenTone {
            evening,
            layer: editor.active,
            finding: None,
            tone: Some((Tone::new(&image, &skin, 80.0, radii).unwrap(), radii.map(f32::to_bits))),
            skin: Some((Arc::new(skin), 80.0)),
            redoing: None,
            size: (400, 300),
            centre: pos2(200.0, 150.0),
            shown: None,
        };
        (editor, dialog)
    }

    #[test]
    fn the_dialog_previews_the_face_at_the_amount_and_ok_is_enter() {
        let (editor, mut dialog) = dialog();
        let (ctx, theme) = (egui::Context::default(), Theme::default());
        let frame = |dialog: &mut EvenTone, events: Vec<egui::Event>| {
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
        // The box round the patch, at 60 %.
        assert_eq!(frame(&mut dialog, vec![]), Ok(None));
        let key = Evening::default().radii(80.0).map(f32::to_bits);
        assert_eq!(dialog.shown.as_ref().unwrap().2, ((80, 30), key, 0.6f32.to_bits()));
        // The amount only mixes; the size evens the copy again, and OK
        // waits for it.
        dialog.evening.amount = 40.0;
        frame(&mut dialog, vec![]).unwrap();
        assert!(dialog.redoing.is_none());
        assert_eq!(dialog.shown.as_ref().unwrap().2.2, 0.4f32.to_bits());
        dialog.evening.size = 80.0;
        let enter = egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        assert_eq!(frame(&mut dialog, vec![enter.clone()]), Ok(None));
        assert!(dialog.redoing.is_some());
        while dialog.redoing.is_some() {
            assert_eq!(frame(&mut dialog, vec![]), Ok(None));
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        frame(&mut dialog, vec![]).unwrap();
        let larger = dialog.evening.radii(80.0).map(f32::to_bits);
        assert_ne!(larger, key);
        assert_eq!(dialog.shown.as_ref().unwrap().2.1, larger);
        assert_eq!(frame(&mut dialog, vec![enter]), Ok(Some(true)));
    }

    #[test]
    fn ok_adds_a_dodge_and_burn_group_above_in_one_undo_step() {
        let (mut editor, mut dialog) = dialog();
        let ctx = egui::Context::default();
        let before = editor.doc.composite().get(200, 150);
        dialog.apply(&ctx, &mut editor);
        while editor.busy().is_some() {
            std::thread::sleep(std::time::Duration::from_millis(1));
            editor.update(&ctx);
        }
        // The group is selected: its opacity is the amount.
        let group = editor.doc.layer(editor.active).unwrap();
        assert_eq!((group.name.as_str(), group.is_group, group.opacity), ("Dodge & Burn", true, 0.6));
        let names: Vec<_> = editor.doc.layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names[1..], ["Burn", "Dodge", "Dodge & Burn"]);
        assert!(editor.doc.composite().get(200, 150)[0] > before[0] + 500);
        assert_eq!(editor.undo_label(), Some("Even Tone"));
        editor.undo();
        assert_eq!(editor.doc.layers.len(), 1);
    }

    /// A look at real photos: for `OMAPIX_FACE_PHOTO` (a photo, or a
    /// folder of them), the largest face before, after and as the masks
    /// (lighter where Dodge is open, darker where Burn is), side by side as
    /// `<photo>-tone.png` in `OMAPIX_FACE_OUT`. `AMOUNT` and `SIZE` set the
    /// sliders. Needs the models (scripts/fetch-models.sh) and ImageMagick.
    #[test]
    #[ignore]
    fn even_tone_in_photos() {
        let (Ok(photos), Ok(out)) = (std::env::var("OMAPIX_FACE_PHOTO"), std::env::var("OMAPIX_FACE_OUT")) else {
            return;
        };
        let photos = std::path::Path::new(&photos);
        let mut paths: Vec<_> = match std::fs::read_dir(photos) {
            Ok(dir) => dir.map(|e| e.unwrap().path()).collect(),
            Err(_) => vec![photos.to_owned()],
        };
        paths.sort();
        let slider = |name: &str, or: f32| std::env::var(name).map_or(or, |v| v.parse().unwrap());
        let evening = Evening {
            amount: slider("AMOUNT", 100.0),
            size: slider("SIZE", 50.0),
        };
        for path in paths {
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            let Ok(mut doc) = omapix_engine::io::load(&path) else { continue };
            let found = match find_skin(doc.composite(), &doc.profile) {
                Ok(found) => found,
                Err(e) => {
                    eprintln!("{name}: {e}");
                    continue;
                }
            };
            let t = std::time::Instant::now();
            let Some(tone) = Tone::new(&found.image, &found.skin, found.iod, evening.radii(found.iod)) else { continue };
            let evened_in = t.elapsed();
            let t = std::time::Instant::now();
            let top = doc.layers.len() - 1;
            tone::add_layers(&mut doc, top, &tone, &found.skin, evening.amount);
            eprintln!(
                "{name}: iod {:.0}, {} × {}, evened in {evened_in:?}, layers in {:?}",
                found.iod,
                tone.width,
                tone.height,
                t.elapsed()
            );
            let after = doc.composite();
            let masks: Vec<_> = doc.layers.iter().filter_map(|l| l.adjustment.as_ref().and(l.mask.as_ref())).collect();
            let before = &found.image;
            let (iw, ih) = (before.width() as f32, before.height() as f32);
            let r = found.iod * 1.9;
            let (cx, cy) = (found.nose.x, found.nose.y);
            let [x0, y0, x1, y1] = [(cx - r).max(0.0), (cy - r).max(0.0), (cx + r).min(iw), (cy + r).min(ih)];
            let step = ((x1 - x0) / 620.0).max(1.0);
            let (ow, oh) = (((x1 - x0) / step) as u32, ((y1 - y0) / step) as u32);
            let mut ppm = format!("P6 {} {} 255\n", ow * 3, oh).into_bytes();
            for y in 0..oh {
                let at = |x: u32| ((x0 + x as f32 * step) as u32, (y0 + y as f32 * step) as u32);
                for half in [before, &after] {
                    for x in 0..ow {
                        let (px, py) = at(x);
                        let p = half.get(px, py);
                        ppm.extend([0, 1, 2].map(|c| (p[c] >> 8) as u8));
                    }
                }
                for x in 0..ow {
                    let (px, py) = at(x);
                    // Burn is the lower of the two.
                    let v = 32768 + i32::from(masks[1].pixels.get(px, py) / 2) - i32::from(masks[0].pixels.get(px, py) / 2);
                    ppm.extend([(v >> 8).clamp(0, 255) as u8; 3]);
                }
            }
            let ppm_path = format!("{out}/{name}-tone.ppm");
            std::fs::write(&ppm_path, ppm).unwrap();
            std::process::Command::new("magick").args([&ppm_path, &format!("{out}/{name}-tone.png")]).status().unwrap();
            std::fs::remove_file(ppm_path).unwrap();
        }
    }
}
