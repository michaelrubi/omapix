//! Denoising with a model (Filter › Noise › Denoise…, docs/AI.md): the
//! image cut into the overlapping squares the model sees, in sRGB, its
//! answers blended back together, luminance and colour noise taken out by
//! separate amounts, and the result back in the document's colours.

use rayon::prelude::*;

use crate::color::{self, ColorProfile};
use crate::raster::Raster;
use crate::tiled::Tiled;

/// How much of each edge of the model's answer is thrown away (at most an
/// eighth of the square): NIND's outer 16 pixels are wrong, very dark. The
/// image is reflected beyond its own edges so they're seen from inside a
/// square too.
const MARGIN: usize = 32;
/// How much the parts of neighbouring squares that are kept overlap (at
/// most a quarter), for one answer to hand over to the next gradually.
const OVERLAP: usize = 32;

/// The model's answer is only trusted for detail: what it changes more
/// broadly than this (a blur's standard deviation, in pixels) is put back.
/// NIND darkens shadows and tints black and white photos a little, which
/// noise doesn't do.
const BROAD: f32 = 16.0;

/// Luma, from gamma-encoded sRGB, as noise is split into luminance and
/// colour.
const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

/// How much of each kind of noise to take out, 0–1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Amounts {
    pub luminance: f32,
    pub color: f32,
}

/// `image` (in `profile`'s colours) denoised by `model`, which takes and
/// gives back `size` × `size` squares of sRGB from 0 to 1, as red, green
/// then blue planes. `progress` hears how many squares are done, of how
/// many.
pub fn denoise(
    image: &Raster,
    profile: &ColorProfile,
    size: usize,
    amounts: Amounts,
    model: impl FnMut(&[f32]) -> Result<Vec<f32>, String>,
    progress: impl FnMut(usize, usize),
) -> Result<Raster, String> {
    let before = srgb(image, profile)?;
    let after = run(&before, size, model, progress)?;
    finish(image, profile, &before, &after, amounts)
}

/// `image` in sRGB, as the model sees it. Colours outside sRGB are clipped
/// here, but [`finish`] gives them back.
pub fn srgb(image: &Raster, profile: &ColorProfile) -> Result<Raster, String> {
    let (w, h) = (image.width(), image.height());
    let tiled = Tiled::from_slice(w, h, [0; 4], image.pixels());
    Ok(color::convert(&tiled, profile, &ColorProfile::srgb()).map_err(|e| e.to_string())?.to_raster())
}

/// `srgb` denoised by `model` (see [`denoise`]), square by square, as sRGB
/// from 0 to 1.
pub fn run(
    srgb: &Raster,
    size: usize,
    mut model: impl FnMut(&[f32]) -> Result<Vec<f32>, String>,
    mut progress: impl FnMut(usize, usize),
) -> Result<Vec<[f32; 3]>, String> {
    let (w, h) = (srgb.width() as usize, srgb.height() as usize);
    // Each square is kept `kept` across, from `margin` in.
    let margin = MARGIN.min(size / 8);
    let kept = size - 2 * margin;
    let overlap = OVERLAP.min(kept / 4);
    let (xs, ys) = (starts(w, kept, overlap), starts(h, kept, overlap));
    let total = xs.len() * ys.len();
    let mut sum = vec![[0f32; 3]; w * h];
    let mut weights = vec![0f32; w * h];
    let plane = size * size;
    for (n, (&y0, &x0)) in ys.iter().flat_map(|y| xs.iter().map(move |x| (y, x))).enumerate() {
        // The square, reflected beyond the image's edges.
        let mut square = vec![0f32; 3 * plane];
        for i in 0..plane {
            let at = |start: usize, i: usize, len: usize| mirror((start + i) as isize - margin as isize, len) as u32;
            let p = srgb.get(at(x0, i % size, w), at(y0, i / size, h));
            for c in 0..3 {
                square[c * plane + i] = f32::from(p[c]) / 65535.0;
            }
        }
        let answer = model(&square)?;
        if answer.len() != 3 * plane {
            return Err(format!("The denoising model answered {} values, not {}", answer.len(), 3 * plane));
        }
        let (wx, wy) = (ramp(x0, w, kept, overlap), ramp(y0, h, kept, overlap));
        for (ky, fy) in wy.iter().enumerate().take(h - y0) {
            for (kx, fx) in wx.iter().enumerate().take(w - x0) {
                let (i, k) = ((y0 + ky) * w + x0 + kx, fx * fy);
                let at = (ky + margin) * size + kx + margin;
                for c in 0..3 {
                    sum[i][c] += answer[c * plane + at] * k;
                }
                weights[i] += k;
            }
        }
        progress(n + 1, total);
    }
    // The change the model made, less its broad part.
    let mut change: Vec<Vec<f32>> = (0..3)
        .map(|c| (0..w * h).into_par_iter().map(|i| sum[i][c] / weights[i] - f32::from(srgb.pixels()[i][c]) / 65535.0).collect())
        .collect();
    let broad: Vec<Vec<f32>> = change.iter().map(|plane| blur(plane, w, h, BROAD)).collect();
    for (plane, broad) in change.iter_mut().zip(&broad) {
        plane.par_iter_mut().zip(broad).for_each(|(v, b)| *v -= b);
    }
    Ok((0..w * h)
        .into_par_iter()
        .map(|i| [0, 1, 2].map(|c| f32::from(srgb.pixels()[i][c]) / 65535.0 + change[c][i]))
        .collect())
}

