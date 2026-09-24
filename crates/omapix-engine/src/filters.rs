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

#[cfg(test)]
mod tests {
    use super::*;

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
}
