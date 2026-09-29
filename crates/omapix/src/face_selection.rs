//! Select › Skin, Hair, Eyes, Lips and Teeth (docs/AI.md, feature 2), found
//! by the face models in omapix-ai on a thread of their own. Skin and hair
//! come from the segmentation, and skin leaves out each face's eyes, brows
//! and lips; eyes, lips and teeth are drawn from each face's points.

use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::sync::{Arc, Mutex};

use omapix_ai::face::{BODY_SKIN, FACE_SKIN, Faces, HAIR, Image, outline};
use omapix_engine::selection::{Combine, Selection};
use omapix_engine::tiled::{TILE, Tiled};
use omapix_engine::{ColorProfile, DisplayTransform, refine};
use rayon::prelude::*;

use crate::canvas::Render;
use crate::editor::Editor;

/// Loaded on first use and kept until Omapix quits: dropping CUDA sessions
/// can crash it as it quits (docs/AI.md).
static MODELS: Mutex<Option<Faces>> = Mutex::new(None);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Skin,
    Hair,
    /// The eye openings: whites and irises.
    Eyes,
    /// The lips, less the mouth between them.
    Lips,
    /// The mouth between the lips, where it's open.
    Teeth,
}

impl Part {
    fn label(self) -> &'static str {
        match self {
            Part::Skin => "Select Skin",
            Part::Hair => "Select Hair",
            Part::Eyes => "Select Eyes",
            Part::Lips => "Select Lips",
            Part::Teeth => "Select Teeth",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Part::Skin => "skin",
            Part::Hair => "hair",
            Part::Eyes => "eyes",
            Part::Lips => "lips",
            Part::Teeth => "teeth",
        }
    }

    /// What the status bar says while it's found.
    pub fn finding(self) -> String {
        format!("Finding {}…", self.name())
    }
}

/// Outlines to draw (added or taken away, in order) round each face, grown
/// and feathered by fractions of the distance between its irises.
struct Shapes {
    outlines: &'static [(&'static [usize], Combine)],
    grow: f32,
    feather: f32,
}

/// What Skin leaves out: grown a little to take in lashes and the edges of
/// lipstick.
const FEATURES: Shapes = Shapes {
    outlines: &[
        (&outline::LEFT_EYE, Combine::Add),
        (&outline::RIGHT_EYE, Combine::Add),
        (&outline::LEFT_BROW, Combine::Add),
        (&outline::RIGHT_BROW, Combine::Add),
        (&outline::LIPS, Combine::Add),
    ],
    grow: 0.03,
    feather: 0.015,
};
const EYES: Shapes = Shapes {
    outlines: &[(&outline::LEFT_EYE, Combine::Add), (&outline::RIGHT_EYE, Combine::Add)],
    grow: 0.0,
    feather: 0.01,
};
const LIPS: Shapes = Shapes {
    outlines: &[(&outline::LIPS, Combine::Add), (&outline::MOUTH, Combine::Subtract)],
    grow: 0.0,
    feather: 0.01,
};
const TEETH: Shapes = Shapes {
    outlines: &[(&outline::MOUTH, Combine::Add)],
    grow: 0.0,
    feather: 0.01,
};

/// The middle of the upper and lower lips' inner edges: a mouth whose are
/// closer than [`OPEN`] shows no teeth.
const MOUTH_GAP: [usize; 2] = [13, 14];
const OPEN: f32 = 0.03;

/// Where the selection arrives once it's found.
type Found = Receiver<Result<Selection, String>>;

#[derive(Default)]
pub struct FaceSelection {
    /// What's being found, and how it will combine with the selection.
    running: Option<(Part, Combine, Found)>,
}

impl FaceSelection {
    /// Find `part` in what `editor` shows.
    pub fn start(&mut self, ctx: &egui::Context, editor: &Editor, part: Part, how: Combine) -> Result<(), String> {
        let image = editor.canvas.render().ok_or("The image isn't ready yet")?;
        let (image, profile) = (Arc::clone(image), editor.doc.profile.clone());
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(find(&image, &profile, part));
            ctx.request_repaint();
        });
        self.running = Some((part, how, rx));
        Ok(())
    }

    /// Make it the selection once it's found. Returns what went wrong, if
    /// anything.
    pub fn poll(&mut self, editor: &mut Editor) -> Option<String> {
        let (part, how, rx) = self.running.as_ref()?;
        let (part, how) = (*part, *how);
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err(format!("{} stopped unexpectedly", part.label())),
        };
        self.running = None;
        match result {
            Ok(selection) => {
                editor.set_selection(part.label(), selection, how);
                None
            }
            Err(e) => Some(e),
        }
    }

    /// What's being found, if anything.
    pub fn busy(&self) -> Option<Part> {
        self.running.as_ref().map(|(part, ..)| *part)
    }
}

