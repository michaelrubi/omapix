//! Auto Retouch (docs/AI.md, feature 6): Heal Blemishes, Reduce Shine,
//! Smooth Skin and Even Tone done for each face in turn, then the shadows under its eyes
//! lifted, each step made from what the last one shows, and its teeth and
//! eyes whitened, as a "Retouch" group holding a group for each face:
//!
//! ```text
//! Retouch
//!   ├─ Face 1
//!   │    ├─ Whiten Teeth
//!   │    ├─ Whiten Eyes
//!   │    ├─ Under Eyes
//!   │    ├─ Dodge & Burn
//!   │    ├─ Smooth Skin
//!   │    ├─ Reduce Shine
//!   │    └─ Blemishes
//!   └─ Face 2
//! ```
//!
//! Each face has its own share of the skin and is measured by the distance
//! between its own eyes, so a small face at the back of a group is smoothed
//! as finely as it needs.

use crate::blemish::{self, Spot};
use crate::body::{Joint, Joints};
use crate::document::Document;
use crate::layer::Layer;
use crate::selection::Selection;
use crate::shine::{self, Matte};
use crate::skin::{self, Smoothing};
use crate::tiled::{TILE, Tiled};
use crate::tone::{self, Evening, Tone};
use crate::under_eyes;
use crate::whiten::{self, Whiten};

/// A face to retouch, and how.
pub struct Face<'a> {
    /// Its number, counted from 1: its group is "Face 1".
    pub number: usize,
    /// This person's skin, and the distance between their eyes.
    pub skin: &'a Selection,
    pub iod: f32,
    /// The spots to heal: with none, there's no Blemishes layer.
    pub spots: &'a [Spot],
    /// How much of the shine on its skin goes (0–100): without, or with
    /// none, there's no Reduce Shine layer.
    pub shine: Option<f32>,
    /// Without these, there's no Smooth Skin layer or Dodge & Burn group.
    pub smoothing: Option<Smoothing>,
    pub evening: Option<Evening>,
    /// The skin under its eyes, the cheek below it, and how much of the
    /// shadows there is lifted (0–100): without, or with none, there's no
    /// Under Eyes layer.
    pub under_eyes: Option<(&'a Selection, &'a Selection, f32)>,
    /// The whites of its eyes, and its teeth, each with how much it's
    /// whitened (0–100): without, there's no Whiten Eyes or Whiten Teeth
    /// layer.
    pub eyes: Option<(&'a Selection, f32)>,
    pub teeth: Option<(&'a Selection, f32)>,
}

/// A person, for telling whose skin is whose: the middle of their face,
/// the distance between its eyes, and their joints if the pose model saw
/// them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Person {
    pub face: [f32; 2],
    pub iod: f32,
    pub joints: Option<Joints>,
}

/// Part of a body, as the line from one joint to another and how far the
/// body reaches from it, in pixels.
struct Bone {
    from: [f32; 2],
    to: [f32; 2],
    reach: f32,
}

impl Bone {
    /// How far (x, y) is from it, in reaches: under 1 is on the body.
    fn away(&self, x: f32, y: f32) -> f32 {
        let (dx, dy) = (self.to[0] - self.from[0], self.to[1] - self.from[1]);
        let along = ((x - self.from[0]) * dx + (y - self.from[1]) * dy) / (dx * dx + dy * dy).max(1e-6);
        let t = along.clamp(0.0, 1.0);
        (x - self.from[0] - t * dx).hypot(y - self.from[1] - t * dy) / self.reach
    }
}

/// A joint counts if the pose model thinks it more likely in view than not.
const SEEN: f32 = 0.5;

