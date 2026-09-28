//! LaMa (big-lama, Apache-2.0), which fills a hole from its surroundings:
//! Edit › Content-Aware Fill. It sees a 512 × 512 patch; the engine's
//! `fill` module cuts the patch out and puts the answer back.

use ort::session::Session;
use ort::value::Tensor;

use crate::Result;
use crate::runtime::{find_model, session};

/// The model's id: its folder in Omapix's models (scripts/fetch-models.sh).
pub const MODEL: &str = "inpaint-lama";
/// The patch it sees is this many pixels across.
pub const SIZE: usize = 512;

pub struct Lama {
    session: Session,
}

impl Lama {
    pub fn load() -> Result<Self> {
        let dir = find_model(MODEL)
            .ok_or_else(|| format!("Content-Aware Fill needs the {MODEL} model: run scripts/fetch-models.sh"))?;
        Ok(Self {
            session: session(&dir.join("lama_fp32.onnx"))?,
        })
    }

    /// `image` (sRGB 0–1, red, green then blue planes, [`SIZE`]²) with
    /// `mask`'s 1s filled in, in the same form.
    pub fn fill(&mut self, image: &[f32], mask: &[f32]) -> Result<Vec<f32>> {
        let side = SIZE as i64;
        let tensor = |channels: i64, data: &[f32]| {
            Tensor::from_array((vec![1, channels, side, side], data.to_vec())).map_err(|e| e.to_string())
        };
        let outputs = self
            .session
            .run(ort::inputs!["image" => tensor(3, image)?, "mask" => tensor(1, mask)?])
            .map_err(|e| e.to_string())?;
        let (_, filled) = outputs[0].try_extract_tensor::<f32>().map_err(|e| e.to_string())?;
        // It answers in 0–255.
        Ok(filled.iter().map(|v| v / 255.0).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs the model (scripts/fetch-models.sh).
    #[test]
    #[ignore]
    fn a_hole_in_a_gradient_is_filled_with_the_gradient() {
        let mut lama = Lama::load().unwrap();
        let plane = SIZE * SIZE;
        // Left to right from dark to light, in all three channels.
        let image: Vec<f32> = (0..3 * plane).map(|i| (i % SIZE) as f32 / SIZE as f32).collect();
        // A black square hole in the middle, to fill.
        let (lo, hi) = (SIZE / 2 - 64, SIZE / 2 + 64);
        let hole = |i: usize| (lo..hi).contains(&(i % SIZE)) && (lo..hi).contains(&(i / SIZE % SIZE));
        let holed: Vec<f32> = image.iter().enumerate().map(|(i, &v)| if hole(i) { 0.0 } else { v }).collect();
        let mask: Vec<f32> = (0..plane).map(|i| if hole(i) { 1.0 } else { 0.0 }).collect();
        let filled = lama.fill(&holed, &mask).unwrap();
        assert_eq!(filled.len(), 3 * plane);
        let mid = SIZE / 2 * SIZE + SIZE / 2;
        assert!((filled[mid] - 0.5).abs() < 0.1, "{}", filled[mid]);
        assert!((filled[mid + 40] - filled[mid - 40]) > 0.05, "still a gradient");
    }

    /// A look at a real photo: `OMAPIX_FILL_PHOTO` with an ellipse at
    /// `OMAPIX_FILL_ELLIPSE` ("x0,y0,x1,y1") filled, before and after, as
    /// PNGs in `OMAPIX_FILL_OUT`.
    #[test]
    #[ignore]
    fn fill_a_photo() {
        use omapix_engine::{fill, selection::Selection};
        let (Ok(photo), Ok(ellipse), Ok(out)) = (
            std::env::var("OMAPIX_FILL_PHOTO"),
            std::env::var("OMAPIX_FILL_ELLIPSE"),
            std::env::var("OMAPIX_FILL_OUT"),
        ) else {
            return;
        };
        let e: Vec<f32> = ellipse.split(',').map(|v| v.parse().unwrap()).collect();
        let doc = omapix_engine::io::load(std::path::Path::new(&photo)).unwrap();
        let visible = doc.composite();
        let selection = Selection::ellipse(doc.width, doc.height, (e[0], e[1]), (e[2], e[3])).feather(2.0);
        let mut lama = Lama::load().unwrap();
        lama.fill(&vec![0.5; 3 * SIZE * SIZE], &vec![0.0; SIZE * SIZE]).unwrap();
        let started = std::time::Instant::now();
        let patch = fill::patch(&visible, &doc.profile, &selection, SIZE).unwrap().unwrap();
        let filled = lama.fill(&patch.image, &patch.mask).unwrap();
        let layer = fill::layer(1, "fill", &patch, &filled, &doc.profile, &selection).unwrap();
        eprintln!("patch {:?}, filled in {:?} once loaded", patch.rect, started.elapsed());
        let [x, y, w, h] = patch.rect;
        let save = |name: &str, filled: bool| {
            let mut png = image::RgbImage::new(w, h);
            for (px, py, p) in png.enumerate_pixels_mut() {
                let (ix, iy) = (x + px, y + py);
                let mut v = visible.get(ix, iy);
                if filled {
                    let a = f32::from(layer.mask.as_ref().unwrap().pixels.get(ix, iy)) / 65535.0;
                    let f = layer.pixels.get(ix, iy);
                    v = [0, 1, 2, 3].map(|c| (f32::from(v[c]) * (1.0 - a) + f32::from(f[c]) * a) as u16);
                }
                *p = image::Rgb([0, 1, 2].map(|c| (v[c] >> 8) as u8));
            }
            png.save(format!("{out}/{name}.png")).unwrap();
        };
        save("before", false);
        save("after", true);
    }
}
