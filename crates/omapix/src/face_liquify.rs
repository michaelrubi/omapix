//! Face-Aware Liquify and Face Symmetry (docs/AI.md, features 7 and 8):
//! while Liquify is open, sliders that reshape each face the face models
//! find in the layer (`omapix_engine::reshape`), with the brushes' warp
//! over theirs.

use std::sync::mpsc::{Receiver, TryRecvError, channel};

use egui::{RichText, Slider, vec2};
use omapix_ai::face::outline;
use omapix_engine::Document;
use omapix_engine::reshape::{Face, Shape, Sides};

use crate::editor::Editor;
use crate::face_selection::analyse;
use crate::theme::Theme;

/// Down the bridge of the nose, from between the eyes.
const BRIDGE: [usize; 5] = [168, 6, 197, 195, 5];

/// A slider's value in a face's shape.
type Value = fn(&mut Shape) -> &mut f32;

/// What Symmetry's sliders say when hovered, after their own hints.
const FACING: &str = "For faces looking at the camera: less is done as a face turns away, and nothing turned far";

/// A group of sliders: its name and their least value, then each slider's
/// name, what dragging it right does, and its value.
type Group = (&'static str, f32, &'static [(&'static str, &'static str, Value)]);

const SLIDERS: [Group; 5] = [
    (
        "Symmetry",
        0.0,
        &[
            ("Eyes", "Evens the eyes' height, size and shape", |s| &mut s.symmetry.eyes),
            ("Brows", "Evens the eyebrows", |s| &mut s.symmetry.brows),
            ("Nose", "Straightens the nose", |s| &mut s.symmetry.nose),
            ("Mouth", "Evens the mouth and lips", |s| &mut s.symmetry.mouth),
            ("Jaw", "Evens the face's outline", |s| &mut s.symmetry.jaw),
        ],
    ),
    (
        "Eyes",
        -100.0,
        &[
            ("Eye Size", "Larger eyes", |s| &mut s.eye_size),
            ("Eye Distance", "Eyes further apart", |s| &mut s.eye_distance),
        ],
    ),
    (
        "Nose",
        -100.0,
        &[
            ("Nose Length", "A longer nose", |s| &mut s.nose_length),
            ("Nose Width", "A wider nose", |s| &mut s.nose_width),
        ],
    ),
    (
        "Mouth",
        -100.0,
        &[
            ("Smile", "The corners of the mouth up", |s| &mut s.smile),
            ("Lip Fullness", "Fuller lips", |s| &mut s.lips),
            ("Mouth Width", "A wider mouth", |s| &mut s.mouth_width),
        ],
    ),
    (
        "Face Shape",
        -100.0,
        &[
            ("Forehead", "A higher forehead", |s| &mut s.forehead),
            ("Chin Height", "A longer chin", |s| &mut s.chin),
            ("Jawline", "A wider jaw", |s| &mut s.jaw),
            ("Face Width", "A wider face", |s| &mut s.face_width),
        ],
    ),
];

/// Where the faces arrive once they're found.
type Found = Receiver<Result<Vec<Face>, String>>;

#[derive(Default)]
pub struct FaceLiquify {
    /// The layer whose faces are being found.
    finding: Option<(u64, Found)>,
    /// Why there are none, if finding them went wrong.
    failed: Option<String>,
    /// The face the sliders are for.
    selected: usize,
    /// A slider's being dragged: its changes are one step to undo.
    dragging: bool,
}

impl FaceLiquify {
    /// The panel, while Liquify is open: finds the layer's faces the first
    /// time, then shows the selected face's sliders. `open` is its close
    /// button.
    pub fn show(&mut self, ctx: &egui::Context, editor: &mut Editor, theme: &Theme, open: &mut bool) {
        self.find(ctx, editor);
        let mut shapes: Option<Vec<Shape>> = editor.liquify_faces().map(|faces| faces.iter().map(|(_, shape)| *shape).collect());
        egui::Window::new("Face-Aware Liquify")
            .open(open)
            .resizable(false)
            .pivot(egui::Align2::RIGHT_TOP)
            .default_pos(ctx.content_rect().right_top() + vec2(-320.0, 90.0))
            .show(ctx, |ui| match &mut shapes {
                None => drop(ui.label(RichText::new("Finding faces…").color(theme.dark_foreground))),
                Some(shapes) if shapes.is_empty() => {
                    let why = self.failed.as_deref().unwrap_or("Found no faces facing the camera");
                    ui.label(RichText::new(why).color(theme.dark_foreground));
                }
                Some(shapes) => self.sliders(ui, shapes),
            });
        if let Some(shapes) = shapes {
            self.shape(ctx, editor, &shapes);
        }
    }

