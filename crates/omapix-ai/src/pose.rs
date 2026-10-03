//! Body poses (docs/AI.md, feature 8): MediaPipe's pose detector finds the
//! people in a photo, and its Pose Landmarker puts 33 points on each and
//! mattes them, for Body Reshape. scripts/fetch-models.sh installs both.

use omapix_engine::body::{Body, Joints, Matte};
use ort::session::Session;
use ort::value::Tensor;

use crate::Result;
use crate::face::{Crop, Image};
use crate::find_model;
use crate::runtime::session;

/// The model's id: its folder in Omapix's models.
pub const MODEL: &str = "pose-landmarks-mediapipe";

/// The detector sees the image fitted into this square, and the landmarker
/// its crop in this one, which its matte covers too.
const DETECT_SIZE: usize = 224;
pub const SIZE: usize = 256;
/// The square the landmarker sees is this many times the body's size.
const ROOM: f32 = 1.25;

/// The landmarker's points, in the order of its output. Left and right are
/// the person's own.
pub mod point {
    pub const LEFT_EAR: usize = 7;
    pub const RIGHT_EAR: usize = 8;
    pub const LEFT_SHOULDER: usize = 11;
    pub const RIGHT_SHOULDER: usize = 12;
    pub const LEFT_ELBOW: usize = 13;
    pub const RIGHT_ELBOW: usize = 14;
    pub const LEFT_WRIST: usize = 15;
    pub const RIGHT_WRIST: usize = 16;
    pub const LEFT_HIP: usize = 23;
    pub const RIGHT_HIP: usize = 24;
    pub const LEFT_KNEE: usize = 25;
    pub const RIGHT_KNEE: usize = 26;
    pub const LEFT_ANKLE: usize = 27;
    pub const RIGHT_ANKLE: usize = 28;
    /// How many there are: the model's other six are its own.
    pub const COUNT: usize = 33;
}

/// A person the landmarker saw.
#[derive(Clone, Debug)]
pub struct Pose {
    /// The square it saw them in.
    pub crop: Crop,
    /// Each of [`point`]'s: x and y in image pixels, depth, and how likely
    /// it is to be in view, 0–1.
    pub points: Vec<[f32; 4]>,
    /// How much of each of [`SIZE`]² points of `crop` is the person, 0–1.
    pub matte: Vec<f32>,
}

impl Pose {
    /// The person, for Body Reshape: `None` if their shoulders are out of
    /// view.
    pub fn body(&self) -> Option<Body> {
        let pair = |left: usize, right: usize| [left, right].map(|i| [self.points[i][0], self.points[i][1], self.points[i][3]]);
        let joints = Joints {
            ears: pair(point::LEFT_EAR, point::RIGHT_EAR),
            shoulders: pair(point::LEFT_SHOULDER, point::RIGHT_SHOULDER),
            elbows: pair(point::LEFT_ELBOW, point::RIGHT_ELBOW),
            wrists: pair(point::LEFT_WRIST, point::RIGHT_WRIST),
            hips: pair(point::LEFT_HIP, point::RIGHT_HIP),
            knees: pair(point::LEFT_KNEE, point::RIGHT_KNEE),
            ankles: pair(point::LEFT_ANKLE, point::RIGHT_ANKLE),
        };
        let Crop { centre, side, angle } = self.crop;
        Body::new(&joints, &Matte { centre, side, angle, size: SIZE, cover: &self.matte })
    }
}

fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn sigmoid(v: f32) -> f32 {
    1.0 / (1.0 + (-v).exp())
}

/// The square round a body whose middle is `centre`, with `edge` on the
/// circle round it, turned so the way from one to the other is up.
fn body_crop(centre: [f32; 2], edge: [f32; 2]) -> Crop {
    let (dx, dy) = (edge[0] - centre[0], edge[1] - centre[1]);
    Crop { centre, side: 2.0 * dx.hypot(dy) * ROOM, angle: std::f32::consts::FRAC_PI_2 - (-dy).atan2(dx) }
}

pub struct Poses {
    detector: Session,
    landmarker: Session,
}

impl Poses {
    pub fn load() -> Result<Self> {
        let files =
            find_model(MODEL).ok_or_else(|| format!("Body Reshape needs the {MODEL} model: run scripts/fetch-models.sh"))?;
        Ok(Self {
            detector: session(&files["pose_detection.onnx"])?,
            landmarker: session(&files["pose_landmarks_detector_heavy.onnx"])?,
        })
    }

