//! Filter › Render › Relight: light a photo again from another direction.
//!
//! Ported from PhotoCraft's `crates/algo/src/relight.rs` (MIT OR
//! Apache-2.0, [`b70bf524`](https://github.com/storytold/photocraft/commit/b70bf524e)).
//! The photo's brightness, blurred, stands in for the shape of what's in it
//! (bright as near, dark as far), and its slopes are the surfaces' normals.
//! A lamp at `angle` and `elevation` lights those surfaces as a matte one
//! would be lit (Lambert).
//!
//! Where PhotoCraft divides each pixel by its blurred brightness and
//! multiplies in the new light, which takes every area towards the same
//! tone, here each pixel is scaled by how much more (or less) light its
//! surface gets than one facing the camera. Flat areas keep their tones.

use rayon::prelude::*;

use crate::Pixel;
use crate::adjust::luminance;
use crate::tiled::Tiled;

const MAX: f32 = 65535.0;
/// Softness maps onto this blur range, in pixels.
const SIGMA_MIN: f32 = 2.0;
const SIGMA_MAX: f32 = 96.0;
/// The slope's step is the blur's size, but never more than this.
const STEP_MAX: f32 = 32.0;
/// How steep a slope of brightness makes a surface (PhotoCraft's).
const SLOPE: f32 = 2.0;

/// Settings for Relight, in the dialog's units.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct RelightOptions {
    /// Where the light comes from, in degrees: 0 from the right, 90 from
    /// above, as Photoshop's Angle.
    pub angle: f32,
    /// How high the light is, in degrees: 0 grazing, 90 from the camera,
    /// as Photoshop's Altitude.
    pub elevation: f32,
    /// How strong the lamp is (0–100 %). 0 leaves the image as it is.
    pub intensity: f32,
    /// Light from everywhere, which fills the shadows (0–100 %).
    pub ambient: f32,
    /// How broad the shapes the light follows are (1–100 %).
    pub softness: f32,
    /// The lamp's colour, in sRGB.
    pub colour: [u8; 3],
}

impl Default for RelightOptions {
    fn default() -> Self {
        Self {
            angle: 45.0,
            elevation: 40.0,
            intensity: 40.0,
            ambient: 55.0,
            softness: 25.0,
            colour: [255, 255, 255],
        }
    }
}

impl RelightOptions {
    /// The blur's size in pixels, for an image `whole` big.
    pub fn sigma(&self, (w, h): (u32, u32)) -> f32 {
        let side = w.min(h).max(1) as f32;
        (self.softness.clamp(1.0, 100.0) / 100.0 * 0.08 * side).clamp(SIGMA_MIN, SIGMA_MAX)
    }

    /// How far a pixel's result reaches, in pixels: the blur and the slope's
    /// step.
    pub fn reach(&self, whole: (u32, u32)) -> u32 {
        let sigma = self.sigma(whole);
        (sigma * 3.0).ceil() as u32 + 1 + step(sigma) as u32
    }

    /// Unit vector towards the light, in image space (x right, y down, z
    /// towards the camera).
    fn towards_light(&self) -> [f32; 3] {
        let (s, c) = self.angle.to_radians().sin_cos();
        let e = self.elevation.clamp(0.0, 90.0).to_radians();
        [c * e.cos(), -s * e.cos(), e.sin()]
    }

    /// The lamp's colour as a filter: white is 1 in each channel, and a
    /// coloured one holds back the others.
    fn filter(&self) -> [f32; 3] {
        let top = f32::from(*self.colour.iter().max().unwrap_or(&255)).max(1.0);
        self.colour.map(|c| f32::from(c) / top)
    }
}

fn step(sigma: f32) -> usize {
    sigma.round().clamp(1.0, STEP_MAX) as usize
}