impl Person {
    /// Their face, and each part of their body between joints the pose
    /// model saw. How far each reaches is in distances between the eyes
    /// (about 6 cm), from an adult's proportions.
    fn bones(&self) -> Vec<Bone> {
        let mut bones = vec![Bone {
            from: self.face,
            to: self.face,
            reach: 1.4 * self.iod,
        }];
        let Some(j) = &self.joints else {
            return bones;
        };
        let at = |joint: Joint| [joint[0], joint[1]];
        let between = |a: Joint, b: Joint| [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0, a[2].min(b[2])];
        // On from `to`, as far again as `by` times the way there from `from`.
        let beyond = |from: Joint, to: Joint, by: f32| [to[0] + (to[0] - from[0]) * by, to[1] + (to[1] - from[1]) * by, from[2].min(to[2])];
        let mut bone = |from: Joint, to: Joint, reach: f32| {
            if from[2] > SEEN && to[2] > SEEN {
                bones.push(Bone {
                    from: at(from),
                    to: at(to),
                    reach: reach * self.iod,
                });
            }
        };
        let (shoulders, hips) = (between(j.shoulders[0], j.shoulders[1]), between(j.hips[0], j.hips[1]));
        bone([self.face[0], self.face[1], 1.0], shoulders, 1.0);
        bone(j.shoulders[0], j.shoulders[1], 1.0);
        bone(shoulders, hips, 1.6);
        for side in 0..2 {
            let (shoulder, elbow, wrist) = (j.shoulders[side], j.elbows[side], j.wrists[side]);
            let (hip, knee, ankle) = (j.hips[side], j.knees[side], j.ankles[side]);
            bone(shoulder, hip, 1.2);
            bone(shoulder, elbow, 0.8);
            bone(elbow, wrist, 0.7);
            // The hand and the foot, which have no joints of their own here.
            bone(wrist, beyond(elbow, wrist, 0.45), 0.7);
            bone(hip, knee, 1.2);
            bone(knee, ankle, 0.9);
            bone(ankle, beyond(knee, ankle, 0.2), 0.8);
        }
        bones
    }
}

/// Skin is shared out in squares of this many pixels: whose it is changes
/// over far more than that.
const CELL: u32 = 4;

/// The part of `skin` that's `people[which]`'s: what's nearer their face
/// or body than anyone else's, measured in how far each part reaches (so a
/// small figure has a small share, and an arm across someone else is still
/// its owner's). Someone the pose model didn't see has only their face to
/// go by.
pub fn share(skin: &Selection, people: &[Person], which: usize) -> Selection {
    if people.len() < 2 {
        return skin.clone();
    }
    let c = &skin.coverage;
    let bones: Vec<Vec<Bone>> = people.iter().map(Person::bones).collect();
    let away = |n: usize, x: f32, y: f32| bones[n].iter().map(|b| b.away(x, y)).fold(f32::INFINITY, f32::min);
    Selection::from_coverage(Tiled::from_tiles(c.width(), c.height(), 0, |col, row| {
        let (tx, ty) = (col * TILE, row * TILE);
        let mut tile = vec![0; (TILE * TILE) as usize];
        let cells = TILE / CELL;
        let mut mine: Vec<Option<bool>> = vec![None; (cells * cells) as usize];
        let mut any = false;
        for (i, v) in tile.iter_mut().enumerate() {
            let (ix, iy) = (i as u32 % TILE, i as u32 / TILE);
            let covered = c.tile(col, row).map_or(c.fill(), |t| t[i]);
            if covered == 0 || tx + ix >= c.width() || ty + iy >= c.height() {
                continue;
            }
            let cell = &mut mine[(ix / CELL + iy / CELL * cells) as usize];
            let mine = *cell.get_or_insert_with(|| {
                let middle = |t: u32, i: u32| (t + i / CELL * CELL) as f32 + CELL as f32 / 2.0;
                let (x, y) = (middle(tx, ix), middle(ty, iy));
                let own = away(which, x, y);
                (0..people.len()).all(|n| n == which || own <= away(n, x, y))
            });
            if mine {
                *v = covered;
                any = true;
            }
        }
        any.then_some(tile)
    }))
}

