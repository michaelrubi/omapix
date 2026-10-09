//! The membrane: the smoothest surface through the pixels round a hole,
//! which is what fills it when there's nothing to copy, and what a fill's
//! tone is matched to ([`crate::inpaint`]). It solves Laplace's equation
//! over the hole (Pérez, Gangnet and Blake, "Poisson Image Editing", 2003)
//! by over-relaxation, at half the size first and so on down, so each size
//! starts close to its answer and needs only a few sweeps.
//!
//! Ported from PhotoCraft's `crates/algo/src/poisson.rs`
//! (<https://github.com/storytold/photocraft>, commit `ec477ca`), under its
//! MIT licence:
//!
//! Copyright (c) 2026 ArtCraft Team and the PhotoCraft contributors
//!
//! Permission is hereby granted, free of charge, to any person obtaining a copy
//! of this software and associated documentation files (the "Software"), to deal
//! in the Software without restriction, including without limitation the rights
//! to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
//! copies of the Software, and to permit persons to whom the Software is
//! furnished to do so, subject to the following conditions:
//!
//! The above copyright notice and this permission notice shall be included in all
//! copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//! IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//! FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//! AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//! LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
//! OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
//! SOFTWARE.

use rayon::prelude::*;

/// Sweeps stop once no pixel moves by more than this.
const TOLERANCE: f32 = 2e-5;
/// The most sweeps at each size above the smallest, and how far each
/// overshoots: within a thousandth of a full solve.
const SWEEPS: usize = 30;
const OVERSHOOT: f32 = 1.7;

/// Fill the `unknown` pixels of `v` (`w` × `h`) smoothly from the others.
/// Unchanged if there are none of either.
pub fn solve_membrane(w: usize, h: usize, unknown: &[bool], v: &mut [f32]) {
    assert_eq!(unknown.len(), w * h);
    assert_eq!(v.len(), w * h);
    solve(w, h, unknown, v, 0);
}

fn solve(w: usize, h: usize, unknown: &[bool], v: &mut [f32], depth: usize) {
    let holes = unknown.iter().filter(|u| **u).count();
    let (sum, known) = v.iter().zip(unknown).filter(|(_, u)| !**u).fold((0.0f32, 0usize), |(s, n), (x, _)| (s + x, n + 1));
    if holes == 0 || known == 0 {
        return;
    }
    if w < 8 || h < 8 || holes <= 64 || depth >= 16 {
        let mean = sum / known as f32;
        for (x, u) in v.iter_mut().zip(unknown) {
            if *u {
                *x = mean;
            }
        }
        return sweep(w, h, unknown, v, 2000, 1.85);
    }
    // At half the size, a pixel is known if any of its four is.
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let mut coarse_unknown = vec![true; cw * ch];
    let mut coarse = vec![0.0f32; cw * ch];
    for cy in 0..ch {
        for cx in 0..cw {
            let (mut s, mut n) = (0.0f32, 0u32);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let (x, y) = (cx * 2 + dx, cy * 2 + dy);
                if x < w && y < h && !unknown[y * w + x] {
                    s += v[y * w + x];
                    n += 1;
                }
            }
            if n > 0 {
                coarse_unknown[cy * cw + cx] = false;
                coarse[cy * cw + cx] = s / n as f32;
            }
        }
    }
    solve(cw, ch, &coarse_unknown, &mut coarse, depth + 1);
    // Its answer, interpolated, is where this size starts.
    for y in 0..h {
        let fy = ((y as f32 + 0.5) / 2.0 - 0.5).clamp(0.0, (ch - 1) as f32);
        let y0 = fy.floor() as usize;
        let y1 = (y0 + 1).min(ch - 1);
        let ty = fy - y0 as f32;
        for x in 0..w {
            if !unknown[y * w + x] {
                continue;
            }
            let fx = ((x as f32 + 0.5) / 2.0 - 0.5).clamp(0.0, (cw - 1) as f32);
            let x0 = fx.floor() as usize;
            let x1 = (x0 + 1).min(cw - 1);
            let tx = fx - x0 as f32;
            let a = coarse[y0 * cw + x0] + (coarse[y0 * cw + x1] - coarse[y0 * cw + x0]) * tx;
            let b = coarse[y1 * cw + x0] + (coarse[y1 * cw + x1] - coarse[y1 * cw + x0]) * tx;
            v[y * w + x] = a + (b - a) * ty;
        }
    }
    sweep(w, h, unknown, v, SWEEPS, OVERSHOOT);
}

