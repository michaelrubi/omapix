//! Retouch › Auto Retouch… (docs/AI.md, feature 6): Heal Blemishes, Smooth
//! Skin, Even Tone, Lighten Under Eyes and Whiten Eyes and Teeth in one go, for each face found
//! in what the active layer and those below it show, as a Retouch group
//! above with a group for each face (`omapix_engine::retouch`). The dialog has a switch and a
//! strength for each step, three presets, and with several faces a strip of
//! them to give one its own settings or leave it out. The blemishes to be
//! healed are circled on the canvas meanwhile.

use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::sync::Arc;

use egui::{Color32, Pos2, RichText, Slider, vec2};
use omapix_engine::blemish::Spot;
use omapix_engine::skin::Smoothing;
use omapix_engine::tone::Evening;
use omapix_engine::{ColorProfile, Pixel, Raster, Selection, retouch};
use serde::{Deserialize, Serialize};

use crate::editor::{Editor, Target};
use crate::face_selection::{FoundFace, find_faces};
use crate::heal_blemishes::at_sensitivity;
use crate::theme::Theme;

/// One step: whether it's done, and how strongly (0–100).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub on: bool,
    pub amount: f32,
}

/// Auto Retouch's settings for a face: Heal Blemishes' Sensitivity, Smooth
/// Skin's and Even Tone's Amounts, and how much the shadows under its eyes
/// are lifted and its eyes and teeth whitened. Their other settings are
/// their defaults.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Retouch {
    pub blemishes: Step,
    pub smooth_skin: Step,
    pub even_tone: Step,
    pub under_eyes: Step,
    pub whiten_eyes: Step,
    pub whiten_teeth: Step,
}

impl Default for Retouch {
    fn default() -> Self {
        Preset::Standard.settings()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    Natural,
    Standard,
    Strong,
}

impl Preset {
    const ALL: [Preset; 3] = [Preset::Natural, Preset::Standard, Preset::Strong];

    fn name(self) -> &'static str {
        match self {
            Preset::Natural => "Natural",
            Preset::Standard => "Standard",
            Preset::Strong => "Strong",
        }
    }

    /// A preset by its name, as in OMAPIX_SCRIPT (`AutoRetouch Natural`).
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }

    /// Every step on: Standard is each step's own default.
    pub fn settings(self) -> Retouch {
        let [blemishes, smooth_skin, even_tone, lighten] = match self {
            Preset::Natural => [35.0, 40.0, 40.0, 30.0],
            Preset::Standard => [50.0, 70.0, 60.0, 50.0],
            Preset::Strong => [65.0, 90.0, 80.0, 70.0],
        }
        .map(|amount| Step { on: true, amount });
        Retouch {
            blemishes,
            smooth_skin,
            even_tone,
            under_eyes: lighten,
            whiten_eyes: lighten,
            whiten_teeth: lighten,
        }
    }
}

/// A thumbnail's side in pixels, shown at half that in points.
const THUMB: u32 = 96;

struct Face {
    found: Arc<FoundFace>,
    /// A small picture of it for the strip, `THUMB` square, and that as a
    /// texture once it's been shown.
    thumbnail: Vec<Pixel>,
    texture: Option<egui::TextureHandle>,
    /// Its own settings, once it's given any: until then, All Faces'.
    own: Option<Retouch>,
    /// Switched off, it's left as it is.
    on: bool,
}

pub struct AutoRetouch {
    /// All Faces' settings.
    pub all: Retouch,
    /// OK it as soon as the faces are found: for scripts.
    pub accept: bool,
    /// The layer it goes above.
    layer: u64,
    finding: Option<Receiver<Result<Vec<Face>, String>>>,
    /// The faces found, from left to right.
    faces: Vec<Face>,
    /// The face the settings shown are for: with none, All Faces.
    selected: Option<usize>,
    /// The spots that will be healed.
    circled: Vec<Spot>,
}

