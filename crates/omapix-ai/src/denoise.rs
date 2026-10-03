//! NIND (Benoit Brummer's UNet trained on the Natural Image Noise Dataset,
//! GPL-3.0), which darktable installs as `denoise-nind`: Filter › Noise ›
//! Denoise. It sees a 768 × 768 square; the engine's `denoise` module
//! cuts the image into them and puts the answers back together.

use ort::session::Session;
use ort::value::Tensor;

use crate::Result;
use crate::find_model;
use crate::runtime::session;

/// The model's id: its folder in darktable's models.
pub const MODEL: &str = "denoise-nind";
/// The square it sees is this many pixels across.
pub const SIZE: usize = 768;

pub struct Nind {
    session: Session,
}

impl Nind {
    pub fn load() -> Result<Self> {
        let files = find_model(MODEL)
            .ok_or_else(|| format!("Denoise needs the {MODEL} model, which darktable installs"))?;
        Ok(Self {
            session: session(&files["model.onnx"])?,
        })
    }

    /// `square` (sRGB 0–1, red, green then blue planes, [`SIZE`]²)
    /// denoised, in the same form.
    pub fn denoise(&mut self, square: &[f32]) -> Result<Vec<f32>> {
        let side = SIZE as i64;
        let input = Tensor::from_array((vec![1, 3, side, side], square.to_vec())).map_err(|e| e.to_string())?;
        let outputs = self.session.run(ort::inputs!["input" => input]).map_err(|e| e.to_string())?;
        let (_, denoised) = outputs[0].try_extract_tensor::<f32>().map_err(|e| e.to_string())?;
        Ok(denoised.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs the model (darktable installs it). How well it denoises is
    /// for real photos: see examples/denoise.rs.
    #[test]
    #[ignore]
    fn noise_on_grey_is_smoothed() {
        let mut nind = Nind::load().unwrap();
        let plane = SIZE * SIZE;
        let noisy: Vec<f32> = (0..3 * plane)
            .map(|i| 0.5 + ((i as u32).wrapping_mul(2654435761) >> 16) as f32 / 65536.0 * 0.2 - 0.1)
            .collect();
        let out = nind.denoise(&noisy).unwrap();
        assert_eq!(out.len(), 3 * plane);
        let spread = |v: &[f32]| (1..plane).map(|i| (v[i] - v[i - 1]).abs()).sum::<f32>() / plane as f32;
        let mean = out.iter().sum::<f32>() / out.len() as f32;
        assert!((mean - 0.5).abs() < 0.03, "{mean}");
        assert!(spread(&out) < spread(&noisy) / 3.0, "{} {}", spread(&out), spread(&noisy));
    }
}