/// Move each unknown pixel towards the mean of its neighbours, and a bit
/// beyond, up to `most` times.
fn sweep(w: usize, h: usize, unknown: &[bool], v: &mut [f32], most: usize, overshoot: f32) {
    let cells: Vec<usize> = (0..w * h).filter(|&i| unknown[i]).collect();
    for _ in 0..most {
        let mut moved = 0.0f32;
        for &i in &cells {
            let (x, y) = (i % w, i / w);
            let (mut s, mut n) = (0.0f32, 0.0f32);
            if x > 0 {
                s += v[i - 1];
                n += 1.0;
            }
            if x + 1 < w {
                s += v[i + 1];
                n += 1.0;
            }
            if y > 0 {
                s += v[i - w];
                n += 1.0;
            }
            if y + 1 < h {
                s += v[i + w];
                n += 1.0;
            }
            if n == 0.0 {
                continue;
            }
            let d = overshoot * (s / n - v[i]);
            v[i] += d;
            moved = moved.max(d.abs());
        }
        if moved < TOLERANCE {
            break;
        }
    }
}

/// `img` (`w` × `h`) with `hole` filled smoothly from the pixels round it,
/// with no texture.
pub fn membrane_fill<const N: usize>(w: usize, h: usize, img: &[[f32; N]], hole: &[bool]) -> Vec<[f32; N]> {
    let planes: Vec<Vec<f32>> = (0..N)
        .into_par_iter()
        .map(|c| {
            let mut v: Vec<f32> = img.iter().zip(hole).map(|(p, &hole)| if hole { 0.0 } else { p[c] }).collect();
            solve_membrane(w, h, hole, &mut v);
            v
        })
        .collect();
    (0..w * h).map(|i| std::array::from_fn(|c| planes[c][i])).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disc(w: usize, h: usize, cx: f32, cy: f32, r: f32) -> Vec<bool> {
        (0..w * h).map(|i| ((i % w) as f32 - cx).hypot((i / w) as f32 - cy) < r).collect()
    }

    #[test]
    fn a_hole_in_a_ramp_is_filled_with_the_ramp() {
        // A ramp is its own smoothest surface, so the hole comes back as
        // it was, whatever was in it.
        let (w, h) = (96, 72);
        let ramp: Vec<[f32; 3]> = (0..w * h).map(|i| [(i % w) as f32 / 96.0, (i / w) as f32 / 72.0, 0.5]).collect();
        let hole = disc(w, h, 48.0, 36.0, 20.0);
        let dirty: Vec<[f32; 3]> = ramp.iter().zip(&hole).map(|(p, &hole)| if hole { [9.0; 3] } else { *p }).collect();
        let out = membrane_fill(w, h, &dirty, &hole);
        for i in 0..w * h {
            for c in 0..3 {
                assert!((out[i][c] - ramp[i][c]).abs() < 2e-3, "{i} {c}: {} {}", out[i][c], ramp[i][c]);
            }
        }
    }

    #[test]
    fn the_fill_stays_between_the_values_round_it_and_leaves_them_alone() {
        let (w, h) = (64, 64);
        let mut v: Vec<f32> = (0..w * h).map(|i| if i % w < 32 { 0.2 } else { 0.8 }).collect();
        let before = v.clone();
        let hole = disc(w, h, 32.0, 32.0, 12.0);
        solve_membrane(w, h, &hole, &mut v);
        for i in 0..w * h {
            if hole[i] {
                assert!((0.2..=0.8).contains(&v[i]), "{}", v[i]);
            } else {
                assert_eq!(v[i], before[i]);
            }
        }
        // Midway between the two sides in the middle.
        assert!((v[32 * w + 32] - 0.5).abs() < 0.1, "{}", v[32 * w + 32]);

        // Nothing known: left as it was.
        let mut blank = vec![0.3f32; 16];
        solve_membrane(4, 4, &[true; 16], &mut blank);
        assert_eq!(blank, vec![0.3f32; 16]);
    }
}