fn find(image: &Render, profile: &ColorProfile, part: Part) -> Result<Selection, String> {
    let transform = DisplayTransform::to_srgb(profile).map_err(|e| e.to_string())?;
    image
        .with_image(|image| {
            let mut srgb = vec![[0u8; 4]; image.pixels().len()];
            // In parallel: a 24 MP image takes half a second on one thread.
            srgb.par_chunks_mut(1 << 16)
                .zip(image.pixels().par_chunks(1 << 16))
                .for_each(|(out, pixels)| transform.convert(pixels, out));
            let analysis = {
                let mut models = MODELS.lock().map_err(|e| e.to_string())?;
                let faces = match &mut *models {
                    Some(faces) => faces,
                    None => models.insert(Faces::load()?),
                };
                let (width, height) = (image.width() as usize, image.height() as usize);
                faces.analyse(&Image {
                    pixels: &srgb,
                    width,
                    height,
                })?
            };
            let (width, height) = (image.width(), image.height());
            let faces: Vec<&[[f32; 3]]> = analysis.faces.iter().filter_map(|(_, p)| p.as_deref()).collect();
            let found = match part {
                Part::Skin | Part::Hair => {
                    let classes: &[usize] = if part == Part::Skin { &[BODY_SKIN, FACE_SKIN] } else { &[HAIR] };
                    let (logits, lw, lh) = analysis.logits(classes);
                    let found = Selection::from_coverage(refine::mask_coverage(&logits, lw, lh, 0.0, image));
                    if part == Part::Skin {
                        found.combine(&draw(&faces, &FEATURES, width, height), Combine::Subtract)
                    } else {
                        found
                    }
                }
                Part::Eyes => draw(&faces, &EYES, width, height),
                Part::Lips => draw(&faces, &LIPS, width, height),
                Part::Teeth => {
                    let open: Vec<_> = faces.into_iter().filter(|p| mouth_open(p)).collect();
                    draw(&open, &TEETH, width, height)
                }
            };
            if found.is_empty() {
                Err(format!("Found no {}", part.name()))
            } else {
                Ok(found)
            }
        })
        .ok_or("The image isn't ready yet")?
}

/// The distance between a face's irises' centres, which its features are
/// measured in.
fn iod(points: &[[f32; 3]]) -> f32 {
    let [l, r] = [outline::LEFT_IRIS[0], outline::RIGHT_IRIS[0]].map(|i| points[i]);
    (r[0] - l[0]).hypot(r[1] - l[1])
}

fn mouth_open(points: &[[f32; 3]]) -> bool {
    let [a, b] = MOUTH_GAP.map(|i| points[i]);
    (b[0] - a[0]).hypot(b[1] - a[1]) > OPEN * iod(points)
}

