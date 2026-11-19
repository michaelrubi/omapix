//! Filter › Render › Relight: light a photo again with a lamp placed on it.
//!
//! Ported from PhotoCraft's `crates/algo/src/relight.rs` (MIT OR
//! Apache-2.0, [`b70bf524`](https://github.com/storytold/photocraft/commit/b70bf524e)).
//! The photo's brightness, blurred, stands in for the shape of what's in it
//! (bright as near, dark as far), and its slopes are the surfaces' normals.
//! A lamp hanging over the photo at (`x`, `y`), `reach` above it, lights
//! those surfaces as a matte one would be lit (Lambert), less with distance
//! (inverse square), as Photoshop's Lighting Effects' point light.
//!
//! Where PhotoCraft divides each pixel by its blurred brightness and
//! multiplies in the new light, which takes every area towards the same
//! tone, here each pixel is scaled by how much more (or less) light its
//! surface gets than a flat one right under the lamp, which keeps its tone.

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
    /// Where the lamp is, across the image (0 its left edge, 1 its right);
    /// it can be off the image.
    pub x: f32,
    /// Where the lamp is, down the image (0 its top, 1 its bottom).
    pub y: f32,
    /// How high over the image the lamp hangs, which is how far its light
    /// reaches, as a percentage of the image's shorter side.
    pub reach: f32,
    /// How much light the lamp adds right under it, as a percentage of the
    /// light the photo had (0–100 %).
    pub intensity: f32,
    /// How much of the photo's own light is kept, which fills the shadows
    /// (0–100 %). 100 % with no Intensity leaves the image as it is.
    pub ambient: f32,
    /// How broad the shapes the light follows are (1–100 %).
    pub softness: f32,
    /// The lamp's colour, in sRGB.
    pub colour: [u8; 3],
}

