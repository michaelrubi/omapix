//! Warps, for Liquify (and later Face Symmetry and Reshape, see AI.md): a
//! displacement field over the image, shaped by brush dabs, and images
//! resampled through it.

use crate::Pixel;
use crate::tiled::{TILE, TILE_PIXELS, Tiled};
use crate::transform::{Resample, Resampling};

/// Pixels between the field's grid points.
pub const STEP: u32 = 4;

/// Liquify's brushes, with Photoshop's keys.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Brush {
    /// W: pushes pixels along with the pointer.
    #[default]
    ForwardWarp,
    /// R: takes the warp back out.
    Reconstruct,
    /// S: pulls pixels in towards the brush's centre.
    Pucker,
    /// B: pushes them out from it.
    Bloat,
    /// O: pushes pixels to the left of the way the pointer goes (Alt: right).
    PushLeft,
}

impl Brush {
    pub const ALL: [Brush; 5] = [Brush::ForwardWarp, Brush::Reconstruct, Brush::Pucker, Brush::Bloat, Brush::PushLeft];

    pub fn name(self) -> &'static str {
        match self {
            Brush::ForwardWarp => "Forward Warp",
            Brush::Reconstruct => "Reconstruct",
            Brush::Pucker => "Pucker",
            Brush::Bloat => "Bloat",
            Brush::PushLeft => "Push Left",
        }
    }

    /// Keeps working while the button is held still, not only as the
    /// pointer moves.
    pub fn continuous(self) -> bool {
        matches!(self, Brush::Reconstruct | Brush::Pucker | Brush::Bloat)
    }
}

/// Where each pixel of the result comes from: the pixel at (x, y) is the
/// source's at (x, y) plus the field there. Held on a grid every [`STEP`]
/// pixels, and interpolated in between.
#[derive(Clone)]
pub struct Field {
    width: u32,
    height: u32,
    cols: usize,
    rows: usize,
    d: Vec<[f32; 2]>,
}

impl Field {
    /// No warp, over a `width` × `height` image.
    pub fn new(width: u32, height: u32) -> Self {
        let (cols, rows) = ((width.div_ceil(STEP) + 1) as usize, (height.div_ceil(STEP) + 1) as usize);
        Self { width, height, cols, rows, d: vec![[0.0; 2]; cols * rows] }
    }