impl AutoRetouch {
    /// Start finding the faces in what the active layer and those below it
    /// show.
    pub fn open(ctx: &egui::Context, editor: &Editor, all: Retouch) -> Result<Self, String> {
        let index = editor.active_index().ok_or("Select a layer first")?;
        let doc = editor.doc.clone();
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(find(&doc.composite_current_and_below(index), &doc.profile));
            ctx.request_repaint();
        });
        Ok(Self {
            all,
            accept: false,
            layer: editor.active,
            finding: Some(rx),
            faces: Vec::new(),
            selected: None,
            circled: Vec::new(),
        })
    }

    /// The spots that will be healed.
    pub fn spots(&self) -> &[Spot] {
        &self.circled
    }

    /// Face `n`'s settings, or All Faces'.
    fn settings(&self, face: Option<usize>) -> Retouch {
        face.and_then(|n| self.faces[n].own).unwrap_or(self.all)
    }

    /// The spots face `n` has healed: none if it or the step is off.
    fn healed(&self, n: usize) -> &[Spot] {
        let (face, blemishes) = (&self.faces[n], self.settings(Some(n)).blemishes);
        if face.on && blemishes.on { at_sensitivity(&face.found.spots, blemishes.amount) } else { &[] }
    }

    /// Select the face at `at` (image pixels), if there's one there.
    fn pick(&mut self, at: Pos2) {
        let inside = |f: &Face| {
            let [l, t, r, b] = f.found.bounds;
            (l..=r).contains(&at.x) && (t..=b).contains(&at.y)
        };
        if let Some(n) = self.faces.iter().position(inside) {
            self.selected = Some(n);
        }
    }

    /// Show the dialog. Returns `Some(true)` once OK'd, `Some(false)` if
    /// cancelled, and an error if the faces couldn't be found.
    pub fn show(&mut self, ctx: &egui::Context, editor: &Editor, theme: &Theme) -> Result<Option<bool>, String> {
        if let Some(rx) = &self.finding {
            match rx.try_recv() {
                Ok(found) => {
                    self.faces = found?;
                    self.finding = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => return Err("Auto Retouch stopped unexpectedly".into()),
            }
        }
        let ready = self.finding.is_none();
        self.circled = (0..self.faces.len()).flat_map(|n| self.healed(n).to_vec()).collect();
        let mut result = (ready && self.accept).then_some(true);
        let area = egui::Modal::default_area(egui::Id::new("auto-retouch"))
            .anchor(egui::Align2::RIGHT_TOP, vec2(-320.0, 90.0));
        let modal = egui::Modal::new(egui::Id::new("auto-retouch"))
            .area(area)
            .backdrop_color(Color32::TRANSPARENT)
            .show(ctx, |ui| {
                ui.set_width(340.0);
                ui.heading("Auto Retouch");
                ui.add_space(8.0);
                if self.faces.len() > 1 {
                    self.strip(ui, editor);
                    ui.add_space(8.0);
                }
                self.controls(ui);
                ui.add_space(8.0);
                let status = match self.circled.len() {
                    _ if !ready => "Finding faces…".to_owned(),
                    0 => "No blemishes to heal".to_owned(),
                    1 => "1 blemish, circled".to_owned(),
                    n => format!("{n} blemishes, circled"),
                };
                ui.label(RichText::new(status).color(theme.dark_foreground));
                ui.label(RichText::new("As a Retouch group, with a group for each face.").color(theme.dark_foreground));
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
        // A click on a face on the canvas selects it, as one in the strip.
        let backdrop = modal.backdrop_response;
        let clicked = backdrop.clicked().then(|| backdrop.interact_pointer_pos()).flatten();
        if let Some(at) = clicked.and_then(|p| editor.canvas.image_at(p)) {
            self.pick(at);
        }
        Ok(result)
    }

    /// The strip of faces: All Faces, then each face, dimmed if it's off.
    fn strip(&mut self, ui: &mut egui::Ui, editor: &Editor) {
        let shown = vec2(THUMB as f32, THUMB as f32) / 2.0;
        ui.horizontal_wrapped(|ui| {
            let all = egui::Button::new("All Faces").selected(self.selected.is_none()).min_size(vec2(0.0, shown.y));
            if ui.add(all).clicked() {
                self.selected = None;
            }
            for (n, face) in self.faces.iter_mut().enumerate() {
                let texture = face.texture.get_or_insert_with(|| {
                    let name = format!("auto-retouch-face-{n}");
                    crate::preview_box::texture(ui.ctx(), editor, &name, (THUMB, THUMB), &face.thumbnail)
                });
                let tint = if face.on { Color32::WHITE } else { Color32::from_gray(70) };
                let image = egui::Image::new((texture.id(), shown)).tint(tint);
                let button = egui::Button::image(image).selected(self.selected == Some(n));
                if ui.add(button).on_hover_text(format!("Face {}", n + 1)).clicked() {
                    self.selected = Some(n);
                }
            }
        });
    }

    /// The presets and each step's switch and strength, for the face
    /// selected or for all of them. Changing a face's gives it its own.
    fn controls(&mut self, ui: &mut egui::Ui) {
        let mut on = true;
        if let Some(n) = self.selected {
            ui.checkbox(&mut self.faces[n].on, format!("Retouch Face {}", n + 1));
            ui.add_space(4.0);
            on = self.faces[n].on;
        }
        let before = self.settings(self.selected);
        let mut settings = before;
        ui.add_enabled_ui(on, |ui| {
            ui.horizontal(|ui| {
                for preset in Preset::ALL {
                    if ui.selectable_label(settings == preset.settings(), preset.name()).clicked() {
                        settings = preset.settings();
                    }
                }
            });
            ui.add_space(4.0);
            egui::Grid::new("auto-retouch-steps").num_columns(2).show(ui, |ui| {
                for (label, hint, step) in [
                    ("Heal Blemishes", "Sensitivity: how many of the spots found are healed", &mut settings.blemishes),
                    ("Smooth Skin", "Amount: the Smooth Skin layer's opacity", &mut settings.smooth_skin),
                    ("Even Tone", "Amount: the Dodge & Burn group's opacity", &mut settings.even_tone),
                    ("Lighten Under Eyes", "Amount: the Under Eyes layer's opacity", &mut settings.under_eyes),
                    ("Whiten Eyes", "Amount: the Whiten Eyes layer's opacity", &mut settings.whiten_eyes),
                    ("Whiten Teeth", "Amount: the Whiten Teeth layer's opacity", &mut settings.whiten_teeth),
                ] {
                    ui.checkbox(&mut step.on, label);
                    let slider = Slider::new(&mut step.amount, 0.0..=100.0).suffix(" %").fixed_decimals(0);
                    ui.add_enabled(step.on, slider).on_hover_text(hint);
                    ui.end_row();
                }
            });
        });
        if settings != before {
            match self.selected {
                Some(n) => self.faces[n].own = Some(settings),
                None => self.all = settings,
            }
        }
    }

    /// Add the Retouch group above the layer it was opened on (or the
    /// active one, if that's gone), as one undo step, and select it. With
    /// nothing to do, nothing changes.
    pub fn apply(&self, ctx: &egui::Context, editor: &mut Editor) {
        let Some(index) = editor.doc.index_of(self.layer).or(editor.active_index()) else {
            return;
        };
        // Each face that's on: its number, how many of its spots are
        // healed, the other steps' settings, and how much under its eyes
        // is lightened and its eyes and teeth whitened, if it has any.
        let faces: Vec<_> = (0..self.faces.len())
            .filter(|&n| self.faces[n].on)
            .map(|n| {
                let (settings, found) = (self.settings(Some(n)), &self.faces[n].found);
                let amount = |step: Step| step.on.then_some(step.amount);
                let smoothing = amount(settings.smooth_skin).map(|amount| Smoothing { amount, ..Default::default() });
                let evening = amount(settings.even_tone).map(|amount| Evening { amount, ..Default::default() });
                let lighten = |step: Step, part: &Selection| amount(step).filter(|_| !part.is_empty());
                let lighten = [
                    lighten(settings.under_eyes, &found.under_eyes.0),
                    lighten(settings.whiten_eyes, &found.eyes),
                    lighten(settings.whiten_teeth, &found.teeth),
                ];
                (n + 1, Arc::clone(found), self.healed(n).len(), smoothing, evening, lighten)
            })
            .filter(|(_, _, healed, smoothing, evening, lighten)| {
                *healed > 0 || smoothing.is_some() || evening.is_some() || lighten.iter().any(Option::is_some)
            })
            .collect();
        if faces.is_empty() {
            return;
        }
        editor.target = Target::Pixels;
        editor.edit_in_background(
            "Auto Retouch",
            move |doc, active| {
                let faces: Vec<_> = (faces.iter())
                    .map(|(number, found, healed, smoothing, evening, [under, eyes, teeth])| retouch::Face {
                        number: *number,
                        skin: &found.skin,
                        iod: found.iod,
                        spots: &found.spots[..*healed],
                        smoothing: *smoothing,
                        evening: *evening,
                        under_eyes: under.map(|amount| (&found.under_eyes.0, &found.under_eyes.1, amount)),
                        eyes: eyes.map(|amount| (&found.eyes, amount)),
                        teeth: teeth.map(|amount| (&found.teeth, amount)),
                    })
                    .collect();
                if let Some(id) = retouch::add_layers(doc, index, &faces) {
                    *active = id;
                }
            },
            ctx,
        );
    }
}

/// The faces in `image`, each with a picture of it for the strip.
fn find(image: &Raster, profile: &ColorProfile) -> Result<Vec<Face>, String> {
    let faces = find_faces(image, profile)?.into_iter().map(|found| Face {
        thumbnail: thumbnail(image, found.bounds),
        found: Arc::new(found),
        texture: None,
        own: None,
        on: true,
    });
    Ok(faces.collect())
}

/// The face in `bounds` (left, top, right, bottom) and a little round it,
/// `THUMB` pixels square.
fn thumbnail(image: &Raster, [l, t, r, b]: [f32; 4]) -> Vec<Pixel> {
    // Each pixel is the average of a grid of points across it.
    const GRID: u32 = 4;
    let side = (r - l).max(b - t) * 1.2;
    let (corner, n) = ([(l + r - side) / 2.0, (t + b - side) / 2.0], (THUMB * GRID) as f32);
    let at = |c: usize, i: u32, size: u32| ((corner[c] + (i as f32 + 0.5) / n * side).max(0.0) as u32).min(size - 1);
    (0..THUMB * THUMB)
        .map(|i| {
            let mut sum = [0u32; 4];
            for g in 0..GRID * GRID {
                let (x, y) = (i % THUMB * GRID + g % GRID, i / THUMB * GRID + g / GRID);
                let p = image.get(at(0, x, image.width()), at(1, y, image.height()));
                for c in 0..4 {
                    sum[c] += u32::from(p[c]);
                }
            }
            sum.map(|s| (s / (GRID * GRID)) as u16)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Rect, pos2};
    use omapix_engine::Document;

    const W: u32 = 1000;
    const H: u32 = 800;

    /// Skin-coloured, with a dark spot on each of two faces: one on the
    /// left, its box 200 px square round (150, 400), and one on the right
    /// round (850, 400).
    fn image() -> Raster {
        let pixels = (0..W * H)
            .map(|i| {
                let (x, y) = ((i % W) as f32, (i / W) as f32);
                let spot = [150.0, 850.0].iter().any(|cx| (x - cx).hypot(y - 400.0) < 4.0);
                let v = if spot { 20000 } else { 45000 };
                [v, v - 8000, v - 12000, 65535]
            })
            .collect();
        Raster::new(W, H, pixels)
    }

    /// The dialog open on [`image`] with its faces found: each has half
    /// the image as skin, a clear spot and a faint one, an eye's white,
    /// the skin under it (no darker than the cheek) and teeth.
    fn dialog() -> (Editor, AutoRetouch) {
        dialog_on(image())
    }

    fn dialog_on(image: Raster) -> (Editor, AutoRetouch) {
        let doc = Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16);
        let mut editor = Editor::new(doc).unwrap();
        // Image pixels are screen points.
        editor.canvas.lay_out_for_test(Rect::from_min_size(Pos2::ZERO, vec2(W as f32, H as f32)), 1.0);
        let faces = [150.0, 850.0].map(|x: f32| {
            let half = if x < 500.0 { 0.0 } else { 500.0 };
            let spot = |y, score| Spot { x, y, radius: 4.0, score };
            Face {
                thumbnail: thumbnail(&image, [x - 100.0, 300.0, x + 100.0, 500.0]),
                found: Arc::new(FoundFace {
                    bounds: [x - 100.0, 300.0, x + 100.0, 500.0],
                    iod: 80.0,
                    skin: Selection::rectangle(W, H, (half, 0.0), (half + 500.0, H as f32)),
                    spots: vec![spot(400.0, 25.0), spot(600.0, 6.0)],
                    teeth: Selection::rectangle(W, H, (x - 30.0, 450.0), (x + 30.0, 470.0)),
                    eyes: Selection::rectangle(W, H, (x - 50.0, 340.0), (x - 30.0, 350.0)),
                    under_eyes: (
                        Selection::rectangle(W, H, (x - 60.0, 355.0), (x - 20.0, 375.0)),
                        Selection::rectangle(W, H, (x - 60.0, 380.0), (x - 20.0, 395.0)),
                    ),
                }),
                texture: None,
                own: None,
                on: true,
            }
        });
        let dialog = AutoRetouch {
            all: Retouch::default(),
            accept: false,
            layer: editor.active,
            finding: None,
            faces: faces.into(),
            selected: None,
            circled: Vec::new(),
        };
        (editor, dialog)
    }

    /// A frame of the dialog with `events`.
    fn frame(ctx: &egui::Context, editor: &Editor, dialog: &mut AutoRetouch, events: Vec<egui::Event>) -> Result<Option<bool>, String> {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(W as f32, H as f32))),
            events,
            ..Default::default()
        };
        let mut result = Ok(None);
        let mut out = ctx.run_ui(input, |ui| result = dialog.show(ui.ctx(), editor, &Theme::default()));
        out.textures_delta.clear();
        result
    }

    fn apply(editor: &mut Editor, dialog: &AutoRetouch) -> Vec<String> {
        let ctx = egui::Context::default();
        dialog.apply(&ctx, editor);
        while editor.busy().is_some() {
            std::thread::sleep(std::time::Duration::from_millis(1));
            editor.update(&ctx);
        }
        editor.doc.layers.iter().map(|l| l.name.clone()).collect()
    }

    #[test]
    fn standard_is_each_steps_own_default_and_presets_have_names() {
        let standard = Preset::Standard.settings();
        assert_eq!(standard, Retouch::default());
        assert_eq!(standard.smooth_skin.amount, Smoothing::default().amount);
        assert_eq!(standard.even_tone.amount, Evening::default().amount);
        assert_eq!(standard.blemishes.amount, crate::settings::FilterSettings::default().blemish_sensitivity);
        for step in [standard.under_eyes, standard.whiten_eyes, standard.whiten_teeth] {
            assert_eq!(step.amount, crate::whiten::AMOUNT);
        }
        assert_eq!(Preset::from_name("Natural"), Some(Preset::Natural));
        assert_eq!(Preset::from_name("Gentle"), None);
        let (natural, strong) = (Preset::Natural.settings(), Preset::Strong.settings());
        assert!(natural.smooth_skin.amount < standard.smooth_skin.amount && standard.smooth_skin.amount < strong.smooth_skin.amount);
    }

    #[test]
    fn ok_adds_a_retouch_group_with_a_group_for_each_face_in_one_undo_step() {
        let (mut editor, mut dialog) = dialog();
        let ctx = egui::Context::default();
        // At 50 % each face's clear spot is circled, and its thumbnail
        // made.
        assert_eq!(frame(&ctx, &editor, &mut dialog, vec![]), Ok(None));
        assert_eq!(dialog.spots().len(), 2);
        assert!(dialog.faces.iter().all(|f| f.texture.is_some()));
        let enter = egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        assert_eq!(frame(&ctx, &editor, &mut dialog, vec![enter]), Ok(Some(true)));

        let names = apply(&mut editor, &dialog);
        let steps = ["Blemishes", "Smooth Skin", "Burn", "Dodge", "Dodge & Burn", "Whiten Eyes", "Whiten Teeth"];
        assert_eq!(names[1..8], steps);
        assert_eq!(names[8], "Face 2");
        assert_eq!(names[9..16], steps);
        assert_eq!(names[16..], ["Face 1", "Retouch"]);
        // The group's selected, and both spots are healed.
        assert_eq!(editor.doc.layer(editor.active).unwrap().name, "Retouch");
        let after = editor.doc.composite();
        assert!(after.get(150, 400)[0] > 40000 && after.get(850, 400)[0] > 40000);
        assert_eq!(editor.undo_label(), Some("Auto Retouch"));
        editor.undo();
        assert_eq!(editor.doc.layers.len(), 1);
    }

    #[test]
    fn a_face_follows_all_faces_until_it_has_its_own_settings_or_is_switched_off() {
        let (mut editor, mut dialog) = dialog();
        let ctx = egui::Context::default();
        // All Faces' sensitivity is both faces'.
        dialog.all.blemishes.amount = 100.0;
        frame(&ctx, &editor, &mut dialog, vec![]).unwrap();
        assert_eq!(dialog.spots().len(), 4);
        // The second face on its own: only blemishes, and fewer.
        let mut own = Preset::Natural.settings();
        (own.smooth_skin.on, own.even_tone.on) = (false, false);
        (own.under_eyes.on, own.whiten_eyes.on, own.whiten_teeth.on) = (false, false, false);
        dialog.faces[1].own = Some(own);
        frame(&ctx, &editor, &mut dialog, vec![]).unwrap();
        assert_eq!(dialog.spots().len(), 3);
        assert_eq!((dialog.settings(Some(0)), dialog.settings(Some(1))), (dialog.all, own));
        // The first switched off: only the second's spot is left.
        dialog.faces[0].on = false;
        frame(&ctx, &editor, &mut dialog, vec![]).unwrap();
        assert_eq!(dialog.spots(), &dialog.faces[1].found.spots[..1]);

        let names = apply(&mut editor, &dialog);
        assert_eq!(names[1..], ["Blemishes", "Face 2", "Retouch"]);
        let after = editor.doc.composite();
        assert!(after.get(850, 400)[0] > 40000 && after.get(150, 400)[0] < 25000);

        // With every step off, there's nothing to do.
        editor.undo();
        dialog.faces[1].own.as_mut().unwrap().blemishes.on = false;
        dialog.apply(&ctx, &mut editor);
        assert!(editor.busy().is_none());
        assert_eq!(editor.doc.layers.len(), 1);
    }

    #[test]
    fn eyes_and_teeth_are_whitened_by_their_amounts_where_a_face_has_any() {
        let (mut editor, mut dialog) = dialog();
        // Only whitening, and the second face's mouth is closed.
        let mut only = Preset::Strong.settings();
        (only.blemishes.on, only.smooth_skin.on, only.even_tone.on, only.under_eyes.on) = (false, false, false, false);
        only.whiten_eyes.amount = 20.0;
        dialog.all = only;
        let face = Arc::get_mut(&mut dialog.faces[1].found).unwrap();
        face.teeth = Selection::rectangle(W, H, (0.0, 0.0), (0.0, 0.0));
        let before = editor.doc.composite();

        let names = apply(&mut editor, &dialog);
        assert_eq!(names[1..], ["Whiten Eyes", "Face 2", "Whiten Eyes", "Whiten Teeth", "Face 1", "Retouch"]);
        let opacity = |name: &str| editor.doc.layers.iter().find(|l| l.name == name).unwrap().opacity;
        assert_eq!((opacity("Whiten Eyes"), opacity("Whiten Teeth")), (0.2, 0.7));
        let after = editor.doc.composite();
        assert!(after.get(150, 460)[2] > before.get(150, 460)[2] + 1000);
        assert_eq!(after.get(850, 460), before.get(850, 460));

        // With neither to whiten, and nothing else on, there's nothing to
        // do.
        editor.undo();
        dialog.faces[0].on = false;
        dialog.all.whiten_eyes.on = false;
        dialog.apply(&egui::Context::default(), &mut editor);
        assert!(editor.busy().is_none());
        assert_eq!(editor.doc.layers.len(), 1);
    }

    #[test]
    fn shadows_under_the_eyes_are_lifted_where_a_face_has_any() {
        // The first face's skin under its eye is darker than its cheek.
        let image = image();
        let pixels: Vec<_> = (image.pixels().iter().enumerate())
            .map(|(i, &p)| {
                let (x, y) = (i as u32 % W, i as u32 / W);
                if (90..130).contains(&x) && (355..375).contains(&y) { [p[0] - 5000, p[1] - 4000, p[2] - 3500, p[3]] } else { p }
            })
            .collect();
        let (mut editor, mut dialog) = dialog_on(Raster::new(W, H, pixels));
        let mut only = Preset::Standard.settings();
        (only.blemishes.on, only.smooth_skin.on, only.even_tone.on) = (false, false, false);
        (only.whiten_eyes.on, only.whiten_teeth.on) = (false, false);
        dialog.all = only;
        let before = editor.doc.composite();

        // The second face has no shadows, so no layer and no group.
        let names = apply(&mut editor, &dialog);
        assert_eq!(names[1..], ["Under Eyes", "Face 1", "Retouch"]);
        assert_eq!(editor.doc.layers[1].opacity, 0.5);
        let after = editor.doc.composite();
        assert!(after.get(110, 365)[0] > before.get(110, 365)[0] + 500, "{} {}", after.get(110, 365)[0], before.get(110, 365)[0]);
        assert_eq!(after.get(110, 388), before.get(110, 388));
    }

    #[test]
    fn clicking_a_face_on_the_canvas_selects_it() {
        let (editor, mut dialog) = dialog();
        let ctx = egui::Context::default();
        let click = |dialog: &mut AutoRetouch, at: Pos2| {
            frame(&ctx, &editor, dialog, vec![egui::Event::PointerMoved(at)]).unwrap();
            for pressed in [true, false] {
                let button = egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                };
                frame(&ctx, &editor, dialog, vec![button]).unwrap();
            }
        };
        // The dialog lays itself out in its first frame.
        frame(&ctx, &editor, &mut dialog, vec![]).unwrap();
        click(&mut dialog, pos2(850.0, 450.0));
        assert_eq!(dialog.selected, Some(1));
        click(&mut dialog, pos2(100.0, 320.0));
        assert_eq!(dialog.selected, Some(0));
        // Off the faces, it stays.
        click(&mut dialog, pos2(150.0, 700.0));
        assert_eq!(dialog.selected, Some(0));
    }

    #[test]
    fn a_script_oks_it_once_the_faces_are_found() {
        let (editor, mut dialog) = dialog();
        let ctx = egui::Context::default();
        dialog.accept = true;
        let (tx, rx) = channel();
        dialog.finding = Some(rx);
        let faces = std::mem::take(&mut dialog.faces);
        assert_eq!(frame(&ctx, &editor, &mut dialog, vec![]), Ok(None));
        tx.send(Ok(faces)).unwrap();
        assert_eq!(frame(&ctx, &editor, &mut dialog, vec![]), Ok(Some(true)));
        // No faces is an error for the status bar.
        dialog.finding = Some(rx_with(Err("Found no faces".into())));
        assert_eq!(frame(&ctx, &editor, &mut dialog, vec![]), Err("Found no faces".into()));
    }

    fn rx_with(result: Result<Vec<Face>, String>) -> Receiver<Result<Vec<Face>, String>> {
        let (tx, rx) = channel();
        tx.send(result).unwrap();
        rx
    }

    #[test]
    fn a_thumbnail_is_the_face_with_a_little_round_it() {
        // Left half dark, right half light: a face across the middle.
        let pixels = (0..400 * 400).map(|i| if i % 400 < 200 { [10000; 4] } else { [50000; 4] }).collect();
        let thumb = thumbnail(&Raster::new(400, 400, pixels), [150.0, 150.0, 250.0, 250.0]);
        assert_eq!(thumb.len(), (THUMB * THUMB) as usize);
        let at = |x: u32, y: u32| thumb[(y * THUMB + x) as usize][0];
        assert_eq!((at(10, 48), at(85, 48)), (10000, 50000));
        // At the image's edge it repeats the edge, not wrapping.
        let pixels = (0..400 * 400).map(|i| if i % 400 < 200 { [10000; 4] } else { [50000; 4] }).collect();
        let thumb = thumbnail(&Raster::new(400, 400, pixels), [350.0, 0.0, 450.0, 100.0]);
        assert!(thumb.iter().all(|p| p[0] == 50000));
    }

    /// A look at real photos: for `OMAPIX_FACE_PHOTO` (a photo, or a folder
    /// of them), Auto Retouch at `PRESET` (Standard, if unset), each face
    /// before and after side by side as `<photo>-face-<n>.png` in
    /// `OMAPIX_FACE_OUT`. Needs the models (scripts/fetch-models.sh) and
    /// ImageMagick.
    #[test]
    #[ignore]
    fn auto_retouch_in_photos() {
        let (Ok(photos), Ok(out)) = (std::env::var("OMAPIX_FACE_PHOTO"), std::env::var("OMAPIX_FACE_OUT")) else {
            return;
        };
        let photos = std::path::Path::new(&photos);
        let mut paths: Vec<_> = match std::fs::read_dir(photos) {
            Ok(dir) => dir.map(|e| e.unwrap().path()).collect(),
            Err(_) => vec![photos.to_owned()],
        };
        paths.sort();
        let preset = std::env::var("PRESET").map_or(Preset::Standard, |name| Preset::from_name(&name).unwrap());
        for path in paths {
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            let Ok(doc) = omapix_engine::io::load(&path) else { continue };
            let before = doc.composite();
            let t = std::time::Instant::now();
            let faces = match find(&before, &doc.profile) {
                Ok(faces) => faces,
                Err(e) => {
                    eprintln!("{name}: {e}");
                    continue;
                }
            };
            let found_in = t.elapsed();
            let mut editor = Editor::new(doc).unwrap();
            let dialog = AutoRetouch {
                all: preset.settings(),
                accept: false,
                layer: editor.active,
                finding: None,
                faces,
                selected: None,
                circled: Vec::new(),
            };
            let t = std::time::Instant::now();
            let names = apply(&mut editor, &dialog);
            eprintln!("{name}: {} faces found in {found_in:?}, retouched in {:?}", dialog.faces.len(), t.elapsed());
            for (n, face) in dialog.faces.iter().enumerate() {
                let found = &face.found;
                eprintln!("  face {}: iod {:.0}, {} spots ({} healed), skin {:?}", n + 1, found.iod, found.spots.len(), dialog.healed(n).len(), found.skin.bounds());
            }
            eprintln!("  {}", names.join(", "));
            let after = editor.doc.composite();
            let (iw, ih) = (before.width() as f32, before.height() as f32);
            for (n, face) in dialog.faces.iter().enumerate() {
                let [l, t, r, b] = face.found.bounds;
                let pad = (r - l).max(b - t) * 0.25;
                let [x0, y0, x1, y1] = [(l - pad).max(0.0), (t - pad).max(0.0), (r + pad).min(iw), (b + pad).min(ih)];
                let step = ((x1 - x0) / 900.0).max(1.0);
                let (ow, oh) = (((x1 - x0) / step) as u32, ((y1 - y0) / step) as u32);
                let mut ppm = format!("P6 {} {} 255\n", ow * 2, oh).into_bytes();
                for y in 0..oh {
                    for half in [&before, &after] {
                        for x in 0..ow {
                            let p = half.get((x0 + x as f32 * step) as u32, (y0 + y as f32 * step) as u32);
                            ppm.extend([0, 1, 2].map(|c| (p[c] >> 8) as u8));
                        }
                    }
                }
                let ppm_path = format!("{out}/{name}-face-{}.ppm", n + 1);
                std::fs::write(&ppm_path, ppm).unwrap();
                let png = format!("{out}/{name}-face-{}.png", n + 1);
                std::process::Command::new("magick").args([&ppm_path, &png]).status().unwrap();
                std::fs::remove_file(ppm_path).unwrap();
            }
        }
    }
}
