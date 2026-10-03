//! Body Reshape (docs/AI.md, feature 8): while Liquify is open, sliders
//! that reshape each person the pose model finds in the layer
//! (`omapix_engine::body`), in a panel like Face-Aware Liquify's, with the
//! brushes' warp over theirs.

use std::sync::Mutex;

use omapix_ai::face::Image;
use omapix_ai::pose::{Pose, Poses};
use omapix_engine::Document;
use omapix_engine::body::{Body, Shape};

use crate::face_liquify::{Group, Panel, Sliders};
use crate::face_selection::srgb;

/// Loaded on first use, and kept until Generative Fill needs the card.
static MODELS: Mutex<Option<Poses>> = Mutex::new(None);

/// Drop the model, giving its GPU memory back. It loads again when next
/// used.
pub fn unload() {
    if let Ok(mut model) = MODELS.lock() {
        *model = None;
    }
}

const SLIDERS: [Group<Shape>; 3] = [
    (
        "Head",
        true,
        -100.0,
        None,
        &[("Head Size", "A larger head", |s| &mut s.head), ("Neck Length", "A longer neck", |s| &mut s.neck)],
    ),
    (
        "Torso",
        true,
        -100.0,
        None,
        &[
            ("Shoulders", "Wider shoulders", |s| &mut s.shoulders),
            ("Waist", "A wider waist", |s| &mut s.waist),
            ("Hips", "Wider hips", |s| &mut s.hips),
        ],
    ),
    (
        "Arms and Legs",
        true,
        -100.0,
        None,
        &[
            ("Arms", "Thicker arms", |s| &mut s.arms),
            ("Legs", "Thicker legs", |s| &mut s.legs),
            ("Leg Length", "Longer legs, from the hips down", |s| &mut s.leg_length),
        ],
    ),
];

impl Sliders for Body {
    const TITLE: &'static str = "Body Reshape";
    // Beside Face-Aware Liquify's.
    const PLACE: f32 = 610.0;
    const NAME: &'static str = "Body";
    const FINDING: &'static str = "Finding bodies…";
    const NONE: &'static str = "Found nobody with their shoulders in view";
    const GROUPS: &'static [Group<Shape>] = &SLIDERS;

    fn find(doc: &Document) -> Result<Vec<Body>, String> {
        let image = doc.composite();
        let srgb = srgb(&image, &doc.profile)?;
        let mut models = MODELS.lock().map_err(|e| e.to_string())?;
        let poses = match &mut *models {
            Some(poses) => poses,
            None => models.insert(Poses::load()?),
        };
        let image = Image { pixels: &srgb, width: image.width() as usize, height: image.height() as usize };
        Ok(poses.find(&image)?.iter().filter_map(Pose::body).collect())
    }
}

