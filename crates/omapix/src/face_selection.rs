//! Select › Skin, Hair, Eyes, Lips and Teeth (docs/AI.md, feature 2), found
//! by the face models in omapix-ai on a thread of their own. Skin and hair
//! come from the segmentation, and skin leaves out each face's eyes, brows
//! and lips; eyes, lips and teeth are drawn from each face's points.

use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::sync::{Arc, Mutex};

use egui::{Pos2, pos2};
use omapix_ai::face::{Analysis, BODY_SKIN, Detection, FACE_SKIN, Faces, HAIR, Image, outline};
use omapix_ai::pose::point;
use omapix_engine::blemish::{self, Spot};
use omapix_engine::makeup::Product;
use omapix_engine::selection::{Combine, Selection};
use omapix_engine::tiled::{TILE, Tiled};
use omapix_engine::whiten::{self, Whiten};
use omapix_engine::{ColorProfile, DisplayTransform, Raster, refine, retouch, under_eyes};
use rayon::prelude::*;

use crate::canvas::Render;
use crate::editor::Editor;

/// Loaded on first use, and kept until Generative Fill needs the card.
static MODELS: Mutex<Option<Faces>> = Mutex::new(None);

/// Drop the model, giving its GPU memory back. It loads again when next
/// used.
pub fn unload() {
    if let Ok(mut model) = MODELS.lock() {
        *model = None;
    }
}

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
/// What else blemishes aren't looked for on: the bottom of the nose, grown
/// to take in the creases beside it, whose nostrils and shadows look like
/// spots.
const NOSE: Shapes = Shapes {
    outlines: &[(&outline::NOSE, Combine::Add)],
    grow: 0.06,
    feather: 0.02,
};
/// Nor round the eyes, where lashes, eyeliner and the crease of the lid
/// look like spots.
const ROUND_EYES: Shapes = Shapes {
    outlines: &[(&outline::LEFT_EYE, Combine::Add), (&outline::RIGHT_EYE, Combine::Add)],
    grow: 0.1,
    feather: 0.02,
};
/// What the skin under the eyes leaves out: the lower lashes and eyeliner.
const LASHES: Shapes = Shapes {
    outlines: &[(&outline::LEFT_EYE, Combine::Add), (&outline::RIGHT_EYE, Combine::Add)],
    grow: 0.06,
    feather: 0.03,
};
/// Round each face, grown to take in the jaw's edge.
const OUTLINE: Shapes = Shapes {
    outlines: &[(&outline::FACE, Combine::Add)],
    grow: 0.08,
    feather: 0.0,
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

/// An eye on its own: its opening, its iris, its corners, the middle of
/// its upper and lower lids, and along its lower lid and the top of the
/// cheek below it, from the outer corner in.
struct Eye {
    shape: Shapes,
    iris: [usize; 5],
    corners: [usize; 2],
    lids: [usize; 2],
    lower_lid: [usize; 9],
    cheek: [usize; 9],
}

const EACH_EYE: [Eye; 2] = [
    Eye {
        shape: Shapes {
            outlines: &[(&outline::LEFT_EYE, Combine::Add)],
            grow: 0.0,
            feather: 0.01,
        },
        iris: outline::LEFT_IRIS,
        corners: [33, 133],
        lids: [159, 145],
        lower_lid: [33, 7, 163, 144, 145, 153, 154, 155, 133],
        cheek: [143, 111, 117, 118, 119, 120, 121, 128, 245],
    },
    Eye {
        shape: Shapes {
            outlines: &[(&outline::RIGHT_EYE, Combine::Add)],
            grow: 0.0,
            feather: 0.01,
        },
        iris: outline::RIGHT_IRIS,
        corners: [362, 263],
        lids: [386, 374],
        lower_lid: [263, 249, 390, 373, 374, 380, 381, 382, 362],
        cheek: [372, 340, 346, 347, 348, 349, 350, 357, 465],
    },
];
/// An eye whose lids are closer than this shows no white: a wink's were
/// 0.06 apart and closed eyes' 0.04, and open eyes' 0.09 or more.
const EYE_OPEN: f32 = 0.07;
/// An eye this much narrower than the other is out of sight: with one at
/// 0.38 the whites found were on the nose, and at 0.51 on the eye.
const HIDDEN: f32 = 0.45;
/// The top of the forehead and the chin: three distances between the eyes
/// apart, on a face looking at the camera.
const FACE_HEIGHT: [usize; 2] = [10, 152];
/// What the whites of an eye leave out: its iris, grown to take in the dark
/// ring round it.
const IRIS: f32 = 1.15;
/// How far the edges of teeth and whites are softened, in distances between
/// the irises.
const SOFTEN: f32 = 0.008;
/// And the edge of the skin under an eye.
const UNDER_EYE_FEATHER: f32 = 0.03;

/// Where makeup goes round an eye: along its upper lid between the
/// corners, the crease above that, and under the brow above that again,
/// each from the outer corner in; and round its brow.
struct Lid {
    lashes: [usize; 7],
    crease: [usize; 7],
    under_brow: [usize; 7],
    brow: [usize; 10],
}

const EACH_LID: [Lid; 2] = [
    Lid {
        lashes: [246, 161, 160, 159, 158, 157, 173],
        crease: [247, 30, 29, 27, 28, 56, 190],
        under_brow: [113, 225, 224, 223, 222, 221, 189],
        brow: outline::LEFT_BROW,
    },
    Lid {
        lashes: [466, 388, 387, 386, 385, 384, 398],
        crease: [467, 260, 259, 257, 258, 286, 414],
        under_brow: [342, 445, 444, 443, 442, 441, 413],
        brow: outline::RIGHT_BROW,
    },
];
/// How far up from the lashes eyeliner goes, towards the crease, and eye
/// shadow, towards the brow.
const LINER: f32 = 0.3;
const SHADOW: f32 = 0.75;
/// Their edges' feathers, in distances between the irises, with lipstick's,
/// the brows' and blush's.
const LINER_FEATHER: f32 = 0.008;
const SHADOW_FEATHER: f32 = 0.04;
const BROW_FEATHER: f32 = 0.02;
const BLUSH_FEATHER: f32 = 0.16;
/// Blush is centred a third of the way from the apple of each cheek to its
/// cheekbone, and reaches this far across and down, in distances between
/// the irises.
const CHEEKS: [[usize; 2]; 2] = [[205, 123], [425, 352]];
const BLUSH: [f32; 2] = [0.3, 0.2];

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
    image
        .with_image(|image| {
            let (_, analysis) = analyse(image, profile)?;
            let (width, height) = (image.width(), image.height());
            let faces = points(&analysis);
            let found = match part {
                Part::Skin => skin(&analysis, image, &[BODY_SKIN, FACE_SKIN]),
                Part::Hair => segmented(&analysis, image, &[HAIR]),
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

/// `image` in 8-bit sRGB, as the models see it.
pub fn srgb(image: &Raster, profile: &ColorProfile) -> Result<Vec<[u8; 4]>, String> {
    let transform = DisplayTransform::to_srgb(profile).map_err(|e| e.to_string())?;
    let mut srgb = vec![[0u8; 4]; image.pixels().len()];
    // In parallel: a 24 MP image takes half a second on one thread.
    srgb.par_chunks_mut(1 << 16)
        .zip(image.pixels().par_chunks(1 << 16))
        .for_each(|(out, pixels)| transform.convert(pixels, out));
    Ok(srgb)
}

/// `image` in 8-bit sRGB, and what the face models find in it.
pub fn analyse(image: &Raster, profile: &ColorProfile) -> Result<(Vec<[u8; 4]>, Analysis), String> {
    let srgb = srgb(image, profile)?;
    let mut models = MODELS.lock().map_err(|e| e.to_string())?;
    let faces = match &mut *models {
        Some(faces) => faces,
        None => models.insert(Faces::load()?),
    };
    let (width, height) = (image.width() as usize, image.height() as usize);
    let analysis = faces.analyse(&Image {
        pixels: &srgb,
        width,
        height,
    })?;
    Ok((srgb, analysis))
}

/// The face skin to look for blemishes on: [`skin`] within each face's
/// outline (leaving out the ears), less the bottom of its nose and round
/// its eyes. Faces the landmarker didn't see (in profile) are left out:
/// without their points, nostrils and lips would be taken for spots.
pub fn blemish_skin(analysis: &Analysis, image: &Raster) -> Selection {
    blemish_area(&skin(analysis, image, &[FACE_SKIN]), &points(analysis))
}

/// The part of `face_skin` to look for blemishes on, for `faces`' points.
fn blemish_area(face_skin: &Selection, faces: &[&[[f32; 3]]]) -> Selection {
    let (w, h) = (face_skin.width(), face_skin.height());
    face_skin
        .combine(&draw(faces, &OUTLINE, w, h), Combine::Intersect)
        .combine(&draw(faces, &NOSE, w, h), Combine::Subtract)
        .combine(&draw(faces, &ROUND_EYES, w, h), Combine::Subtract)
}

/// The distance between the eyes of the largest face the landmarker saw
/// (the others aren't looked at), as it would be facing the camera: a
/// turned face's eyes look closer together, so it's at least a third of
/// the face's height, as it is on faces facing the camera.
pub fn scale(analysis: &Analysis) -> Option<f32> {
    analysis
        .faces
        .iter()
        .filter(|(_, points)| points.is_some())
        .map(|(face, _)| face_scale(face))
        .reduce(f32::max)
}

/// The distance between `face`'s eyes, as it would be facing the camera.
fn face_scale(face: &Detection) -> f32 {
    let [l, r] = [face.points[0], face.points[1]];
    (r[0] - l[0]).hypot(r[1] - l[1]).max((face.bounds[3] - face.bounds[1]) / 3.0)
}

/// What Smooth Skin and Even Tone work from: what the active layer and
/// those below it show, the skin in it, the distance between the eyes of
/// its largest face, and that face's cheek and nose.
pub struct FoundSkin {
    pub image: Raster,
    pub skin: Selection,
    pub iod: f32,
    pub cheek: Pos2,
    pub nose: Pos2,
}

/// The skin in `image`, its faces and body, measured by its largest face.
pub fn find_skin(image: Raster, profile: &ColorProfile) -> Result<FoundSkin, String> {
    let (_, analysis) = analyse(&image, profile)?;
    let iod = scale(&analysis).ok_or("Found no faces facing the camera")?;
    let skin = skin(&analysis, &image, &[BODY_SKIN, FACE_SKIN]);
    let height = |b: &[f32; 4]| b[3] - b[1];
    let face = analysis.faces.iter().map(|(f, _)| f).max_by(|a, b| height(&a.bounds).total_cmp(&height(&b.bounds)));
    let middle = pos2(image.width() as f32 / 2.0, image.height() as f32 / 2.0);
    // The cheek: halfway between an eye and the corner of the mouth below
    // it.
    let (cheek, nose) = face.map_or((middle, middle), |f| {
        let [eye, nose, mouth] = [f.points[0], f.points[2], f.points[3]];
        (pos2((eye[0] + mouth[0]) / 2.0, (eye[1] + mouth[1]) / 2.0), pos2(nose[0], nose[1]))
    });
    Ok(FoundSkin { image, skin, iod, cheek, nose })
}

/// Each of `faces` as a person, for sharing the skin out: the middle of
/// the face and the distance between its eyes, and with more than one
/// (one has all the skin), the joints of whoever the pose model sees with
/// their nose on it. Without the pose model there are just the faces.
fn people(faces: &[&Detection], srgb: &[[u8; 4]], image: &Raster) -> Vec<retouch::Person> {
    let mut poses = Vec::new();
    if faces.len() > 1 {
        let image = Image {
            pixels: srgb,
            width: image.width() as usize,
            height: image.height() as usize,
        };
        match crate::body_liquify::poses(&image) {
            Ok(found) => poses = found.iter().map(|p| (p.points[point::NOSE], p.joints())).collect(),
            Err(e) => log::debug!("sharing skin out by faces alone: {e}"),
        }
    }
    (faces.iter())
        .map(|f| {
            let [left, top, right, bottom] = f.bounds;
            let face = [(left + right) / 2.0, (top + bottom) / 2.0];
            let away = |nose: &[f32; 4]| (nose[0] - face[0]).hypot(nose[1] - face[1]);
            let on = |nose: &[f32; 4]| (left..right).contains(&nose[0]) && (top..bottom).contains(&nose[1]);
            let nearest = (0..poses.len()).filter(|&i| on(&poses[i].0)).min_by(|&a, &b| away(&poses[a].0).total_cmp(&away(&poses[b].0)));
            retouch::Person {
                face,
                iod: face_scale(f),
                // Nobody else's now.
                joints: nearest.map(|i| poses.swap_remove(i).1),
            }
        })
        .collect()
}

/// What Auto Retouch works from, for one face: its box (left, top, right,
/// bottom), the distance between its eyes, that person's share of the skin,
/// face and body, the spots on the face, most prominent first, its teeth
/// and the whites of its eyes, and the skin under its eyes with the cheek
/// below it.
pub struct FoundFace {
    pub bounds: [f32; 4],
    pub iod: f32,
    pub skin: Selection,
    pub spots: Vec<Spot>,
    pub teeth: Selection,
    pub eyes: Selection,
    pub under_eyes: (Selection, Selection),
}

/// The faces in `image`, from left to right, each measured on its own. A
/// face the landmarker didn't see (in profile) has skin but no spots, teeth,
/// whites or under-eyes, as in [`blemish_skin`]. One with neither skin nor spots (too
/// small for the segmenter to see its skin, or not a face at all) is left
/// out.
pub fn find_faces(image: &Raster, profile: &ColorProfile) -> Result<Vec<FoundFace>, String> {
    let (srgb, analysis) = analyse(image, profile)?;
    let mut faces: Vec<_> = analysis.faces.iter().collect();
    if faces.is_empty() {
        return Err("Found no faces".into());
    }
    faces.sort_by(|a, b| a.0.bounds[0].total_cmp(&b.0.bounds[0]));
    let all_skin = skin(&analysis, image, &[BODY_SKIN, FACE_SKIN]);
    let on_faces = segmented(&analysis, image, &[FACE_SKIN]);
    let face_skin = on_faces.combine(&features(&analysis, image), Combine::Subtract);
    let boxes: Vec<_> = faces.iter().map(|(f, _)| f).collect();
    let people = people(&boxes, &srgb, image);
    let found = faces.iter().enumerate().map(|(n, (face, points))| {
        let iod = people[n].iod;
        let none = || Selection::from_coverage(Tiled::new(image.width(), image.height(), 0));
        let whites = |what| points.as_deref().map_or_else(none, |p| whites(&srgb, p, what, &on_faces));
        FoundFace {
            bounds: face.bounds,
            iod,
            skin: retouch::share(&all_skin, &people, n),
            spots: points.as_deref().map_or(Vec::new(), |p| blemish::find(&srgb, &blemish_area(&face_skin, &[p]), iod)),
            teeth: whites(Whiten::Teeth),
            eyes: whites(Whiten::Eyes),
            under_eyes: points.as_deref().map_or_else(|| (none(), none()), |p| under_eyes(p, &face_skin)),
        }
    });
    let found: Vec<_> = found.filter(|f| !f.skin.is_empty() || !f.spots.is_empty()).collect();
    if found.is_empty() {
        return Err("Found no skin on the faces".into());
    }
    Ok(found)
}

/// The teeth, or the whites of the eyes, of every face in `image` the
/// landmarker saw.
pub fn find_whites(image: &Raster, profile: &ColorProfile, what: Whiten) -> Result<Selection, String> {
    let (srgb, analysis) = analyse(image, profile)?;
    let face = segmented(&analysis, image, &[FACE_SKIN]);
    let found = (points(&analysis).into_iter())
        .map(|p| whites(&srgb, p, what, &face))
        .reduce(|all, face| all.combine(&face, Combine::Add))
        .filter(|found| !found.is_empty());
    found.ok_or_else(|| match what {
        Whiten::Teeth => "Found no teeth".into(),
        Whiten::Eyes => "Found no eyes".into(),
    })
}

/// A face's teeth (none, with its mouth closed) or the whites of its eyes
/// (of those that are open): what's light and not red in `srgb` between its
/// lips, or in its eyes less their irises. Only on `face`, what the
/// segmentation takes for one.
///
/// On a face turned so far that one eye looks under [`HIDDEN`] of the
/// other's width, that eye and the mouth are guesses (behind the nose, or
/// out on the background), so only the nearer eye is looked in.
fn whites(srgb: &[[u8; 4]], points: &[[f32; 3]], what: Whiten, face: &Selection) -> Selection {
    let (width, height) = (face.width(), face.height());
    let (scale, seen) = sight(points);
    let seen = |n: usize| seen[n];
    let mut within = Selection::from_coverage(Tiled::new(width, height, 0));
    match what {
        Whiten::Teeth => {
            if seen(0) && seen(1) && apart(points, MOUTH_GAP) > OPEN * scale {
                within = draw(&[points], &TEETH, width, height);
            }
        }
        Whiten::Eyes => {
            for (n, eye) in EACH_EYE.iter().enumerate() {
                if !seen(n) || apart(points, eye.lids) <= EYE_OPEN * scale {
                    continue;
                }
                let [[cx, cy, _], round @ ..] = eye.iris.map(|i| points[i]);
                let r = IRIS * round.iter().map(|p| (p[0] - cx).hypot(p[1] - cy)).sum::<f32>() / round.len() as f32;
                let iris = Selection::ellipse(width, height, (cx - r, cy - r), (cx + r, cy + r));
                let opening = draw(&[points], &eye.shape, width, height);
                within = within.combine(&opening.combine(&iris, Combine::Subtract), Combine::Add);
            }
        }
    }
    whiten::whites(srgb, &within.combine(face, Combine::Intersect), SOFTEN * scale)
}

/// The distance between a face's eyes as it would be facing the camera (a
/// turned face's look closer together: as `face_scale`), and which of its
/// eyes are in sight.
fn sight(points: &[[f32; 3]]) -> (f32, [bool; 2]) {
    let widths = EACH_EYE.each_ref().map(|eye| apart(points, eye.corners));
    (iod(points).max(apart(points, FACE_HEIGHT) / 3.0), [0, 1].map(|n| widths[n] >= HIDDEN * widths[1 - n]))
}

/// The skin under the eyes of every face in `image` the landmarker saw,
/// where it's darker than the cheek below it: how far a Dodge curve is
/// opened to bring it level.
pub fn find_under_eyes(image: &Raster, profile: &ColorProfile) -> Result<Selection, String> {
    let (_, analysis) = analyse(image, profile)?;
    let skin = skin(&analysis, image, &[FACE_SKIN]);
    let found = (points(&analysis).into_iter())
        .map(|p| {
            let (under, cheek) = under_eyes(p, &skin);
            under_eyes::shadows(image, &under, &cheek, sight(p).0)
        })
        .reduce(|all, face| all.combine(&face, Combine::Add))
        .filter(|found| !found.is_empty());
    found.ok_or_else(|| "Found no shadows under the eyes".into())
}

/// Where each product of makeup goes on every face in `image` the
/// landmarker saw.
pub fn find_makeup(image: &Raster, profile: &ColorProfile) -> Result<Vec<(Product, Selection)>, String> {
    let (_, analysis) = analyse(image, profile)?;
    let faces = points(&analysis);
    if faces.is_empty() {
        return Err("Found no faces facing the camera".into());
    }
    let products = makeup(&faces, &segmented(&analysis, image, &[FACE_SKIN]));
    if products.iter().all(|(_, on)| on.is_empty()) {
        return Err("Found nowhere for makeup".into());
    }
    Ok(products)
}

/// Where each product of makeup goes on `faces` (their points), in the
/// order their layers stack, bottom first. Only on `on_faces`, what the
/// segmentation takes for a face, so nothing lands on the background
/// beside a face turned away, and only round the eyes in sight.
fn makeup(faces: &[&[[f32; 3]]], on_faces: &Selection) -> Vec<(Product, Selection)> {
    let (width, height) = (on_faces.width(), on_faces.height());
    let (mut blush, mut brows, mut shadow, mut liner) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for face in faces {
        let (scale, seen) = sight(face);
        let at = |i: usize| (face[i][0], face[i][1]);
        for n in (0..2).filter(|&n| seen[n]) {
            let (eye, lid) = (&EACH_EYE[n], &EACH_LID[n]);
            // From the lashes, corner to corner, up to part of the way to
            // `to`, and back.
            let band = |to: &[usize; 7], part: f32| {
                let up = |(&from, &to): (&usize, &usize)| {
                    let (a, b) = (at(from), at(to));
                    (a.0 + (b.0 - a.0) * part, a.1 + (b.1 - a.1) * part)
                };
                let lashes = [eye.corners[0]].into_iter().chain(lid.lashes).chain([eye.corners[1]]).map(at);
                lashes.chain(lid.lashes.iter().zip(to).rev().map(up)).collect::<Vec<_>>()
            };
            liner.extend(drawn(&band(&lid.crease, LINER), LINER_FEATHER * scale, width, height));
            shadow.extend(drawn(&band(&lid.under_brow, SHADOW), SHADOW_FEATHER * scale, width, height));
            brows.extend(drawn(&lid.brow.map(at), BROW_FEATHER * scale, width, height));
            let [apple, bone] = CHEEKS[n].map(at);
            let (cx, cy) = (apple.0 + (bone.0 - apple.0) / 3.0, apple.1 + (bone.1 - apple.1) / 3.0);
            let [rx, ry] = BLUSH.map(|r| r * scale);
            let ellipse: Vec<_> = (0..32).map(|i| (i as f32 * std::f32::consts::TAU / 32.0).sin_cos()).map(|(s, c)| (cx + rx * c, cy + ry * s)).collect();
            blush.extend(drawn(&ellipse, BLUSH_FEATHER * scale, width, height));
        }
    }
    let on_face = |on: Selection| on.combine(on_faces, Combine::Intersect);
    let placed = |boxes: &[([u32; 4], Selection)]| on_face(place(boxes, width, height));
    let features = draw(faces, &FEATURES, width, height);
    let eyes = draw(faces, &EYES, width, height);
    vec![
        (Product::Blush, placed(&blush).combine(&features, Combine::Subtract)),
        (Product::Brows, placed(&brows)),
        (Product::EyeShadow, placed(&shadow).combine(&eyes, Combine::Subtract)),
        (Product::Eyeliner, placed(&liner).combine(&eyes, Combine::Subtract)),
        (Product::Lipstick, on_face(draw(faces, &LIPS, width, height))),
    ]
}

/// The skin under a face's eyes (those in sight), from below each lower
/// lid's lashes down to the top of the cheek, and as much of the cheek
/// below that again, to measure it against. Both only where there's `skin`.
fn under_eyes(points: &[[f32; 3]], skin: &Selection) -> (Selection, Selection) {
    let (width, height) = (skin.width(), skin.height());
    let (scale, seen) = sight(points);
    let (mut under, mut cheek) = (Vec::new(), Vec::new());
    for eye in EACH_EYE.iter().zip(seen).filter_map(|(eye, seen)| seen.then_some(eye)) {
        let at = |i: usize| (points[i][0], points[i][1]);
        let (lid, top) = (eye.lower_lid.map(at), eye.cheek.map(at));
        let below: Vec<_> = lid.iter().zip(&top).map(|(l, t)| (2.0 * t.0 - l.0, 2.0 * t.1 - l.1)).collect();
        let ring = |a: &[(f32, f32)], b: &[(f32, f32)]| a.iter().chain(b.iter().rev()).copied().collect::<Vec<_>>();
        under.extend(drawn(&ring(&lid, &top), UNDER_EYE_FEATHER * scale, width, height));
        cheek.extend(drawn(&ring(&top, &below), 0.0, width, height));
    }
    let on_skin = |boxes: &[([u32; 4], Selection)]| place(boxes, width, height).combine(skin, Combine::Intersect);
    (on_skin(&under).combine(&draw(&[points], &LASHES, width, height), Combine::Subtract), on_skin(&cheek))
}

/// `polygon` filled and feathered, in a box round it: its corners in an
/// image `width` × `height`, and the selection within them.
fn drawn(polygon: &[(f32, f32)], feather: f32, width: u32, height: u32) -> Option<([u32; 4], Selection)> {
    let pad = 3.0 * feather + 1.0;
    let (mut lo, mut hi) = ((f32::MAX, f32::MAX), (f32::MIN, f32::MIN));
    for &(x, y) in polygon {
        lo = (lo.0.min(x - pad), lo.1.min(y - pad));
        hi = (hi.0.max(x + pad), hi.1.max(y + pad));
    }
    let (x0, y0) = (lo.0.max(0.0) as u32, lo.1.max(0.0) as u32);
    let (x1, y1) = ((hi.0.ceil().max(0.0) as u32).min(width), (hi.1.ceil().max(0.0) as u32).min(height));
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let shape: Vec<_> = polygon.iter().map(|&(x, y)| (x - x0 as f32, y - y0 as f32)).collect();
    Some(([x0, y0, x1, y1], Selection::polygon(x1 - x0, y1 - y0, &shape).feather(feather)))
}

/// The points of each face the landmarker saw.
fn points(analysis: &Analysis) -> Vec<&[[f32; 3]]> {
    analysis.faces.iter().filter_map(|(_, p)| p.as_deref()).collect()
}

/// The skin in `image` of `classes` (face skin, body skin or both), from its
/// segmentation, less each face's eyes, brows and lips.
pub fn skin(analysis: &Analysis, image: &Raster, classes: &[usize]) -> Selection {
    segmented(analysis, image, classes).combine(&features(analysis, image), Combine::Subtract)
}

/// What `image`'s segmentation takes for `classes`, its edges the image's.
fn segmented(analysis: &Analysis, image: &Raster, classes: &[usize]) -> Selection {
    let (logits, lw, lh) = analysis.logits(classes);
    Selection::from_coverage(refine::mask_coverage(&logits, lw, lh, 0.0, image))
}

/// Each face's eyes, brows and lips.
fn features(analysis: &Analysis, image: &Raster) -> Selection {
    draw(&points(analysis), &FEATURES, image.width(), image.height())
}

/// The distance between a face's irises' centres, which its features are
/// measured in.
fn iod(points: &[[f32; 3]]) -> f32 {
    let [l, r] = [outline::LEFT_IRIS[0], outline::RIGHT_IRIS[0]].map(|i| points[i]);
    (r[0] - l[0]).hypot(r[1] - l[1])
}

/// How far apart two of a face's points are.
fn apart(points: &[[f32; 3]], pair: [usize; 2]) -> f32 {
    let [a, b] = pair.map(|i| points[i]);
    (b[0] - a[0]).hypot(b[1] - a[1])
}

fn mouth_open(points: &[[f32; 3]]) -> bool {
    apart(points, MOUTH_GAP) > OPEN * iod(points)
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
    place(&boxes, width, height)
}

/// Selections drawn in `boxes` (left, top, right, bottom), placed in an
/// image `width` × `height`.
fn place(boxes: &[([u32; 4], Selection)], width: u32, height: u32) -> Selection {
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

    /// [`face`] with both eyes open, 40 × 20 round irises 12 across, in an
    /// image that's white all over.
    fn face_with_eyes() -> (Vec<[u8; 4]>, Vec<[f32; 3]>) {
        let mut points = face();
        outline_box(&mut points, &outline::RIGHT_EYE, [330.0, 270.0, 370.0, 290.0]);
        for (iris, cx) in [(outline::LEFT_IRIS, 250.0), (outline::RIGHT_IRIS, 350.0)] {
            for (i, (dx, dy)) in iris[1..].iter().zip([(6.0, 0.0), (0.0, -6.0), (-6.0, 0.0), (0.0, 6.0)]) {
                points[*i] = [cx + dx, 280.0 + dy, 0.0];
            }
        }
        (vec![[230, 228, 224, 255]; 600 * 500], points)
    }

    /// [`face_with_eyes`]' points with each eye's lid, brow and cheek: lashes
    /// along the top of the eye, the crease 8 px above them and under the
    /// brow 20, a brow 50 × 10 above that, and a cheek 60 px below the eye.
    fn face_with_lids() -> Vec<[f32; 3]> {
        let (_, mut points) = face_with_eyes();
        for (n, cx) in [250.0f32, 350.0].into_iter().enumerate() {
            let (eye, lid) = (&EACH_EYE[n], &EACH_LID[n]);
            // From the outer corner in: leftwards on the right of the face.
            let inward = if n == 0 { 1.0 } else { -1.0 };
            for (ring, y) in [(&lid.lashes, 270.0), (&lid.crease, 262.0), (&lid.under_brow, 250.0)] {
                for (k, &i) in ring.iter().enumerate() {
                    points[i] = [cx + inward * (k as f32 - 3.0) * 5.0, y, 0.0];
                }
            }
            points[eye.corners[0]] = [cx - inward * 20.0, 275.0, 0.0];
            points[eye.corners[1]] = [cx + inward * 20.0, 275.0, 0.0];
            outline_box(&mut points, &lid.brow, [cx - 25.0, 235.0, cx + 25.0, 245.0]);
            points[CHEEKS[n][0]] = [cx, 340.0, 0.0];
            points[CHEEKS[n][1]] = [cx - inward * 30.0, 330.0, 0.0];
        }
        points
    }

    #[test]
    fn makeup_goes_on_the_lids_brows_cheeks_and_lips_of_the_eyes_in_sight() {
        let points = face_with_lids();
        let products = makeup(&[&points], &Selection::all(600, 500));
        let order: Vec<_> = products.iter().map(|(product, _)| *product).collect();
        assert_eq!(order, [Product::Blush, Product::Brows, Product::EyeShadow, Product::Eyeliner, Product::Lipstick]);
        let [blush, brows, shadow, liner, lipstick] = &products[..] else { unreachable!() };
        // Both sides alike.
        for cx in [250, 350] {
            // Eyeliner just above the lashes (a thin line, soft where it
            // meets the eye), eye shadow up to under the brow, and neither
            // in the eye.
            assert!(liner.1.at(cx, 269) > 0.3 && liner.1.at(cx, 262) < 0.05, "{} {}", liner.1.at(cx, 269), liner.1.at(cx, 262));
            assert!(shadow.1.at(cx, 262) > 0.8 && shadow.1.at(cx, 240) < 0.1, "{} {}", shadow.1.at(cx, 262), shadow.1.at(cx, 240));
            assert!(liner.1.at(cx, 281) < 0.01 && shadow.1.at(cx, 281) < 0.01);
            assert!(brows.1.at(cx, 240) > 0.9 && brows.1.at(cx, 262) < 0.05);
            // Blush between the apple of the cheek and the cheekbone,
            // fading out.
            let out = if cx == 250 { cx - 10 } else { cx + 10 };
            assert!(blush.1.at(out, 337) > 0.4 && blush.1.at(out, 420) < 0.01, "{} {}", blush.1.at(out, 337), blush.1.at(out, 420));
        }
        // Lipstick on the lips, not the mouth between them.
        assert!(lipstick.1.at(300, 378) > 0.9 && lipstick.1.at(300, 400) < 0.01);

        // Only on what the segmentation takes for a face.
        let left = Selection::rectangle(600, 500, (0.0, 0.0), (300.0, 500.0));
        let products = makeup(&[&points], &left);
        assert!(products.iter().all(|(_, on)| on.bounds().is_none_or(|[x, _, w, _]| x + w <= 300)));
        assert!(products[1].1.at(250, 240) > 0.9);

        // An eye out of sight, on a face turned away, has none round it,
        // nor the cheek below it.
        let mut turned = points.clone();
        let [outer, inner] = EACH_EYE[1].corners;
        turned[outer] = [turned[inner][0] + 10.0, 275.0, 0.0];
        let products = makeup(&[&turned], &Selection::all(600, 500));
        for (product, on) in &products[..4] {
            assert!(on.at(350, 240) < 0.01 && on.at(350, 262) < 0.01 && on.at(360, 337) < 0.01, "{product:?}");
        }
        assert!(products[1].1.at(250, 240) > 0.9 && products[4].1.at(300, 378) > 0.9);
    }

    #[test]
    fn whites_are_the_open_eyes_without_their_irises_and_the_open_mouth() {
        let (srgb, points) = face_with_eyes();
        let all = Selection::all(600, 500);
        let eyes = whites(&srgb, &points, Whiten::Eyes, &all);
        assert!(eyes.at(236, 280) > 0.9 && eyes.at(364, 280) > 0.9, "{} {}", eyes.at(236, 280), eyes.at(364, 280));
        // Not the irises (grown to 6.9 px), nor the mouth.
        assert!(eyes.at(250, 280) < 0.01 && eyes.at(355, 280) < 0.01 && eyes.at(300, 400) < 0.01);
        let teeth = whites(&srgb, &points, Whiten::Teeth, &all);
        assert!(teeth.at(300, 400) > 0.9 && teeth.at(236, 280) < 0.01);
        // Only on what the segmentation takes for a face.
        let left = Selection::rectangle(600, 500, (0.0, 0.0), (300.0, 500.0));
        let eyes = whites(&srgb, &points, Whiten::Eyes, &left);
        assert!(eyes.at(236, 280) > 0.9 && eyes.at(364, 280) < 0.01);

        // A closed eye has no white, and a closed mouth no teeth.
        let mut closed = points.clone();
        let [upper, lower] = EACH_EYE[1].lids;
        (closed[upper], closed[lower]) = ([350.0, 278.0, 0.0], [350.0, 282.0, 0.0]);
        closed[MOUTH_GAP[1]] = [300.0, 392.0, 0.0];
        let eyes = whites(&srgb, &closed, Whiten::Eyes, &all);
        assert!(eyes.at(236, 280) > 0.9 && eyes.bounds().unwrap()[2] < 60, "{:?}", eyes.bounds());
        assert!(whites(&srgb, &closed, Whiten::Teeth, &all).is_empty());
    }

    #[test]
    fn a_face_turned_far_has_only_its_nearer_eye_whitened() {
        // The right eye a fifth as wide as the left: out of sight.
        let (srgb, mut points) = face_with_eyes();
        let [inner, outer] = EACH_EYE[1].corners;
        (points[inner], points[outer]) = ([346.0, 280.0, 0.0], [354.0, 281.0, 0.0]);
        let all = Selection::all(600, 500);
        let eyes = whites(&srgb, &points, Whiten::Eyes, &all);
        assert!(eyes.at(236, 280) > 0.9 && eyes.bounds().unwrap()[2] < 60, "{:?}", eyes.bounds());
        // Its mouth is a guess too.
        assert!(whites(&srgb, &points, Whiten::Teeth, &all).is_empty());
    }

    #[test]
    fn under_each_eye_in_sight_is_the_skin_below_its_lashes_and_the_cheek_below_that() {
        // Each lower lid along y = 290, and the top of the cheek 20 px
        // below it.
        let (_, mut points) = face_with_eyes();
        for (eye, x0) in EACH_EYE.iter().zip([230.0, 330.0]) {
            for (k, (&lid, &cheek)) in eye.lower_lid.iter().zip(&eye.cheek).enumerate() {
                let x = x0 + 5.0 * k as f32;
                (points[lid], points[cheek]) = ([x, 290.0, 0.0], [x, 310.0, 0.0]);
            }
        }
        let skin = Selection::all(600, 500);
        let (under, cheek) = under_eyes(&points, &skin);
        assert!(under.at(250, 303) > 0.9 && under.at(350, 303) > 0.9, "{} {}", under.at(250, 303), under.at(350, 303));
        // Not the lashes (6 px below the lid, fading in from there), nor
        // the cheek.
        assert!(under.at(250, 289) < 0.05 && under.at(250, 295) < 0.6 && under.at(250, 320) < 0.05, "{} {}", under.at(250, 289), under.at(250, 295));
        assert!(cheek.at(250, 320) > 0.99 && cheek.at(350, 320) > 0.99 && cheek.at(250, 300) < 0.01 && cheek.at(250, 335) < 0.01);
        // Only where there's skin.
        let left = Selection::rectangle(600, 500, (0.0, 0.0), (300.0, 500.0));
        let (under, cheek) = under_eyes(&points, &left);
        assert!(under.at(250, 303) > 0.9 && under.at(350, 303) < 0.01 && cheek.at(350, 320) < 0.01);
        // An eye out of sight has none.
        let [inner, outer] = EACH_EYE[1].corners;
        (points[inner], points[outer]) = ([346.0, 280.0, 0.0], [354.0, 281.0, 0.0]);
        let (under, cheek) = under_eyes(&points, &skin);
        assert!(under.at(250, 303) > 0.9 && under.at(350, 303) < 0.01 && cheek.at(350, 320) < 0.01);
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