    /// The square round each person in `image`, most confident first.
    pub fn detect(&mut self, image: &Image) -> Result<Vec<Crop>> {
        // The whole image, in the middle of a square.
        let whole = Crop {
            centre: [image.width as f32 / 2.0, image.height as f32 / 2.0],
            side: image.width.max(image.height) as f32,
            angle: 0.0,
        };
        // −1 to 1.
        let pixels: Vec<f32> = image.sample(&whole, DETECT_SIZE).into_iter().flatten().map(|v| v * 2.0 - 1.0).collect();
        let n = DETECT_SIZE as i64;
        let input = Tensor::from_array((vec![1, n, n, 3], pixels)).map_err(error)?;
        let outputs = self.detector.run(ort::inputs![input]).map_err(error)?;
        let (_, boxes) = outputs[0].try_extract_tensor::<f32>().map_err(error)?;
        let (_, scores) = outputs[1].try_extract_tensor::<f32>().map_err(error)?;
        // An answer for each anchor: two to a cell every 8 and 16 pixels,
        // and six every 32. Each is the face's box, then the middle of the
        // hips and a point on the circle round the body, measured from the
        // cell's middle.
        let anchors = [(8, 2), (16, 2), (32, 6)].into_iter().flat_map(|(stride, each)| {
            let cells = DETECT_SIZE / stride;
            (0..cells * cells * each).map(move |k| {
                let cell = k / each;
                [((cell % cells) as f32 + 0.5) / cells as f32, ((cell / cells) as f32 + 0.5) / cells as f32]
            })
        });
        let size = DETECT_SIZE as f32;
        let mut found: Vec<(f32, [f32; 4], Crop)> = anchors
            .zip(boxes.chunks(12))
            .zip(scores)
            .filter_map(|((anchor, b), &score)| {
                let score = sigmoid(score.clamp(-100.0, 100.0));
                let at = |x: f32, y: f32| whole.to_image(x / size + anchor[0], y / size + anchor[1]);
                let ([x, y], [w, h]) = (at(b[0], b[1]), [b[2], b[3]].map(|v| v / size * whole.side / 2.0));
                (score >= 0.5).then(|| (score, [x - w, y - h, x + w, y + h], body_crop(at(b[4], b[5]), at(b[6], b[7]))))
            })
            .collect();
        // Non-maximum suppression, by the faces' boxes.
        found.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut kept: Vec<(f32, [f32; 4], Crop)> = Vec::new();
        for f in found {
            if kept.iter().all(|k| crate::face::iou(&k.1, &f.1) < 0.3) {
                kept.push(f);
            }
        }
        Ok(kept.into_iter().map(|(.., crop)| crop).collect())
    }

    /// The person in `crop`, or `None` if the landmarker doesn't see one
    /// there. It looks twice, the second time in the square round the body
    /// it found the first time, as MediaPipe tracks.
    pub fn landmarks(&mut self, image: &Image, crop: &Crop) -> Result<Option<Pose>> {
        let mut pose = None;
        let mut crop = *crop;
        for _ in 0..2 {
            let pixels: Vec<f32> = image.sample(&crop, SIZE).into_iter().flatten().collect();
            let n = SIZE as i64;
            let input = Tensor::from_array((vec![1, n, n, 3], pixels)).map_err(error)?;
            let outputs = self.landmarker.run(ort::inputs![input]).map_err(error)?;
            let (_, raw) = outputs[0].try_extract_tensor::<f32>().map_err(error)?;
            let (_, presence) = outputs[1].try_extract_tensor::<f32>().map_err(error)?;
            let (_, matte) = outputs[2].try_extract_tensor::<f32>().map_err(error)?;
            if presence[0] < 0.5 {
                return Ok(None);
            }
            let size = SIZE as f32;
            // x, y, depth, and how likely it's in view and in the picture.
            let points: Vec<[f32; 4]> = raw
                .chunks(5)
                .map(|p| {
                    let [x, y] = crop.to_image(p[0] / size, p[1] / size);
                    [x, y, p[2] / size * crop.side, sigmoid(p[3]).min(sigmoid(p[4]))]
                })
                .collect();
            let seen = crop;
            // After the body's points come the middle of the hips and a
            // point on the circle round the body, as the detector gives.
            let at = |i: usize| [points[i][0], points[i][1]];
            crop = body_crop(at(point::COUNT), at(point::COUNT + 1));
            pose = Some(Pose {
                crop: seen,
                points: points[..point::COUNT].to_vec(),
                matte: matte.iter().map(|&v| sigmoid(v)).collect(),
            });
        }
        Ok(pose)
    }

    /// Everyone in `image` the landmarker sees, from left to right.
    pub fn find(&mut self, image: &Image) -> Result<Vec<Pose>> {
        let mut poses = Vec::new();
        for crop in self.detect(image)? {
            poses.extend(self.landmarks(image, &crop)?);
        }
        poses.sort_by(|a, b| a.crop.centre[0].total_cmp(&b.crop.centre[0]));
        Ok(poses)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bodys_crop_is_centred_on_its_hips_and_turned_upright() {
        // Standing: the circle's top is straight up.
        let crop = body_crop([100.0, 200.0], [100.0, 120.0]);
        assert!(crop.angle.abs() < 1e-5 && (crop.side - 200.0).abs() < 1e-3, "{crop:?}");
        // Lying with the head to the right: the crop's top is the right.
        let crop = body_crop([100.0, 200.0], [180.0, 200.0]);
        let [x, y] = crop.to_image(0.5, 0.0);
        assert!((x - 200.0).abs() < 1e-3 && (y - 200.0).abs() < 1e-3, "{x} {y}");
    }

    /// Needs the model (scripts/fetch-models.sh). How well it finds people
    /// is for real photos: see examples/pose.rs.
    #[test]
    #[ignore]
    fn a_blank_image_has_nobody_in_it() {
        let pixels = vec![[128u8, 128, 128, 255]; 300 * 200];
        let image = Image { pixels: &pixels, width: 300, height: 200 };
        assert!(Poses::load().unwrap().find(&image).unwrap().is_empty());
    }
}
