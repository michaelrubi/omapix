//! Pixel filters.

use rayon::prelude::*;

use crate::Pixel;
use crate::tiled::Tiled;

const MAX: f32 = u16::MAX as f32;

/// Gaussian blur, in the style of Photoshop's Filter › Blur › Gaussian Blur
/// where the radius is the standard deviation in pixels.
///
/// Approximated with three successive box blurs, which is visually
/// indistinguishable from a true Gaussian and runs in constant time per
/// pixel whatever the radius. Colour is blurred premultiplied by alpha so
/// transparent areas don't bleed dark fringes into the result.
pub fn gaussian_blur(image: &Tiled<Pixel>, radius: f32) -> Tiled<Pixel> {
    let (w, h) = (image.width() as usize, image.height() as usize);
    if radius < 0.1 || w == 0 || h == 0 {
        return image.clone();
    }
    let buf: Vec<[f32; 4]> = image
        .to_vec()
        .into_par_iter()
        .map(|p| {
            let a = f32::from(p[3]) / MAX;
            [
                f32::from(p[0]) * a,
                f32::from(p[1]) * a,
                f32::from(p[2]) * a,
                f32::from(p[3]),
            ]
        })
        .collect();

    let buf = blur_buffer(buf, w, h, radius);

    let pixels: Vec<Pixel> = buf
        .into_par_iter()
        .map(|[r, g, b, a]| {
            if a <= 0.5 {
                return [0; 4];
            }
            let k = MAX / a;
            let c = |v: f32| (v * k).round().clamp(0.0, MAX) as u16;
            [c(r), c(g), c(b), a.round().clamp(0.0, MAX) as u16]
        })
        .collect();
    Tiled::from_slice(image.width(), image.height(), [0; 4], &pixels)
}

/// Photoshop's Filter › Other › High Pass: the detail a Gaussian blur of
/// `radius` removes, around 50 % grey, so flat areas come out mid grey.
/// Alpha is kept.
pub fn high_pass(image: &Tiled<Pixel>, radius: f32) -> Tiled<Pixel> {
    let blurred = gaussian_blur(image, radius).to_vec();
    let pixels: Vec<Pixel> = image
        .to_vec()
        .into_par_iter()
        .zip(blurred)
        .map(|(p, b)| {
            let c = |i: usize| (i32::from(p[i]) - i32::from(b[i]) + 32768).clamp(0, 65535) as u16;
            [c(0), c(1), c(2), p[3]]
        })
        .collect();
    Tiled::from_slice(image.width(), image.height(), [0; 4], &pixels)
}

/// A filter from the Filter menu, applied to one layer's pixels, with its
/// settings (so it can be previewed live, then applied).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LayerFilter {
    GaussianBlur { radius: f32 },
    HighPass { radius: f32 },
    /// `amount` 1 = 100 %; `threshold` in levels (0–255), as in Photoshop.
    UnsharpMask { amount: f32, radius: f32, threshold: f32 },
    /// Noise added straight to the pixels (Omapix otherwise puts grain on
    /// its own layer); used on masks.
    AddNoise(NoiseOptions),
}

impl LayerFilter {
    pub fn name(&self) -> &'static str {
        match self {
            Self::GaussianBlur { .. } => "Gaussian Blur",
            Self::HighPass { .. } => "High Pass",
            Self::UnsharpMask { .. } => "Unsharp Mask",
            Self::AddNoise(_) => "Add Noise",
        }
    }

    pub fn apply(&self, image: &Tiled<Pixel>) -> Tiled<Pixel> {
        match *self {
            Self::GaussianBlur { radius } => gaussian_blur(image, radius),
            Self::HighPass { radius } => high_pass(image, radius),
            Self::UnsharpMask {
                amount,
                radius,
                threshold,
            } => unsharp_mask(image, amount, radius, threshold),
            Self::AddNoise(options) => add_noise(image, &options),
        }
    }
}