/// `shapes` drawn round each face (from its points), in an image `width` ×
/// `height`.
fn draw(faces: &[&[[f32; 3]]], shapes: &Shapes, width: u32, height: u32) -> Selection {
    // Room round the outlines for growing and feathering.
    const PAD: f32 = 0.2;
    // Drawn in a box round each face only, then placed in the image.
    let boxes: Vec<([u32; 4], Selection)> = faces
        .iter()
        .filter_map(|points| {
            let iod = iod(points);
            let pad = PAD * iod;
            let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
            for &i in shapes.outlines.iter().flat_map(|(ring, _)| ring.iter()) {
                for c in 0..2 {
                    lo[c] = lo[c].min(points[i][c] - pad);
                    hi[c] = hi[c].max(points[i][c] + pad);
                }
            }
            let (x0, y0) = (lo[0].max(0.0) as u32, lo[1].max(0.0) as u32);
            let (x1, y1) = (
                (hi[0].ceil().max(0.0) as u32).min(width),
                (hi[1].ceil().max(0.0) as u32).min(height),
            );
            if x1 <= x0 || y1 <= y0 {
                return None;
            }
            let (w, h) = (x1 - x0, y1 - y0);
            let mut drawn = Selection::from_coverage(Tiled::new(w, h, 0));
            for &(ring, how) in shapes.outlines {
                let shape: Vec<(f32, f32)> = ring
                    .iter()
                    .map(|&i| (points[i][0] - x0 as f32, points[i][1] - y0 as f32))
                    .collect();
                drawn = drawn.combine(&Selection::polygon(w, h, &shape), how);
            }
            let drawn = drawn.expand(shapes.grow * iod).feather(shapes.feather * iod);
            Some(([x0, y0, x1, y1], drawn))
        })
        .collect();
    Selection::from_coverage(Tiled::from_tiles(width, height, 0, |col, row| {
        let (tx, ty) = (col * TILE, row * TILE);
        let (tx1, ty1) = ((tx + TILE).min(width), (ty + TILE).min(height));
        let near: Vec<_> = boxes
            .iter()
            .filter(|([x0, y0, x1, y1], _)| *x0 < tx1 && tx < *x1 && *y0 < ty1 && ty < *y1)
            .collect();
        if near.is_empty() {
            return None;
        }
        let mut tile = vec![0u16; (TILE * TILE) as usize];
        for ([x0, y0, x1, y1], drawn) in near {
            for y in ty.max(*y0)..ty1.min(*y1) {
                for x in tx.max(*x0)..tx1.min(*x1) {
                    let v = &mut tile[((y - ty) * TILE + x - tx) as usize];
                    *v = (*v).max(drawn.coverage.get(x - x0, y - y0));
                }
            }
        }
        Some(tile)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use omapix_engine::{Document, Raster};

    /// Outline `ring` of `points` as the box from `(x0, y0)` to `(x1, y1)`,
    /// clockwise from its top left corner.
    fn outline_box(points: &mut [[f32; 3]], ring: &[usize], [x0, y0, x1, y1]: [f32; 4]) {
        let (w, h) = (x1 - x0, y1 - y0);
        for (k, &i) in ring.iter().enumerate() {
            let t = k as f32 / ring.len() as f32 * 4.0;
            let (x, y) = match t {
                t if t < 1.0 => (x0 + w * t, y0),
                t if t < 2.0 => (x1, y0 + h * (t - 1.0)),
                t if t < 3.0 => (x1 - w * (t - 2.0), y1),
                t => (x0, y1 - h * (t - 3.0)),
            };
            points[i] = [x, y, 0.0];
        }
    }

    /// A face whose points are all at (300, 300) but for an eye outlined as
    /// a 40 × 20 box centred on (250, 280), lips as a 100 × 60 box centred
    /// on (300, 400) round a mouth open 60 × 20, and irises 100 apart.
    fn face() -> Vec<[f32; 3]> {
        let mut points = vec![[300.0, 300.0, 0.0]; 478];
        outline_box(&mut points, &outline::LEFT_EYE, [230.0, 270.0, 270.0, 290.0]);
        outline_box(&mut points, &outline::LIPS, [250.0, 370.0, 350.0, 430.0]);
        outline_box(&mut points, &outline::MOUTH, [270.0, 390.0, 330.0, 410.0]);
        points[MOUTH_GAP[0]] = [300.0, 390.0, 0.0];
        points[MOUTH_GAP[1]] = [300.0, 410.0, 0.0];
        points[outline::LEFT_IRIS[0]] = [250.0, 280.0, 0.0];
        points[outline::RIGHT_IRIS[0]] = [350.0, 280.0, 0.0];
        points
    }

    #[test]
    fn features_cover_the_eye_grown_a_little_and_nothing_far_from_the_face() {
        let face = face();
        let features = draw(&[&face], &FEATURES, 1000, 800);
        assert_eq!((features.width(), features.height()), (1000, 800));
        assert!(features.at(250, 280) > 0.99);
        // Grown by 3 px (0.03 × 100), feathered by 1.5.
        assert!(features.at(271, 280) > 0.5, "{}", features.at(271, 280));
        assert!(features.at(280, 280) < 0.01);
        assert!(features.at(250, 320) < 0.01);
        // The lips and the mouth, all of it.
        assert!(features.at(300, 400) > 0.99 && features.at(260, 380) > 0.99);
        // Tiles away from the face stay empty.
        assert!(features.coverage.tile(3, 2).is_none());
    }

    #[test]
    fn eyes_lips_and_teeth_are_drawn_inside_their_outlines() {
        let face = face();
        let eyes = draw(&[&face], &EYES, 1000, 800);
        assert!(eyes.at(250, 280) > 0.99);
        assert!(eyes.at(274, 280) < 0.01, "not grown: {}", eyes.at(274, 280));
        assert!(eyes.at(300, 400) < 0.01);

        // The lips have a hole where the mouth is; the teeth are that hole.
        let lips = draw(&[&face], &LIPS, 1000, 800);
        let teeth = draw(&[&face], &TEETH, 1000, 800);
        assert!(lips.at(260, 380) > 0.99 && lips.at(300, 375) > 0.99);
        assert!(lips.at(300, 400) < 0.01);
        assert!(teeth.at(300, 400) > 0.99);
        assert!(teeth.at(260, 380) < 0.01 && teeth.at(250, 280) < 0.01);
    }

    #[test]
    fn a_closed_mouth_shows_no_teeth() {
        let mut face = face();
        assert!(mouth_open(&face));
        // 2 px apart, with irises 100 apart.
        face[MOUTH_GAP[1]] = [300.0, 392.0, 0.0];
        assert!(!mouth_open(&face));
    }

    /// A look at a real photo: each part found in `OMAPIX_FACE_PHOTO`,
    /// tinted over a small copy, as `skin.ppm`, `hair.ppm` and so on in
    /// `OMAPIX_FACE_OUT`. Needs the models (scripts/fetch-models.sh).
    #[test]
    #[ignore]
    fn select_parts_in_a_photo() {
        let (Ok(photo), Ok(out)) = (std::env::var("OMAPIX_FACE_PHOTO"), std::env::var("OMAPIX_FACE_OUT")) else {
            return;
        };
        let doc = omapix_engine::io::load(std::path::Path::new(&photo)).unwrap();
        let render = Render::new(doc.composite());
        for part in [Part::Skin, Part::Hair, Part::Eyes, Part::Lips, Part::Teeth] {
            let started = std::time::Instant::now();
            let found = match find(&render, &doc.profile, part) {
                Ok(found) => found,
                Err(e) => {
                    eprintln!("{part:?}: {e}");
                    continue;
                }
            };
            eprintln!("{part:?} in {:?}, bounds {:?}", started.elapsed(), found.bounds());
            let (w, h) = (doc.width, doc.height);
            let ow = w.min(3000);
            let oh = ow * h / w;
            let mut ppm = format!("P6 {ow} {oh} 255\n").into_bytes();
            render.with_image(|image| {
                for y in 0..oh {
                    for x in 0..ow {
                        let (sx, sy) = (x * w / ow, y * h / oh);
                        let (p, k) = (image.get(sx, sy), found.at(sx, sy) * 0.6);
                        ppm.extend(
                            [0, 1, 2].map(|c| (f32::from(p[c] >> 8) * (1.0 - k) + [255.0, 0.0, 80.0][c] * k) as u8),
                        );
                    }
                }
            });
            std::fs::write(format!("{out}/{part:?}.ppm").to_lowercase(), ppm).unwrap();
        }
    }

    #[test]
    fn a_found_part_becomes_the_selection_combined_as_asked_in_one_undo_step() {
        let image = Raster::new(600, 400, vec![[30000, 30000, 30000, 65535]; 600 * 400]);
        let doc = Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16);
        let mut editor = Editor::new(doc).unwrap();
        editor.set_selection(
            "Rectangle",
            Selection::rectangle(600, 400, (0.0, 0.0), (100.0, 100.0)),
            Combine::Replace,
        );

        let (tx, rx) = channel();
        let mut faces = FaceSelection {
            running: Some((Part::Skin, Combine::Add, rx)),
        };
        assert_eq!(faces.poll(&mut editor), None);
        assert_eq!(faces.busy(), Some(Part::Skin));
        tx.send(Ok(Selection::rectangle(600, 400, (300.0, 200.0), (400.0, 300.0))))
            .unwrap();
        assert_eq!(faces.poll(&mut editor), None);
        assert_eq!(faces.busy(), None);
        let selection = editor.doc.selection.as_ref().unwrap();
        assert!(selection.at(50, 50) > 0.99 && selection.at(350, 250) > 0.99);
        assert_eq!(editor.undo_label(), Some("Select Skin"));

        // Failures come back as messages.
        let (tx, rx) = channel();
        faces.running = Some((Part::Hair, Combine::Replace, rx));
        tx.send(Err("Found no hair".into())).unwrap();
        assert_eq!(faces.poll(&mut editor), Some("Found no hair".into()));
    }
}
