//! Enlarging with a model (Image › Image Size, docs/AI.md): the image cut
//! into the overlapping squares the model sees, in sRGB, as Denoise does,
//! its larger answers blended back together, fitted to the size asked for,
//! and the result back in the document's colours.

use rayon::prelude::*;

use crate::color::ColorProfile;
use crate::denoise::{self, Amounts, BROAD, MARGIN, OVERLAP, blur, mirror, ramp, starts};
use crate::raster::Raster;
use crate::tiled::Tiled;
use crate::transform::{Resampling::Bicubic, resized};

/// `image` (in `profile`'s colours) enlarged to `width` × `height` by
/// `model`, which takes `size` × `size` squares of sRGB from 0 to 1, as
/// red, green then blue planes, and gives them back `factor` times the
/// size. `progress` hears how many squares are done, of how many.
///
/// What the model adds goes on as a change to the image enlarged the
/// ordinary way (bicubic), so colours outside sRGB aren't clipped, and
/// transparency is the ordinary one's.
pub fn upscale(
    image: &Raster,
    profile: &ColorProfile,
    (width, height): (u32, u32),
    (size, factor): (usize, usize),
    model: impl FnMut(&[f32]) -> Result<Vec<f32>, String>,
    progress: impl FnMut(usize, usize),
) -> Result<Raster, String> {
    let enlarged = run(&denoise::srgb(image, profile)?, size, factor, model, progress)?;
    let fit = |r: &Raster| resized(&Tiled::from_raster(r), width, height, Bicubic).to_raster();
    let plain = fit(image);
    let before = denoise::srgb(&plain, profile)?;
    let after: Vec<[f32; 3]> = fit(&enlarged).pixels().par_iter().map(|p| [0, 1, 2].map(|c| f32::from(p[c]) / 65535.0)).collect();
    drop(enlarged);
    denoise::finish(&plain, profile, &before, &after, Amounts { luminance: 1.0, color: 1.0 })
}