    /// Start finding the faces in the layer being liquified, as it was
    /// before, if they haven't been looked for; and once they're found,
    /// they're the editor's.
    fn find(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        if editor.liquify_faces().is_some() {
            self.finding = None;
            return;
        }
        let Some((doc, layer)) = editor.liquify_original() else {
            return;
        };
        if self.finding.as_ref().is_none_or(|(finding, _)| *finding != layer) {
            let (tx, rx) = channel();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let _ = tx.send(find(&doc));
                ctx.request_repaint();
            });
            (self.finding, self.failed, self.selected) = (Some((layer, rx)), None, 0);
        }
        let found = match self.finding.as_ref().map(|(_, rx)| rx.try_recv()) {
            Some(Ok(found)) => found,
            Some(Err(TryRecvError::Disconnected)) => Err("Finding faces stopped unexpectedly".into()),
            _ => return,
        };
        self.finding = None;
        // With none, they aren't looked for again.
        self.failed = found.as_ref().err().cloned();
        editor.set_liquify_faces(layer, found.unwrap_or_default());
    }

    /// The face to shape, if there's more than one, then its sliders.
    fn sliders(&mut self, ui: &mut egui::Ui, shapes: &mut [Shape]) {
        self.selected = self.selected.min(shapes.len() - 1);
        if shapes.len() > 1 {
            ui.horizontal_wrapped(|ui| {
                for n in 0..shapes.len() {
                    ui.selectable_value(&mut self.selected, n, format!("Face {}", n + 1))
                        .on_hover_text("Faces are numbered from left to right");
                }
            });
            ui.add_space(4.0);
        }
        let shape = &mut shapes[self.selected];
        egui::Grid::new("face-liquify").num_columns(2).show(ui, |ui| {
            for (group, least, sliders) in SLIDERS {
                ui.label(RichText::new(group).strong());
                ui.end_row();
                for (name, hint, value) in sliders {
                    ui.label(*name);
                    let slider = ui.add(Slider::new(value(shape), least..=100.0).fixed_decimals(0)).on_hover_text(*hint);
                    if group == "Symmetry" {
                        slider.on_hover_text(FACING);
                    }
                    ui.end_row();
                }
            }
        });
        ui.add_space(8.0);
        if ui.add_enabled(*shape != Shape::default(), egui::Button::new("Reset")).clicked() {
            *shape = Shape::default();
        }
    }

    /// Give the faces `shapes`. A slider's whole drag is one step to undo.
    fn shape(&mut self, ctx: &egui::Context, editor: &mut Editor, shapes: &[Shape]) {
        let before = editor.liquify_faces().is_some_and(|faces| faces.iter().map(|(_, shape)| shape).eq(shapes));
        if !before {
            editor.shape_liquify_faces(shapes);
            self.dragging = true;
        }
        if self.dragging && !ctx.input(|i| i.pointer.any_down()) {
            editor.end_liquify_stroke();
            self.dragging = false;
        }
    }
}

/// The faces in `doc` that the landmarker sees (not those in profile), from
/// left to right.
fn find(doc: &Document) -> Result<Vec<Face>, String> {
    let (_, analysis) = analyse(&doc.composite(), &doc.profile)?;
    let faces = analysis.faces.iter().filter_map(|(found, points)| Some((found.bounds[0], face(points.as_deref()?))));
    let mut faces: Vec<_> = faces.collect();
    faces.sort_by(|a, b| a.0.total_cmp(&b.0));
    Ok(faces.into_iter().map(|(_, face)| face).collect())
}

