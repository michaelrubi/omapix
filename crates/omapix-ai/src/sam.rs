//! Segment Anything 2.1, as darktable's `mask-object-sam21-small`: the
//! image is encoded once (a fraction of a second on the GPU), then each
//! click or box is decoded in milliseconds.

use ort::session::Session;
use ort::value::Tensor;
use rayon::prelude::*;

use crate::Result;
use crate::runtime::{find_model, session};

/// The model's id, as darktable names its folder.
pub const MODEL: &str = "mask-object-sam21-small";

/// The encoder sees the image stretched to this square.
const SIZE: usize = 1024;
/// ImageNet's mean and spread, which the encoder was trained with.
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];
/// The decoder's masks are this many pixels across.
pub const MASK_SIZE: usize = 256;

pub struct Sam {
    encoder: Session,
    decoder: Session,
}

/// An image as the encoder saw it, ready for any number of prompts.
pub struct Encoded {
    width: u32,
    height: u32,
    /// The encoder's outputs, by the decoder input they go to.
    features: Vec<(&'static str, Vec<i64>, Vec<f32>)>,
}

/// What to select, in image pixels.
#[derive(Clone, Debug, PartialEq)]
pub enum Prompt {
    /// The object under this point.
    Point(f32, f32),
    /// The object in this box, from one corner to the other.
    Box(f32, f32, f32, f32),
    /// The object under these points (`true`) and not under these
    /// (`false`), as painted with Quick Selection.
    Points(Vec<(f32, f32, bool)>),
}

fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl Sam {
    /// Load the model from Omapix's or darktable's model folder.
    pub fn load() -> Result<Self> {
        let dir = find_model(MODEL).ok_or_else(|| {
            format!("Object Selection needs the {MODEL} model: install it from darktable's AI preferences")
        })?;
        Ok(Self {
            encoder: session(&dir.join("encoder.onnx"))?,
            decoder: session(&dir.join("decoder.onnx"))?,
        })
    }

    /// Encode an image given as 8-bit sRGB, `width` × `height`.
    pub fn encode(&mut self, srgb: &[[u8; 4]], width: u32, height: u32) -> Result<Encoded> {
        let (w, h) = (width as usize, height as usize);
        // Averaged down (or stretched) to SIZE × SIZE, normalised, channels
        // first.
        let pixels: Vec<[f32; 3]> = (0..SIZE * SIZE)
            .into_par_iter()
            .map(|i| {
                let (x, y) = (i % SIZE, i / SIZE);
                let (x0, y0) = (x * w / SIZE, y * h / SIZE);
                let (x1, y1) = (((x + 1) * w / SIZE).max(x0 + 1), ((y + 1) * h / SIZE).max(y0 + 1));
                let mut sum = [0u32; 3];
                for row in srgb[y0 * w..y1 * w].chunks(w) {
                    for p in &row[x0..x1] {
                        for c in 0..3 {
                            sum[c] += u32::from(p[c]);
                        }
                    }
                }
                let n = ((x1 - x0) * (y1 - y0)) as f32 * 255.0;
                [0, 1, 2].map(|c| (sum[c] as f32 / n - MEAN[c]) / STD[c])
            })
            .collect();
        let planar: Vec<f32> = (0..3).flat_map(|c| pixels.iter().map(move |p| p[c])).collect();
        let image = Tensor::from_array((vec![1i64, 3, SIZE as i64, SIZE as i64], planar)).map_err(error)?;
        let outputs = self.encoder.run(ort::inputs!["image" => image]).map_err(error)?;
        let features = ["image_embed", "high_res_feats_0", "high_res_feats_1"]
            .into_iter()
            .map(|name| {
                let (shape, data) = outputs[name].try_extract_tensor::<f32>().map_err(error)?;
                Ok((name, shape.to_vec(), data.to_vec()))
            })
            .collect::<Result<_>>()?;
        Ok(Encoded { width, height, features })
    }

    /// The object `prompt` points to, as [`MASK_SIZE`]² logits stretched
    /// over the whole image, positive inside it: the best of the masks the
    /// model offers. `previous`, the last answer about the same object,
    /// helps it refine rather than start again.
    pub fn select(&mut self, image: &Encoded, prompt: &Prompt, previous: Option<&[f32]>) -> Result<Vec<f32>> {
        // Prompts are given in the encoder's SIZE × SIZE square. Box
        // corners are labelled 2 and 3, points in the object 1 and points
        let (sx, sy) = (SIZE as f32 / image.width as f32, SIZE as f32 / image.height as f32);
        // not in it 0.
        let (coords, labels) = match *prompt {
            Prompt::Point(x, y) => (vec![x * sx, y * sy], vec![1.0]),
            Prompt::Box(x0, y0, x1, y1) => (
                vec![x0.min(x1) * sx, y0.min(y1) * sy, x0.max(x1) * sx, y0.max(y1) * sy],
                vec![2.0, 3.0],
            ),
            Prompt::Points(ref points) => (
                points.iter().flat_map(|&(x, y, _)| [x * sx, y * sy]).collect(),
                points.iter().map(|&(_, _, inside)| if inside { 1.0 } else { 0.0 }).collect(),
            ),
        };
        let points = labels.len() as i64;
        let tensor = |shape: Vec<i64>, data: Vec<f32>| Tensor::from_array((shape, data)).map_err(error);
        let mut inputs = ort::inputs![
            "point_coords" => tensor(vec![1, points, 2], coords)?,
            "point_labels" => tensor(vec![1, points], labels)?,
            "mask_input" => tensor(
                vec![1, 1, MASK_SIZE as i64, MASK_SIZE as i64],
                previous.map_or_else(|| vec![0.0; MASK_SIZE * MASK_SIZE], <[f32]>::to_vec),
            )?,
            "has_mask_input" => tensor(vec![1], vec![if previous.is_some() { 1.0 } else { 0.0 }])?,
        ];
        for (name, shape, data) in &image.features {
            inputs.push(((*name).into(), tensor(shape.clone(), data.clone())?.into()));
        }
        let outputs = self.decoder.run(inputs).map_err(error)?;
        let (_, scores) = outputs["iou_predictions"].try_extract_tensor::<f32>().map_err(error)?;
        let (_, masks) = outputs["masks"].try_extract_tensor::<f32>().map_err(error)?;
        let best = (0..scores.len()).max_by(|&a, &b| scores[a].total_cmp(&scores[b])).unwrap_or(0);
        let plane = MASK_SIZE * MASK_SIZE;
        Ok(masks[best * plane..(best + 1) * plane].to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A light disc on a darker background, 600 × 400.
    fn disc() -> Vec<[u8; 4]> {
        (0..600 * 400)
            .map(|i| {
                let (x, y) = ((i % 600) as f32 - 200.0, (i / 600) as f32 - 200.0);
                if x * x + y * y < 100.0 * 100.0 { [230, 180, 150, 255] } else { [40, 60, 90, 255] }
            })
            .collect()
    }

    /// Needs the model (darktable's AI preferences install it).
    #[test]
    #[ignore]
    fn a_click_or_a_box_selects_the_disc() {
        let mut sam = Sam::load().unwrap();
        let image = sam.encode(&disc(), 600, 400).unwrap();
        let at = |mask: &[f32], x: f32, y: f32| {
            mask[(y / 400.0 * MASK_SIZE as f32) as usize * MASK_SIZE + (x / 600.0 * MASK_SIZE as f32) as usize]
        };
        let painted = Prompt::Points(vec![(180.0, 200.0, true), (220.0, 200.0, true), (500.0, 200.0, false)]);
        for prompt in [Prompt::Point(200.0, 200.0), Prompt::Box(90.0, 90.0, 310.0, 310.0), painted] {
            let mask = sam.select(&image, &prompt, None).unwrap();
            let again = sam.select(&image, &prompt, Some(&mask)).unwrap();
            assert!(at(&again, 200.0, 200.0) > 0.0 && at(&again, 500.0, 200.0) < 0.0, "{prompt:?}: refined");
            assert!(at(&mask, 200.0, 200.0) > 0.0, "{prompt:?}: centre");
            assert!(at(&mask, 150.0, 250.0) > 0.0, "{prompt:?}: inside");
            assert!(at(&mask, 500.0, 200.0) < 0.0, "{prompt:?}: outside");
            assert!(at(&mask, 20.0, 20.0) < 0.0, "{prompt:?}: corner");
        }
    }
}
