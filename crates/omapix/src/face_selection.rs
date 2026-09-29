//! Select › Skin and Hair (docs/AI.md, feature 2), found by the face models
//! in omapix-ai on a thread of their own. Skin is face and body skin, less
//! each face's eyes, brows and lips.

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
}

impl Part {
    fn label(self) -> &'static str {
        match self {
            Part::Skin => "Select Skin",
            Part::Hair => "Select Hair",
        }
    }

    /// What the status bar says while it's found.
    pub fn finding(self) -> &'static str {
        match self {
            Part::Skin => "Finding skin…",
            Part::Hair => "Finding hair…",
        }
    }
}

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
            let classes: &[usize] = match part {
                Part::Skin => &[BODY_SKIN, FACE_SKIN],
                Part::Hair => &[HAIR],
            };
            let (logits, lw, lh) = analysis.logits(classes);
            let found = Selection::from_coverage(refine::mask_coverage(&logits, lw, lh, 0.0, image));
            let found = match part {
                Part::Skin => {
                    let points: Vec<&[[f32; 3]]> = analysis.faces.iter().filter_map(|(_, p)| p.as_deref()).collect();
                    found.combine(&features(&points, image.width(), image.height()), Combine::Subtract)
                }
                Part::Hair => found,
            };
            if found.is_empty() {
                Err(match part {
                    Part::Skin => "Found no skin".into(),
                    Part::Hair => "Found no hair".into(),
                })
            } else {
                Ok(found)
            }
        })
        .ok_or("The image isn't ready yet")?
}

/// Each face's eyes, brows and lips (from its landmarks), grown a little to
/// take in lashes and the edges of lipstick, and feathered, in an image
/// `width` × `height`.
fn features(faces: &[&[[f32; 3]]], width: u32, height: u32) -> Selection {
    // Fractions of the distance between the irises' centres.
    const GROW: f32 = 0.03;
    const FEATHER: f32 = 0.015;
    const PAD: f32 = 0.2;
    let rings: [&[usize]; 5] = [
        &outline::LEFT_EYE,
        &outline::RIGHT_EYE,
        &outline::LEFT_BROW,
        &outline::RIGHT_BROW,
        &outline::LIPS,
    ];
    // Drawn in a box round each face only, then placed in the image.
    let boxes: Vec<([u32; 4], Selection)> = faces
        .iter()
        .filter_map(|points| {
            let [l, r] = [outline::LEFT_IRIS[0], outline::RIGHT_IRIS[0]].map(|i| points[i]);
            let iod = (r[0] - l[0]).hypot(r[1] - l[1]);
            let pad = PAD * iod;
            let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
            for &i in rings.iter().flat_map(|r| r.iter()) {
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
            for ring in rings {
                let shape: Vec<(f32, f32)> = ring
                    .iter()
                    .map(|&i| (points[i][0] - x0 as f32, points[i][1] - y0 as f32))
                    .collect();
                drawn = drawn.combine(&Selection::polygon(w, h, &shape), Combine::Add);
            }
            Some(([x0, y0, x1, y1], drawn.expand(GROW * iod).feather(FEATHER * iod)))
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

    /// A face whose points are all at (300, 300) but for an eye outlined as
    /// a 40 × 20 box centred on (250, 280), and irises 100 apart.
    fn face() -> Vec<[f32; 3]> {
        let mut points = vec![[300.0, 300.0, 0.0]; 478];
        let n = outline::LEFT_EYE.len();
        for (k, &i) in outline::LEFT_EYE.iter().enumerate() {
            // Round the box's edge, clockwise from its left end.
            let t = k as f32 / n as f32 * 4.0;
            let (x, y) = match t {
                t if t < 1.0 => (230.0 + 40.0 * t, 270.0),
                t if t < 2.0 => (270.0, 270.0 + 20.0 * (t - 1.0)),
                t if t < 3.0 => (270.0 - 40.0 * (t - 2.0), 290.0),
                t => (230.0, 290.0 - 20.0 * (t - 3.0)),
            };
            points[i] = [x, y, 0.0];
        }
        points[outline::LEFT_IRIS[0]] = [250.0, 280.0, 0.0];
        points[outline::RIGHT_IRIS[0]] = [350.0, 280.0, 0.0];
        points
    }

    #[test]
    fn features_cover_the_eye_grown_a_little_and_nothing_far_from_the_face() {
        let face = face();
        let features = features(&[&face], 1000, 800);
        assert_eq!((features.width(), features.height()), (1000, 800));
        assert!(features.at(250, 280) > 0.99);
        // Grown by 3 px (0.03 × 100), feathered by 1.5.
        assert!(features.at(271, 280) > 0.5, "{}", features.at(271, 280));
        assert!(features.at(280, 280) < 0.01);
        assert!(features.at(250, 320) < 0.01);
        // Tiles away from the face stay empty.
        assert!(features.coverage.tile(3, 2).is_none());
    }

    /// A look at a real photo: skin and hair found in `OMAPIX_FACE_PHOTO`,
    /// tinted over a small copy, as `skin.ppm` and `hair.ppm` in
    /// `OMAPIX_FACE_OUT`. Needs the models (scripts/fetch-models.sh).
    #[test]
    #[ignore]
    fn select_skin_and_hair_in_a_photo() {
        let (Ok(photo), Ok(out)) = (std::env::var("OMAPIX_FACE_PHOTO"), std::env::var("OMAPIX_FACE_OUT")) else {
            return;
        };
        let doc = omapix_engine::io::load(std::path::Path::new(&photo)).unwrap();
        let render = Render::new(doc.composite());
        for part in [Part::Skin, Part::Hair] {
            let started = std::time::Instant::now();
            let found = find(&render, &doc.profile, part).unwrap();
            eprintln!("{part:?} in {:?}", started.elapsed());
            let (w, h) = (doc.width, doc.height);
            let (ow, oh) = (1000, 1000 * h / w);
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
