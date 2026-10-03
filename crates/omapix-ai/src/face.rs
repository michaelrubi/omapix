//! Face analysis (docs/AI.md, feature 2): YuNet finds faces, MediaPipe's
//! Face Landmarker puts 478 points on each, and MediaPipe's multiclass
//! selfie segmentation tells hair, face skin and body skin apart.
//! scripts/fetch-models.sh installs all three.

use ort::session::Session;
use ort::value::Tensor;
use rayon::prelude::*;

use crate::Result;
use crate::find_model;
use crate::runtime::session;

pub const DETECTOR: &str = "face-detect-yunet";
pub const LANDMARKER: &str = "face-landmarks-mediapipe";
pub const SEGMENTER: &str = "mask-selfie-multiclass";

/// YuNet sees the image fitted into this square.
const DETECT_SIZE: usize = 640;
/// The landmarker's crop, and the segmenter's, are this many pixels across.
const LANDMARK_SIZE: usize = 256;
pub const SEGMENT_SIZE: usize = 256;
/// The segmenter's classes, in the order of its output.
pub const CLASSES: [&str; 6] = ["background", "hair", "body skin", "face skin", "clothes", "others"];
pub const HAIR: usize = 1;
pub const BODY_SKIN: usize = 2;
pub const FACE_SKIN: usize = 3;

/// Outlines of the features, as indices into the landmarker's points, in
/// order round each one. "Left" is the left of the image.
pub mod outline {
    pub const LEFT_EYE: [usize; 16] = [
        33, 246, 161, 160, 159, 158, 157, 173, 133, 155, 154, 153, 145, 144, 163, 7,
    ];
    pub const RIGHT_EYE: [usize; 16] = [
        263, 466, 388, 387, 386, 385, 384, 398, 362, 382, 381, 380, 374, 373, 390, 249,
    ];
    pub const LEFT_BROW: [usize; 10] = [70, 63, 105, 66, 107, 55, 65, 52, 53, 46];
    pub const RIGHT_BROW: [usize; 10] = [300, 293, 334, 296, 336, 285, 295, 282, 283, 276];
    /// Round the outside of the lips.
    pub const LIPS: [usize; 20] = [
        61, 185, 40, 39, 37, 0, 267, 269, 270, 409, 291, 375, 321, 405, 314, 17, 84, 181, 91, 146,
    ];
    /// Between the lips: the mouth, and the teeth when they show.
    pub const MOUTH: [usize; 20] = [
        78, 191, 80, 81, 82, 13, 312, 311, 310, 415, 308, 324, 318, 402, 317, 14, 87, 178, 88, 95,
    ];
    /// Round the face, from the top of the forehead.
    pub const FACE: [usize; 36] = [
        10, 338, 297, 332, 284, 251, 389, 356, 454, 323, 361, 288, 397, 365, 379, 378, 400, 377, 152, 148, 176, 149,
        150, 136, 172, 58, 132, 93, 234, 127, 162, 21, 54, 103, 67, 109,
    ];
    /// Round the bottom of the nose: its wings, nostrils and tip
    /// (MediaPipe's nose outline).
    pub const NOSE: [usize; 16] = [98, 97, 2, 326, 327, 294, 278, 344, 440, 275, 4, 45, 220, 115, 48, 64];
    /// Each iris: its centre, then four points round it.
    pub const LEFT_IRIS: [usize; 5] = [468, 469, 470, 471, 472];
    pub const RIGHT_IRIS: [usize; 5] = [473, 474, 475, 476, 477];
}

/// A face YuNet found, in image pixels.
#[derive(Clone, Debug)]
pub struct Detection {
    pub score: f32,
    /// Left, top, right, bottom.
    pub bounds: [f32; 4],
    /// The eye on the left of the image, the other eye, the tip of the
    /// nose, then the mouth's corners, left of the image first.
    pub points: [[f32; 2]; 5],
}

/// A square, turned by `angle` (radians, clockwise on screen) about its
/// centre: the part of the image a model sees.
#[derive(Clone, Copy, Debug)]
pub struct Crop {
    pub centre: [f32; 2],
    pub side: f32,
    pub angle: f32,
}

impl Crop {
    /// Where `(u, v)`, 0–1 across and down the crop, is in the image.
    pub fn to_image(&self, u: f32, v: f32) -> [f32; 2] {
        let (dx, dy) = ((u - 0.5) * self.side, (v - 0.5) * self.side);
        let (s, c) = self.angle.sin_cos();
        [self.centre[0] + dx * c - dy * s, self.centre[1] + dx * s + dy * c]
    }