    /// The displacement at (x, y), in pixels.
    pub fn at(&self, x: f32, y: f32) -> [f32; 2] {
        let step = STEP as f32;
        let gx = (x / step).clamp(0.0, (self.cols - 1) as f32);
        let gy = (y / step).clamp(0.0, (self.rows - 1) as f32);
        let (i, j) = ((gx as usize).min(self.cols - 2), (gy as usize).min(self.rows - 2));
        let (fx, fy) = (gx - i as f32, gy - j as f32);
        let g = |i: usize, j: usize| self.d[j * self.cols + i];
        let lerp = |a: [f32; 2], b: [f32; 2], t: f32| [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
        lerp(lerp(g(i, j), g(i + 1, j), fx), lerp(g(i, j + 1), g(i + 1, j + 1), fx), fy)
    }

    /// One dab of `brush` centred on `c`, `radius` pixels, at strength
    /// `amount` (0–1). `motion` is how far the pointer moved since the last
    /// dab, for Forward Warp and Push Left. Returns the area of the image it
    /// changed, (x0, y0, x1, y1).
    pub fn dab(&mut self, brush: Brush, c: [f32; 2], radius: f32, amount: f32, motion: [f32; 2]) -> Option<[u32; 4]> {
        let (r, step) = (radius.max(1.0), STEP as f32);
        let range = |c: f32, n: usize| {
            let lo = ((c - r) / step).ceil().max(0.0) as usize;
            let hi = (((c + r) / step).floor().max(0.0) as usize).min(n - 1);
            lo..=hi
        };
        let (across, down) = (range(c[0], self.cols), range(c[1], self.rows));
        let push = match brush {
            Brush::PushLeft => [motion[1], -motion[0]],
            _ => motion,
        };
        let changes: Vec<(usize, [f32; 2])> = down
            .clone()
            .flat_map(|j| across.clone().map(move |i| (i, j)))
            .filter_map(|(i, j)| {
                let p = [i as f32 * step, j as f32 * step];
                let t = (p[0] - c[0]).hypot(p[1] - c[1]) / r;
                if t >= 1.0 {
                    return None;
                }
                let w = (1.0 - t * t).powi(2) * amount;
                let k = j * self.cols + i;
                // Where p reads from shifts by `shift`; what was read from
                // there before is what p shows now.
                let shift = match brush {
                    Brush::Reconstruct => return Some((k, self.d[k].map(|v| v * (1.0 - w.min(1.0))))),
                    Brush::ForwardWarp | Brush::PushLeft => [-w * push[0], -w * push[1]],
                    Brush::Bloat => [-w * (p[0] - c[0]), -w * (p[1] - c[1])],
                    Brush::Pucker => [w * (p[0] - c[0]), w * (p[1] - c[1])],
                };
                let before = self.at(p[0] + shift[0], p[1] + shift[1]);
                Some((k, [before[0] + shift[0], before[1] + shift[1]]))
            })
            .collect();
        if changes.is_empty() {
            return None;
        }
        for (k, v) in changes {
            self.d[k] = v;
        }
        // Interpolation carries a grid point's change a step either side.
        let lo = |g: usize| g.saturating_sub(1) as u32 * STEP;
        let hi = |g: usize, n: u32| ((g as u32 + 1) * STEP + 1).min(n);
        Some([lo(*across.start()), lo(*down.start()), hi(*across.end(), self.width), hi(*down.end(), self.height)])
    }
}

/// Write `source` warped by `field` into `dest` within `area` (x0, y0, x1,
/// y1), returning the tiles written.
pub fn warp_area(source: &Tiled<Pixel>, field: &Field, dest: &mut Tiled<Pixel>, [x0, y0, x1, y1]: [u32; 4]) -> Vec<(u32, u32)> {
    if x0 >= x1 || y0 >= y1 {
        return Vec::new();
    }
    let (c0, c1, r0, r1) = (x0 / TILE, (x1 - 1) / TILE, y0 / TILE, (y1 - 1) / TILE);
    let fill = dest.fill();
    dest.par_update(|col, row, tile| {
        if !(c0..=c1).contains(&col) || !(r0..=r1).contains(&row) {
            return None;
        }
        let mut tile = tile.map_or_else(|| vec![fill; TILE_PIXELS], <[Pixel]>::to_vec);
        let (tx, ty) = (col * TILE, row * TILE);
        for y in y0.max(ty)..y1.min(ty + TILE) {
            for x in x0.max(tx)..x1.min(tx + TILE) {
                let [dx, dy] = field.at(x as f32, y as f32);
                tile[((y - ty) * TILE + x - tx) as usize] = if dx == 0.0 && dy == 0.0 {
                    source.get(x, y)
                } else {
                    sample(source, x as f32 + dx, y as f32 + dy)
                };
            }
        }
        Some(tile)
    });
    (r0..=r1).flat_map(|r| (c0..=c1).map(move |c| (c, r))).collect()
}

/// `source` warped by `field`.
pub fn warped(source: &Tiled<Pixel>, field: &Field) -> Tiled<Pixel> {
    let mut out = source.clone();
    warp_area(source, field, &mut out, [0, 0, source.width(), source.height()]);
    out
}

/// Bicubic sample of `image` at (x, y), repeating the edges.
fn sample(image: &Tiled<Pixel>, x: f32, y: f32) -> Pixel {
    let (w, h) = (image.width() as i64 - 1, image.height() as i64 - 1);
    let (fx, fy) = (x.floor(), y.floor());
    let mut sum = [0.0f32; 4];
    for j in -1..=2 {
        let wy = Resampling::Bicubic.weight(f64::from(fy + j as f32 - y)) as f32;
        let sy = (fy as i64 + j).clamp(0, h) as u32;
        for i in -1..=2 {
            let wx = Resampling::Bicubic.weight(f64::from(fx + i as f32 - x)) as f32;
            let sx = (fx as i64 + i).clamp(0, w) as u32;
            let v = image.get(sx, sy).to_f();
            for (s, v) in sum.iter_mut().zip(v) {
                *s += v * wx * wy;
            }
        }
    }
    Pixel::from_f(sum)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Pixel = [65535, 0, 0, 65535];
    const GREY: Pixel = [30000, 30000, 30000, 65535];

    /// A red disc of `radius` at (100, 100) on grey, 200 × 200.
    fn disc(radius: f32) -> Tiled<Pixel> {
        let px: Vec<Pixel> = (0..200 * 200)
            .map(|i| {
                let (x, y) = ((i % 200) as f32 - 100.0, (i / 200) as f32 - 100.0);
                if x.hypot(y) <= radius { RED } else { GREY }
            })
            .collect();
        Tiled::from_slice(200, 200, [0; 4], &px)
    }

    #[test]
    fn no_warp_gives_the_image_back_exactly() {
        let image = disc(20.0);
        let out = warped(&image, &Field::new(200, 200));
        assert_eq!(out.to_vec(), image.to_vec());
    }

    #[test]
    fn forward_warp_carries_pixels_with_the_pointer_and_reconstruct_takes_it_back() {
        let image = disc(10.0);
        let mut field = Field::new(200, 200);
        // Dragging 12 px right from the disc's centre, in small steps.
        for n in 0..12 {
            field.dab(Brush::ForwardWarp, [100.0 + n as f32, 100.0], 40.0, 1.0, [1.0, 0.0]);
        }
        let out = warped(&image, &field);
        assert_eq!(out.get(118, 100), RED, "the disc's right edge moved right");
        assert_eq!(out.get(100, 60), GREY, "outside the brush nothing moves");
        // Reconstruct at full strength puts it back where it bites.
        let area = field.dab(Brush::Reconstruct, [100.0, 100.0], 60.0, 1.0, [0.0; 2]).unwrap();
        assert!(field.at(100.0, 100.0)[0].abs() < 1e-3);
        assert!(area[0] <= 40 && area[2] >= 160, "{area:?}");
    }

    #[test]
    fn bloat_grows_and_pucker_shrinks_what_is_under_the_brush() {
        let image = disc(10.0);
        let mut grow = Field::new(200, 200);
        let mut shrink = Field::new(200, 200);
        for _ in 0..5 {
            grow.dab(Brush::Bloat, [100.0, 100.0], 40.0, 0.1, [0.0; 2]);
            shrink.dab(Brush::Pucker, [100.0, 100.0], 40.0, 0.1, [0.0; 2]);
        }
        assert_eq!(warped(&image, &grow).get(113, 100), RED);
        assert_eq!(warped(&image, &shrink).get(108, 100), GREY);
    }

    #[test]
    fn push_left_moves_pixels_left_of_the_way_the_pointer_goes() {
        let image = disc(10.0);
        let mut field = Field::new(200, 200);
        // Dragging up pushes left.
        for n in 0..10 {
            field.dab(Brush::PushLeft, [100.0, 105.0 - n as f32], 30.0, 1.0, [0.0, -1.0]);
        }
        let out = warped(&image, &field);
        assert_eq!(out.get(87, 100), RED);
        assert_eq!(out.get(108, 100), GREY);
    }

    #[test]
    fn a_dab_changes_only_the_area_it_reports() {
        let image = disc(10.0);
        let mut field = Field::new(200, 200);
        let area = field.dab(Brush::ForwardWarp, [50.0, 60.0], 10.0, 1.0, [5.0, 3.0]).unwrap();
        let mut dest = image.clone();
        let tiles = warp_area(&image, &field, &mut dest, area);
        assert_eq!(tiles, [(0, 0)]);
        assert_eq!(dest.to_vec(), warped(&image, &field).to_vec());
    }
}