/// A "Retouch" group above the layer at `above`, with a group for each of
/// `faces` (the first on top), each holding that face's steps: a Blemishes
/// layer ([`blemish::heal`]), a Reduce Shine layer ([`shine::add_layer`]) made
/// from the image with the blemishes healed, a Smooth Skin layer
/// ([`skin::add_layer`]) made from that, a Dodge & Burn group
/// ([`tone::add_layers`]) made from the image smoothed, an Under Eyes layer
/// ([`under_eyes::add_layer`]) made from the image evened, and Whiten Eyes
/// and Whiten Teeth layers ([`whiten::add_layer`]). Returns the Retouch group's
/// id, or `None` if there was nothing to do.
pub fn add_layers(doc: &mut Document, above: usize, faces: &[Face]) -> Option<u64> {
    let group = |doc: &mut Document, above: usize, name: String| {
        let id = doc.next_layer_id();
        doc.insert_above(above, Layer::group(id, name, doc.width, doc.height));
        id
    };
    let index = |doc: &Document, id: u64| doc.index_of(id).expect("just added");
    let retouch = group(doc, above, "Retouch".into());
    // The last first: each goes in at the top.
    for face in faces.iter().rev() {
        let id = group(doc, index(doc, retouch), format!("Face {}", face.number));
        if !face.spots.is_empty() {
            blemish::heal(doc, index(doc, id), face.spots);
        }
        if let Some(amount) = face.shine {
            let at = index(doc, id);
            let image = doc.composite_current_and_below(at);
            if let Some(matte) = Matte::new(&image, face.skin, face.iod) {
                shine::add_layer(doc, at, &matte, amount);
            }
        }
        if let Some(smoothing) = &face.smoothing {
            let at = index(doc, id);
            let image = doc.composite_current_and_below(at);
            skin::add_layer(doc, at, &image, face.skin, face.iod, smoothing);
        }
        if let Some(evening) = &face.evening {
            let at = index(doc, id);
            let image = doc.composite_current_and_below(at);
            if let Some(tone) = Tone::new(&image, face.skin, face.iod, evening.radii(face.iod)) {
                tone::add_layers(doc, at, &tone, face.skin, evening.amount);
            }
        }
        if let Some((under, cheek, amount)) = face.under_eyes {
            let at = index(doc, id);
            let image = doc.composite_current_and_below(at);
            under_eyes::add_layer(doc, at, &under_eyes::shadows(&image, under, cheek, face.iod), amount);
        }
        for (what, whites) in [(Whiten::Eyes, face.eyes), (Whiten::Teeth, face.teeth)] {
            if let Some((whites, amount)) = whites {
                whiten::add_layer(doc, index(doc, id), what, whites, amount);
            }
        }
        remove_if_empty(doc, id);
    }
    remove_if_empty(doc, retouch).then_some(retouch)
}

