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

    /// Recompute the part of every level that depends on the base image
    /// rectangle `[x0, x1) × [y0, y1)`, after the base changed there.
    pub fn update_region(&mut self, base: &Raster, region: (u32, u32, u32, u32)) {
        self.update_from(base, 0, &[region]);
    }

    /// Recompute the levels above `level` where they depend on its
    /// rectangles `[x0, x1) × [y0, y1)` (in that level's pixels), after it
    /// changed there.
    pub fn update_from(&mut self, base: &Raster, level: usize, areas: &[(u32, u32, u32, u32)]) {
        let mut areas = areas.to_vec();
        for i in level..self.reduced.len() {
            let (before, rest) = self.reduced.split_at_mut(i);
            let src = before.last().unwrap_or(base);
            let dst = &mut rest[0];
            let (w, h) = (dst.width(), dst.height());
            for (x0, y0, x1, y1) in &mut areas {
                (*x0, *y0, *x1, *y1) = (*x0 / 2, *y0 / 2, x1.div_ceil(2).min(w), y1.div_ceil(2).min(h));
            }
            let top = areas.iter().map(|a| a.1).min().unwrap_or(0);
            let bottom = areas.iter().map(|a| a.3).max().unwrap_or(0).max(top);
            // A row to a core: rectangles that meet can share a pixel high
            // up, but a row is only ever one core's.
            let rows = &mut dst.pixels_mut()[(top * w) as usize..(bottom * w) as usize];
            rows.par_chunks_mut(w as usize).with_min_len(16).enumerate().for_each(|(dy, row)| {
                let y = top + dy as u32;
                for &(x0, _, x1, _) in areas.iter().filter(|a| a.1 <= y && y < a.3) {
                    for x in x0..x1 {
                        row[x as usize] = halved_pixel(src, x, y);
                    }
                }
            });
        }
    }

    /// A level to write into; follow with [`Self::update_from`].
    pub fn level_mut<'a>(&'a mut self, base: &'a mut Raster, index: usize) -> &'a mut Raster {
        if index == 0 {
            base
        } else {
            &mut self.reduced[index - 1]
        }
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
            for (x, out) in row.iter_mut().enumerate() {
                *out = halved_pixel(src, x as u32, y as u32);
            }
        });
    Raster::new(w, h, pixels)
}

/// One pixel of the half-size image: the average of the 2×2 block it
/// covers, clamped at the source's edges.
fn halved_pixel(src: &Raster, x: u32, y: u32) -> Pixel {
    let (sw, sh) = (src.width(), src.height());
    let (x0, y0) = (x * 2, y * 2);
    let (x1, y1) = ((x0 + 1).min(sw - 1), (y0 + 1).min(sh - 1));
    average([
        src.get(x0, y0),
        src.get(x1, y0),
        src.get(x0, y1),
        src.get(x1, y1),
    ])
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
    fn region_update_matches_full_rebuild() {
        let (w, h) = (700, 500);
        let mut base = Raster::new(w, h, vec![[1000, 2000, 3000, 65535]; (w * h) as usize]);
        let mut p = Pyramid::build(&base);
        for y in 100..180 {
            for px in &mut base.row_mut(y)[333..401] {
                *px = [60000, 100, 5000, 65535];
            }
        }
        p.update_region(&base, (333, 100, 401, 180));
        let fresh = Pyramid::build(&base);
        for i in 0..p.len() {
            assert_eq!(
                p.level(&base, i).pixels(),
                fresh.level(&base, i).pixels(),
                "level {i}"
            );
        }
    }

    #[test]
    fn updates_from_a_level_reach_the_levels_above_it() {
        let (w, h) = (700, 500);
        let mut base = Raster::new(w, h, vec![[1000, 2000, 3000, 65535]; (w * h) as usize]);
        let mut p = Pyramid::build(&base);
        let level = p.level_mut(&mut base, 1);
        for y in 50..90 {
            for px in &mut level.row_mut(y)[166..201] {
                *px = [60000, 100, 5000, 65535];
            }
        }
        p.update_from(&base, 1, &[(166, 50, 201, 90)]);
        let fresh = Pyramid::build(p.level(&base, 1));
        for i in 2..p.len() {
            assert_eq!(
                p.level(&base, i).pixels(),
                fresh.level(&base, i - 1).pixels(),
                "level {i}"
            );
        }
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
