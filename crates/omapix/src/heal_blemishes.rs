//! Retouch › Heal Blemishes… (docs/AI.md, feature 4): spots found on the
//! skin, on a thread of their own, shown as circles over the image while
//! the Sensitivity slider decides how many; OK heals them onto a new empty
//! Blemishes layer.

use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::sync::Arc;

use egui::{Color32, RichText, Slider, vec2};
use omapix_engine::blemish::{self, LEAST_SCORE, Spot};
use omapix_engine::ColorProfile;

use crate::canvas::Render;
use crate::editor::{Editor, Target};
use crate::face_selection::{analyse, blemish_skin, scale};
use crate::theme::Theme;

pub struct HealBlemishes {
    /// 0–100: how many of the spots found are healed.
    pub sensitivity: f32,
    finding: Option<Receiver<Result<Vec<Spot>, String>>>,
    /// Every spot found, most prominent first.
    found: Vec<Spot>,
}

/// The least score a spot needs at `sensitivity`: at 100 %, every spot
/// found; at 50 %, about 9 (clear pimples and marks, few shadows or
/// pores, on the test portraits); at 0 %, 20 (only obvious pimples).
fn least_score(sensitivity: f32) -> f32 {
    LEAST_SCORE * 5f32.powf((100.0 - sensitivity) / 100.0)
}

/// Those of `found` (most prominent first) healed at `sensitivity`.
pub fn at_sensitivity(found: &[Spot], sensitivity: f32) -> &[Spot] {
    let least = least_score(sensitivity);
    &found[..found.partition_point(|s| s.score >= least)]
}

impl HealBlemishes {
    /// Start finding the spots in what `editor` shows.
    pub fn open(ctx: &egui::Context, editor: &Editor, sensitivity: f32) -> Result<Self, String> {
        let image = editor.canvas.render().ok_or("The image isn't ready yet")?;
        let (image, profile) = (Arc::clone(image), editor.doc.profile.clone());
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(find(&image, &profile));
            ctx.request_repaint();
        });
        Ok(Self {
            sensitivity,
            finding: Some(rx),
            found: Vec::new(),
        })
    }

    /// The spots that will be healed.
    pub fn spots(&self) -> &[Spot] {
        at_sensitivity(&self.found, self.sensitivity)
    }

    /// Show the dialog. Returns `Some(true)` once OK'd, `Some(false)` if
    /// cancelled, and an error if the spots couldn't be found.
    pub fn show(&mut self, ctx: &egui::Context, theme: &Theme) -> Result<Option<bool>, String> {
        if let Some(rx) = &self.finding {
            match rx.try_recv() {
                Ok(found) => {
                    self.found = found?;
                    self.finding = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => return Err("Heal Blemishes stopped unexpectedly".into()),
            }
        }
        let ready = self.finding.is_none();
        let mut result = None;
        let area = egui::Modal::default_area(egui::Id::new("heal-blemishes"))
            .anchor(egui::Align2::RIGHT_TOP, vec2(-320.0, 90.0));
        egui::Modal::new(egui::Id::new("heal-blemishes"))
            .area(area)
            .backdrop_color(Color32::TRANSPARENT)
            .show(ctx, |ui| {
                ui.set_width(340.0);
                ui.heading("Heal Blemishes");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label("Sensitivity");
                    ui.add(Slider::new(&mut self.sensitivity, 0.0..=100.0).suffix(" %").fixed_decimals(0));
                });
                ui.add_space(8.0);
                let status = match self.spots().len() {
                    _ if !ready => "Finding blemishes…".to_owned(),
                    0 => "No blemishes at this sensitivity".to_owned(),
                    1 => "1 blemish, circled".to_owned(),
                    n => format!("{n} blemishes, circled"),
                };
                ui.label(RichText::new(status).color(theme.dark_foreground));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.add_enabled(ready, egui::Button::new("OK")).clicked() || (enter && ready) {
                        result = Some(true);
                    }
                    if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        result = Some(false);
                    }
                });
            });
        Ok(result)
    }

    /// Heal the spots onto a new layer above the active one, as one undo
    /// step. With none to heal, nothing changes.
    pub fn apply(&self, ctx: &egui::Context, editor: &mut Editor) {
        let spots = self.spots().to_vec();
        let Some(index) = editor.doc.index_of(editor.active).filter(|_| !spots.is_empty()) else {
            return;
        };
        editor.target = Target::Pixels;
        editor.edit_in_background(
            "Heal Blemishes",
            move |doc, active| *active = blemish::heal(doc, index, &spots),
            ctx,
        );
    }
}