/// `image` lit again with `options`, as if it were all of an image `whole`
/// big (so a part of it, previewed, comes out as it would in the whole).
/// Alpha is kept.
pub fn relight(image: &Tiled<Pixel>, options: &RelightOptions, whole: (u32, u32)) -> Tiled<Pixel> {
    let (w, h) = (image.width() as usize, image.height() as usize);
    if w == 0 || h == 0 || options.intensity.is_nan() || options.intensity <= 0.0 {
        return image.clone();
    }
    let pixels = image.to_vec();
    let luma: Vec<f32> = pixels.par_iter().map(|p| luminance([p[0], p[1], p[2]].map(|v| f32::from(v) / MAX))).collect();
    let sigma = options.sigma(whole);
    let shape = crate::denoise::blur(&luma, w, h, sigma);
    let d = step(sigma) as isize;
    let at = |x: isize, y: isize| shape[y.clamp(0, h as isize - 1) as usize * w + x.clamp(0, w as isize - 1) as usize];

    let light = options.towards_light();
    let filter = options.filter();
    let ambient = options.ambient.clamp(0.0, 100.0) / 100.0;
    let lamp = options.intensity.clamp(0.0, 100.0) / 100.0;
    // What a surface facing the camera gets, which keeps its tone.
    let facing = (ambient + lamp * light[2]).max(1e-3);
    let out: Vec<Pixel> = pixels
        .par_iter()
        .enumerate()
        .map(|(i, p)| {
            let (x, y) = ((i % w) as isize, (i / w) as isize);
            let nx = (at(x - d, y) - at(x + d, y)) * SLOPE;
            let ny = (at(x, y - d) - at(x, y + d)) * SLOPE;
            let len = (nx * nx + ny * ny + 1.0).sqrt();
            let lit = ((nx * light[0] + ny * light[1] + light[2]) / len).max(0.0);
            let c = |k: usize| {
                let gain = (ambient + lamp * lit * filter[k]) / facing;
                (f32::from(p[k]) * gain).round().clamp(0.0, MAX) as u16
            };
            [c(0), c(1), c(2), p[3]]
        })
        .collect();
    Tiled::from_slice(image.width(), image.height(), [0; 4], &out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A grey dome, brightest in the middle, on a darker grey.
    fn dome(size: u32) -> Tiled<Pixel> {
        let c = size as f32 / 2.0;
        let px: Vec<Pixel> = (0..size * size)
            .map(|i| {
                let (x, y) = ((i % size) as f32 - c, (i / size) as f32 - c);
                let t = (1.0 - (x * x + y * y).sqrt() / (c * 0.8)).clamp(0.0, 1.0);
                let v = (20000.0 + 30000.0 * t) as u16;
                [v, v, v, u16::MAX]
            })
            .collect();
        Tiled::from_slice(size, size, [0; 4], &px)
    }

    fn mean(img: &Tiled<Pixel>, x0: u32, y0: u32, x1: u32, y1: u32, k: usize) -> f32 {
        let mut sum = 0.0;
        for y in y0..y1 {
            for x in x0..x1 {
                sum += f32::from(img.get(x, y)[k]);
            }
        }
        sum / ((x1 - x0) * (y1 - y0)) as f32
    }

    #[test]
    fn light_from_the_left_brightens_the_left_and_darkens_the_right() {
        let img = dome(256);
        let options = RelightOptions {
            angle: 180.0,
            elevation: 30.0,
            intensity: 80.0,
            ambient: 30.0,
            ..Default::default()
        };
        let out = relight(&img, &options, (256, 256));
        let (left, right) = ((60, 120, 90, 136), (166, 120, 196, 136));
        let before = mean(&img, left.0, left.1, left.2, left.3, 0);
        assert!(mean(&out, left.0, left.1, left.2, left.3, 0) > before * 1.05, "the side facing the light");
        assert!(mean(&out, right.0, right.1, right.2, right.3, 0) < before * 0.95, "the side away from it");
        // From the right, the other way round.
        let out = relight(&img, &RelightOptions { angle: 0.0, ..options }, (256, 256));
        assert!(mean(&out, right.0, right.1, right.2, right.3, 0) > mean(&out, left.0, left.1, left.2, left.3, 0));
    }

    #[test]
    fn flat_areas_keep_their_tone_and_none_does_nothing() {
        let flat = Tiled::new(128, 128, [12000, 30000, 50000, 40000]);
        let out = relight(&flat, &RelightOptions::default(), (128, 128));
        assert_eq!(out.get(64, 64), [12000, 30000, 50000, 40000]);
        let img = dome(128);
        let none = RelightOptions {
            intensity: 0.0,
            ..Default::default()
        };
        assert_eq!(relight(&img, &none, (128, 128)).to_vec(), img.to_vec());
    }

    #[test]
    fn a_part_previewed_comes_out_as_in_the_whole() {
        let img = dome(400);
        let filter = crate::filters::LayerFilter::Relight(RelightOptions {
            softness: 60.0,
            ..Default::default()
        });
        let whole = filter.apply(&img);
        let part = filter.preview(&img, 150, 120, 60, 40);
        let want = whole.crop(150, 120, 60, 40);
        // The blur's running sums round a little differently from another start.
        let worst = part
            .iter()
            .zip(&want)
            .map(|(a, b)| (0..4).map(|k| a[k].abs_diff(b[k])).max().unwrap_or(0))
            .max();
        assert!(worst <= Some(1), "{worst:?} levels off");
    }

    #[test]
    fn a_coloured_lamp_tints_what_it_lights() {
        let img = dome(256);
        let warm = RelightOptions {
            angle: 180.0,
            colour: [255, 160, 80],
            ..Default::default()
        };
        let out = relight(&img, &warm, (256, 256));
        let lit = out.get(75, 128);
        assert!(lit[0] > lit[1] && lit[1] > lit[2], "orange where it's lit: {lit:?}");
    }
}

#[cfg(test)]
mod timing {
    use super::*;

    /// Relight a photo from the left and the right, for looking at:
    /// `OMAPIX_RELIGHT=photo.tif cargo test --release -p omapix-engine relight_a_photo -- --ignored`
    /// writes `target/relight/{before,left,right}.jpg`.
    #[test]
    #[ignore]
    fn relight_a_photo() {
        let Ok(path) = std::env::var("OMAPIX_RELIGHT") else {
            return;
        };
        let mut doc = crate::io::load(std::path::Path::new(&path)).expect("the photo");
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/relight");
        std::fs::create_dir_all(&out).unwrap();
        let before = doc.layers[0].pixels.clone();
        crate::export::jpeg(&doc, &out.join("before.jpg"), 85).unwrap();
        for (name, angle) in [("left", 180.0), ("right", 0.0)] {
            let options = RelightOptions { angle, ..Default::default() };
            doc.layers[0].pixels = relight(&before, &options, (doc.width, doc.height));
            crate::export::jpeg(&doc, &out.join(format!("{name}.jpg")), 85).unwrap();
        }
    }

    /// `cargo test --release -p omapix-engine relight_at_24_mp -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn relight_at_24_mp() {
        let (w, h) = (6000, 4000);
        let px: Vec<Pixel> = (0..w * h).map(|i| [(i % 65536) as u16, ((i / 7) % 65536) as u16, 30000, u16::MAX]).collect();
        let img = Tiled::from_slice(w, h, [0; 4], &px);
        for softness in [25.0, 100.0] {
            let start = std::time::Instant::now();
            let out = relight(
                &img,
                &RelightOptions {
                    softness,
                    ..Default::default()
                },
                (w, h),
            );
            eprintln!("softness {softness}: {:.2} s", start.elapsed().as_secs_f32());
            assert_eq!(out.width(), w);
        }
    }
}