pub type BodyLiquify = Panel<Body>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;
    use crate::face_liquify::tests::{a_face, frame, frame_at};
    use egui::Pos2;
    use omapix_engine::body::{Joints, Matte};
    use omapix_engine::reshape::Face;
    use omapix_engine::{ColorProfile, Raster, reshape};

    const W: u32 = 600;
    const H: u32 = 600;

    /// Someone standing in a 600 × 600 image, only their top half in view:
    /// shoulders at y = 200 and hips at y = 360 either side of x = 300, a
    /// torso 120 wide and a head 40 in radius at (300, 130).
    fn body() -> Body {
        const SIZE: usize = 150;
        let cover: Vec<f32> = (0..SIZE * SIZE)
            .map(|k| {
                let (x, y) = ((k % SIZE) as f32 * 4.0 + 2.0 - 300.0, (k / SIZE) as f32 * 4.0 + 2.0);
                let torso = x.abs() <= 60.0 && (200.0..=420.0).contains(&y);
                if torso || x.hypot(y - 130.0) <= 40.0 || (x.abs() <= 15.0 && (130.0..=200.0).contains(&y)) { 1.0 } else { 0.0 }
            })
            .collect();
        let pair = |x: f32, y: f32| [[300.0 + x, y, 1.0], [300.0 - x, y, 1.0]];
        let joints = Joints { ears: pair(20.0, 130.0), shoulders: pair(50.0, 200.0), hips: pair(30.0, 360.0), ..Default::default() };
        Body::new(&joints, &Matte { centre: [300.0; 2], side: 600.0, angle: 0.0, size: SIZE, cover: &cover }).unwrap()
    }

    /// Liquify open on a grey image with a red spot, 16 px across, at the
    /// left edge of [`body`]'s waist, and that body found.
    fn liquifying() -> Editor {
        let pixels = (0..W * H).map(|i| {
            let (x, y) = ((i % W) as f32 - 248.0, (i / W) as f32 - 300.0);
            if x.hypot(y) <= 8.0 { [65535, 0, 0, 65535] } else { [30000, 30000, 30000, 65535] }
        });
        let image = Raster::new(W, H, pixels.collect());
        let mut editor = Editor::new(Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16)).unwrap();
        editor.begin_liquify().unwrap();
        editor.set_liquify_figures(editor.active, vec![body()]);
        editor
    }

    fn red(editor: &Editor, x: u32, y: u32) -> bool {
        editor.doc.layers[0].pixels.get(x, y)[1] < 10000
    }

    #[test]
    fn a_body_slider_drag_reshapes_the_body_as_one_step() {
        let ctx = egui::Context::default();
        let (mut editor, mut panel) = (liquifying(), BodyLiquify::default());
        assert!(frame(&ctx, &mut panel, &mut editor, false));
        assert!(editor.undo_label().is_none());
        assert!(red(&editor, 242, 300) && !red(&editor, 259, 300));

        // The waist's edge comes in as the slider's dragged, all one step.
        for waist in [-40.0, -100.0] {
            editor.shape_liquify::<Body>(&[Shape { waist, ..Default::default() }]);
        }
        editor.end_liquify_stroke();
        assert!(red(&editor, 259, 300) && !red(&editor, 242, 300));
        editor.undo();
        assert!(red(&editor, 242, 300) && !red(&editor, 259, 300));
        assert!(editor.undo_label().is_none());
        editor.redo();
        assert!(red(&editor, 259, 300));

        // Applied, it's one Liquify step, and Liquify opened again carries
        // on with the body and its shape.
        editor.commit_liquify();
        assert_eq!(editor.undo_label(), Some("Liquify"));
        editor.begin_liquify().unwrap();
        assert_eq!(editor.liquify_figures::<Body>().unwrap()[0].1.waist, -100.0);
    }

    #[test]
    fn the_panel_names_its_sliders_and_says_when_nobody_is_found() {
        let ctx = egui::Context::default();
        let (mut editor, mut panel) = (liquifying(), BodyLiquify::default());
        let away = Pos2::new(10.0, 500.0);
        frame_at(&ctx, &mut panel, &mut editor, away, false);
        let (_, texts) = frame_at(&ctx, &mut panel, &mut editor, away, false);
        let has = |texts: &[(String, Pos2)], text: &str| texts.iter().any(|(t, _)| t == text);
        assert!(has(&texts, "Body Reshape") && has(&texts, "Waist") && has(&texts, "Leg Length") && has(&texts, "Head Size"));
        assert!(!has(&texts, "Body 1"), "only one");

        editor.cancel_liquify();
        editor.begin_liquify().unwrap();
        editor.set_liquify_figures(editor.active, Vec::<Body>::new());
        frame_at(&ctx, &mut panel, &mut editor, away, false);
        let (_, texts) = frame_at(&ctx, &mut panel, &mut editor, away, false);
        assert!(has(&texts, "Found nobody with their shoulders in view") && !has(&texts, "Waist"));
    }

    #[test]
    fn faces_and_bodies_are_shaped_together_and_undone_in_turn() {
        let mut editor = liquifying();
        editor.set_liquify_figures(editor.active, vec![a_face()]);
        let narrow = Shape { waist: -100.0, ..Default::default() };
        editor.shape_liquify::<Body>(&[narrow]);
        editor.end_liquify_stroke();
        let wide_eyed = reshape::Shape { eye_size: 100.0, ..Default::default() };
        editor.shape_liquify::<Face>(&[wide_eyed]);
        editor.end_liquify_stroke();
        assert!(red(&editor, 259, 300));
        let shapes = |editor: &Editor| (editor.liquify_figures::<Face>().unwrap()[0].1, editor.liquify_figures::<Body>().unwrap()[0].1);
        assert_eq!(shapes(&editor), (wide_eyed, narrow));
        // The face's sliders are undone first, then the body's.
        editor.undo();
        assert_eq!(shapes(&editor), (Default::default(), narrow));
        assert!(red(&editor, 259, 300));
        editor.undo();
        assert_eq!(shapes(&editor), Default::default());
        assert!(red(&editor, 242, 300) && !red(&editor, 259, 300));
        editor.redo();
        editor.redo();
        assert_eq!(shapes(&editor), (wide_eyed, narrow));
        // Preview off hides both; applied, both are kept.
        editor.show_liquify_shapes(false);
        assert!(red(&editor, 242, 300) && !red(&editor, 259, 300));
        editor.commit_liquify();
        assert!(red(&editor, 259, 300));
    }

    /// A look at real photos: for `OMAPIX_BODY_PHOTO` (a photo, or a folder
    /// of them), how long everyone in it takes to find, and to give every
    /// slider −50 as a slider's dragged in a canvas 1600 × 1000 and then
    /// let go. Needs the model (scripts/fetch-models.sh); omapix-ai's `pose`
    /// example draws what's found, and the photo before and after.
    #[test]
    #[ignore]
    fn body_reshape_in_photos() {
        use crate::canvas::Render;
        use egui::{Rect, vec2};
        let Ok(photos) = std::env::var("OMAPIX_BODY_PHOTO") else {
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
            let Ok(doc) = omapix_engine::io::load(&path) else { continue };
            let t = std::time::Instant::now();
            let bodies = Body::find(&doc).unwrap();
            let found_in = t.elapsed();
            let (w, h) = (doc.width, doc.height);
            let mut editor = Editor::new(doc).unwrap();
            let zoom = (1600.0 / w as f32).min(1000.0 / h as f32).min(1.0);
            editor.canvas.set_render(std::sync::Arc::new(Render::new(editor.doc.composite())));
            editor.canvas.lay_out_for_test(Rect::from_min_size(Pos2::ZERO, vec2(1600.0, 1000.0)), zoom);
            editor.begin_liquify().unwrap();
            editor.set_liquify_figures(editor.active, bodies.clone());
            let mut shape = Shape::default();
            for (.., at) in SLIDERS.iter().flat_map(|(.., sliders)| *sliders) {
                *at(&mut shape) = -25.0;
            }
            let t = std::time::Instant::now();
            editor.shape_liquify::<Body>(&vec![shape; bodies.len()]);
            let first = t.elapsed();
            for (.., at) in SLIDERS.iter().flat_map(|(.., sliders)| *sliders) {
                *at(&mut shape) = -50.0;
            }
            let t = std::time::Instant::now();
            editor.shape_liquify::<Body>(&vec![shape; bodies.len()]);
            let quick = t.elapsed();
            let t = std::time::Instant::now();
            editor.end_liquify_stroke();
            eprintln!(
                "{name}: {w} × {h}, {} found in {found_in:?}; at {:.0} %, a quick look in {quick:?} (the first {first:?}), warped in {:?}",
                bodies.len(),
                zoom * 100.0,
                t.elapsed()
            );
        }
    }
}