/// Remove group `id` if there's nothing in it. Returns whether it's still
/// there.
fn remove_if_empty(doc: &mut Document, id: u64) -> bool {
    let at = doc.index_of(id).expect("just added");
    let empty = doc.span(at).len() == 1;
    if empty {
        doc.remove_layer(at);
    }
    !empty
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blend::BlendMode;
    use crate::color::ColorProfile;
    use crate::raster::Raster;

    const W: u32 = 600;
    const H: u32 = 300;

    /// Two faces' worth of skin side by side, each with a small dark spot
    /// (at x 150 and 450) and a soft dark patch below it.
    fn image() -> Raster {
        let pixels = (0..W * H)
            .map(|i| {
                let (x, y) = ((i % W) as f32, (i / W) as f32);
                let mut v = 45000.0;
                for cx in [150.0, 450.0] {
                    if (x - cx).hypot(y - 100.0) < 4.0 {
                        v = 20000.0;
                    }
                    v -= 3000.0 * (-((x - cx).hypot(y - 200.0) / 8.0).powi(2)).exp();
                }
                [v as u16, v as u16 - 8000, v as u16 - 12000, u16::MAX]
            })
            .collect();
        Raster::new(W, H, pixels)
    }

    fn spot(x: f32) -> [Spot; 1] {
        [Spot { x, y: 100.0, radius: 4.0, score: 20.0 }]
    }

    /// The left and right halves, with eyes 80 px apart.
    fn skins() -> [Selection; 2] {
        let half = W as f32 / 2.0;
        [0.0, half].map(|x| Selection::rectangle(W, H, (x, 0.0), (x + half, H as f32)))
    }

    fn face<'a>(number: usize, skin: &'a Selection, spots: &'a [Spot]) -> Face<'a> {
        Face {
            number,
            skin,
            iod: 80.0,
            spots,
            shine: None,
            smoothing: Some(Smoothing::default()),
            evening: Some(Evening::default()),
            under_eyes: None,
            eyes: None,
            teeth: None,
        }
    }

    fn document() -> Document {
        Document::from_image("t.tif".into(), &image(), ColorProfile::srgb(), 16)
    }

    #[test]
    fn each_face_gets_a_group_of_its_steps_with_the_first_on_top() {
        let mut doc = document();
        let before = doc.composite();
        let ([left, right], spots) = (skins(), [spot(150.0), spot(450.0)]);
        let retouch = add_layers(&mut doc, 0, &[face(1, &left, &spots[0]), face(2, &right, &spots[1])]).unwrap();

        let names: Vec<_> = doc.layers.iter().map(|l| l.name.as_str()).collect();
        let steps = ["Blemishes", "Smooth Skin", "Burn", "Dodge", "Dodge & Burn"];
        assert_eq!(names[1..6], steps);
        assert_eq!(names[6], "Face 2");
        assert_eq!(names[7..12], steps);
        assert_eq!(names[12..], ["Face 1", "Retouch"]);
        // Groups in groups, all Pass Through, so the curves see the image.
        let layer = |name: &str| doc.layers.iter().rfind(|l| l.name == name).unwrap();
        assert_eq!(doc.layers[13].id, retouch);
        for group in ["Face 1", "Face 2"] {
            let group = layer(group);
            assert_eq!((group.parent, group.is_group, group.blend), (Some(retouch), true, BlendMode::PassThrough));
        }
        let face_1 = layer("Face 1").id;
        assert_eq!(layer("Smooth Skin").parent, Some(face_1));
        assert_eq!(layer("Dodge & Burn").parent, Some(face_1));
        // Each step's strength is its own layer's.
        assert_eq!((layer("Smooth Skin").opacity, layer("Dodge & Burn").opacity), (0.7, 0.6));

        // Both faces: the spot's healed, and the patch is nearer the skin
        // round it.
        let after = doc.composite();
        let red = |image: &Raster, x: u32, y: u32| f32::from(image.get(x, y)[0]);
        for x in [150, 450] {
            assert!(red(&after, x, 100) > 40000.0, "{}", red(&after, x, 100));
            let off = |image: &Raster| red(image, x, 260) - red(image, x, 200);
            assert!(off(&before) > 2900.0 && off(&after) < 0.7 * off(&before), "{} {}", off(&before), off(&after));
        }
        // Hiding the group shows the before.
        doc.layer_mut(retouch).unwrap().visible = false;
        assert_eq!(doc.composite().get(150, 100), before.get(150, 100));
    }

    #[test]
    fn each_step_is_made_from_what_the_last_shows() {
        let mut doc = document();
        let ([left, _], spots) = (skins(), spot(150.0));
        add_layers(&mut doc, 0, &[face(1, &left, &spots)]);
        // The Smooth Skin layer was made from the image with the spot
        // healed.
        let smooth = doc.layers.iter().find(|l| l.name == "Smooth Skin").unwrap();
        assert!(smooth.pixels.get(150, 100)[0] > 40000, "{:?}", smooth.pixels.get(150, 100));
        // Only this face's skin is touched.
        assert_eq!(doc.composite().get(450, 200), image().get(450, 200));
    }

    #[test]
    fn steps_left_out_leave_no_layers_and_faces_keep_their_numbers() {
        let mut doc = document();
        let [left, right] = skins();
        let none = Face {
            smoothing: None,
            evening: None,
            ..face(1, &left, &[])
        };
        let tone_only = Face {
            smoothing: None,
            ..face(2, &right, &[])
        };
        add_layers(&mut doc, 0, &[none, tone_only]).unwrap();
        let names: Vec<_> = doc.layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names[1..], ["Burn", "Dodge", "Dodge & Burn", "Face 2", "Retouch"]);

        // Nothing to do, or no skin to do it on: nothing's added.
        let mut doc = document();
        let none = Face {
            smoothing: None,
            evening: None,
            ..face(1, &left, &[])
        };
        let no_skin = Selection::from_coverage(Tiled::new(W, H, 0));
        assert_eq!(add_layers(&mut doc, 0, &[none, face(2, &no_skin, &[])]), None);
        assert_eq!(doc.layers.len(), 1);
    }

    #[test]
    fn eyes_and_teeth_are_whitened_on_top_of_a_faces_group() {
        let mut doc = document();
        let [left, _] = skins();
        let before = doc.composite();
        let eyes = Selection::rectangle(W, H, (100.0, 40.0), (120.0, 50.0));
        let teeth = Selection::rectangle(W, H, (130.0, 240.0), (170.0, 250.0));
        let none = Selection::from_coverage(Tiled::new(W, H, 0));
        let whitened = Face {
            smoothing: None,
            eyes: Some((&eyes, 40.0)),
            teeth: Some((&teeth, 50.0)),
            ..face(1, &left, &[])
        };
        // A face with no teeth showing has no layer for them.
        let closed = Face {
            smoothing: None,
            evening: None,
            eyes: Some((&eyes, 40.0)),
            teeth: Some((&none, 50.0)),
            ..face(2, &left, &[])
        };
        add_layers(&mut doc, 0, &[whitened, closed]).unwrap();
        let names: Vec<_> = doc.layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names[1..3], ["Whiten Eyes", "Face 2"]);
        assert_eq!(names[3..], ["Burn", "Dodge", "Dodge & Burn", "Whiten Eyes", "Whiten Teeth", "Face 1", "Retouch"]);
        let layer = |name: &str| doc.layers.iter().rfind(|l| l.name == name).unwrap();
        assert_eq!(layer("Whiten Teeth").parent, Some(layer("Face 1").id));
        assert_eq!((layer("Whiten Eyes").opacity, layer("Whiten Teeth").opacity), (0.4, 0.5));
        // Lighter and less coloured where they are, and nowhere else.
        let after = doc.composite();
        assert!(after.get(150, 245)[2] > before.get(150, 245)[2] + 1000);
        assert!(after.get(110, 45)[2] > before.get(110, 45)[2] + 1000);
        assert_eq!(after.get(450, 45), before.get(450, 45));
    }

    #[test]
    fn shadows_under_the_eyes_are_lifted_from_the_image_evened() {
        let mut doc = document();
        let [left, _] = skins();
        let before = doc.composite();
        // The soft dark patch at (150, 200) is under an eye, with cheek
        // below it.
        let under = Selection::rectangle(W, H, (120.0, 185.0), (180.0, 215.0));
        let cheek = Selection::rectangle(W, H, (120.0, 230.0), (180.0, 260.0));
        let shadowed = Face {
            smoothing: None,
            under_eyes: Some((&under, &cheek, 50.0)),
            ..face(1, &left, &[])
        };
        add_layers(&mut doc, 0, &[shadowed]).unwrap();
        let names: Vec<_> = doc.layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names[1..], ["Burn", "Dodge", "Dodge & Burn", "Under Eyes", "Face 1", "Retouch"]);
        let layer = |name: &str| doc.layers.iter().rfind(|l| l.name == name).unwrap();
        assert_eq!((layer("Under Eyes").opacity, layer("Under Eyes").parent), (0.5, Some(layer("Face 1").id)));
        // Lighter there than Even Tone alone leaves it.
        let both = doc.composite().get(150, 200)[0];
        let id = layer("Under Eyes").id;
        doc.layer_mut(id).unwrap().visible = false;
        let evened = doc.composite().get(150, 200)[0];
        assert!(evened > before.get(150, 200)[0] && both > evened + 200, "{both} {evened}");

        // With no shadow there, there's no layer.
        let mut doc = document();
        let light = Selection::rectangle(W, H, (20.0, 20.0), (60.0, 40.0));
        let none = Face {
            smoothing: None,
            evening: None,
            under_eyes: Some((&light, &cheek, 50.0)),
            ..face(1, &left, &[])
        };
        assert_eq!(add_layers(&mut doc, 0, &[none]), None);
    }

    #[test]
    fn shine_is_taken_down_after_the_blemishes_and_before_the_smoothing() {
        // A hot spot on the first face's skin.
        let mut pixels = image().pixels().to_vec();
        for (i, p) in pixels.iter_mut().enumerate() {
            let (x, y) = ((i as u32 % W) as f32, (i as u32 / W) as f32);
            let hot = 13000.0 * (-((x - 80.0).hypot(y - 60.0) / 12.0).powi(2)).exp();
            *p = [p[0] + hot as u16, p[1] + (hot * 1.3) as u16, p[2] + (hot * 1.5) as u16, p[3]];
        }
        let image = Raster::new(W, H, pixels);
        let mut doc = Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16);
        let ([left, right], spots) = (skins(), spot(150.0));
        let shiny = |number, skin, spots| Face {
            shine: Some(50.0),
            evening: None,
            ..face(number, skin, spots)
        };
        add_layers(&mut doc, 0, &[shiny(1, &left, &spots), shiny(2, &right, &[])]).unwrap();
        let names: Vec<_> = doc.layers.iter().map(|l| l.name.as_str()).collect();
        // The second face has no shine, so no layer for it.
        assert_eq!(names[1..], ["Smooth Skin", "Face 2", "Blemishes", "Reduce Shine", "Smooth Skin", "Face 1", "Retouch"]);
        let shine = doc.layers.iter().find(|l| l.name == "Reduce Shine").unwrap();
        assert_eq!(shine.opacity, 0.5);
        // Made from the image with the spot healed, and the hot spot's
        // lower for it.
        assert!(shine.pixels.get(150, 100)[0] > 40000, "{:?}", shine.pixels.get(150, 100));
        assert!(doc.composite().get(80, 60)[0] < image.get(80, 60)[0] - 2000);
    }

    #[test]
    fn skin_is_shared_out_by_the_nearest_face_in_its_own_size() {
        let skin = Selection::rectangle(W, H, (0.0, 100.0), (W as f32, 200.0));
        // A face on the left twice the size of the one on the right: its
        // share reaches two thirds of the way across.
        let person = |x: f32, iod: f32| Person {
            face: [x, 150.0],
            iod,
            joints: None,
        };
        let people = [person(0.0, 80.0), person(600.0, 40.0)];
        let [left, right] = [0, 1].map(|n| share(&skin, &people, n));
        assert!(left.at(100, 150) > 0.99 && left.at(390, 150) > 0.99 && left.at(410, 150) < 0.01);
        assert!(right.at(410, 150) > 0.99 && right.at(390, 150) < 0.01);
        // Only skin, and all of it between them.
        assert!(left.at(100, 50) < 0.01 && right.at(500, 250) < 0.01);
        assert_eq!(left.combine(&right, crate::Combine::Add).bounds(), skin.bounds());
        // One face has it all.
        assert_eq!(share(&skin, &people[..1], 0).bounds(), skin.bounds());
    }

    #[test]
    fn an_arm_across_someone_else_is_still_its_owners() {
        // Two people side by side, the one on the left with an arm out
        // across the other's chest, and skin everywhere.
        let skin = Selection::rectangle(W, H, (0.0, 0.0), (W as f32, H as f32));
        let seen = |x: f32, y: f32| [x, y, 1.0];
        let standing = |x: f32| Joints {
            ears: [seen(x + 20.0, 60.0), seen(x - 20.0, 60.0)],
            shoulders: [seen(x + 60.0, 130.0), seen(x - 60.0, 130.0)],
            elbows: [seen(x + 70.0, 200.0), seen(x - 70.0, 200.0)],
            wrists: [seen(x + 70.0, 270.0), seen(x - 70.0, 270.0)],
            hips: [seen(x + 40.0, 280.0), seen(x - 40.0, 280.0)],
            ..Default::default()
        };
        let mut reaching = standing(150.0);
        (reaching.elbows[0], reaching.wrists[0]) = (seen(300.0, 170.0), seen(400.0, 180.0));
        let person = |x: f32, joints| Person {
            face: [x, 60.0],
            iod: 20.0,
            joints,
        };
        let people = [person(150.0, Some(reaching)), person(450.0, Some(standing(450.0)))];
        let [left, right] = [0, 1].map(|n| share(&skin, &people, n));
        // The forearm and hand over the other's chest are the left one's,
        // and the chest round them the right one's.
        assert!(left.at(390, 180) > 0.99 && left.at(420, 182) > 0.99 && right.at(390, 180) < 0.01);
        assert!(right.at(450, 230) > 0.99 && right.at(450, 130) > 0.99 && right.at(450, 60) > 0.99);
        assert!(left.at(150, 200) > 0.99 && left.at(230, 150) > 0.99);
        // By their faces alone, the arm would be the right one's.
        let faces = people.map(|p| Person { joints: None, ..p });
        assert!(share(&skin, &faces, 1).at(390, 180) > 0.99);
        // Joints the pose model didn't see (the legs here) are left out,
        // and someone it didn't see at all still has their face.
        let half = [people[0], faces[1]];
        assert!(share(&skin, &half, 1).at(450, 60) > 0.99 && share(&skin, &half, 0).at(390, 180) > 0.99);
    }
}
