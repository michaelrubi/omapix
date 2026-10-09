//! A sampled brush tip: a picture each dab stamps, as Photoshop's brushes
//! made from an image are. Its longer side is the brush's diameter.
//!
//! The picture is kept at half its size, and half that, and so on, so a
//! small dab reads one close to its own size rather than skipping pixels
//! of the full one.
//!
//! Ported from PhotoCraft's `Mips` in `crates/paint/src/tile.rs`
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

use crate::dynamics::mix;

/// The longest side a tip is kept at: Photoshop's own limit is 5000 px,
/// and beyond this a dab is slower without looking any different.
pub const MAX_EDGE: usize = 2500;

#[derive(Debug, PartialEq)]
pub struct Tip {
    id: u64,
    /// Width, height and how much paint each pixel lays (65535 is all of
    /// it), at full size first.
    levels: Vec<(usize, usize, Vec<u16>)>,
}

impl Tip {
    /// A tip from its pixels, row by row. `None` if there are none, or not
    /// `width` × `height` of them.
    pub fn new(width: u32, height: u32, values: Vec<u16>) -> Option<Self> {
        let (w, h) = (width as usize, height as usize);
        if w == 0 || h == 0 || values.len() != w * h {
            return None;
        }
        let mut levels = vec![(w, h, values)];
        while let Some((w, h, v)) = levels.last().filter(|(w, h, _)| *w > 1 || *h > 1) {
            // Each pixel is the mean of the four it covers.
            let (nw, nh) = (w.div_ceil(2), h.div_ceil(2));
            let half = (0..nw * nh)
                .map(|i| {
                    let (x, y) = (i % nw * 2, i / nw * 2);
                    let four = [(x, y), (x + 1, y), (x, y + 1), (x + 1, y + 1)];
                    let there = four.iter().filter(|(x, y)| x < w && y < h);
                    let (sum, n) = there.fold((0u32, 0u32), |(s, n), (x, y)| (s + u32::from(v[y * w + x]), n + 1));
                    ((sum + n / 2) / n) as u16
                })
                .collect();
            levels.push((nw, nh, half));
        }
        while levels.len() > 1 && levels[0].0.max(levels[0].1) > MAX_EDGE {
            levels.remove(0);
        }
        // What tells one tip from another, in a brush's settings: its
        // size and pixels.
        let (w, h, v) = &levels[0];
        let id = v.iter().fold(mix((*w as u64) << 32 | *h as u64), |id, &v| mix(id ^ u64::from(v)));
        Some(Self { id, levels })
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    /// Width and height in pixels.
    pub fn size(&self) -> (usize, usize) {
        (self.levels[0].0, self.levels[0].1)
    }

    /// The smallest of its sizes that's at least `edge` on its longer
    /// side, for a thumbnail: width, height and pixels.
    pub fn at_least(&self, edge: usize) -> (usize, usize, &[u16]) {
        let level = self.levels.iter().rev().find(|(w, h, _)| *w.max(h) >= edge).unwrap_or(&self.levels[0]);
        (level.0, level.1, &level.2)
    }

    /// Which size to read for a dab `across` pixels on its longer side:
    /// the smallest that's no smaller than the dab.
    pub(crate) fn level_for(&self, across: f32) -> usize {
        let (w, h) = self.size();
        let ratio = (w.max(h) as f32 / across.max(1.0)).max(1.0);
        (ratio.log2().floor() as usize).min(self.levels.len() - 1)
    }

    /// The paint (0–1) at (`u`, `v`), each 0–1 across the tip, between
    /// its pixels' centres. Nothing outside it.
    #[inline]
    pub(crate) fn sample(&self, level: usize, u: f32, v: f32) -> f32 {
        let (w, h, d) = &self.levels[level];
        let (fx, fy) = (u * *w as f32 - 0.5, v * *h as f32 - 0.5);
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - x0, fy - y0);
        let (x0, y0) = (x0 as i64, y0 as i64);
        let get = |x: i64, y: i64| {
            let inside = x >= 0 && y >= 0 && x < *w as i64 && y < *h as i64;
            if inside { f32::from(d[y as usize * w + x as usize]) } else { 0.0 }
        };
        let a = get(x0, y0) + (get(x0 + 1, y0) - get(x0, y0)) * tx;
        let b = get(x0, y0 + 1) + (get(x0 + 1, y0 + 1) - get(x0, y0 + 1)) * tx;
        (a + (b - a) * ty) / 65535.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tip_halves_down_to_a_pixel_and_reads_between_pixels() {
        // Black on the left, full on the right.
        let tip = Tip::new(4, 2, vec![0, 0, 65535, 65535, 0, 0, 65535, 65535]).unwrap();
        assert_eq!(tip.size(), (4, 2));
        let sizes: Vec<_> = tip.levels.iter().map(|(w, h, _)| (*w, *h)).collect();
        assert_eq!(sizes, [(4, 2), (2, 1), (1, 1)]);
        assert_eq!(tip.levels[1].2, [0, 65535]);
        assert_eq!(tip.levels[2].2, [32768]);
        // At a pixel's centre, between two, and outside.
        assert_eq!(tip.sample(0, 0.125, 0.25), 0.0);
        assert_eq!(tip.sample(0, 0.875, 0.75), 1.0);
        assert!((tip.sample(0, 0.5, 0.5) - 0.5).abs() < 1e-6);
        assert_eq!(tip.sample(0, 1.5, 0.5), 0.0);
        // Fading out over the half pixel past its edge.
        assert!((tip.sample(0, 1.0, 0.5) - 0.5).abs() < 1e-6);

        // A dab the tip's size reads it whole, one half as big the half.
        assert_eq!((tip.level_for(4.0), tip.level_for(3.0), tip.level_for(2.0), tip.level_for(0.2)), (0, 0, 1, 2));
        assert_eq!(tip.at_least(2).0, 2);
        assert_eq!(tip.at_least(100).0, 4);
    }

    #[test]
    fn tips_are_told_apart_by_their_pixels_and_too_big_ones_are_halved() {
        let a = Tip::new(2, 2, vec![1, 2, 3, 4]).unwrap();
        assert_eq!(a.id(), Tip::new(2, 2, vec![1, 2, 3, 4]).unwrap().id());
        assert_ne!(a.id(), Tip::new(2, 2, vec![1, 2, 3, 5]).unwrap().id());
        assert_ne!(a.id(), Tip::new(4, 1, vec![1, 2, 3, 4]).unwrap().id());
        assert!(Tip::new(2, 2, vec![1, 2, 3]).is_none() && Tip::new(0, 2, vec![]).is_none());

        let big = Tip::new(6000, 2, vec![65535; 12_000]).unwrap();
        assert_eq!(big.size(), (1500, 1));
    }
}