impl Default for RelightOptions {
    fn default() -> Self {
        Self {
            // A key light up and to the left.
            x: 0.25,
            y: 0.2,
            reach: 75.0,
            intensity: 45.0,
            ambient: 70.0,
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
    pub fn margin(&self, whole: (u32, u32)) -> u32 {
        let sigma = self.sigma(whole);
        (sigma * 3.0).ceil() as u32 + 1 + step(sigma) as u32
    }

    /// Where the lamp is, in an image `whole` big: x right and y down, in
    /// pixels, and how high over it.
    pub fn lamp(&self, (w, h): (u32, u32)) -> [f32; 3] {
        let side = w.min(h).max(1) as f32;
        [self.x * w as f32, self.y * h as f32, (self.reach / 100.0 * side).max(1.0)]
    }

    /// The lamp's colour as a filter: white is 1 in each channel, and a
    /// coloured one holds back the others.
    fn filter(&self) -> [f32; 3] {
        let top = f32::from(*self.colour.iter().max().unwrap_or(&255)).max(1.0);
        self.colour.map(|c| f32::from(c) / top)
    }
}

/// Each 16-bit value, encoded with sRGB's curve, as linear light (0–1).
fn linear_table() -> Vec<f32> {
    (0..=u16::MAX)
        .map(|v| {
            let v = f32::from(v) / MAX;
            if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        })
        .collect()
}

/// `v` (linear) times `gain`, where light added eases into white rather
/// than clipping, as film's shoulder does.
fn lighten(v: f32, gain: f32) -> f32 {
    if gain <= 1.0 || v >= 1.0 {
        return v * gain;
    }
    let room = 1.0 - v;
    v + room * (1.0 - (-(v * (gain - 1.0)) / room).exp())
}

/// Linear light encoded with sRGB's curve as 16-bit values, in 65536
/// steps of its square root, so the shadows keep their precision.
fn encode_table() -> Vec<u16> {
    (0..=u16::MAX)
        .map(|v| {
            let root = f32::from(v) / MAX;
            (from_linear(root * root) * MAX).round() as u16
        })
        .collect()
}

/// Linear light `v` as a 16-bit value encoded with sRGB's curve.
fn encode_lookup(table: &[u16], v: f32) -> u16 {
    table[(v.max(0.0).sqrt() * MAX).round().min(MAX) as usize]
}

/// Linear light (0–1 and above) encoded with sRGB's curve.
fn from_linear(v: f32) -> f32 {
    if v <= 0.0031308 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}

fn step(sigma: f32) -> usize {
    sigma.round().clamp(1.0, STEP_MAX) as usize
}

/// `image` lit again with `options`, as if it were the part at `origin` of
/// an image `whole` big (so a part, previewed, comes out as it would in
/// the whole). Alpha is kept.
pub fn relight(image: &Tiled<Pixel>, options: &RelightOptions, origin: (u32, u32), whole: (u32, u32)) -> Tiled<Pixel> {
    let (w, h) = (image.width() as usize, image.height() as usize);
    if w == 0 || h == 0 || options.intensity.is_nan() || options.ambient.is_nan() {
        return image.clone();
    }
    let pixels = image.to_vec();
    let luma: Vec<f32> = pixels.par_iter().map(|p| luminance([p[0], p[1], p[2]].map(|v| f32::from(v) / MAX))).collect();
    let sigma = options.sigma(whole);
    let shape = crate::denoise::blur(&luma, w, h, sigma);
    let d = step(sigma) as isize;
    let at = |x: isize, y: isize| shape[y.clamp(0, h as isize - 1) as usize * w + x.clamp(0, w as isize - 1) as usize];

    let [lx, ly, lz] = options.lamp(whole);
    let (ox, oy) = (origin.0 as f32, origin.1 as f32);
    let filter = options.filter();
    let ambient = options.ambient.clamp(0.0, 100.0) / 100.0;
    let lamp = options.intensity.clamp(0.0, 100.0) / 100.0;
    let decode = linear_table();
    let encode = encode_table();
    let out: Vec<Pixel> = pixels
        .par_iter()
        .enumerate()
        .map(|(i, p)| {
            let (x, y) = ((i % w) as isize, (i / w) as isize);
            let nx = (at(x - d, y) - at(x + d, y)) * SLOPE;
            let ny = (at(x, y - d) - at(x, y + d)) * SLOPE;
            let len = (nx * nx + ny * ny + 1.0).sqrt();
            // Towards the lamp, and its light falling off with the square of
            // the distance: 1 on a flat surface right under it.
            let (vx, vy) = (lx - ox - x as f32, ly - oy - y as f32);
            let dist = (vx * vx + vy * vy + lz * lz).sqrt();
            let facing = ((nx * vx + ny * vy + lz) / (len * dist)).max(0.0);
            let lit = facing * lz * lz / (dist * dist);
            // Light adds up in linear values, not the encoded ones.
            let c = |k: usize| {
                let gain = ambient + lamp * lit * filter[k];
                encode_lookup(&encode, lighten(decode[p[k] as usize], gain))
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

    /// A lamp to the left of the dome, low enough to rake it.
    fn from_the_left() -> RelightOptions {
        RelightOptions {
            x: 0.15,
            y: 0.5,
            reach: 8.0,
            intensity: 80.0,
            ambient: 5.0,
            ..Default::default()
        }
    }

    #[test]
    fn a_lamp_to_the_left_brightens_the_left_and_darkens_the_right() {
        let img = dome(256);
        let out = relight(&img, &from_the_left(), (0, 0), (256, 256));
        // How much the light changes each side, in linear light, against a
        // flat surface there.
        let flat_in = Tiled::new(256, 256, [30000, 30000, 30000, u16::MAX]);
        let flat = relight(&flat_in, &from_the_left(), (0, 0), (256, 256));
        let lin = linear_table();
        let light = |a: &Tiled<Pixel>, b: &Tiled<Pixel>, (x0, y0, x1, y1): (u32, u32, u32, u32)| {
            let mut sum = 0.0;
            for y in y0..y1 {
                for x in x0..x1 {
                    sum += lin[a.get(x, y)[0] as usize] / lin[b.get(x, y)[0] as usize];
                }
            }
            sum / ((x1 - x0) * (y1 - y0)) as f32
        };
        let gain = |r| light(&out, &img, r) / light(&flat, &flat_in, r);
        let (left, right) = ((60, 120, 90, 136), (166, 120, 196, 136));
        assert!(gain(left) > 1.05, "the side facing the lamp: {}", gain(left));
        assert!(gain(right) < 1.0, "the side away from it: {}", gain(right));
        // To the right, the other way round.
        let out = relight(&img, &RelightOptions { x: 0.85, ..from_the_left() }, (0, 0), (256, 256));
        assert!(mean(&out, right.0, right.1, right.2, right.3, 0) > mean(&out, left.0, left.1, left.2, left.3, 0));
    }

    #[test]
    fn flat_under_the_lamp_keeps_its_tone_and_falls_off_away_from_it() {
        let colour = [12000, 30000, 50000, 40000];
        let flat = Tiled::new(400, 400, colour);
        let options = RelightOptions {
            x: 0.25,
            y: 0.25,
            reach: 25.0,
            intensity: 35.0,
            ambient: 65.0,
            ..Default::default()
        };
        let out = relight(&flat, &options, (0, 0), (400, 400));
        let under = out.get(100, 100);
        assert!((0..4).all(|k| under[k].abs_diff(colour[k]) <= 3), "right under it: {under:?}");
        let (near, far) = (out.get(150, 100)[2], out.get(399, 399)[2]);
        assert!(near < 50000 && far < near, "darker further away: {near}, {far}");
        // Out of reach, only the ambient light is left.
        let ambient = options.ambient / 100.0;
        let lin = linear_table();
        assert!((lin[far as usize] / lin[50000] - ambient).abs() < 0.03, "{far}");
        assert_eq!(out.get(399, 399)[3], 40000, "alpha kept");

        let img = dome(128);
        let none = RelightOptions {
            intensity: 0.0,
            ambient: 100.0,
            ..Default::default()
        };
        let same = relight(&img, &none, (0, 0), (128, 128));
        assert!(same.to_vec().iter().zip(img.to_vec()).all(|(a, b)| (0..4).all(|k| a[k].abs_diff(b[k]) <= 3)));
    }

    #[test]
    fn a_part_previewed_comes_out_as_in_the_whole() {
        let img = dome(400);
        let filter = crate::filters::LayerFilter::Relight(RelightOptions {
            softness: 60.0,
            ..from_the_left()
        });
        let whole = filter.apply(&img);
        let part = filter.preview(&img, 150, 120, 60, 40);
        let want = whole.crop(150, 120, 60, 40);
        // The blur's running sums round a little differently from another
        // start, and the linear tables round too.
        let worst = part
            .iter()
            .zip(&want)
            .map(|(a, b)| (0..4).map(|k| a[k].abs_diff(b[k])).max().unwrap_or(0))
            .max();
        assert!(worst <= Some(3), "{worst:?} levels off");
    }

    #[test]
    fn added_light_eases_into_white() {
        let bright = Tiled::new(64, 64, [60000, 60000, 60000, u16::MAX]);
        let options = RelightOptions {
            x: 0.5,
            y: 0.5,
            intensity: 100.0,
            ambient: 100.0,
            ..Default::default()
        };
        let v = relight(&bright, &options, (0, 0), (64, 64)).get(32, 32)[0];
        assert!(v > 60000 && v < u16::MAX, "brighter but short of white: {v}");
        // Dimmer, it simply scales.
        assert_eq!(lighten(0.2, 0.5), 0.1);
        assert!((lighten(0.01, 2.0) - 0.02).abs() < 1e-3, "barely bent in the shadows");
    }

    #[test]
    fn a_coloured_lamp_tints_what_it_lights() {
        let img = dome(256);
        let warm = RelightOptions {
            colour: [255, 160, 80],
            ..from_the_left()
        };
        let out = relight(&img, &warm, (0, 0), (256, 256));
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
        for (name, x) in [("left", 0.1), ("right", 0.9)] {
            let options = RelightOptions { x, ..Default::default() };
            doc.layers[0].pixels = relight(&before, &options, (0, 0), (doc.width, doc.height));
            crate::export::jpeg(&doc, &out.join(format!("{name}.jpg")), 85).unwrap();
        }
    }

    /// Relight a photo with lamps given as `name:x,y,reach,intensity,ambient`
    /// separated by `;`, for comparing versions by eye:
    /// `OMAPIX_RELIGHT=photo.jpg OMAPIX_RELIGHT_LAMPS="left:0.4,0.25,40,50,50"
    /// OMAPIX_RELIGHT_OUT=dir cargo test --release -p omapix-engine relight_lamps -- --ignored`
    #[test]
    #[ignore]
    fn relight_lamps() {
        let (Ok(path), Ok(lamps), Ok(out)) = (
            std::env::var("OMAPIX_RELIGHT"),
            std::env::var("OMAPIX_RELIGHT_LAMPS"),
            std::env::var("OMAPIX_RELIGHT_OUT"),
        ) else {
            return;
        };
        let out = std::path::Path::new(&out);
        std::fs::create_dir_all(out).unwrap();
        let mut doc = crate::io::load(std::path::Path::new(&path)).expect("the photo");
        let before = doc.layers[0].pixels.clone();
        for lamp in lamps.split(';') {
            let (name, values) = lamp.split_once(':').unwrap();
            let v: Vec<f32> = values.split(',').map(|v| v.trim().parse().unwrap()).collect();
            let options = RelightOptions {
                x: v[0],
                y: v[1],
                reach: v[2],
                intensity: v[3],
                ambient: v[4],
                ..Default::default()
            };
            doc.layers[0].pixels = relight(&before, &options, (0, 0), (doc.width, doc.height));
            crate::export::jpeg(&doc, &out.join(format!("{name}.jpg")), 90).unwrap();
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
                (0, 0),
                (w, h),
            );
            eprintln!("softness {softness}: {:.2} s", start.elapsed().as_secs_f32());
            assert_eq!(out.width(), w);
        }
    }
}