/// `plane` (`w` × `h`) blurred to about a Gaussian of `sigma`: three box
/// blurs each way, with the edges held.
fn blur(plane: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    let r = (((4.0 * sigma * sigma + 1.0).sqrt() - 1.0) / 2.0).round().max(1.0) as usize;
    // One box blur along each row of `v` (`len` long, `rows` of them).
    let rows = |v: &mut [f32], len: usize| {
        v.par_chunks_mut(len).for_each(|row| {
            let mut out = vec![0f32; len];
            for _ in 0..3 {
                let at = |i: isize| row[i.clamp(0, len as isize - 1) as usize];
                let mut acc: f32 = (-(r as isize)..=r as isize).map(at).sum();
                for (i, o) in out.iter_mut().enumerate() {
                    *o = acc / (2 * r + 1) as f32;
                    acc += at(i as isize + r as isize + 1) - at(i as isize - r as isize);
                }
                row.copy_from_slice(&out);
            }
        })
    };
    let mut v = plane.to_vec();
    rows(&mut v, w);
    let mut t: Vec<f32> = (0..w * h).into_par_iter().map(|i| v[(i % h) * w + i / h]).collect();
    rows(&mut t, h);
    (0..w * h).into_par_iter().map(|i| t[(i % w) * h + i / w]).collect()
}

/// The denoised image in `profile`'s colours: `image`, less as much of
/// each kind of noise as `amounts` says, the noise being the difference
/// between `before` (the image in sRGB) and `after` (the model's answer).
/// It goes on as a change to `image`, so colours outside sRGB change as
/// much as their clipped selves did, rather than being clipped.
pub fn finish(
    image: &Raster,
    profile: &ColorProfile,
    before: &Raster,
    after: &[[f32; 3]],
    amounts: Amounts,
) -> Result<Raster, String> {
    let (w, h) = (image.width(), image.height());
    let mixed: Vec<_> = before
        .pixels()
        .par_iter()
        .zip(after)
        .map(|(b, a)| {
            let b = [0, 1, 2].map(|c| f32::from(b[c]) / 65535.0);
            let luma = |p: [f32; 3]| (0..3).map(|c| p[c] * LUMA[c]).sum::<f32>();
            let dl = luma(*a) - luma(b);
            let [r, g, b] = [0, 1, 2].map(|c| {
                let v = b[c] + amounts.luminance * dl + amounts.color * (a[c] - b[c] - dl);
                (v.clamp(0.0, 1.0) * 65535.0).round() as u16
            });
            [r, g, b, u16::MAX]
        })
        .collect();
    let back = |pixels: &[crate::Pixel]| {
        color::convert(&Tiled::from_slice(w, h, [0; 4], pixels), &ColorProfile::srgb(), profile)
            .map(|t| t.to_raster())
            .map_err(|e| e.to_string())
    };
    let (was, now) = (back(before.pixels())?, back(&mixed)?);
    let pixels = image
        .pixels()
        .par_iter()
        .zip(was.pixels().par_iter().zip(now.pixels()))
        .map(|(p, (was, now))| {
            let [r, g, b] = [0, 1, 2].map(|c| (i32::from(p[c]) + i32::from(now[c]) - i32::from(was[c])).clamp(0, 65535) as u16);
            [r, g, b, p[3]]
        })
        .collect();
    Ok(Raster::new(w, h, pixels))
}

