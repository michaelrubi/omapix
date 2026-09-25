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

/// Algorithm used by Smart Sharpen to remove blur.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SharpenRemove {
    GaussianBlur,
    LensBlur,
}

impl SharpenRemove {
    pub fn name(&self) -> &'static str {
        match self {
            Self::GaussianBlur => "Gaussian Blur",
            Self::LensBlur => "Lens Blur",
        }
    }
}

/// Settings for Photoshop's Smart Sharpen filter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SmartSharpenOptions {
    /// Sharpening strength in percent (100 = 100 %).
    pub amount: f32,
    /// Blur radius in pixels.
    pub radius: f32,
    /// Noise reduction in percent (0–100 %). Suppresses small-amplitude detail.
    pub reduce_noise: f32,
    /// Blur removal algorithm: Gaussian Blur or Lens Blur.
    pub remove: SharpenRemove,
    /// Fade amount in dark tones (0–100 %).
    pub shadow_fade: f32,
    /// Fade amount in bright tones (0–100 %).
    pub highlight_fade: f32,
}

impl Default for SmartSharpenOptions {
    fn default() -> Self {
        Self {
            amount: 100.0,
            radius: 1.5,
            reduce_noise: 10.0,
            remove: SharpenRemove::GaussianBlur,
            shadow_fade: 0.0,
            highlight_fade: 0.0,
        }
    }
}

/// Settings for Photoshop's Reduce Noise filter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReduceNoiseOptions {
    /// Noise reduction strength (0–10).
    pub strength: f32,
    /// Percentage of detail to preserve (0–100 %). Lowers regularization.
    pub preserve_details: f32,
    /// Percentage of colour noise reduction (0–100 %).
    pub reduce_color_noise: f32,
    /// Percentage of details to sharpen (0–100 %).
    pub sharpen_details: f32,
}

/// Quality setting for Photoshop's Smart Blur filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmartBlurQuality {
    Low,
    Medium,
    High,
}

/// Mode setting for Photoshop's Smart Blur filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmartBlurMode {
    Normal,
    EdgeOnly,
    OverlayEdge,
}

/// Settings for Photoshop's Smart Blur filter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SmartBlurOptions {
    /// Blur radius in pixels (0.1–100 px).
    pub radius: f32,
    /// Threshold in levels (0.1–100). Determines how different a neighbour's
    /// tone can be before it is excluded from the blur.
    pub threshold: f32,
    /// Quality setting: Low, Medium, or High.
    pub quality: SmartBlurQuality,
    /// Mode setting: Normal, Edge Only, or Overlay Edge.
    pub mode: SmartBlurMode,
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
    SmartSharpen(SmartSharpenOptions),
    ReduceNoise(ReduceNoiseOptions),
    MaskDensity { density: f32 },
    SmartBlur(SmartBlurOptions),
}

impl LayerFilter {
    pub fn name(&self) -> &'static str {
        match self {
            Self::GaussianBlur { .. } => "Gaussian Blur",
            Self::HighPass { .. } => "High Pass",
            Self::UnsharpMask { .. } => "Unsharp Mask",
            Self::AddNoise(_) => "Add Noise",
            Self::SmartSharpen(_) => "Smart Sharpen",
            Self::ReduceNoise(_) => "Reduce Noise",
            Self::MaskDensity { .. } => "Mask Density",
            Self::SmartBlur(_) => "Smart Blur",
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
            Self::SmartSharpen(options) => smart_sharpen(image, &options),
            Self::ReduceNoise(options) => reduce_noise(image, &options),
            Self::MaskDensity { density } => mask_density(image, density),
            Self::SmartBlur(options) => smart_blur(image, &options),
        }
    }
}