/// `srgb` enlarged `factor` times by `model` (see [`upscale`]), square by
/// square, opaque. As with Denoise, the model is only trusted for detail:
/// where its answer, shrunk back, is broadly lighter, darker or tinted, the
/// difference is taken out (RealPLKSR lightens reds and greens a little).
pub fn run(
    srgb: &Raster,
    size: usize,
    factor: usize,
    mut model: impl FnMut(&[f32]) -> Result<Vec<f32>, String>,
    mut progress: impl FnMut(usize, usize),
) -> Result<Raster, String> {
    let (w, h) = (srgb.width() as usize, srgb.height() as usize);
    // Each square is kept `kept` across, from `margin` in.
    let margin = MARGIN.min(size / 8);
    let kept = size - 2 * margin;
    let overlap = OVERLAP.min(kept / 4);
    let (xs, ys) = (starts(w, kept, overlap), starts(h, kept, overlap));
    // How much each square counts along a side, so that at every pixel
    // those of the squares over it add up to 1.
    let shares = |starts: &[usize], len: usize| -> Vec<Vec<f32>> {
        let ramps: Vec<Vec<f32>> = starts.iter().map(|&s| ramp(s, len, kept, overlap)).collect();
        let mut total = vec![0f32; len];
        for (&s, ramp) in starts.iter().zip(&ramps) {
            for (t, k) in total[s..].iter_mut().zip(ramp) {
                *t += k;
            }
        }
        let share = |(&s, ramp): (&usize, &Vec<f32>)| ramp.iter().zip(&total[s..]).map(|(k, t)| k / t).collect();
        starts.iter().zip(&ramps).map(share).collect()
    };
    let (wxs, wys) = (shares(&xs, w), shares(&ys, h));
    let total = xs.len() * ys.len();
    let (plane, side) = (size * size, size * factor);
    let (ow, oh) = (w * factor, h * factor);
    let mut sum = vec![[0f32; 3]; ow * oh];
    for (n, (j, i)) in (0..ys.len()).flat_map(|j| (0..xs.len()).map(move |i| (j, i))).enumerate() {
        let (x0, y0) = (xs[i], ys[j]);
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
        if answer.len() != 3 * side * side {
            return Err(format!("The upscaling model answered {} values, not {}", answer.len(), 3 * side * side));
        }
        // The rows of the answer that are kept, onto the rows they cover.
        let rows = (y0 * factor * ow)..((y0 + kept).min(h) * factor * ow);
        sum[rows].par_chunks_mut(ow).enumerate().for_each(|(oy, row)| {
            let fy = wys[j][oy / factor];
            let from = (oy + margin * factor) * side + margin * factor;
            for (ox, out) in row[x0 * factor..(x0 + kept).min(w) * factor].iter_mut().enumerate() {
                let k = wxs[i][ox / factor] * fy;
                for c in 0..3 {
                    out[c] += answer[c * side * side + from + ox] * k;
                }
            }
        });
        progress(n + 1, total);
    }
    // What the answer changes broadly, at the image's own size.
    let broad: Vec<Vec<f32>> = (0..3)
        .map(|c| {
            let change: Vec<f32> = (0..w * h)
                .into_par_iter()
                .map(|i| {
                    let (x, y) = (i % w * factor, i / w * factor);
                    let block = (0..factor * factor).map(|k| sum[(y + k / factor) * ow + x + k % factor][c]).sum::<f32>();
                    block / (factor * factor) as f32 - f32::from(srgb.pixels()[i][c]) / 65535.0
                })
                .collect();
            blur(&change, w, h, BROAD)
        })
        .collect();
    // Along a side `len` long, the two pixels an enlarged one lies between,
    // and how far it is from the first to the second.
    let between = |o: usize, len: usize| {
        let u = ((o as f32 + 0.5) / factor as f32 - 0.5).clamp(0.0, (len - 1) as f32);
        (u as usize, (u as usize + 1).min(len - 1), u.fract())
    };
    let pixels = sum
        .par_iter()
        .enumerate()
        .map(|(i, p)| {
            let ((x0, x1, fx), (y0, y1, fy)) = (between(i % ow, w), between(i / ow, h));
            let [r, g, b] = [0, 1, 2].map(|c| {
                let at = |x: usize, y: usize| broad[c][y * w + x];
                let top = at(x0, y0) + (at(x1, y0) - at(x0, y0)) * fx;
                let bottom = at(x0, y1) + (at(x1, y1) - at(x0, y1)) * fx;
                ((p[c] - top - (bottom - top) * fy).clamp(0.0, 1.0) * 65535.0).round() as u16
            });
            [r, g, b, u16::MAX]
        })
        .collect();
    Ok(Raster::new(ow as u32, oh as u32, pixels))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A gradient across and down, a different one in each channel.
    fn gradient(w: u32, h: u32) -> Raster {
        let pixels = (0..w * h).map(|i| [10000 + (i % w) * 200, 20000 + (i / w) * 150, 30000, 65535].map(|v| v as u16)).collect();
        Raster::new(w, h, pixels)
    }

    /// A stand-in model: each pixel repeated `factor` times each way.
    fn repeat(size: usize, factor: usize) -> impl FnMut(&[f32]) -> Result<Vec<f32>, String> {
        move |square: &[f32]| {
            let side = size * factor;
            Ok((0..3 * side * side)
                .map(|i| {
                    let (c, x, y) = (i / (side * side), i % side / factor, i % (side * side) / side / factor);
                    square[c * size * size + y * size + x]
                })
                .collect())
        }
    }

    #[test]
    fn squares_are_put_back_where_they_came_from_at_the_larger_size() {
        // Squares of 64 over 150 × 100: overlapping, and one hanging off.
        let image = gradient(150, 100);
        for factor in [2, 4] {
            let mut seen = Vec::new();
            let out = run(&image, 64, factor, repeat(64, factor), |n, of| seen.push((n, of))).unwrap();
            assert_eq!(seen.last(), Some(&(12, 12)));
            assert_eq!((out.width(), out.height()), (150 * factor as u32, 100 * factor as u32));
            for (i, p) in out.pixels().iter().enumerate() {
                let (x, y) = (i as u32 % out.width() / factor as u32, i as u32 / out.width() / factor as u32);
                let want = image.get(x, y);
                assert!((0..4).all(|c| p[c].abs_diff(want[c]) <= 1), "×{factor} at {x}, {y}: {p:?} {want:?}");
            }
        }
        // Smaller than one square: reflected to fill it.
        let small = gradient(40, 30);
        let out = run(&small, 64, 2, repeat(64, 2), |_, _| {}).unwrap();
        assert_eq!(out.get(79, 59), small.get(39, 29));
    }

    #[test]
    fn the_result_is_the_size_asked_for_with_the_models_detail() {
        // A model that draws a fine checker over what it enlarges: detail
        // the bicubic enlargement doesn't have.
        let image = gradient(150, 100);
        let checker = |s: &[f32]| {
            let mut out = repeat(64, 2)(s)?;
            for (i, v) in out.iter_mut().enumerate() {
                *v += if (i % 128 + i % (128 * 128) / 128) % 2 == 0 { 0.02 } else { -0.02 };
            }
            Ok(out)
        };
        let srgb = ColorProfile::srgb();
        let out = upscale(&image, &srgb, (300, 200), (64, 2), checker, |_, _| {}).unwrap();
        assert_eq!((out.width(), out.height()), (300, 200));
        let (a, b) = (out.get(150, 100), out.get(151, 100));
        assert!(a[2].abs_diff(b[2]) > 2000, "{a:?} {b:?}");
        // Between the model's sizes, its answer is fitted to what's asked.
        let out = upscale(&image, &srgb, (225, 150), (64, 2), repeat(64, 2), |_, _| {}).unwrap();
        assert_eq!((out.width(), out.height()), (225, 150));
        let (p, want) = (out.get(112, 75), image.get(75, 50));
        assert!((0..4).all(|c| p[c].abs_diff(want[c]) < 300), "{p:?} {want:?}");
    }

    #[test]
    fn broad_changes_are_taken_out() {
        // A model that lightens red everywhere: not detail.
        let image = gradient(150, 100);
        let tint = |s: &[f32]| {
            let mut out = repeat(64, 2)(s)?;
            out[..128 * 128].iter_mut().for_each(|v| *v += 0.03);
            Ok(out)
        };
        let out = run(&image, 64, 2, tint, |_, _| {}).unwrap();
        for (x, y) in [(0, 0), (150, 100), (299, 199)] {
            let (p, want) = (out.get(x, y), image.get(x / 2, y / 2));
            assert!((0..3).all(|c| p[c].abs_diff(want[c]) < 30), "{p:?} {want:?}");
        }
    }

    #[test]
    fn transparency_and_colours_outside_srgb_come_from_the_ordinary_enlargement() {
        // A saturated ProPhoto green that sRGB can't hold, half transparent.
        let prophoto = crate::color::tests::linear_prophoto().with_gamma(1.8).unwrap();
        let image = Raster::new(16, 16, vec![[5000, 40000, 3000, 30000]; 256]);
        let out = upscale(&image, &prophoto, (32, 32), (16, 2), repeat(16, 2), |_, _| {}).unwrap();
        let p = out.get(16, 16);
        assert!((0..3).all(|c| p[c].abs_diff([5000, 40000, 3000][c]) <= 2) && p[3] == 30000, "{p:?}");
    }
}