    /// The square `scale` times the size of the box round `points` (in
    /// the frame turned by `angle`), as MediaPipe crops faces.
    fn around(points: impl Iterator<Item = [f32; 2]> + Clone, angle: f32, scale: f32) -> Self {
        let (s, c) = angle.sin_cos();
        // In the turned frame.
        let turned = points.map(|[x, y]| [x * c + y * s, -x * s + y * c]);
        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
        for p in turned {
            for i in 0..2 {
                lo[i] = lo[i].min(p[i]);
                hi[i] = hi[i].max(p[i]);
            }
        }
        let (mx, my) = ((lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0);
        Self {
            centre: [mx * c - my * s, mx * s + my * c],
            side: (hi[0] - lo[0]).max(hi[1] - lo[1]) * scale,
            angle,
        }
    }
}

/// The angle that levels the line from `a` to `b`.
fn level(a: [f32; 2], b: [f32; 2]) -> f32 {
    (b[1] - a[1]).atan2(b[0] - a[0])
}

/// 8-bit sRGB, as the models see it.
pub struct Image<'a> {
    pub pixels: &'a [[u8; 4]],
    pub width: usize,
    pub height: usize,
}

impl Image<'_> {
    /// `crop` resampled to `size` × `size` RGB, 0–1, rows of pixels
    /// (outside the image is black). Each output pixel averages enough
    /// taps to cover its footprint, so shrinking a 24 MP image doesn't
    /// alias.
    pub fn sample(&self, crop: &Crop, size: usize) -> Vec<[f32; 3]> {
        let taps = (crop.side / size as f32).ceil().max(1.0) as usize;
        (0..size * size)
            .into_par_iter()
            .map(|i| {
                let (x, y) = ((i % size) as f32, (i / size) as f32);
                let mut sum = [0.0f32; 3];
                for ty in 0..taps {
                    for tx in 0..taps {
                        let u = (x + (tx as f32 + 0.5) / taps as f32) / size as f32;
                        let v = (y + (ty as f32 + 0.5) / taps as f32) / size as f32;
                        let [px, py] = crop.to_image(u, v);
                        let p = self.bilinear(px - 0.5, py - 0.5);
                        for c in 0..3 {
                            sum[c] += p[c];
                        }
                    }
                }
                sum.map(|s| s / (taps * taps) as f32 / 255.0)
            })
            .collect()
    }

    fn bilinear(&self, x: f32, y: f32) -> [f32; 3] {
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let at = |x: f32, y: f32| {
            if x < 0.0 || y < 0.0 || x >= self.width as f32 || y >= self.height as f32 {
                [0.0; 3]
            } else {
                let p = self.pixels[y as usize * self.width + x as usize];
                [0, 1, 2].map(|c| f32::from(p[c]))
            }
        };
        let (a, b, c, d) = (at(x0, y0), at(x0 + 1.0, y0), at(x0, y0 + 1.0), at(x0 + 1.0, y0 + 1.0));
        [0, 1, 2].map(|i| (a[i] * (1.0 - fx) + b[i] * fx) * (1.0 - fy) + (c[i] * (1.0 - fx) + d[i] * fx) * fy)
    }
}

fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn load(id: &str, file: &str) -> Result<Session> {
    let files =
        find_model(id).ok_or_else(|| format!("Face analysis needs the {id} model: run scripts/fetch-models.sh"))?;
    session(&files[file])
}

pub struct Faces {
    detector: Session,
    landmarker: Session,
    segmenter: Session,
}

impl Faces {
    pub fn load() -> Result<Self> {
        Ok(Self {
            detector: load(DETECTOR, "face_detection_yunet_2023mar.onnx")?,
            landmarker: load(LANDMARKER, "face_landmarks_detector.onnx")?,
            segmenter: load(SEGMENTER, "selfie_multiclass_256x256.onnx")?,
        })
    }

