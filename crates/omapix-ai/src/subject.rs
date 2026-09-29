//! BiRefNet portrait (MIT), which mattes the person in a photo: Select ›
//! Subject. It sees the image stretched to a 1024 px square and answers
//! how much of each pixel is the person, 0–1, soft along hair.

use ort::session::Session;
use ort::value::Tensor;

use crate::Result;
use crate::find_model;
use crate::runtime::{imagenet_input, session};

/// The model's id: its folder in Omapix's models (scripts/fetch-models.sh).
pub const MODEL: &str = "mask-subject-birefnet-portrait";
/// It sees, and answers, this many pixels across.
pub const SIZE: usize = 1024;

pub struct Subject {
    session: Session,
}

impl Subject {
    pub fn load() -> Result<Self> {
        let files = find_model(MODEL)
            .ok_or_else(|| format!("Select Subject needs the {MODEL} model: run scripts/fetch-models.sh"))?;
        Ok(Self {
            session: session(&files["model_fp16.onnx"])?,
        })
    }

    /// The person in an image given as 8-bit sRGB, `width` × `height`, as
    /// [`SIZE`]² coverage, 0–1, stretched over the whole image.
    pub fn matte(&mut self, srgb: &[[u8; 4]], width: u32, height: u32) -> Result<Vec<f32>> {
        let side = SIZE as i64;
        let image = Tensor::from_array((vec![1, 3, side, side], imagenet_input(srgb, width, height, SIZE)))
            .map_err(|e| e.to_string())?;
        let outputs = self.session.run(ort::inputs!["input_image" => image]).map_err(|e| e.to_string())?;
        // It answers in logits.
        let (_, logits) = outputs[0].try_extract_tensor::<f32>().map_err(|e| e.to_string())?;
        Ok(logits.iter().map(|v| 1.0 / (1.0 + (-v).exp())).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs the model (scripts/fetch-models.sh). How well it finds
    /// people is for real photos: see examples/subject.rs.
    #[test]
    #[ignore]
    fn a_bare_wall_has_no_subject() {
        let (w, h) = (600u32, 800u32);
        let matte = Subject::load().unwrap().matte(&vec![[225, 225, 220, 255]; (w * h) as usize], w, h).unwrap();
        assert_eq!(matte.len(), SIZE * SIZE);
        assert!(matte.iter().all(|v| (0.0..=1.0).contains(v)));
        let mean = matte.iter().sum::<f32>() / matte.len() as f32;
        assert!(mean < 0.1, "{mean}");
    }
}
