//! RealPLKSR (Dongheon Lee et al., MIT), which darktable installs as
//! `upscale-realplksr`: Image › Image Size's AI enlargement. It sees a
//! square and answers one twice or four times the size; the engine's
//! `upscale` module cuts the image into them and puts the answers back
//! together.

use ort::session::Session;
use ort::value::Tensor;

use crate::Result;
use crate::find_model;
use crate::runtime::session;

/// The model's id: its folder in darktable's models.
pub const MODEL: &str = "upscale-realplksr";

pub struct Upscaler {
    session: Session,
    input: String,
    /// How many times larger its answer is: 2 or 4.
    pub factor: usize,
    /// The square it sees is this many pixels across.
    pub size: usize,
}

impl Upscaler {
    /// The model that enlarges by `factor`: 2 or 4.
    pub fn load(factor: usize) -> Result<Self> {
        let files = find_model(MODEL)
            .ok_or_else(|| format!("AI upscaling needs the {MODEL} model, which darktable installs"))?;
        let (file, size) = if factor == 2 { ("model_x2.onnx", 512) } else { ("model_x4.onnx", 256) };
        let session = session(&files[file])?;
        let input = session.inputs()[0].name().to_owned();
        Ok(Self { session, input, factor, size })
    }

    /// `square` (sRGB 0–1, red, green then blue planes, `size`²) enlarged
    /// by `factor`, in the same form.
    pub fn upscale(&mut self, square: &[f32]) -> Result<Vec<f32>> {
        let side = self.size as i64;
        let input = Tensor::from_array((vec![1, 3, side, side], square.to_vec())).map_err(|e| e.to_string())?;
        let outputs = self.session.run(ort::inputs![self.input.as_str() => input]).map_err(|e| e.to_string())?;
        let (_, enlarged) = outputs[0].try_extract_tensor::<f32>().map_err(|e| e.to_string())?;
        Ok(enlarged.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs the model (darktable installs it). How well it enlarges is
    /// for real photos: see examples/upscale.rs.
    #[test]
    #[ignore]
    fn a_gradient_comes_back_larger_and_still_a_gradient() {
        for factor in [2, 4] {
            let mut model = Upscaler::load(factor).unwrap();
            let (size, plane) = (model.size, model.size * model.size);
            let square: Vec<f32> = (0..3 * plane).map(|i| 0.2 + 0.6 * (i % size) as f32 / size as f32).collect();
            let out = model.upscale(&square).unwrap();
            let side = size * factor;
            assert_eq!(out.len(), 3 * side * side);
            // Away from the edges, each pixel is what the gradient is there.
            for x in (side / 8..side * 7 / 8).step_by(side / 16) {
                let v = out[side * side / 2 + x];
                let want = 0.2 + 0.6 * (x as f32 + 0.5) / side as f32;
                assert!((v - want).abs() < 0.01, "×{factor} at {x}: {v} {want}");
            }
        }
    }
}