    /// The faces in `image`, most confident first.
    pub fn detect(&mut self, image: &Image) -> Result<Vec<Detection>> {
        // The whole image, fitted into the top left of the square.
        let side = image.width.max(image.height) as f32;
        let crop = Crop {
            centre: [side / 2.0; 2],
            side,
            angle: 0.0,
        };
        let pixels = image.sample(&crop, DETECT_SIZE);
        // Blue, green, red planes, 0–255.
        let planar: Vec<f32> = [2, 1, 0]
            .iter()
            .flat_map(|&c| pixels.iter().map(move |p| p[c] * 255.0))
            .collect();
        let n = DETECT_SIZE as i64;
        let input = Tensor::from_array((vec![1, 3, n, n], planar)).map_err(error)?;
        let outputs = self.detector.run(ort::inputs!["input" => input]).map_err(error)?;
        let to_image = side / DETECT_SIZE as f32;
        let mut found = Vec::new();
        for stride in [8usize, 16, 32] {
            let get = |name: &str| -> Result<Vec<f32>> {
                let (_, data) = outputs[format!("{name}_{stride}").as_str()]
                    .try_extract_tensor::<f32>()
                    .map_err(error)?;
                Ok(data.to_vec())
            };
            let (cls, obj, bbox, kps) = (get("cls")?, get("obj")?, get("bbox")?, get("kps")?);
            let cols = DETECT_SIZE / stride;
            let s = stride as f32;
            for (i, (&cls, &obj)) in cls.iter().zip(&obj).enumerate() {
                let score = (cls.clamp(0.0, 1.0) * obj.clamp(0.0, 1.0)).sqrt();
                if score < 0.6 {
                    continue;
                }
                let (col, row) = ((i % cols) as f32, (i / cols) as f32);
                let b = &bbox[i * 4..i * 4 + 4];
                let (cx, cy) = ((col + b[0]) * s, (row + b[1]) * s);
                let (w, h) = (b[2].exp() * s, b[3].exp() * s);
                let k = &kps[i * 10..i * 10 + 10];
                found.push(Detection {
                    score,
                    bounds: [cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0].map(|v| v * to_image),
                    points: [0, 1, 2, 3, 4]
                        .map(|p| [(k[2 * p] + col) * s * to_image, (k[2 * p + 1] + row) * s * to_image]),
                });
            }
        }
        // Non-maximum suppression.
        found.sort_by(|a, b| b.score.total_cmp(&a.score));
        let mut kept: Vec<Detection> = Vec::new();
        for d in found {
            if kept.iter().all(|k| iou(&k.bounds, &d.bounds) < 0.3) {
                kept.push(d);
            }
        }
        Ok(kept)
    }

    /// The face's 478 points (x, y in image pixels, and depth), or `None`
    /// if the landmarker doesn't see a face there. The crop is found from
    /// the detection, then again from the first pass's points, as
    /// MediaPipe tracks.
    pub fn landmarks(&mut self, image: &Image, face: &Detection) -> Result<Option<(Crop, Vec<[f32; 3]>)>> {
        let [x0, y0, x1, y1] = face.bounds;
        let angle = level(face.points[0], face.points[1]);
        let corners = [[x0, y0], [x1, y0], [x0, y1], [x1, y1]];
        let mut crop = Crop::around(corners.into_iter(), angle, 1.5);
        let mut points = Vec::new();
        for _ in 0..2 {
            let pixels = image.sample(&crop, LANDMARK_SIZE);
            let n = LANDMARK_SIZE as i64;
            let input = Tensor::from_array((vec![1, n, n, 3], pixels.into_iter().flatten().collect::<Vec<f32>>()))
                .map_err(error)?;
            let outputs = self.landmarker.run(ort::inputs![input]).map_err(error)?;
            let (_, raw) = outputs[0].try_extract_tensor::<f32>().map_err(error)?;
            let (_, presence) = outputs[1].try_extract_tensor::<f32>().map_err(error)?;
            if 1.0 / (1.0 + (-presence[0]).exp()) < 0.5 {
                return Ok(None);
            }
            let size = LANDMARK_SIZE as f32;
            points = raw
                .chunks(3)
                .map(|p| {
                    let [x, y] = crop.to_image(p[0] / size, p[1] / size);
                    [x, y, p[2] / size * crop.side]
                })
                .collect();
            // Eye corners 33 and 263 level the next crop.
            let angle = level([points[33][0], points[33][1]], [points[263][0], points[263][1]]);
            crop = Crop::around(points.iter().map(|p| [p[0], p[1]]), angle, 1.5);
        }
        Ok(Some((crop, points)))
    }

    /// The chance of each of [`CLASSES`] at each of [`SEGMENT_SIZE`]²
    /// points of `crop`, classes last.
    pub fn segment(&mut self, image: &Image, crop: &Crop) -> Result<Vec<[f32; 6]>> {
        let pixels = image.sample(crop, SEGMENT_SIZE);
        let n = SEGMENT_SIZE as i64;
        // −1 to 1.
        let data: Vec<f32> = pixels.into_iter().flatten().map(|v| v * 2.0 - 1.0).collect();
        let input = Tensor::from_array((vec![1, n, n, 3], data)).map_err(error)?;
        let outputs = self.segmenter.run(ort::inputs![input]).map_err(error)?;
        let (_, logits) = outputs[0].try_extract_tensor::<f32>().map_err(error)?;
        Ok(logits
            .chunks(6)
            .map(|l| {
                let max = l.iter().copied().fold(f32::MIN, f32::max);
                let e: [f32; 6] = std::array::from_fn(|i| (l[i] - max).exp());
                let sum: f32 = e.iter().sum();
                e.map(|v| v / sum)
            })
            .collect())
    }
}