/// A face's features, from the landmarker's `points`.
fn face(points: &[[f32; 3]]) -> Face {
    let at = |ring: &[usize]| ring.iter().map(|&i| points[i]).collect::<Vec<_>>();
    // A ring that crosses the midline at `ring[middle]`, and again half way
    // round: the points either side of there mirror each other.
    let sides = |ring: &[usize], middle: usize| {
        let n = ring.len();
        let at = |k: usize| points[ring[k % n]];
        Sides {
            left: (1..n / 2).map(|k| at(middle + n - k)).collect(),
            right: (1..n / 2).map(|k| at(middle + k)).collect(),
            middle: vec![at(middle), at(middle + n / 2)],
        }
    };
    let pair = |left: &[usize], right: &[usize]| Sides { left: at(left), right: at(right), middle: Vec::new() };
    let mut nose = sides(&outline::NOSE, 2);
    nose.middle.extend(at(&BRIDGE));
    Face {
        eyes: pair(&outline::LEFT_EYE, &outline::RIGHT_EYE),
        brows: pair(&outline::LEFT_BROW, &outline::RIGHT_BROW),
        nose,
        lips: sides(&outline::LIPS, 5),
        mouth: sides(&outline::MOUTH, 5),
        outline: sides(&outline::FACE, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Pos2, Rect};
    use omapix_engine::{ColorProfile, Raster};

    const W: u32 = 600;
    const H: u32 = 600;

    /// A face's 478 points, the same either side of x = 300: eyes 100 apart
    /// at y = 300, the nose's base at y = 365, the mouth at y = 400, and the
    /// outline from y = 170 to 470. Points with no outline are at (300, 300).
    fn landmarks() -> Vec<[f32; 3]> {
        let mut points = vec![[300.0, 300.0, 0.0]; 478];
        // `ring` round an ellipse, clockwise from `first` (in turns, from
        // its right).
        let mut ellipse = |ring: &[usize], first: f32, [cx, cy, rx, ry]: [f32; 4], clockwise: bool| {
            for (k, &i) in ring.iter().enumerate() {
                let turn = k as f32 / ring.len() as f32 * if clockwise { 1.0 } else { -1.0 };
                let (sin, cos) = ((first + turn) * std::f32::consts::TAU).sin_cos();
                points[i] = [cx + rx * cos, cy + ry * sin, 0.0];
            }
        };
        // The right eye and brow go the other way round, as mirror images.
        ellipse(&outline::LEFT_EYE, 0.5, [250.0, 300.0, 18.0, 8.0], true);
        ellipse(&outline::RIGHT_EYE, 0.0, [350.0, 300.0, 18.0, 8.0], false);
        ellipse(&outline::LEFT_BROW, 0.5, [250.0, 275.0, 25.0, 4.0], true);
        ellipse(&outline::RIGHT_BROW, 0.0, [350.0, 275.0, 25.0, 4.0], false);
        // The nose's ring starts two before its base, and the lips' at
        // their left corner; the outline's at its top.
        ellipse(&outline::NOSE, 0.25 + 2.0 / 16.0, [300.0, 355.0, 15.0, 10.0], false);
        ellipse(&outline::LIPS, 0.5, [300.0, 400.0, 35.0, 14.0], true);
        ellipse(&outline::MOUTH, 0.5, [300.0, 400.0, 30.0, 5.0], true);
        ellipse(&outline::FACE, 0.75, [300.0, 320.0, 110.0, 150.0], true);
        for (k, i) in BRIDGE.into_iter().enumerate() {
            points[i] = [300.0, 300.0 + 9.0 * k as f32, 0.0];
        }
        points
    }

    /// Liquify open on a grey image with a red eye, 16 px across, where
    /// [`landmarks`] has its left one, and that face found.
    fn liquifying() -> Editor {
        let pixels = (0..W * H).map(|i| {
            let (x, y) = ((i % W) as f32 - 250.0, (i / W) as f32 - 300.0);
            if x.hypot(y) <= 8.0 { [65535, 0, 0, 65535] } else { [30000, 30000, 30000, 65535] }
        });
        let image = Raster::new(W, H, pixels.collect());
        let mut editor = Editor::new(Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16)).unwrap();
        editor.begin_liquify().unwrap();
        editor.set_liquify_faces(editor.active, vec![face(&landmarks())]);
        editor
    }

    /// A frame of the panel, with the mouse button `down` or not.
    fn frame(ctx: &egui::Context, panel: &mut FaceLiquify, editor: &mut Editor, down: bool) -> bool {
        let button = egui::Event::PointerButton {
            pos: Pos2::new(10.0, 500.0),
            button: egui::PointerButton::Primary,
            pressed: down,
            modifiers: egui::Modifiers::NONE,
        };
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 800.0))),
            events: vec![egui::Event::PointerMoved(Pos2::new(10.0, 500.0)), button],
            ..Default::default()
        };
        let mut open = true;
        let mut out = ctx.run_ui(input, |ui| panel.show(ui.ctx(), editor, &Theme::default(), &mut open));
        out.textures_delta.clear();
        open
    }

    fn shapes(editor: &Editor) -> Vec<Shape> {
        editor.liquify_faces().unwrap().iter().map(|(_, shape)| *shape).collect()
    }

    #[test]
    fn a_faces_points_pair_up_either_side_of_its_midline() {
        let face = face(&landmarks());
        let features = [&face.eyes, &face.brows, &face.nose, &face.lips, &face.mouth, &face.outline];
        for (n, sides) in features.into_iter().enumerate() {
            assert!(!sides.left.is_empty() && sides.left.len() == sides.right.len(), "{n}");
            for (l, r) in sides.left.iter().zip(&sides.right) {
                assert!(l[0] < 299.0 && (l[0] + r[0] - 600.0).abs() < 1e-3 && (l[1] - r[1]).abs() < 1e-3, "{n}: {l:?} {r:?}");
            }
            assert!(sides.middle.iter().all(|m| (m[0] - 300.0).abs() < 1e-3), "{n}");
        }
        assert_eq!((face.eyes.left.len(), face.nose.left.len(), face.nose.middle.len()), (16, 7, 7));
        assert_eq!((face.lips.left.len(), face.outline.left.len()), (9, 17));
        // The lips' points and the mouth's go together: the lips' are
        // further out, the same way from the mouth's middle.
        for (lip, mouth) in face.lips.left.iter().zip(&face.mouth.left) {
            let (lip, mouth) = ([lip[0] - 300.0, lip[1] - 400.0], [mouth[0] - 300.0, mouth[1] - 400.0]);
            assert!(lip[0] * mouth[0] + lip[1] * mouth[1] > 0.0 && lip[0].hypot(lip[1]) > mouth[0].hypot(mouth[1]));
        }
    }

    #[test]
    fn a_slider_drag_reshapes_the_face_as_one_step_under_the_brushes() {
        let ctx = egui::Context::default();
        let (mut editor, mut panel) = (liquifying(), FaceLiquify::default());
        let red = |editor: &Editor, x, y| editor.doc.layers[0].pixels.get(x, y)[1] < 10000;
        assert!(frame(&ctx, &mut panel, &mut editor, false));
        assert!(panel.finding.is_none() && editor.undo_label().is_none());
        assert!(red(&editor, 250, 300) && !red(&editor, 262, 300));

        // The eye grows as the slider's dragged, and it's all one step.
        frame(&ctx, &mut panel, &mut editor, true);
        for size in [40.0, 100.0] {
            panel.shape(&ctx, &mut editor, &[Shape { eye_size: size, eye_distance: 100.0, ..Default::default() }]);
        }
        assert!(red(&editor, 235, 300) && !red(&editor, 258, 300), "bigger, and further out");
        frame(&ctx, &mut panel, &mut editor, false);
        assert!(!panel.dragging);
        assert_eq!(shapes(&editor)[0].eye_size, 100.0);

        // A brush stroke goes over it: the eye, where it is now, pushed down.
        editor.liquify(omapix_engine::warp::Brush::ForwardWarp, &[([242.0, 300.0], [0.0, 20.0])], 40.0, 1.0);
        editor.end_liquify_stroke();
        assert!(red(&editor, 242, 318) && !red(&editor, 242, 290));

        // Undo takes the stroke back, then the whole drag; redo brings it back.
        editor.undo();
        assert!(red(&editor, 235, 300) && !red(&editor, 242, 318));
        editor.undo();
        assert_eq!(shapes(&editor), [Shape::default()]);
        assert!(red(&editor, 250, 300) && !red(&editor, 238, 300));
        assert!(editor.undo_label().is_none());
        editor.redo();
        assert_eq!(shapes(&editor)[0].eye_size, 100.0);
        assert!(red(&editor, 235, 300));

        // Applied, it's one Liquify step, and opening Liquify again carries
        // on with the face and its shape.
        editor.commit_liquify();
        assert_eq!(editor.undo_label(), Some("Liquify"));
        editor.begin_liquify().unwrap();
        assert_eq!(shapes(&editor)[0].eye_size, 100.0);
        editor.shape_liquify_faces(&[Shape::default()]);
        assert!(red(&editor, 250, 300) && !red(&editor, 238, 300));
        // Esc puts it back as it was applied.
        editor.cancel_liquify();
        assert!(red(&editor, 235, 300));
    }

    /// A look at real photos: for `OMAPIX_FACE_PHOTO` (a photo, or a folder
    /// of them), each face before and after `SHAPE` side by side as
    /// `<photo>-face-<n>.png` in `OMAPIX_FACE_OUT`. `SHAPE` is sliders by
    /// name, as `Symmetry=100` (all five), `Jaw=50,Face Width=-40`. Needs
    /// the models (scripts/fetch-models.sh) and ImageMagick.
    #[test]
    #[ignore]
    fn face_liquify_in_photos() {
        let (Ok(photos), Ok(out)) = (std::env::var("OMAPIX_FACE_PHOTO"), std::env::var("OMAPIX_FACE_OUT")) else {
            return;
        };
        let mut shape = Shape::default();
        for setting in std::env::var("SHAPE").unwrap_or("Symmetry=100".into()).split(',') {
            let (name, value) = setting.split_once('=').unwrap();
            let value: f32 = value.parse().unwrap();
            let group = SLIDERS.iter().find(|(group, ..)| *group == name).map(|(.., sliders)| *sliders);
            let slider = SLIDERS.iter().flat_map(|(.., sliders)| *sliders).find(|(slider, ..)| *slider == name);
            for (.., at) in group.into_iter().flatten().chain(slider) {
                *at(&mut shape) = value;
            }
        }
        assert_ne!(shape, Shape::default(), "no such slider");
        let photos = std::path::Path::new(&photos);
        let mut paths: Vec<_> = match std::fs::read_dir(photos) {
            Ok(dir) => dir.map(|e| e.unwrap().path()).collect(),
            Err(_) => vec![photos.to_owned()],
        };
        paths.sort();
        for path in paths {
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            let Ok(doc) = omapix_engine::io::load(&path) else { continue };
            let before = doc.composite();
            let t = std::time::Instant::now();
            let faces = match find(&doc) {
                Ok(faces) => faces,
                Err(e) => {
                    eprintln!("{name}: {e}");
                    continue;
                }
            };
            let found_in = t.elapsed();
            let mut editor = Editor::new(doc).unwrap();
            editor.begin_liquify().unwrap();
            editor.set_liquify_faces(editor.active, faces.clone());
            let t = std::time::Instant::now();
            editor.shape_liquify_faces(&vec![shape; faces.len()]);
            eprintln!("{name}: {} faces found in {found_in:?}, reshaped in {:?}", faces.len(), t.elapsed());
            editor.commit_liquify();
            let after = editor.doc.composite();
            let (iw, ih) = (before.width() as f32, before.height() as f32);
            for (n, face) in faces.iter().enumerate() {
                let xs = || face.outline.left.iter().chain(&face.outline.right).chain(&face.outline.middle);
                let bound = |c: usize, least: bool| xs().map(|p| p[c]).fold(if least { f32::MAX } else { f32::MIN }, if least { f32::min } else { f32::max });
                let [l, t, r, b] = [bound(0, true), bound(1, true), bound(0, false), bound(1, false)];
                let iod = (face.eyes.right[0][0] - face.eyes.left[0][0]).hypot(face.eyes.right[0][1] - face.eyes.left[0][1]);
                eprintln!("  face {}: {:.0} × {:.0}, eyes' outer corners {iod:.0} apart", n + 1, r - l, b - t);
                let pad = (r - l).max(b - t) * 0.3;
                let [x0, y0, x1, y1] = [(l - pad).max(0.0), (t - pad).max(0.0), (r + pad).min(iw), (b + pad).min(ih)];
                let step = ((x1 - x0) / 800.0).max(1.0);
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

    #[test]
    fn faces_are_found_once_and_none_says_why() {
        let ctx = egui::Context::default();
        let (mut editor, mut panel) = (liquifying(), FaceLiquify::default());
        editor.cancel_liquify();
        editor.begin_liquify().unwrap();
        // Still looking: nothing to shape yet.
        let (tx, rx) = channel();
        panel.finding = Some((editor.active, rx));
        frame(&ctx, &mut panel, &mut editor, false);
        assert!(editor.liquify_faces().is_none());
        tx.send(Ok(vec![face(&landmarks()), face(&landmarks())])).unwrap();
        frame(&ctx, &mut panel, &mut editor, false);
        assert_eq!(shapes(&editor), [Shape::default(); 2]);
        assert!(panel.finding.is_none());

        // Going wrong, there are none, with the reason, and they aren't
        // looked for again.
        editor.cancel_liquify();
        editor.begin_liquify().unwrap();
        let (tx, rx) = channel();
        panel.finding = Some((editor.active, rx));
        tx.send(Err("No models".into())).unwrap();
        frame(&ctx, &mut panel, &mut editor, false);
        frame(&ctx, &mut panel, &mut editor, false);
        assert_eq!(panel.failed.as_deref(), Some("No models"));
        assert!(shapes(&editor).is_empty() && panel.finding.is_none());
    }
}
