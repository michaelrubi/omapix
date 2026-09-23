use rayon::prelude::*;

use crate::{Pixel, Raster};

/// Levels smaller than this on their longest side are not built.
const SMALLEST_LEVEL: u32 = 128;

/// Successively halved copies of an image, used to draw it zoomed out
/// without aliasing. Level 0 is the full-size image, which the pyramid
/// borrows rather than copies.
pub struct Pyramid {
    reduced: Vec<Raster>,
}

impl Pyramid {
    pub fn build(base: &Raster) -> Self {
        let mut reduced: Vec<Raster> = Vec::new();
        loop {
            let prev = reduced.last().unwrap_or(base);
            if prev.width().max(prev.height()) <= SMALLEST_LEVEL {
                break;
            }
            reduced.push(halve(prev));
        }
        Self { reduced }
    }

    /// Number of levels including the full-size base.
    pub fn len(&self) -> usize {
        self.reduced.len() + 1
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    pub fn level<'a>(&'a self, base: &'a Raster, index: usize) -> &'a Raster {
        if index == 0 {
            base
        } else {
            &self.reduced[index - 1]
        }
    }
}

/// Halve an image with a 2×2 box filter. Odd edges average the pixels that
/// exist rather than reading past the border.
fn halve(src: &Raster) -> Raster {
    let (sw, sh) = (src.width(), src.height());
    let (w, h) = (sw.div_ceil(2), sh.div_ceil(2));
    let mut pixels = vec![[0u16; 4]; w as usize * h as usize];
    pixels
        .par_chunks_mut(w as usize)
        .enumerate()
        .for_each(|(y, row)| {
            let y0 = y as u32 * 2;
            let y1 = (y0 + 1).min(sh - 1);
            let (r0, r1) = (src.row(y0), src.row(y1));
            for (x, out) in row.iter_mut().enumerate() {
                let x0 = x * 2;
                let x1 = (x0 + 1).min(sw as usize - 1);
                *out = average([r0[x0], r0[x1], r1[x0], r1[x1]]);
            }
        });
    Raster::new(w, h, pixels)
}

fn average(px: [Pixel; 4]) -> Pixel {
    let mut out = [0u16; 4];
    for (c, o) in out.iter_mut().enumerate() {
        let sum: u32 = px.iter().map(|p| u32::from(p[c])).sum();
        *o = ((sum + 2) / 4) as u16;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halves_until_small_and_averages() {
        let base = Raster::new(600, 300, vec![[1000, 2000, 3000, 65535]; 600 * 300]);
        let p = Pyramid::build(&base);
        let sizes: Vec<_> = (0..p.len())
            .map(|i| (p.level(&base, i).width(), p.level(&base, i).height()))
            .collect();
        assert_eq!(sizes, vec![(600, 300), (300, 150), (150, 75), (75, 38)]);
        assert_eq!(p.level(&base, 3).get(0, 0), [1000, 2000, 3000, 65535]);
    }

    #[test]
    fn odd_edges_do_not_read_out_of_bounds() {
        let base = Raster::new(3, 3, (0..9).map(|v| [v * 100, 0, 0, 65535]).collect());
        let half = halve(&base);
        assert_eq!((half.width(), half.height()), (2, 2));
        // Bottom-right output covers only the single corner source pixel.
        assert_eq!(half.get(1, 1)[0], 800);
    }
}