/// The spots on the faces in `image`, measured by the largest.
fn find(image: &Render, profile: &ColorProfile) -> Result<Vec<Spot>, String> {
    image
        .with_image(|image| {
            let (srgb, analysis) = analyse(image, profile)?;
            if analysis.faces.is_empty() {
                return Err("Found no faces".into());
            }
            let iod = scale(&analysis).ok_or("The faces are turned too far to look for blemishes")?;
            // Face skin only, for now: the segmenter takes some clothes
            // and props for body skin.
            let skin = blemish_skin(&analysis, image);
            Ok(blemish::find(&srgb, &skin, iod))
        })
        .ok_or("The image isn't ready yet")?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn higher_sensitivity_heals_more_spots() {
        let spot = |score| Spot {
            x: 0.0,
            y: 0.0,
            radius: 2.0,
            score,
        };
        let mut dialog = HealBlemishes {
            sensitivity: 100.0,
            finding: None,
            found: vec![spot(25.0), spot(12.0), spot(6.0), spot(LEAST_SCORE)],
        };
        assert_eq!(dialog.spots().len(), 4);
        dialog.sensitivity = 50.0;
        assert_eq!(dialog.spots().len(), 2);
        dialog.sensitivity = 0.0;
        assert_eq!(dialog.spots().len(), 1);
    }

    #[test]
    fn ok_heals_onto_a_new_blemishes_layer_in_one_undo_step() {
        use omapix_engine::{Document, Raster};
        // Flat skin with a dark spot at (100, 100).
        let pixels = (0..300 * 200)
            .map(|i| {
                let (x, y) = ((i % 300) as f32, (i / 300) as f32);
                let v = if (x - 100.0).hypot(y - 100.0) < 4.0 { 20000 } else { 45000 };
                [v, v - 8000, v - 12000, 65535]
            })
            .collect();
        let doc = Document::from_image("t.tif".into(), &Raster::new(300, 200, pixels), ColorProfile::srgb(), 16);
        let mut editor = Editor::new(doc).unwrap();
        let ctx = egui::Context::default();
        let spot = Spot {
            x: 100.0,
            y: 100.0,
            radius: 4.0,
            score: 20.0,
        };
        let mut dialog = HealBlemishes {
            sensitivity: 0.0,
            finding: None,
            found: vec![spot],
        };
        dialog.apply(&ctx, &mut editor);
        while editor.busy().is_some() {
            std::thread::sleep(std::time::Duration::from_millis(1));
            editor.update(&ctx);
        }
        assert_eq!(editor.doc.layers.len(), 2);
        let layer = editor.doc.layer(editor.active).unwrap();
        assert_eq!(layer.name, "Blemishes");
        assert!(layer.pixels.get(100, 100)[3] > 60000);
        assert!(editor.doc.composite().get(100, 100)[0] > 40000);
        editor.undo();
        assert_eq!(editor.doc.layers.len(), 1);

        // Nothing to heal at this sensitivity: nothing happens.
        dialog.found[0].score = LEAST_SCORE;
        dialog.apply(&ctx, &mut editor);
        assert!(editor.busy().is_none());
        assert_eq!(editor.doc.layers.len(), 1);
    }

    /// A look at real photos: for `OMAPIX_FACE_PHOTO` (a photo, or a
    /// folder of them), the face skin with the spots found circled by
    /// score (red 20 and up, green 10, cyan 7, yellow less), and beside it
    /// those healed at 50 % (or `BLEMISH_SENSITIVITY`), as
    /// `<photo>.png` in `OMAPIX_FACE_OUT`. Needs the models
    /// (scripts/fetch-models.sh) and ImageMagick.
    #[test]
    #[ignore]
    fn find_blemishes_in_photos() {
        let (Ok(photos), Ok(out)) = (std::env::var("OMAPIX_FACE_PHOTO"), std::env::var("OMAPIX_FACE_OUT")) else {
            return;
        };
        let photos = std::path::Path::new(&photos);
        let mut paths: Vec<_> = match std::fs::read_dir(photos) {
            Ok(dir) => dir.map(|e| e.unwrap().path()).collect(),
            Err(_) => vec![photos.to_owned()],
        };
        paths.sort();
        for path in paths {
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            let mut doc = omapix_engine::io::load(&path).unwrap();
            let before = doc.composite();
            let (srgb, analysis) = analyse(&before, &doc.profile).unwrap();
            let Some(iod) = scale(&analysis) else {
                eprintln!("{name}: no faces seen well enough");
                continue;
            };
            let boxes: Vec<_> = analysis.faces.iter().map(|(f, p)| (f.bounds[3] - f.bounds[1], p.is_some())).collect();
            let skin = blemish_skin(&analysis, &before);
            let started = std::time::Instant::now();
            let spots = blemish::find(&srgb, &skin, iod);
            let took = started.elapsed();
            let sensitivity = std::env::var("BLEMISH_SENSITIVITY").map_or(50.0, |v| v.parse().unwrap());
            let healed_spots: Vec<_> = spots.iter().copied().filter(|s| s.score >= least_score(sensitivity)).collect();
            eprintln!(
                "{name}: iod {iod:.0}, faces (height, seen) {boxes:?}, {} spots ({} healed) in {took:?}",
                spots.len(),
                healed_spots.len()
            );
            let top = doc.layers.len() - 1;
            blemish::heal(&mut doc, top, &healed_spots);
            let after = doc.composite();

            let Some([x0, y0, w, h]) = skin.bounds() else { continue };
            // At most 1200 px across each half.
            let step = (w.max(h) as f32 / 1200.0).max(1.0);
            let (ow, oh) = ((w as f32 / step) as u32, (h as f32 / step) as u32);
            let mut ppm = format!("P6 {} {} 255\n", ow * 2, oh).into_bytes();
            for y in 0..oh {
                for half in [&before, &after] {
                    for x in 0..ow {
                        let (ix, iy) = (x0 + (x as f32 * step) as u32, y0 + (y as f32 * step) as u32);
                        let mut rgb = [0, 1, 2].map(|c| (half.get(ix, iy)[c] >> 8) as u8);
                        if std::ptr::eq(half, &before) {
                            let inside = |dx: f32| skin.at((ix as f32 + dx).min(before.width() as f32 - 1.0) as u32, iy) >= 0.5;
                            if inside(0.0) != inside(step) {
                                rgb = [0, 120, 255];
                            }
                            for s in &spots {
                                let d = (ix as f32 - s.x).hypot(iy as f32 - s.y);
                                if (d - s.heal_size() / 2.0).abs() < step * 0.8 {
                                    rgb = match s.score {
                                        v if v >= 20.0 => [255, 0, 0],
                                        v if v >= 10.0 => [0, 255, 0],
                                        v if v >= 7.0 => [0, 255, 255],
                                        _ => [255, 255, 0],
                                    };
                                }
                            }
                        }
                        ppm.extend(rgb);
                    }
                }
            }
            let ppm_path = format!("{out}/{name}.ppm");
            std::fs::write(&ppm_path, ppm).unwrap();
            std::process::Command::new("magick").args([&ppm_path, &format!("{out}/{name}.png")]).status().unwrap();
            std::fs::remove_file(ppm_path).unwrap();
        }
    }
}