/// Where the kept parts of squares, `size` across, start along a side
/// `len` long, spread evenly so neighbours overlap by at least `overlap`.
fn starts(len: usize, size: usize, overlap: usize) -> Vec<usize> {
    if len <= size {
        return vec![0];
    }
    let n = (len - overlap).div_ceil(size - overlap);
    (0..n).map(|i| (i * (len - size) + (n - 1) / 2) / (n - 1)).collect()
}

/// How much the kept part of a square starting at `start` counts, across
/// it: fading in over `overlap` from each edge, except the image's own.
fn ramp(start: usize, len: usize, size: usize, overlap: usize) -> Vec<f32> {
    let fade = |d: usize| ((d as f32 + 0.5) / overlap as f32).min(1.0);
    (0..size)
        .map(|i| {
            let left = if start == 0 { 1.0 } else { fade(i) };
            let right = if start + size >= len { 1.0 } else { fade(size - 1 - i) };
            left * right
        })
        .collect()
}

/// `i` reflected back into `0..len`.
fn mirror(i: isize, len: usize) -> usize {
    let period = (2 * len as isize - 2).max(1);
    let i = i.rem_euclid(period);
    (if i < len as isize { i } else { period - i }) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A gentle gradient with noise that's white, a different amount in
    /// each channel: luminance and colour noise.
    fn noisy(w: u32, h: u32) -> Raster {
        let pixels = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let hash = |k: u32| ((i * 3 + k).wrapping_mul(2654435761) >> 16) % 4000;
                let base = 20000 + x * 10 + y * 5;
                let [r, g, b] = [0, 1, 2].map(|c| (base + hash(c)).min(65535) as u16);
                [r, g, b, 65535]
            })
            .collect();
        Raster::new(w, h, pixels)
    }

    /// A stand-in model: each channel box-blurred 5 × 5 within the square.
    fn blur(size: usize) -> impl FnMut(&[f32]) -> Result<Vec<f32>, String> {
        move |square: &[f32]| {
            let plane = size * size;
            Ok((0..3 * plane)
                .map(|i| {
                    let (c, x, y) = (i / plane, (i % plane % size) as i32, (i % plane / size) as i32);
                    let mut sum = 0.0;
                    for dy in -2..=2 {
                        for dx in -2..=2 {
                            let (sx, sy) = ((x + dx).clamp(0, size as i32 - 1), (y + dy).clamp(0, size as i32 - 1));
                            sum += square[c * plane + sy as usize * size + sx as usize];
                        }
                    }
                    sum / 25.0
                })
                .collect())
        }
    }

    fn spread(image: &Raster) -> f32 {
        // How much neighbours differ: noise, since the gradient is gentle.
        let p = image.pixels();
        let w = image.width() as usize;
        let n = p.len() - w - 1;
        (0..n).map(|i| (0..3).map(|c| (f32::from(p[i][c]) - f32::from(p[i + 1][c])).abs()).sum::<f32>()).sum::<f32>() / n as f32
    }

    #[test]
    fn squares_cover_the_image_overlapping() {
        assert_eq!(starts(500, 768, OVERLAP), vec![0]);
        assert_eq!(starts(768, 768, OVERLAP), vec![0]);
        for len in [769, 1500, 4000, 6048] {
            let s = starts(len, 768, OVERLAP);
            assert_eq!(s[0], 0);
            assert_eq!(s.last().unwrap() + 768, len);
            assert!(s.windows(2).all(|p| p[0] + 768 >= p[1] + OVERLAP), "{len}: {s:?}");
        }
        assert_eq!(mirror(3, 5), 3);
        assert_eq!(mirror(5, 5), 3);
        assert_eq!(mirror(8, 5), 0);
        assert_eq!(mirror(-1, 5), 1);
        assert_eq!(mirror(-4, 5), 4);
        assert_eq!(mirror(-3, 1), 0);
    }

    #[test]
    fn a_model_that_changes_nothing_changes_nothing() {
        // Squares of 64 over 150 × 100: overlapping, and one hanging off.
        let image = noisy(150, 100);
        let amounts = Amounts { luminance: 1.0, color: 1.0 };
        let mut seen = Vec::new();
        let out = denoise(&image, &ColorProfile::srgb(), 64, amounts, |s| Ok(s.to_vec()), |n, of| seen.push((n, of))).unwrap();
        assert_eq!(seen.last(), Some(&(12, 12)));
        for (a, b) in out.pixels().iter().zip(image.pixels()) {
            assert!((0..4).all(|c| a[c].abs_diff(b[c]) <= 1), "{a:?} {b:?}");
        }
        // Smaller than one square: reflected to fill it.
        let small = noisy(40, 30);
        let out = denoise(&small, &ColorProfile::srgb(), 64, amounts, |s| Ok(s.to_vec()), |_, _| {}).unwrap();
        assert!(out.pixels().iter().zip(small.pixels()).all(|(a, b)| (0..4).all(|c| a[c].abs_diff(b[c]) <= 1)));
    }

    #[test]
    fn amounts_take_out_luminance_and_colour_noise_separately() {
        let image = noisy(150, 100);
        let srgb = ColorProfile::srgb();
        let before = super::srgb(&image, &srgb).unwrap();
        let after = run(&before, 64, blur(64), |_, _| {}).unwrap();
        let with = |luminance, color| finish(&image, &srgb, &before, &after, Amounts { luminance, color }).unwrap();
        // How far each pixel's channels are apart (colour), and how much
        // their luma changes between neighbours (luminance).
        let colour = |r: &Raster| r.pixels().iter().map(|p| f32::from(p[0].max(p[1]).max(p[2]) - p[0].min(p[1]).min(p[2]))).sum::<f32>() / r.pixels().len() as f32;
        let grey = |r: &Raster| {
            let pixels = r.pixels().iter().map(|p| { let v = (0..3).map(|c| f32::from(p[c]) * LUMA[c]).sum::<f32>() as u16; [v, v, v, 65535] }).collect();
            spread(&Raster::new(r.width(), r.height(), pixels))
        };
        let (none, all) = (with(0.0, 0.0), with(1.0, 1.0));
        assert!(none.pixels().iter().zip(image.pixels()).all(|(a, b)| (0..3).all(|c| a[c].abs_diff(b[c]) <= 1)));
        assert!(spread(&all) < spread(&image) / 2.0);
        let (luminance, color) = (with(1.0, 0.0), with(0.0, 1.0));
        assert!(grey(&luminance) < grey(&image) / 2.0 && colour(&luminance) > colour(&image) * 0.8);
        assert!(colour(&color) < colour(&image) / 2.0 && grey(&color) > grey(&image) * 0.8);
    }

    #[test]
    fn broad_changes_are_put_back() {
        // A model that tints everything and darkens the middle: not noise.
        let image = noisy(150, 100);
        let amounts = Amounts { luminance: 1.0, color: 1.0 };
        let tint = |s: &[f32]| {
            let plane = s.len() / 3;
            Ok(s.iter().enumerate().map(|(i, v)| v + if i < plane { 0.03 } else { 0.0 } - 0.02 * ((i % plane) as f32 / plane as f32)).collect())
        };
        let out = denoise(&image, &ColorProfile::srgb(), 64, amounts, tint, |_, _| {}).unwrap();
        let mean = |r: &Raster, c: usize| r.pixels().iter().map(|p| f32::from(p[c])).sum::<f32>() / r.pixels().len() as f32;
        for c in 0..3 {
            assert!((mean(&out, c) - mean(&image, c)).abs() < 30.0, "{c}: {} {}", mean(&out, c), mean(&image, c));
        }
    }

    #[test]
    fn colours_outside_srgb_are_changed_not_clipped() {
        // A saturated ProPhoto green that sRGB can't hold.
        let prophoto = crate::color::tests::linear_prophoto().with_gamma(1.8).unwrap();
        let image = Raster::new(8, 8, vec![[5000, 40000, 3000, 65535]; 64]);
        let amounts = Amounts { luminance: 1.0, color: 1.0 };
        let with = |lift: f32| {
            let model = move |s: &[f32]| Ok(s.iter().map(|v| v + lift).collect());
            denoise(&image, &prophoto, 16, amounts, model, |_, _| {}).unwrap().get(4, 4)
        };
        // Changed: every other pixel lighter, the rest darker, a fine
        // pattern the blur of the change doesn't take away.
        let image2 = image.clone();
        let checker = |s: &[f32]| Ok(s.iter().enumerate().map(|(i, v)| v + if (i % 16 + i / 16) % 2 == 0 { 0.02 } else { -0.02 }).collect());
        let out = denoise(&image2, &prophoto, 16, amounts, checker, |_, _| {}).unwrap();
        let (a, b) = (out.get(4, 4), out.get(5, 4));
        assert!(a[1] > 40300 && b[1] < 39700, "{a:?} {b:?}");
        assert!(a[0] < 7000 && a[2] < 5000, "still as green: {a:?}");
        let same = with(0.0);
        assert!((0..3).all(|c| same[c].abs_diff(image.get(4, 4)[c]) <= 2), "{same:?}");
    }
}