/// Lower a layer mask's opacity destructively.
///
/// Density in percent (0–100 %). Maps each value `m` (0 = black/hidden,
/// 1 = white/revealed) to `1 − d·(1 − m)`. At 100 % the mask is unchanged,
/// at 50 % black becomes 50 % grey (32768), at 0 % the mask is all white.
/// White stays white. Alpha is kept.
pub fn mask_density(image: &Tiled<Pixel>, density: f32) -> Tiled<Pixel> {
    let d = (density / 100.0).clamp(0.0, 1.0);
    if d >= 1.0 {
        return image.clone();
    }
    let lift = |v: u16| (MAX - d * (MAX - f32::from(v))).round() as u16;
    image.map(|p| [lift(p[0]), lift(p[1]), lift(p[2]), p[3]])
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

/// Luminance of a 16-bit RGBA pixel.
#[inline]
fn pixel_luma(p: &Pixel) -> f32 {
    0.2126 * f32::from(p[0]) + 0.7152 * f32::from(p[1]) + 0.0722 * f32::from(p[2])
}

/// Blur an image's luminance channel, premultiplied by alpha so transparent
/// areas do not bleed dark fringes into the result.
fn blur_luminance(pixels: &[Pixel], w: usize, h: usize, radius: f32) -> Vec<f32> {
    let buf: Vec<[f32; 4]> = pixels
        .par_iter()
        .map(|p| {
            let a = f32::from(p[3]) / MAX;
            [pixel_luma(p) * a, a, 0.0, 0.0]
        })
        .collect();
    let blurred = blur_buffer(buf, w, h, radius);
    blurred
        .into_par_iter()
        .map(|[yb, a, _, _]| if a > 1e-6 { yb / a } else { 0.0 })
        .collect()
}

/// Apply a luminance offset equally to red, green and blue, keeping colour and alpha.
#[inline]
fn apply_luma_offset(p: Pixel, delta: f32) -> Pixel {
    let c = |v: u16| (f32::from(v) + delta).round().clamp(0.0, MAX) as u16;
    [c(p[0]), c(p[1]), c(p[2]), p[3]]
}

/// Photoshop's Unsharp Mask, on luminance only so edges don't get colour
/// fringes: each pixel's brightness moves away from its Gaussian-blurred
/// surroundings by `amount` (1 = 100 %) times the difference, where that
/// difference is at least `threshold` levels (0–255). The same offset is
/// added to red, green and blue, keeping colour. Alpha is kept.
pub fn unsharp_mask(image: &Tiled<Pixel>, amount: f32, radius: f32, threshold: f32) -> Tiled<Pixel> {
    let (w, h) = (image.width() as usize, image.height() as usize);
    let pixels = image.to_vec();
    let blurred = blur_luminance(&pixels, w, h, radius);
    let threshold = threshold * MAX / 255.0;
    let out: Vec<Pixel> = pixels
        .into_par_iter()
        .zip(blurred)
        .map(|(p, yb)| {
            if p[3] == 0 {
                return p;
            }
            let diff = pixel_luma(&p) - yb;
            if diff.abs() < threshold {
                return p;
            }
            apply_luma_offset(p, amount * diff)
        })
        .collect();
    Tiled::from_slice(image.width(), image.height(), [0; 4], &out)
}

/// Photoshop's Smart Sharpen, on luminance only so edges don't get colour
/// fringes. Supports Gaussian Blur or Lens Blur removal, noise reduction
/// (suppressing low-amplitude texture), and tonal fading in shadows and
/// highlights.
pub fn smart_sharpen(image: &Tiled<Pixel>, options: &SmartSharpenOptions) -> Tiled<Pixel> {
    let (w, h) = (image.width() as usize, image.height() as usize);
    if w == 0 || h == 0 || options.radius < 0.1 || options.amount <= 0.0 {
        return image.clone();
    }
    let pixels = image.to_vec();
    let amount_scale = options.amount / 100.0;
    let noise_thresh = (options.reduce_noise / 100.0) * (0.05 * MAX);
    let s_fade = options.shadow_fade;
    let h_fade = options.highlight_fade;

    let out: Vec<Pixel> = match options.remove {
        SharpenRemove::GaussianBlur => {
            let blurred = blur_luminance(&pixels, w, h, options.radius);
            pixels
                .into_par_iter()
                .zip(blurred)
                .map(|(p, yb)| {
                    sharpen_pixel(p, pixel_luma(&p) - yb, noise_thresh, amount_scale, s_fade, h_fade)
                })
                .collect()
        }
        SharpenRemove::LensBlur => {
            // Lens Blur removal approximation: unsharp masking with a smaller-radius
            // second pass ((radius * 0.5).max(0.2)). This provides a stronger,
            // more edge-preserving detail boost directly along edge transitions,
            // mimicking the sharp defocus bokeh disk of a lens rather than a Gaussian bell.
            let coarse = blur_luminance(&pixels, w, h, options.radius);
            let fine = blur_luminance(&pixels, w, h, (options.radius * 0.5).max(0.2));
            pixels
                .into_par_iter()
                .zip(coarse)
                .zip(fine)
                .map(|((p, yc), yf)| {
                    let y = pixel_luma(&p);
                    sharpen_pixel(p, (y - yc) + 0.5 * (y - yf), noise_thresh, amount_scale, s_fade, h_fade)
                })
                .collect()
        }
    };
    Tiled::from_slice(image.width(), image.height(), [0; 4], &out)
}

#[inline]
fn sharpen_pixel(
    p: Pixel,
    diff_raw: f32,
    noise_thresh: f32,
    amount_scale: f32,
    s_fade: f32,
    h_fade: f32,
) -> Pixel {
    if p[3] == 0 {
        return p;
    }
    let diff = soft_threshold(diff_raw, noise_thresh);
    if diff == 0.0 {
        return p;
    }
    let y = pixel_luma(&p);
    let fade = tonal_fade(y / MAX, s_fade, h_fade);
    apply_luma_offset(p, amount_scale * diff * fade)
}

#[inline]
fn soft_threshold(val: f32, threshold: f32) -> f32 {
    if threshold <= 0.0 {
        val
    } else if val.abs() <= threshold {
        0.0
    } else {
        val.signum() * (val.abs() - threshold)
    }
}

#[inline]
fn tonal_fade(y_norm: f32, shadow_fade_pct: f32, highlight_fade_pct: f32) -> f32 {
    if shadow_fade_pct <= 0.0 && highlight_fade_pct <= 0.0 {
        return 1.0;
    }
    let shadow_w = if y_norm < 0.5 {
        let t = y_norm / 0.5;
        1.0 - t * t * (3.0 - 2.0 * t)
    } else {
        0.0
    };
    let highlight_w = if y_norm > 0.5 {
        let t = (y_norm - 0.5) / 0.5;
        t * t * (3.0 - 2.0 * t)
    } else {
        0.0
    };
    let fade = shadow_w * (shadow_fade_pct / 100.0) + highlight_w * (highlight_fade_pct / 100.0);
    (1.0 - fade).clamp(0.0, 1.0)
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

/// Single-pass 2D box filter of radius `r` using running sums.
fn box_filter_2d(mut buf: Vec<[f32; 4]>, w: usize, h: usize, r: usize) -> Vec<[f32; 4]> {
    if r == 0 || w == 0 || h == 0 {
        return buf;
    }
    blur_rows(&mut buf, w, &[r]);
    let mut t = transpose(&buf, w, h);
    blur_rows(&mut t, h, &[r]);
    transpose(&t, h, w)
}

/// Edge-preserving self-guided filter (He et al.) using box filters.
fn guided_filter(y: &[f32], w: usize, h: usize, r: usize, eps: f32) -> Vec<f32> {
    if r == 0 || eps <= 1e-8 || w == 0 || h == 0 {
        return y.to_vec();
    }
    let r = r.min(w.saturating_sub(1)).min(h.saturating_sub(1));
    if r == 0 {
        return y.to_vec();
    }
    let buf: Vec<[f32; 4]> = y.par_iter().map(|&v| [v, v * v, 0.0, 0.0]).collect();
    let mean_buf = box_filter_2d(buf, w, h, r);

    let ab_buf: Vec<[f32; 4]> = mean_buf
        .into_par_iter()
        .map(|[mean_y, mean_yy, _, _]| {
            let var = (mean_yy - mean_y * mean_y).max(0.0);
            let a = var / (var + eps);
            let b = (1.0 - a) * mean_y;
            [a, b, 0.0, 0.0]
        })
        .collect();

    let mean_ab = box_filter_2d(ab_buf, w, h, r);

    mean_ab
        .into_par_iter()
        .zip(y)
        .map(|([mean_a, mean_b, _, _], &y_val)| (mean_a * y_val + mean_b).clamp(0.0, 1.0))
        .collect()
}

/// Photoshop's Reduce Noise filter: edge-preserving guided filter on luminance,
/// chroma smoothing for colour noise, and unsharp masking for detail sharpening.
pub fn reduce_noise(image: &Tiled<Pixel>, options: &ReduceNoiseOptions) -> Tiled<Pixel> {
    let (w, h) = (image.width() as usize, image.height() as usize);
    if w == 0 || h == 0 {
        return image.clone();
    }
    if options.strength <= 0.0
        && options.reduce_color_noise <= 0.0
        && options.sharpen_details <= 0.0
    {
        return image.clone();
    }

    let pixels = image.to_vec();

    // 1. Luminance filtering with Guided Filter (He et al.) using itself as the guide.
    // Strength sets regularization (epsilon), and Preserve Details lowers it.
    let y_filtered: Vec<f32> = if options.strength > 0.0 {
        let s = (options.strength / 10.0).clamp(0.0, 1.0);
        let detail_factor = 1.0 - 0.9 * (options.preserve_details / 100.0).clamp(0.0, 1.0);
        let eps = s * s * 0.005 * detail_factor;
        let y_norm: Vec<f32> = pixels.par_iter().map(|p| pixel_luma(p) / MAX).collect();
        let guided = guided_filter(&y_norm, w, h, 2, eps);
        guided.into_par_iter().map(|v| v * MAX).collect()
    } else {
        pixels.par_iter().map(pixel_luma).collect()
    };

    // 2. Colour noise: smooth chroma (colour difference from luminance) with a larger
    // radius (6.0 px), scaled by Reduce Color Noise, keeping luminance.
    let k_chroma = (options.reduce_color_noise / 100.0).clamp(0.0, 1.0);
    let smoothed_chroma = if k_chroma > 0.0 {
        let chroma_buf: Vec<[f32; 4]> = pixels
            .par_iter()
            .map(|p| {
                let y = pixel_luma(p);
                let a = f32::from(p[3]) / MAX;
                [
                    (f32::from(p[0]) - y) * a,
                    (f32::from(p[1]) - y) * a,
                    (f32::from(p[2]) - y) * a,
                    a,
                ]
            })
            .collect();
        let blurred = blur_buffer(chroma_buf, w, h, 6.0);
        Some(
            blurred
                .into_par_iter()
                .map(|[cr, cg, cb, a]| {
                    if a > 1e-6 {
                        [cr / a, cg / a, cb / a]
                    } else {
                        [0.0, 0.0, 0.0]
                    }
                })
                .collect::<Vec<[f32; 3]>>(),
        )
    } else {
        None
    };

    // 3. Reconstruct pixels with filtered luminance and smoothed chroma, keeping alpha.
    let out_pixels: Vec<Pixel> = pixels
        .into_par_iter()
        .zip(y_filtered)
        .enumerate()
        .map(|(i, (p, y))| {
            if p[3] == 0 {
                return p;
            }
            let y_orig = pixel_luma(&p);
            let ocr = f32::from(p[0]) - y_orig;
            let ocg = f32::from(p[1]) - y_orig;
            let ocb = f32::from(p[2]) - y_orig;
            let (cr, cg, cb) = if let Some(ref smoothed) = smoothed_chroma {
                let [scr, scg, scb] = smoothed[i];
                (
                    ocr + k_chroma * (scr - ocr),
                    ocg + k_chroma * (scg - ocg),
                    ocb + k_chroma * (scb - ocb),
                )
            } else {
                (ocr, ocg, ocb)
            };
            let c = |v: f32| v.round().clamp(0.0, MAX) as u16;
            [c(y + cr), c(y + cg), c(y + cb), p[3]]
        })
        .collect();

    let out = Tiled::from_slice(image.width(), image.height(), [0; 4], &out_pixels);

    // 4. Sharpen details: reuse existing unsharp-mask luminance code.
    if options.sharpen_details > 0.0 {
        unsharp_mask(&out, options.sharpen_details / 100.0, 1.0, 0.0)
    } else {
        out
    }
}

/// Photoshop's Filter › Blur › Smart Blur: an edge-preserving blur that
/// averages the neighbours within `radius` whose luminance differs from the
/// centre's by less than `threshold` (a sigma filter).
///
/// Quality sets how finely the neighbourhood is sampled: Low takes every
/// third pixel, Medium every second, High every one. Large radii sample
/// more sparsely still, so a radius of 100 costs no more than about 16
/// samples across, keeping the live preview usable.
///
/// Edge Only shows the edges (pixels next to one differing by the threshold
/// or more) in white on black; Overlay Edge draws them in white over the
/// blurred image. Alpha is kept.
pub fn smart_blur(image: &Tiled<Pixel>, options: &SmartBlurOptions) -> Tiled<Pixel> {
    let (w, h) = (image.width() as usize, image.height() as usize);
    let pixels = image.to_vec();
    let luma: Vec<f32> = pixels.par_iter().map(pixel_luma).collect();

    let radius = options.radius.clamp(0.1, 100.0);
    let threshold = options.threshold.clamp(0.1, 100.0) * MAX / 255.0;
    let reach = radius.ceil() as isize;
    let (min_step, samples) = match options.quality {
        SmartBlurQuality::Low => (3, 3),
        SmartBlurQuality::Medium => (2, 5),
        SmartBlurQuality::High => (1, 8),
    };
    // Samples each side of the centre, which is always one of them.
    let step = min_step.max((reach + samples - 1) / samples).min(reach.max(1));
    let span = (reach / step) * step;
    let offsets: Vec<(isize, isize)> = (-span..=span)
        .step_by(step as usize)
        .flat_map(|dy| (-span..=span).step_by(step as usize).map(move |dx| (dx, dy)))
        .filter(|&(dx, dy)| ((dx * dx + dy * dy) as f32) <= radius * radius)
        .collect();

    let (wi, hi) = (w as isize, h as isize);
    let mut out = vec![[0u16; 4]; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let y = y as isize;
        for (x, out) in row.iter_mut().enumerate() {
            let x = x as isize;
            let i = (y * wi + x) as usize;
            let p = pixels[i];
            let centre = luma[i];
            let similar = |j: usize| (luma[j] - centre).abs() < threshold;

            let edge = options.mode != SmartBlurMode::Normal
                && [(-1, 0), (1, 0), (0, -1), (0, 1)].iter().any(|&(dx, dy)| {
                    let (nx, ny) = (x + dx, y + dy);
                    (0..wi).contains(&nx) && (0..hi).contains(&ny) && !similar((ny * wi + nx) as usize)
                });
            *out = match options.mode {
                SmartBlurMode::EdgeOnly => {
                    let v = if edge { u16::MAX } else { 0 };
                    [v, v, v, p[3]]
                }
                _ if edge => [u16::MAX, u16::MAX, u16::MAX, p[3]],
                _ => {
                    let mut sum = [0u64; 3];
                    let mut count = 0u64;
                    for &(dx, dy) in &offsets {
                        let (nx, ny) = (x + dx, y + dy);
                        if !(0..wi).contains(&nx) || !(0..hi).contains(&ny) {
                            continue;
                        }
                        let j = (ny * wi + nx) as usize;
                        if pixels[j][3] > 0 && similar(j) {
                            for c in 0..3 {
                                sum[c] += u64::from(pixels[j][c]);
                            }
                            count += 1;
                        }
                    }
                    if count == 0 {
                        p
                    } else {
                        let avg = |c: usize| ((sum[c] + count / 2) / count) as u16;
                        [avg(0), avg(1), avg(2), p[3]]
                    }
                }
            };
        }
    });

    Tiled::from_slice(image.width(), image.height(), [0; 4], &out)
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
    fn smart_sharpen_flat_areas_unchanged() {
        let img = Tiled::from_slice(
            60,
            20,
            [0; 4],
            &vec![[15000, 25000, 35000, 65535]; 60 * 20],
        );
        for remove in [SharpenRemove::GaussianBlur, SharpenRemove::LensBlur] {
            let opts = SmartSharpenOptions {
                amount: 300.0,
                radius: 3.0,
                reduce_noise: 0.0,
                remove,
                shadow_fade: 0.0,
                highlight_fade: 0.0,
            };
            let out = smart_sharpen(&img, &opts);
            for y in 0..20 {
                for x in 0..60 {
                    let p = out.get(x, y);
                    let orig = img.get(x, y);
                    for c in 0..4 {
                        assert!(p[c].abs_diff(orig[c]) <= 1, "{remove:?} changed flat area: {p:?} vs {orig:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn smart_sharpen_steepens_edges() {
        let (w, h) = (100u32, 10u32);
        let px: Vec<Pixel> = (0..w * h)
            .map(|i| if i % w < 50 { [30000, 30000, 30000, 65535] } else { [40000, 40000, 40000, 65535] })
            .collect();
        let img = Tiled::from_slice(w, h, [0; 4], &px);

        for remove in [SharpenRemove::GaussianBlur, SharpenRemove::LensBlur] {
            let opts = SmartSharpenOptions {
                amount: 150.0,
                radius: 2.0,
                reduce_noise: 0.0,
                remove,
                shadow_fade: 0.0,
                highlight_fade: 0.0,
            };
            let out = smart_sharpen(&img, &opts);
            // Far from edge, unchanged
            assert_eq!(out.get(5, 5), img.get(5, 5));
            assert_eq!(out.get(95, 5), img.get(95, 5));
            // Adjacent to edge, dark side gets darker, bright side gets lighter
            let (dark, light) = (out.get(49, 5), out.get(50, 5));
            assert!(dark[0] < 30000, "{remove:?} dark edge not darkened: {dark:?}");
            assert!(light[0] > 40000, "{remove:?} light edge not lightened: {light:?}");
        }

        // Lens Blur removal provides a stronger detail boost on edges than Gaussian Blur
        let opts_gauss = SmartSharpenOptions {
            amount: 100.0,
            radius: 2.0,
            reduce_noise: 0.0,
            remove: SharpenRemove::GaussianBlur,
            shadow_fade: 0.0,
            highlight_fade: 0.0,
        };
        let opts_lens = SmartSharpenOptions {
            remove: SharpenRemove::LensBlur,
            ..opts_gauss
        };
        let out_gauss = smart_sharpen(&img, &opts_gauss);
        let out_lens = smart_sharpen(&img, &opts_lens);
        let gauss_boost = out_gauss.get(50, 5)[0] - 40000;
        let lens_boost = out_lens.get(50, 5)[0] - 40000;
        assert!(lens_boost > gauss_boost, "lens blur boost {lens_boost} should exceed gaussian boost {gauss_boost}");
    }

    #[test]
    fn smart_sharpen_keeps_colour() {
        let (w, h) = (100u32, 10u32);
        let px: Vec<Pixel> = (0..w * h)
            .map(|i| if i % w < 50 { [30000, 20000, 15000, 65535] } else { [45000, 32000, 28000, 65535] })
            .collect();
        let img = Tiled::from_slice(w, h, [0; 4], &px);

        let opts = SmartSharpenOptions {
            amount: 100.0,
            radius: 2.0,
            reduce_noise: 0.0,
            remove: SharpenRemove::GaussianBlur,
            shadow_fade: 0.0,
            highlight_fade: 0.0,
        };
        let out = smart_sharpen(&img, &opts);
        let dark = out.get(49, 5);
        let light = out.get(50, 5);

        // Same offset in each channel, preserving color ratios
        let dark_delta_r = 30000 - dark[0];
        let dark_delta_g = 20000 - dark[1];
        let dark_delta_b = 15000 - dark[2];
        assert_eq!(dark_delta_r, dark_delta_g);
        assert_eq!(dark_delta_g, dark_delta_b);

        let light_delta_r = light[0] - 45000;
        let light_delta_g = light[1] - 32000;
        let light_delta_b = light[2] - 28000;
        assert_eq!(light_delta_r, light_delta_g);
        assert_eq!(light_delta_g, light_delta_b);
    }

    #[test]
    fn smart_sharpen_reduce_noise_leaves_low_amplitude_texture_alone_at_100_percent() {
        let (w, h) = (100u32, 10u32);
        // Low amplitude texture: 500 units difference (well within noise threshold)
        let px: Vec<Pixel> = (0..w * h)
            .map(|i| {
                let v = if (i % w) % 4 < 2 { 32500 } else { 33000 };
                [v, v, v, 65535]
            })
            .collect();
        let img = Tiled::from_slice(w, h, [0; 4], &px);

        let opts_no_reduction = SmartSharpenOptions {
            amount: 200.0,
            radius: 1.5,
            reduce_noise: 0.0,
            remove: SharpenRemove::GaussianBlur,
            shadow_fade: 0.0,
            highlight_fade: 0.0,
        };
        let out_sharpened = smart_sharpen(&img, &opts_no_reduction);
        assert_ne!(out_sharpened.to_vec(), img.to_vec(), "should be sharpened at 0% reduce noise");

        let opts_full_reduction = SmartSharpenOptions {
            reduce_noise: 100.0,
            ..opts_no_reduction
        };
        let out_suppressed = smart_sharpen(&img, &opts_full_reduction);
        assert_eq!(out_suppressed.to_vec(), img.to_vec(), "100% reduce noise should leave low-amplitude texture alone");
    }

    #[test]
    fn smart_sharpen_shadow_fade_reduces_sharpening_in_dark_areas() {
        let (w, h) = (100u32, 10u32);
        // Step edge in deep shadows (5000 to 10000, normalized < 0.16)
        let px: Vec<Pixel> = (0..w * h)
            .map(|i| if i % w < 50 { [5000, 5000, 5000, 65535] } else { [10000, 10000, 10000, 65535] })
            .collect();
        let img = Tiled::from_slice(w, h, [0; 4], &px);

        let opts_nofade = SmartSharpenOptions {
            amount: 150.0,
            radius: 2.0,
            reduce_noise: 0.0,
            remove: SharpenRemove::GaussianBlur,
            shadow_fade: 0.0,
            highlight_fade: 0.0,
        };
        let opts_fade = SmartSharpenOptions {
            shadow_fade: 100.0,
            ..opts_nofade
        };

        let out_nofade = smart_sharpen(&img, &opts_nofade);
        let out_fade = smart_sharpen(&img, &opts_fade);

        let boost_nofade = out_nofade.get(50, 5)[0] - 10000;
        let boost_fade = out_fade.get(50, 5)[0] - 10000;
        assert!(boost_fade < boost_nofade, "shadow fade should reduce edge boost: {boost_fade} vs {boost_nofade}");
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

    #[test]
    fn reduce_noise_flat_image_with_noise_gets_lower_std_dev() {
        let (w, h) = (60u32, 60u32);
        let flat = Tiled::from_slice(
            w,
            h,
            [0; 4],
            &vec![[30000, 30000, 30000, 65535]; (w * h) as usize],
        );
        let noisy = add_noise(
            &flat,
            &NoiseOptions {
                amount: 15.0,
                distribution: NoiseDistribution::Uniform,
                monochromatic: true,
                grain_size: 1.0,
                roughness: 0.5,
                tonal_falloff: false,
                seed: 42,
            },
        );

        let std_dev = |img: &Tiled<Pixel>| -> f64 {
            let px = img.to_vec();
            let n = px.len() as f64;
            let mean: f64 = px.iter().map(|p| f64::from(p[0])).sum::<f64>() / n;
            let var: f64 = px
                .iter()
                .map(|p| {
                    let diff = f64::from(p[0]) - mean;
                    diff * diff
                })
                .sum::<f64>()
                / n;
            var.sqrt()
        };

        let dev_before = std_dev(&noisy);
        let denoised = reduce_noise(
            &noisy,
            &ReduceNoiseOptions {
                strength: 7.0,
                preserve_details: 0.0,
                reduce_color_noise: 0.0,
                sharpen_details: 0.0,
            },
        );
        let dev_after = std_dev(&denoised);
        assert!(
            dev_after < dev_before * 0.5,
            "std dev after ({dev_after}) should be significantly lower than before ({dev_before})"
        );
    }

    #[test]
    fn reduce_noise_hard_edge_stays_sharp() {
        let (w, h) = (100u32, 20u32);
        let px: Vec<Pixel> = (0..w * h)
            .map(|i| {
                if i % w < 50 {
                    [20000, 20000, 20000, 65535]
                } else {
                    [45000, 45000, 45000, 65535]
                }
            })
            .collect();
        let img = Tiled::from_slice(w, h, [0; 4], &px);

        let orig_step = 45000 - 20000;
        let denoised = reduce_noise(
            &img,
            &ReduceNoiseOptions {
                strength: 5.0,
                preserve_details: 20.0,
                reduce_color_noise: 0.0,
                sharpen_details: 0.0,
            },
        );

        let p_left = denoised.get(49, 10)[0];
        let p_right = denoised.get(50, 10)[0];
        let step = p_right as i32 - p_left as i32;

        let tolerance = (orig_step as f32 * 0.10) as i32;
        assert!(
            (step - orig_step).abs() <= tolerance,
            "hard edge step {step} deviated too much from {orig_step} (tolerance: {tolerance})"
        );
    }

    #[test]
    fn reduce_noise_strength_zero_changes_nothing() {
        let (w, h) = (60u32, 20u32);
        let px: Vec<Pixel> = (0..w * h)
            .map(|i| {
                let v = ((i * 12345) % 65535) as u16;
                [v, v.wrapping_add(1000), v.wrapping_sub(1000), 65535]
            })
            .collect();
        let img = Tiled::from_slice(w, h, [0; 4], &px);

        let out = reduce_noise(
            &img,
            &ReduceNoiseOptions {
                strength: 0.0,
                preserve_details: 0.0,
                reduce_color_noise: 0.0,
                sharpen_details: 0.0,
            },
        );
        assert_eq!(out.to_vec(), img.to_vec());
    }

    #[test]
    fn reduce_noise_color_noise_reduces_chroma_variance_keeping_mean_colour() {
        let (w, h) = (80u32, 40u32);
        let n = (w * h) as usize;
        let mut px: Vec<Pixel> = Vec::with_capacity(n);
        for i in 0..n {
            let x = (i as u32) % w;
            let y = (i as u32) / w;
            let d = match (x + y) % 4 {
                0 => (1500i32, -1000i32, -500i32),
                1 => (-1500i32, 1000i32, 500i32),
                2 => (800i32, -400i32, -400i32),
                _ => (-800i32, 400i32, 400i32),
            };
            let r = (32768 + d.0).clamp(0, 65535) as u16;
            let g = (32768 + d.1).clamp(0, 65535) as u16;
            let b = (32768 + d.2).clamp(0, 65535) as u16;
            px.push([r, g, b, 65535]);
        }
        let img = Tiled::from_slice(w, h, [0; 4], &px);

        let chroma_variance = |t: &Tiled<Pixel>| -> f64 {
            let pixels = t.to_vec();
            let mut var_sum = 0.0;
            for p in &pixels {
                let y = pixel_luma(p);
                let cr = f64::from(p[0]) - f64::from(y);
                let cg = f64::from(p[1]) - f64::from(y);
                let cb = f64::from(p[2]) - f64::from(y);
                var_sum += cr * cr + cg * cg + cb * cb;
            }
            var_sum / (pixels.len() as f64)
        };

        let mean_colour = |t: &Tiled<Pixel>| -> [f64; 3] {
            let pixels = t.to_vec();
            let n = pixels.len() as f64;
            let mr = pixels.iter().map(|p| f64::from(p[0])).sum::<f64>() / n;
            let mg = pixels.iter().map(|p| f64::from(p[1])).sum::<f64>() / n;
            let mb = pixels.iter().map(|p| f64::from(p[2])).sum::<f64>() / n;
            [mr, mg, mb]
        };

        let var_before = chroma_variance(&img);
        let mean_before = mean_colour(&img);

        let denoised = reduce_noise(
            &img,
            &ReduceNoiseOptions {
                strength: 0.0,
                preserve_details: 0.0,
                reduce_color_noise: 100.0,
                sharpen_details: 0.0,
            },
        );

        let var_after = chroma_variance(&denoised);
        let mean_after = mean_colour(&denoised);

        assert!(
            var_after < var_before * 0.1,
            "chroma variance after ({var_after}) should be far less than before ({var_before})"
        );
        for c in 0..3 {
            assert!(
                (mean_after[c] - mean_before[c]).abs() <= 1.0,
                "channel {c} mean colour shifted: {} vs {}",
                mean_after[c],
                mean_before[c]
            );
        }
    }

    #[test]
    fn mask_density_mapping() {
        let mut pixels = Tiled::new(2, 2, [0, 0, 0, u16::MAX]);
        pixels.tile_mut(0, 0)[0] = [0, 0, 0, u16::MAX]; // black
        pixels.tile_mut(0, 0)[1] = [u16::MAX, u16::MAX, u16::MAX, u16::MAX]; // white
        pixels.tile_mut(0, 0)[2] = [32768, 32768, 32768, u16::MAX]; // mid grey
        pixels.tile_mut(0, 0)[3] = [10000, 10000, 10000, u16::MAX];

        // 100 % unchanged
        let at_100 = LayerFilter::MaskDensity { density: 100.0 }.apply(&pixels);
        assert_eq!(at_100.to_vec(), pixels.to_vec());

        // 50 % turns black into mid grey, white stays white
        let at_50 = LayerFilter::MaskDensity { density: 50.0 }.apply(&pixels);
        assert_eq!(at_50.get(0, 0)[0], 32768);
        assert_eq!(at_50.get(1, 0)[0], u16::MAX);

        // 0 % all white, white stays white
        let at_0 = LayerFilter::MaskDensity { density: 0.0 }.apply(&pixels);
        for p in at_0.to_vec() {
            assert_eq!(p[0], u16::MAX);
            assert_eq!(p[1], u16::MAX);
            assert_eq!(p[2], u16::MAX);
        }
    }

    #[test]
    fn smart_blur_flat_area_gets_smoothed() {
        let (w, h) = (40u32, 40u32);
        // Base tone 30000 with a ±600 checkerboard noise.
        let px: Vec<Pixel> = (0..w * h)
            .map(|i| {
                let x = i % w;
                let y = i / w;
                let noise = if (x + y) % 2 == 0 { 600i32 } else { -600i32 };
                let v = (30000 + noise) as u16;
                [v, v, v, 65535]
            })
            .collect();
        let img = Tiled::from_slice(w, h, [0; 4], &px);
        let opts = SmartBlurOptions {
            radius: 3.0,
            threshold: 25.0,
            quality: SmartBlurQuality::High,
            mode: SmartBlurMode::Normal,
        };
        let out = smart_blur(&img, &opts);
        // Centre pixels should be smoothed close to 30000 (much less than 600 diff).
        for y in 10..30 {
            for x in 10..30 {
                let p = out.get(x, y);
                assert!(
                    p[0].abs_diff(30000) < 100,
                    "noise was not smoothed: {p:?} at ({x}, {y})"
                );
            }
        }
    }

    #[test]
    fn smart_blur_hard_edge_stays_sharp() {
        let (w, h) = (100u32, 10u32);
        let px: Vec<Pixel> = (0..w * h)
            .map(|i| {
                if i % w < 50 {
                    [10000, 10000, 10000, 65535]
                } else {
                    [50000, 50000, 50000, 65535]
                }
            })
            .collect();
        let img = Tiled::from_slice(w, h, [0; 4], &px);
        let opts = SmartBlurOptions {
            radius: 3.0,
            threshold: 25.0,
            quality: SmartBlurQuality::High,
            mode: SmartBlurMode::Normal,
        };
        let out = smart_blur(&img, &opts);
        // Next to the edge, the dark side stays 10000 and the bright side stays 50000
        // because neighbours across the boundary differ by 40000 > threshold.
        assert_eq!(out.get(49, 5), [10000, 10000, 10000, 65535]);
        assert_eq!(out.get(50, 5), [50000, 50000, 50000, 65535]);
        assert_eq!(out.get(5, 5), [10000, 10000, 10000, 65535]);
        assert_eq!(out.get(95, 5), [50000, 50000, 50000, 65535]);
    }

    #[test]
    fn smart_blur_preserves_alpha() {
        let (w, h) = (20u32, 20u32);
        let px: Vec<Pixel> = (0..w * h)
            .map(|i| {
                let a = match (i % w) % 3 {
                    0 => 0,
                    1 => 32768,
                    _ => 65535,
                };
                [20000, 25000, 30000, a]
            })
            .collect();
        let img = Tiled::from_slice(w, h, [0; 4], &px);
        for mode in [
            SmartBlurMode::Normal,
            SmartBlurMode::EdgeOnly,
            SmartBlurMode::OverlayEdge,
        ] {
            let opts = SmartBlurOptions {
                radius: 2.0,
                threshold: 25.0,
                quality: SmartBlurQuality::Medium,
                mode,
            };
            let out = smart_blur(&img, &opts);
            for y in 0..h {
                for x in 0..w {
                    assert_eq!(
                        out.get(x, y)[3],
                        img.get(x, y)[3],
                        "alpha changed in mode {mode:?} at ({x}, {y})"
                    );
                }
            }
        }
    }

    #[test]
    fn smart_blur_edge_only_gives_white_on_edges_and_black_elsewhere() {
        let (w, h) = (100u32, 10u32);
        let px: Vec<Pixel> = (0..w * h)
            .map(|i| {
                if i % w < 50 {
                    [10000, 10000, 10000, 65535]
                } else {
                    [50000, 50000, 50000, 65535]
                }
            })
            .collect();
        let img = Tiled::from_slice(w, h, [0; 4], &px);

        // Edge Only
        let edge_only_opts = SmartBlurOptions {
            radius: 3.0,
            threshold: 25.0,
            quality: SmartBlurQuality::High,
            mode: SmartBlurMode::EdgeOnly,
        };
        let out_edge = smart_blur(&img, &edge_only_opts);
        // Flat areas far from the edge are black.
        assert_eq!(out_edge.get(5, 5), [0, 0, 0, 65535]);
        assert_eq!(out_edge.get(95, 5), [0, 0, 0, 65535]);
        // Pixels right at the edge are white.
        assert_eq!(out_edge.get(49, 5), [65535, 65535, 65535, 65535]);
        assert_eq!(out_edge.get(50, 5), [65535, 65535, 65535, 65535]);
        // Edges are one pixel either side, not the whole radius.
        assert_eq!(out_edge.get(48, 5), [0, 0, 0, 65535]);
        assert_eq!(out_edge.get(51, 5), [0, 0, 0, 65535]);

        // Overlay Edge
        let overlay_opts = SmartBlurOptions {
            radius: 3.0,
            threshold: 25.0,
            quality: SmartBlurQuality::High,
            mode: SmartBlurMode::OverlayEdge,
        };
        let out_overlay = smart_blur(&img, &overlay_opts);
        // Flat areas retain their smoothed image colour.
        assert_eq!(out_overlay.get(5, 5), [10000, 10000, 10000, 65535]);
        assert_eq!(out_overlay.get(95, 5), [50000, 50000, 50000, 65535]);
        // Edges are drawn in white.
        assert_eq!(out_overlay.get(49, 5), [65535, 65535, 65535, 65535]);
        assert_eq!(out_overlay.get(50, 5), [65535, 65535, 65535, 65535]);
    }
}