/// Photoshop's Add Noise applied to the pixels themselves: each moves by
/// the grain's offset from mid grey (see [`generate_grain`]), scaled by
/// `options.amount`. Alpha is kept.
pub fn add_noise(image: &Tiled<Pixel>, options: &NoiseOptions) -> Tiled<Pixel> {
    let (w, h) = (image.width(), image.height());
    let grain = generate_grain(w, h, options, Some(image)).to_vec();
    let k = options.amount / 100.0;
    let out: Vec<Pixel> = image
        .to_vec()
        .into_par_iter()
        .zip(grain)
        .map(|(p, g)| {
            let c = |i: usize| (f32::from(p[i]) + (f32::from(g[i]) - 32768.0) * k).round().clamp(0.0, MAX) as u16;
            [c(0), c(1), c(2), p[3]]
        })
        .collect();
    Tiled::from_slice(w, h, [0; 4], &out)
}

/// Photoshop's Unsharp Mask, on luminance only so edges don't get colour
/// fringes: each pixel's brightness moves away from its Gaussian-blurred
/// surroundings by `amount` (1 = 100 %) times the difference, where that
/// difference is at least `threshold` levels (0–255). The same offset is
/// added to red, green and blue, keeping colour. Alpha is kept.
pub fn unsharp_mask(image: &Tiled<Pixel>, amount: f32, radius: f32, threshold: f32) -> Tiled<Pixel> {
    let (w, h) = (image.width() as usize, image.height() as usize);
    let pixels = image.to_vec();
    let luma = |p: &Pixel| 0.2126 * f32::from(p[0]) + 0.7152 * f32::from(p[1]) + 0.0722 * f32::from(p[2]);
    // Luminance premultiplied by alpha, so transparent areas don't count.
    let buf: Vec<[f32; 4]> = pixels
        .par_iter()
        .map(|p| {
            let a = f32::from(p[3]) / MAX;
            [luma(p) * a, a, 0.0, 0.0]
        })
        .collect();
    let blurred = blur_buffer(buf, w, h, radius);
    let threshold = threshold * MAX / 255.0;
    let out: Vec<Pixel> = pixels
        .into_par_iter()
        .zip(blurred)
        .map(|(p, [yb, a, _, _])| {
            let diff = luma(&p) - if a > 1e-6 { yb / a } else { 0.0 };
            if p[3] == 0 || diff.abs() < threshold {
                return p;
            }
            let c = |v: u16| (f32::from(v) + amount * diff).round().clamp(0.0, MAX) as u16;
            [c(p[0]), c(p[1]), c(p[2]), p[3]]
        })
        .collect();
    Tiled::from_slice(image.width(), image.height(), [0; 4], &out)
}

/// Gaussian-blur a row-major buffer of four-channel values, `sigma` being
/// the standard deviation in pixels.
pub fn blur_buffer(mut buf: Vec<[f32; 4]>, w: usize, h: usize, sigma: f32) -> Vec<[f32; 4]> {
    if sigma < 0.1 || w == 0 || h == 0 {
        return buf;
    }
    let radii = box_radii(sigma);
    blur_rows(&mut buf, w, &radii);
    let mut t = transpose(&buf, w, h);
    blur_rows(&mut t, h, &radii);
    transpose(&t, h, w)
}

/// Radii of three box blurs whose combination approximates a Gaussian with
/// standard deviation `sigma` (Kovesi, "Fast almost-Gaussian filtering").
fn box_radii(sigma: f32) -> [usize; 3] {
    let n = 3.0;
    let ideal = (12.0 * sigma * sigma / n + 1.0).sqrt();
    let mut wl = ideal.floor() as i32;
    if wl % 2 == 0 {
        wl -= 1;
    }
    let wu = wl + 2;
    let wlf = wl as f32;
    let m = ((12.0 * sigma * sigma - n * wlf * wlf - 4.0 * n * wlf - 3.0 * n) / (-4.0 * wlf - 4.0))
        .round() as i32;
    let size = |i: i32| if i < m { wl } else { wu };
    [0, 1, 2].map(|i| ((size(i).max(1) - 1) / 2) as usize)
}

/// Box-blur every row in place, once per radius, clamping at the edges.
fn blur_rows(buf: &mut [[f32; 4]], width: usize, radii: &[usize]) {
    buf.par_chunks_mut(width).for_each(|row| {
        let mut scratch = vec![[0f32; 4]; width];
        for &r in radii {
            if r == 0 {
                continue;
            }
            box_row(row, &mut scratch, r);
            row.copy_from_slice(&scratch);
        }
    });
}