/// What the models found in an image.
pub struct Analysis {
    /// Each face, with its points if the landmarker saw it (not in
    /// profile, for one).
    pub faces: Vec<(Detection, Option<Vec<[f32; 3]>>)>,
    width: usize,
    height: usize,
    /// The whole image's segmentation, over the square round it.
    segmentation: Vec<[f32; 6]>,
}

impl Faces {
    /// Find the faces in `image`, their points, and its segmentation.
    pub fn analyse(&mut self, image: &Image) -> Result<Analysis> {
        let mut faces = Vec::new();
        for face in self.detect(image)? {
            let points = self.landmarks(image, &face)?.map(|(_, points)| points);
            faces.push((face, points));
        }
        let (w, h) = (image.width as f32, image.height as f32);
        let whole = Crop {
            centre: [w / 2.0, h / 2.0],
            side: w.max(h),
            angle: 0.0,
        };
        let segmentation = self.segment(image, &whole)?;
        Ok(Analysis {
            faces,
            width: image.width,
            height: image.height,
            segmentation,
        })
    }
}

impl Analysis {
    /// How likely each point is to be one of `classes`, as logits (above
    /// 0 more likely than not) on a grid stretched over the image, and the
    /// grid's width and height: what `refine::mask_coverage` takes.
    pub fn logits(&self, classes: &[usize]) -> (Vec<f32>, usize, usize) {
        // The image's part of the square the segmenter saw.
        let longest = self.width.max(self.height);
        let (lw, lh) = (
            (SEGMENT_SIZE * self.width / longest).max(1),
            (SEGMENT_SIZE * self.height / longest).max(1),
        );
        let (ox, oy) = ((SEGMENT_SIZE - lw) / 2, (SEGMENT_SIZE - lh) / 2);
        let logits = (0..lw * lh)
            .map(|i| {
                let p = &self.segmentation[(oy + i / lw) * SEGMENT_SIZE + ox + i % lw];
                let p: f32 = classes.iter().map(|&c| p[c]).sum::<f32>().clamp(1e-4, 1.0 - 1e-4);
                (p / (1.0 - p)).ln()
            })
            .collect();
        (logits, lw, lh)
    }
}

pub(crate) fn iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let w = (a[2].min(b[2]) - a[0].max(b[0])).max(0.0);
    let h = (a[3].min(b[3]) - a[1].max(b[1])).max(0.0);
    let area = |r: &[f32; 4]| (r[2] - r[0]) * (r[3] - r[1]);
    w * h / (area(a) + area(b) - w * h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_crop_turned_a_quarter_maps_right_to_down() {
        let crop = Crop {
            centre: [100.0, 50.0],
            side: 20.0,
            angle: std::f32::consts::FRAC_PI_2,
        };
        let [x, y] = crop.to_image(1.0, 0.5);
        assert!((x - 100.0).abs() < 1e-4 && (y - 60.0).abs() < 1e-4, "{x} {y}");
    }

    /// Needs the models (scripts/fetch-models.sh). The examples' `faces`
    /// tries them on real photos.
    #[test]
    #[ignore]
    fn a_blank_image_has_no_faces_and_is_all_background() {
        let mut faces = Faces::load().unwrap();
        let pixels = vec![[128u8, 128, 128, 255]; 300 * 200];
        let image = Image {
            pixels: &pixels,
            width: 300,
            height: 200,
        };
        assert!(faces.detect(&image).unwrap().is_empty());
        let whole = Crop {
            centre: [150.0, 100.0],
            side: 300.0,
            angle: 0.0,
        };
        let seg = faces.segment(&image, &whole).unwrap();
        assert_eq!(seg.len(), SEGMENT_SIZE * SEGMENT_SIZE);
        let background = seg.iter().filter(|p| p[0] > 0.5).count();
        assert!(background > seg.len() * 9 / 10, "{background}");
        let analysis = faces.analyse(&image).unwrap();
        assert!(analysis.faces.is_empty());
        let (logits, w, h) = analysis.logits(&[BODY_SKIN, FACE_SKIN]);
        assert_eq!((w, h), (256, 170));
        assert!(logits.iter().all(|&l| l < 0.0));
    }

    #[test]
    fn a_crop_around_points_is_centred_on_them_and_levelled() {
        let points = [[10.0, 10.0], [30.0, 30.0]];
        let angle = level(points[0], points[1]);
        let crop = Crop::around(points.into_iter(), angle, 1.5);
        assert!((crop.centre[0] - 20.0).abs() < 1e-4 && (crop.centre[1] - 20.0).abs() < 1e-4);
        // In the turned frame the points lie along one line, 28.3 apart.
        assert!((crop.side - 800f32.sqrt() * 1.5).abs() < 1e-3, "{}", crop.side);
    }
}