fn box_row(src: &[[f32; 4]], dst: &mut [[f32; 4]], r: usize) {
    let n = src.len();
    let at = |i: isize| src[i.clamp(0, n as isize - 1) as usize];
    let scale = 1.0 / (2 * r + 1) as f64;
    // f64 running sums so long rows don't drift.
    let mut sum = [0f64; 4];
    for i in -(r as isize)..=(r as isize) {
        let p = at(i);
        for c in 0..4 {
            sum[c] += f64::from(p[c]);
        }
    }
    for (x, out) in dst.iter_mut().enumerate().take(n) {
        for c in 0..4 {
            out[c] = (sum[c] * scale) as f32;
        }
        let add = at(x as isize + r as isize + 1);
        let sub = at(x as isize - r as isize);
        for c in 0..4 {
            sum[c] += f64::from(add[c]) - f64::from(sub[c]);
        }
    }
}

fn transpose(src: &[[f32; 4]], w: usize, h: usize) -> Vec<[f32; 4]> {
    let mut out = vec![[0f32; 4]; w * h];
    // Output row x is input column x. Work in blocks of rows for cache locality.
    const BLOCK: usize = 64;
    out.par_chunks_mut(h * BLOCK)
        .enumerate()
        .for_each(|(bi, block)| {
            let x0 = bi * BLOCK;
            for y in 0..h {
                let line = &src[y * w..(y + 1) * w];
                for (dx, out_row) in block.chunks_mut(h).enumerate() {
                    out_row[y] = line[x0 + dx];
                }
            }
        });
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum NoiseDistribution {
    Uniform,
    Gaussian,
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NoiseOptions {
    pub amount: f32,
    pub distribution: NoiseDistribution,
    pub monochromatic: bool,
    pub grain_size: f32,
    pub roughness: f32,
    pub tonal_falloff: bool,
    pub seed: u64,
}

impl Default for NoiseOptions {
    fn default() -> Self {
        Self {
            amount: 25.0,
            distribution: NoiseDistribution::Uniform,
            monochromatic: false,
            grain_size: 1.0,
            roughness: 0.5,
            tonal_falloff: true,
            seed: 1,
        }
    }
}

fn hash2d(x: u32, y: u32, ch: u32, seed: u64) -> u64 {
    let mut h = seed.wrapping_add(0x9e3779b97f4a7c15);
    h ^= (x as u64).wrapping_mul(0xbf58476d1ce4e5b9);
    h = h.rotate_left(31).wrapping_add(y as u64);
    h ^= (ch as u64).wrapping_mul(0x94d049bb133111eb);
    h ^= h >> 30;
    h = h.wrapping_mul(0xbf58476d1ce4e5b9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94d049bb133111eb);
    h ^= h >> 31;
    h
}

fn sample_noise(x: u32, y: u32, ch: u32, seed: u64, dist: NoiseDistribution) -> f32 {
    let h = hash2d(x, y, ch, seed);
    match dist {
        NoiseDistribution::Uniform => {
            let u = (h >> 11) as f64 * (1.0 / (1u64 << 53) as f64);
            (u * 2.0 - 1.0) as f32
        }
        NoiseDistribution::Gaussian => {
            let u1 = ((h >> 32) as u32 as f64 + 1.0) / 4294967297.0;
            let u2 = ((h as u32) as f64 + 1.0) / 4294967297.0;
            let z = (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos();
            (z * 0.35).clamp(-1.0, 1.0) as f32
        }
    }
}

fn grain_sample(
    x: u32,
    y: u32,
    ch: u32,
    seed: u64,
    size: f32,
    roughness: f32,
    dist: NoiseDistribution,
) -> f32 {
    let fine = sample_noise(x, y, ch, seed.wrapping_add(101), dist);
    if size <= 1.001 {
        return fine;
    }
    let u = x as f32 / size;
    let v = y as f32 / size;
    let x0 = u.floor() as u32;
    let y0 = v.floor() as u32;
    let fx = u - x0 as f32;
    let fy = v - y0 as f32;
    let sx = fx * fx * (3.0 - 2.0 * fx);
    let sy = fy * fy * (3.0 - 2.0 * fy);

    let c00 = sample_noise(x0, y0, ch, seed, dist);
    let c10 = sample_noise(x0.wrapping_add(1), y0, ch, seed, dist);
    let c01 = sample_noise(x0, y0.wrapping_add(1), ch, seed, dist);
    let c11 = sample_noise(x0.wrapping_add(1), y0.wrapping_add(1), ch, seed, dist);

    let base = (c00 + (c10 - c00) * sx) * (1.0 - sy) + (c01 + (c11 - c01) * sx) * sy;

    let r = roughness.clamp(0.0, 1.0);
    let w_base = 1.0 - r;
    let w_fine = r;
    let norm = (w_base * w_base + w_fine * w_fine).sqrt().max(1e-4);
    (w_base * base + w_fine * fine) / norm
}

/// Generate a tiled raster filled with 50 % grey carrying noise according
/// to `options`. If `options.tonal_falloff` is true and `base` is provided,
/// grain amplitude decreases in deep shadows and bright highlights.
pub fn generate_grain(
    width: u32,
    height: u32,
    options: &NoiseOptions,
    base: Option<&Tiled<Pixel>>,
) -> Tiled<Pixel> {
    const MID_GREY: Pixel = [32768, 32768, 32768, 65535];
    if width == 0 || height == 0 || options.amount <= 0.0 {
        return Tiled::new(width, height, MID_GREY);
    }
    let size = options.grain_size.max(1.0);
    let roughness = options.roughness;
    let dist = options.distribution;
    let seed = options.seed;
    let mono = options.monochromatic;
    let falloff_active = options.tonal_falloff && base.is_some();

    Tiled::from_tiles(width, height, MID_GREY, |col, row| {
        let base_tile = base.and_then(|b| b.tile(col, row));
        let base_fill = base.map(|b| b.fill());
        let mut pixels = vec![MID_GREY; crate::tiled::TILE_PIXELS];

        for dy in 0..crate::tiled::TILE {
            let y = row * crate::tiled::TILE + dy;
            if y >= height {
                break;
            }
            let row_offset = (dy * crate::tiled::TILE) as usize;
            for dx in 0..crate::tiled::TILE {
                let x = col * crate::tiled::TILE + dx;
                if x >= width {
                    break;
                }
                let idx = row_offset + dx as usize;

                let falloff = if falloff_active {
                    let p = base_tile.map_or_else(|| base_fill.unwrap_or(MID_GREY), |t| t[idx]);
                    let lum = (0.299 * f32::from(p[0]) + 0.587 * f32::from(p[1]) + 0.114 * f32::from(p[2])) / 65535.0;
                    (std::f32::consts::PI * lum.clamp(0.0, 1.0)).sin()
                } else {
                    1.0
                };

                let to_u16 = |val: f32| -> u16 {
                    let offset = (val * falloff * 32767.0).round();
                    (32768.0 + offset).clamp(0.0, 65535.0) as u16
                };

                if mono {
                    let v = to_u16(grain_sample(x, y, 0, seed, size, roughness, dist));
                    pixels[idx] = [v, v, v, 65535];
                } else {
                    let r = to_u16(grain_sample(x, y, 0, seed, size, roughness, dist));
                    let g = to_u16(grain_sample(x, y, 1, seed, size, roughness, dist));
                    let b = to_u16(grain_sample(x, y, 2, seed, size, roughness, dist));
                    pixels[idx] = [r, g, b, 65535];
                }
            }
        }
        Some(pixels)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsharp_mask_steepens_edges_on_luminance_above_the_threshold() {
        // A reddish half and a lighter reddish half, with a hard edge at x = 50.
        let (w, h) = (100u32, 10u32);
        let px: Vec<Pixel> = (0..w * h)
            .map(|i| if i % w < 50 { [30000, 20000, 20000, 65535] } else { [40000, 30000, 30000, 65535] })
            .collect();
        let img = Tiled::from_slice(w, h, [0; 4], &px);
        let out = unsharp_mask(&img, 1.0, 2.0, 0.0);
        // Far from the edge nothing changes.
        assert_eq!(out.get(5, 5), img.get(5, 5));
        assert_eq!(out.get(95, 5), img.get(95, 5));
        // Next to it, dark gets darker and light lighter, by the same
        // amount in each channel, so the colour stays.
        let (dark, light) = (out.get(49, 5), out.get(50, 5));
        assert!(dark[0] < 30000 && light[0] > 40000, "{dark:?} {light:?}");
        assert_eq!(30000 - dark[0], 20000 - dark[1]);
        assert_eq!(light[0] - 40000, light[1] - 30000);
        // A threshold above the edge's contrast leaves it alone.
        assert_eq!(unsharp_mask(&img, 1.0, 2.0, 60.0).to_vec(), img.to_vec());
    }

    #[test]
    fn flat_image_stays_flat() {
        let img = Tiled::from_slice(
            300,
            200,
            [0; 4],
            &vec![[1000, 20000, 60000, 65535]; 300 * 200],
        );
        let out = gaussian_blur(&img, 5.0);
        for (x, y) in [(0, 0), (150, 100), (299, 199)] {
            let p = out.get(x, y);
            for c in 0..4 {
                assert!(p[c].abs_diff(img.get(x, y)[c]) <= 1, "{p:?}");
            }
        }
    }

    #[test]
    fn spreads_a_point_symmetrically() {
        let (w, h) = (101, 101);
        let mut px = vec![[0u16, 0, 0, 65535]; w * h];
        px[50 * w + 50] = [65535, 65535, 65535, 65535];
        let out = gaussian_blur(&Tiled::from_slice(w as u32, h as u32, [0; 4], &px), 3.0);
        let centre = out.get(50, 50)[0];
        assert!(centre > 0 && centre < 65535);
        assert_eq!(out.get(46, 50)[0], out.get(54, 50)[0]);
        assert_eq!(out.get(50, 46)[0], out.get(50, 54)[0]);
        assert!(out.get(50, 50)[0] > out.get(53, 50)[0]);
    }

    #[test]
    fn box_radii_grow_with_sigma() {
        assert!(box_radii(1.0).iter().sum::<usize>() < box_radii(10.0).iter().sum::<usize>());
    }

    #[test]
    fn same_seed_gives_identical_output() {
        let opts1 = NoiseOptions {
            seed: 12345,
            amount: 50.0,
            ..Default::default()
        };
        let opts2 = NoiseOptions {
            seed: 12345,
            amount: 50.0,
            ..Default::default()
        };
        let opts3 = NoiseOptions {
            seed: 54321,
            amount: 50.0,
            ..Default::default()
        };
        let g1 = generate_grain(300, 300, &opts1, None);
        let g2 = generate_grain(300, 300, &opts2, None);
        let g3 = generate_grain(300, 300, &opts3, None);

        assert_eq!(g1.tile(0, 0), g2.tile(0, 0));
        assert_eq!(g1.tile(1, 1), g2.tile(1, 1));
        assert_ne!(g1.tile(0, 0), g3.tile(0, 0));
    }

    #[test]
    fn neighbouring_tiles_have_no_seam() {
        let opts = NoiseOptions {
            amount: 50.0,
            grain_size: 1.0,
            distribution: NoiseDistribution::Uniform,
            monochromatic: true,
            tonal_falloff: false,
            ..Default::default()
        };
        let grain = generate_grain(512, 256, &opts, None);

        let mut boundary_diff_sum = 0.0f64;
        let mut interior_diff_sum = 0.0f64;
        let mut col_255_sum = 0.0f64;
        let mut col_256_sum = 0.0f64;

        for y in 0..256 {
            let p_254 = grain.get(254, y)[0] as f64;
            let p_255 = grain.get(255, y)[0] as f64;
            let p_256 = grain.get(256, y)[0] as f64;

            interior_diff_sum += (p_255 - p_254).abs();
            boundary_diff_sum += (p_256 - p_255).abs();
            col_255_sum += p_255;
            col_256_sum += p_256;
        }

        let mean_col_255 = col_255_sum / 256.0;
        let mean_col_256 = col_256_sum / 256.0;
        assert!((mean_col_255 - 32768.0).abs() < 1500.0);
        assert!((mean_col_256 - 32768.0).abs() < 1500.0);

        let avg_interior_diff = interior_diff_sum / 256.0;
        let avg_boundary_diff = boundary_diff_sum / 256.0;
        let ratio = avg_boundary_diff / avg_interior_diff;
        assert!((ratio - 1.0).abs() < 0.15, "seam ratio across tile boundary was {ratio}");
    }

    #[test]
    fn monochromatic_noise_has_equal_rgb_offsets() {
        let opts = NoiseOptions {
            amount: 50.0,
            monochromatic: true,
            ..Default::default()
        };
        let grain = generate_grain(100, 100, &opts, None);
        for y in 0..100 {
            for x in 0..100 {
                let p = grain.get(x, y);
                assert_eq!(p[0], p[1], "R != G at ({x}, {y})");
                assert_eq!(p[1], p[2], "G != B at ({x}, {y})");
                assert_eq!(p[3], 65535);
            }
        }

        let color_opts = NoiseOptions {
            amount: 50.0,
            monochromatic: false,
            ..Default::default()
        };
        let color_grain = generate_grain(100, 100, &color_opts, None);
        let has_divergent = (0..100).any(|y| {
            (0..100).any(|x| {
                let p = color_grain.get(x, y);
                p[0] != p[1] || p[1] != p[2]
            })
        });
        assert!(has_divergent, "color noise should have divergent RGB channels");
    }

    #[test]
    fn noise_mean_stays_approx_fifty_percent_grey() {
        for dist in [NoiseDistribution::Uniform, NoiseDistribution::Gaussian] {
            let opts = NoiseOptions {
                amount: 100.0,
                distribution: dist,
                monochromatic: false,
                tonal_falloff: false,
                ..Default::default()
            };
            let (w, h) = (256, 256);
            let grain = generate_grain(w, h, &opts, None);
            let mut sum_r = 0.0;
            let mut sum_g = 0.0;
            let mut sum_b = 0.0;
            for y in 0..h {
                for x in 0..w {
                    let p = grain.get(x, y);
                    sum_r += p[0] as f64;
                    sum_g += p[1] as f64;
                    sum_b += p[2] as f64;
                }
            }
            let n = (w * h) as f64;
            let mean_r = sum_r / n;
            let mean_g = sum_g / n;
            let mean_b = sum_b / n;

            assert!((mean_r - 32768.0).abs() < 250.0, "{dist:?} R mean: {mean_r}");
            assert!((mean_g - 32768.0).abs() < 250.0, "{dist:?} G mean: {mean_g}");
            assert!((mean_b - 32768.0).abs() < 250.0, "{dist:?} B mean: {mean_b}");
        }
    }

    #[test]
    fn tonal_falloff_reduces_grain_in_shadows_and_highlights() {
        let (w, h) = (256, 100);
        let px: Vec<Pixel> = (0..w * h)
            .map(|i| {
                let x = i % w;
                let val = (x as f32 / (w - 1) as f32 * 65535.0).round() as u16;
                [val, val, val, 65535]
            })
            .collect();
        let base = Tiled::from_raster(&crate::Raster::new(w, h, px));

        let opts = NoiseOptions {
            amount: 100.0,
            tonal_falloff: true,
            monochromatic: true,
            ..Default::default()
        };
        let grain = generate_grain(w, h, &opts, Some(&base));

        let mut shadow_var = 0.0f64;
        let mut midtone_var = 0.0f64;
        let mut highlight_var = 0.0f64;
        for y in 0..h {
            shadow_var += (grain.get(0, y)[0] as f64 - 32768.0).abs();
            midtone_var += (grain.get(128, y)[0] as f64 - 32768.0).abs();
            highlight_var += (grain.get(255, y)[0] as f64 - 32768.0).abs();
        }
        assert!(midtone_var > shadow_var * 2.0, "midtone noise should exceed shadow noise");
        assert!(midtone_var > highlight_var * 2.0, "midtone noise should exceed highlight noise");
    }
}
