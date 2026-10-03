//! Warps, for Liquify and its face and body sliders (reshape.rs and
//! body.rs, see AI.md): a displacement field over the image, shaped by
//! brush dabs or by points that move, and images resampled through it.

use std::ops::RangeInclusive;

use rayon::prelude::*;

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
        Some(self.shown(&across, &down))
    }

    /// Add the smoothest warp that carries each of `moves`' first points to
    /// its second (a thin-plate spline through them) within `area` (x0, y0,
    /// x1, y1), scaled at each point by `fade` (0–1). A point that mustn't
    /// move is pinned by a move to itself. It takes three points or more,
    /// not all in a line. The spline is worked out at every `every`th grid
    /// point each way and interpolated between: it's smooth, so points far
    /// apart need no more.
    pub fn move_points(&mut self, moves: &[([f32; 2], [f32; 2])], area: [u32; 4], every: usize, fade: impl Fn([f32; 2]) -> f32 + Sync) {
        if let Some(spline) = Spline::through(moves) {
            self.add(area, every, |p| spline.at(p), fade);
        }
    }

    /// Add the warp that reads each point within `area` (x0, y0, x1, y1)
    /// from `offset` away, scaled at each point by `fade` (0–1). `offset` is
    /// worked out at every `every`th grid point each way and interpolated
    /// between.
    pub fn add(
        &mut self,
        [x0, y0, x1, y1]: [u32; 4],
        every: usize,
        offset: impl Fn([f32; 2]) -> [f32; 2] + Sync,
        fade: impl Fn([f32; 2]) -> f32 + Sync,
    ) {
        let range = |a: u32, b: u32, n: usize| a.div_ceil(STEP) as usize..=((b / STEP) as usize).min(n - 1);
        let (across, down) = (range(x0, x1, self.cols), range(y0, y1, self.rows));
        if across.is_empty() || down.is_empty() {
            return;
        }
        let (step, every) = (STEP as f32, every.max(1));
        // At every `every`th grid point from the first, to the last or just
        // past it.
        let (first, top) = (*across.start(), *down.start());
        let (wide, tall) = ((across.end() - first).div_ceil(every) + 1, (down.end() - top).div_ceil(every) + 1);
        let coarse: Vec<[f32; 2]> = (0..wide * tall)
            .into_par_iter()
            .map(|k| offset([(first + k % wide * every) as f32 * step, (top + k / wide * every) as f32 * step]))
            .collect();
        self.d.par_chunks_mut(self.cols).enumerate().filter(|(j, _)| down.contains(j)).for_each(|(j, row)| {
            let (b, fy) = ((j - top) / every, ((j - top) % every) as f32 / every as f32);
            let below = (b + 1).min(tall - 1);
            for i in across.clone() {
                let fade = fade([i as f32 * step, j as f32 * step]);
                if fade <= 0.0 {
                    continue;
                }
                let (a, fx) = ((i - first) / every, ((i - first) % every) as f32 / every as f32);
                let right = (a + 1).min(wide - 1);
                let at = |a: usize, b: usize| coarse[b * wide + a];
                let lerp = |p: [f32; 2], q: [f32; 2], t: f32| [p[0] + (q[0] - p[0]) * t, p[1] + (q[1] - p[1]) * t];
                let d = lerp(lerp(at(a, b), at(right, b), fx), lerp(at(a, below), at(right, below), fx), fy);
                row[i] = [row[i][0] + d[0] * fade, row[i][1] + d[1] * fade];
            }
        });
    }

    /// Put this warp over `under`, as one: `under` is done first, and this
    /// warps the result.
    pub fn over(&mut self, under: &Field) {
        let (cols, step) = (self.cols, STEP as f32);
        self.d.par_iter_mut().enumerate().for_each(|(k, d)| {
            let below = under.at((k % cols) as f32 * step + d[0], (k / cols) as f32 * step + d[1]);
            *d = [d[0] + below[0], d[1] + below[1]];
        });
    }

    /// The area of the image that changes with grid points `across` ×
    /// `down`: interpolation carries a point's change a step either side.
    fn shown(&self, across: &RangeInclusive<usize>, down: &RangeInclusive<usize>) -> [u32; 4] {
        let lo = |g: usize| g.saturating_sub(1) as u32 * STEP;
        let hi = |g: usize, n: u32| ((g as u32 + 1) * STEP + 1).min(n);
        [lo(*across.start()), lo(*down.start()), hi(*across.end(), self.width), hi(*down.end(), self.height)]
    }

    /// The grid's values over `area` of the image (x0, y0, x1, y1), to put
    /// back with [`Self::swap`].
    pub fn patch(&self, [x0, y0, x1, y1]: [u32; 4]) -> Patch {
        let range = |a: u32, b: u32, n: usize| (a / STEP) as usize..=(b.div_ceil(STEP) as usize).min(n - 1);
        let (across, down) = (range(x0, x1, self.cols), range(y0, y1, self.rows));
        let d = down.clone().flat_map(|j| across.clone().map(move |i| self.d[j * self.cols + i])).collect();
        Patch { across, down, d }
    }

    /// Put `patch` back, leaving what it replaced in it, so swapping it
    /// again undoes that. Returns the area of the image that changed.
    pub fn swap(&mut self, patch: &mut Patch) -> [u32; 4] {
        let cols = self.cols;
        let points = patch.down.clone().flat_map(|j| patch.across.clone().map(move |i| j * cols + i));
        for (k, v) in points.zip(&mut patch.d) {
            std::mem::swap(&mut self.d[k], v);
        }
        self.shown(&patch.across, &patch.down)
    }

    /// Scale the whole warp by `k`: 0 takes it all out.
    pub fn scale(&mut self, k: f32) {
        for v in &mut self.d {
            *v = v.map(|v| v * k);
        }
    }

    /// The area of the image the warp moves, if it moves any.
    pub fn extent(&self) -> Option<[u32; 4]> {
        let (mut across, mut down) = ((usize::MAX, 0), (usize::MAX, 0));
        for (k, _) in self.d.iter().enumerate().filter(|(_, v)| **v != [0.0; 2]) {
            let (i, j) = (k % self.cols, k / self.cols);
            across = (across.0.min(i), across.1.max(i));
            down = (down.0.min(j), down.1.max(j));
        }
        (across.0 <= across.1).then(|| self.shown(&(across.0..=across.1), &(down.0..=down.1)))
    }
}

/// A thin-plate spline: the displacement that's given at some points, and
/// bends the least in between.
struct Spline {
    /// The points, measured from `centre` in units of `scale`, so the sums
    /// stay near 1 wherever and however big they are.
    points: Vec<[f64; 2]>,
    centre: [f64; 2],
    scale: f64,
    /// For x and for y: each point's weight, then a constant and a slope
    /// each way.
    weights: [Vec<f64>; 2],
}

impl Spline {
    /// How closely the spline keeps to its points: a little slack, so points
    /// nearly on top of each other don't tear it.
    const SLACK: f64 = 1e-6;

    /// The spline that, where each of `moves` ends up, reads from where it
    /// was: what a [`Field`] holds. `None` if the points don't pin it down.
    fn through(moves: &[([f32; 2], [f32; 2])]) -> Option<Self> {
        let n = moves.len();
        let count = n.max(1) as f64;
        let centre = [0, 1].map(|c| moves.iter().map(|(_, to)| f64::from(to[c])).sum::<f64>() / count);
        let spread = moves.iter().map(|(_, to)| (f64::from(to[0]) - centre[0]).powi(2) + (f64::from(to[1]) - centre[1]).powi(2));
        let scale = (spread.sum::<f64>() / count).sqrt().max(1e-6);
        let points: Vec<[f64; 2]> = moves.iter().map(|(_, to)| [0, 1].map(|c| (f64::from(to[c]) - centre[c]) / scale)).collect();
        // Each point's displacement is the bends from every point, plus the
        // constant and slopes; and the bends balance, so far away it's flat.
        let size = n + 3;
        let mut rows = vec![vec![0.0f64; size + 2]; size];
        for (i, p) in points.iter().enumerate() {
            for (j, q) in points.iter().enumerate() {
                rows[i][j] = if i == j { Self::SLACK } else { bend(p, q) };
            }
            for (k, v) in [1.0, p[0], p[1]].into_iter().enumerate() {
                (rows[i][n + k], rows[n + k][i]) = (v, v);
            }
            let (from, to) = moves[i];
            (rows[i][size], rows[i][size + 1]) = (f64::from(from[0] - to[0]), f64::from(from[1] - to[1]));
        }
        // Gaussian elimination, with the largest pivot each time.
        for k in 0..size {
            let pivot = (k..size).max_by(|&a, &b| rows[a][k].abs().total_cmp(&rows[b][k].abs()))?;
            if rows[pivot][k].abs() < 1e-12 {
                return None;
            }
            rows.swap(k, pivot);
            let (above, below) = rows.split_at_mut(k + 1);
            let row = &above[k];
            for other in below {
                let factor = other[k] / row[k];
                if factor != 0.0 {
                    for c in k..size + 2 {
                        other[c] -= factor * row[c];
                    }
                }
            }
        }
        let mut weights = [vec![0.0f64; size], vec![0.0f64; size]];
        for (c, weights) in weights.iter_mut().enumerate() {
            for k in (0..size).rev() {
                let known: f64 = (k + 1..size).map(|j| rows[k][j] * weights[j]).sum();
                weights[k] = (rows[k][size + c] - known) / rows[k][k];
            }
        }
        Some(Self { points, centre, scale, weights })
    }

    /// The displacement at `v`, in pixels.
    fn at(&self, v: [f32; 2]) -> [f32; 2] {
        let v = [0, 1].map(|c| (f64::from(v[c]) - self.centre[c]) / self.scale);
        let n = self.points.len();
        let mut d = [0, 1].map(|c| self.weights[c][n] + self.weights[c][n + 1] * v[0] + self.weights[c][n + 2] * v[1]);
        for (i, p) in self.points.iter().enumerate() {
            let bend = bend(&v, p);
            d = [d[0] + self.weights[0][i] * bend, d[1] + self.weights[1][i] * bend];
        }
        d.map(|d| d as f32)
    }
}

/// How a thin plate bent at `q` rises at `p`: r² ln r, at a distance of r.
fn bend(p: &[f64; 2], q: &[f64; 2]) -> f64 {
    let r2 = (p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2);
    if r2 > 0.0 { 0.5 * r2 * r2.ln() } else { 0.0 }
}

/// Part of a [`Field`]'s grid, as it was.
pub struct Patch {
    across: RangeInclusive<usize>,
    down: RangeInclusive<usize>,
    d: Vec<[f32; 2]>,
}

/// Write `source` warped by `field` into `dest` within `area` (x0, y0, x1,
/// y1), returning the tiles written. With `under`, the source is warped by
/// that first, and `field` warps the result. With a `level`, `source`,
/// `dest` and `area` are the image's shrunk by 2^`level` each way, as the
/// display pyramid shrinks it: a quick look at the warp, zoomed out.
pub fn warp_area(
    source: &Tiled<Pixel>,
    field: &Field,
    under: Option<&Field>,
    dest: &mut Tiled<Pixel>,
    [x0, y0, x1, y1]: [u32; 4],
    level: u32,
) -> Vec<(u32, u32)> {
    if x0 >= x1 || y0 >= y1 {
        return Vec::new();
    }
    let (c0, c1, r0, r1) = (x0 / TILE, (x1 - 1) / TILE, y0 / TILE, (y1 - 1) / TILE);
    let fill = dest.fill();
    let scale = (1u32 << level) as f32;
    dest.par_update(|col, row, tile| {
        if !(c0..=c1).contains(&col) || !(r0..=r1).contains(&row) {
            return None;
        }
        let mut tile = tile.map_or_else(|| vec![fill; TILE_PIXELS], <[Pixel]>::to_vec);
        let (tx, ty) = (col * TILE, row * TILE);
        for y in y0.max(ty)..y1.min(ty + TILE) {
            for x in x0.max(tx)..x1.min(tx + TILE) {
                // The pixel's middle, in the image.
                let (ix, iy) = ((x as f32 + 0.5) * scale - 0.5, (y as f32 + 0.5) * scale - 0.5);
                let [mut dx, mut dy] = field.at(ix, iy);
                if let Some(under) = under {
                    let below = under.at(ix + dx, iy + dy);
                    (dx, dy) = (dx + below[0], dy + below[1]);
                }
                tile[((y - ty) * TILE + x - tx) as usize] = if dx == 0.0 && dy == 0.0 {
                    source.get(x, y)
                } else {
                    sample(source, x as f32 + dx / scale, y as f32 + dy / scale)
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
    warp_area(source, field, None, &mut out, [0, 0, source.width(), source.height()], 0);
    out
}

/// Bicubic sample of `image` at (x, y), repeating the edges.
fn sample(image: &Tiled<Pixel>, x: f32, y: f32) -> Pixel {
    let (w, h) = (image.width() as i64 - 1, image.height() as i64 - 1);
    let (fx, fy) = (x.floor(), y.floor());
    // The four pixels each way, and how much each counts.
    let weights = |f: f32, at: f32| [-1.0, 0.0, 1.0, 2.0].map(|k| Resampling::Bicubic.weight(f64::from(f + k - at)) as f32);
    let (wx, wy) = (weights(fx, x), weights(fy, y));
    let xs = [-1, 0, 1, 2].map(|i| (fx as i64 + i).clamp(0, w) as u32);
    let mut sum = [0.0f32; 4];
    for (j, wy) in (-1..=2).zip(wy) {
        let sy = (fy as i64 + j).clamp(0, h) as u32;
        // Mostly all four are in one tile, found once.
        let tile = (xs[0] / TILE == xs[3] / TILE).then(|| image.tile(xs[0] / TILE, sy / TILE));
        for (sx, wx) in xs.into_iter().zip(wx) {
            let v = match tile {
                Some(Some(tile)) => tile[((sy % TILE) * TILE + sx % TILE) as usize],
                Some(None) => image.fill(),
                None => image.get(sx, sy),
            };
            for (s, v) in sum.iter_mut().zip(v.to_f()) {
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
    fn a_patch_swapped_back_undoes_a_dab_and_swapped_again_redoes_it() {
        let mut field = Field::new(200, 200);
        assert_eq!(field.extent(), None);
        let before = field.clone();
        let area = field.dab(Brush::Bloat, [60.0, 70.0], 20.0, 0.5, [0.0; 2]).unwrap();
        let after = field.clone();
        let mut patch = before.patch(area);
        let swapped = field.swap(&mut patch);
        assert!(swapped[0] <= area[0] && swapped[1] <= area[1] && swapped[2] >= area[2] && swapped[3] >= area[3]);
        assert_eq!(field.d, before.d);
        field.swap(&mut patch);
        assert_eq!(field.d, after.d);
        let extent = field.extent().unwrap();
        assert!(extent[0] >= area[0] && extent[1] >= area[1] && extent[2] <= area[2] && extent[3] <= area[3]);
        field.scale(0.0);
        assert_eq!(field.extent(), None);
    }

    #[test]
    fn moved_points_carry_what_is_round_them_and_pinned_points_stay() {
        let image = disc(10.0);
        let mut field = Field::new(200, 200);
        // The disc goes 12 px right, with a ring of pins 60 px out.
        let pins = (0..16).map(|k| {
            let (sin, cos) = (k as f32 * std::f32::consts::TAU / 16.0).sin_cos();
            [100.0 + 60.0 * cos, 100.0 + 60.0 * sin]
        });
        let mut moves: Vec<_> = pins.map(|p| (p, p)).collect();
        moves.push(([100.0, 100.0], [112.0, 100.0]));
        field.move_points(&moves, [20, 20, 180, 180], 1, |_| 1.0);
        let at = field.at(112.0, 100.0);
        assert!((at[0] + 12.0).abs() < 1e-2 && at[1].abs() < 1e-2, "{at:?}");
        let pinned = field.at(160.0, 100.0);
        assert!(pinned[0].abs() < 1e-2 && pinned[1].abs() < 1e-2, "{pinned:?}");
        let out = warped(&image, &field);
        assert_eq!((out.get(105, 100), out.get(112, 100), out.get(119, 100)), (RED, RED, RED));
        assert_eq!(out.get(98, 100), GREY, "carried whole, not smeared");
        // Nothing outside the area, and nothing where it fades to nothing.
        let extent = field.extent().unwrap();
        assert!(extent[0] >= 16 && extent[1] >= 16 && extent[2] <= 185 && extent[3] <= 185, "{extent:?}");
        let mut faded = Field::new(200, 200);
        faded.move_points(&moves, [20, 20, 180, 180], 1, |_| 0.0);
        assert_eq!(faded.extent(), None);
    }

    #[test]
    fn a_spline_worked_out_coarsely_is_nearly_the_same() {
        let pins = (0..16).map(|k| {
            let (sin, cos) = (k as f32 * std::f32::consts::TAU / 16.0).sin_cos();
            [100.0 + 80.0 * cos, 100.0 + 80.0 * sin]
        });
        let mut moves: Vec<_> = pins.map(|p| (p, p)).collect();
        moves.extend([([70.0, 100.0], [82.0, 96.0]), ([130.0, 100.0], [122.0, 108.0])]);
        let (mut fine, mut coarse) = (Field::new(200, 200), Field::new(200, 200));
        fine.move_points(&moves, [10, 10, 190, 190], 1, |_| 1.0);
        // Every 12 px, not ending on the area's last grid point.
        coarse.move_points(&moves, [10, 10, 190, 190], 3, |_| 1.0);
        assert_eq!(coarse.extent(), fine.extent());
        let apart = fine.d.iter().zip(&coarse.d).map(|(a, b)| (a[0] - b[0]).hypot(a[1] - b[1])).fold(0.0, f32::max);
        assert!(apart < 1.0, "{apart}");
        assert!(fine.at(82.0, 96.0)[0] < -11.0 && coarse.at(82.0, 96.0)[0] < -11.0);
    }

    #[test]
    fn a_shrunk_image_is_warped_as_the_image_is() {
        // The disc at a quarter of its size, and a field for the whole
        // image that moves everything 8 px right and bloats the disc.
        let small: Vec<Pixel> = (0..50 * 50)
            .map(|i| {
                let (x, y) = ((i % 50) as f32 - 24.5, (i / 50) as f32 - 24.5);
                if x.hypot(y) <= 5.0 { RED } else { GREY }
            })
            .collect();
        let small = Tiled::from_slice(50, 50, [0; 4], &small);
        let mut field = Field::new(200, 200);
        let right = [[0.0, 0.0], [200.0, 0.0], [0.0, 200.0]].map(|p| (p, [p[0] + 8.0, p[1]]));
        field.move_points(&right, [0, 0, 200, 200], 1, |_| 1.0);
        let mut out = small.clone();
        let tiles = warp_area(&small, &field, None, &mut out, [0, 0, 50, 50], 2);
        assert_eq!(tiles, [(0, 0)]);
        // 2 px right at this size.
        for (x, y) in [(26, 24), (31, 24), (20, 24), (26, 29), (26, 31)] {
            assert_eq!(out.get(x, y), small.get(x - 2, y), "{x}, {y}");
        }
        assert_eq!((out.get(31, 24), out.get(20, 24)), (RED, GREY));
    }

    #[test]
    fn points_that_do_not_move_leave_no_warp() {
        let mut field = Field::new(200, 200);
        let pins = [[40.3, 50.7], [150.2, 60.1], [90.9, 160.4]].map(|p| (p, p));
        field.move_points(&pins, [0, 0, 200, 200], 1, |_| 1.0);
        assert_eq!(field.extent(), None);
    }

    #[test]
    fn a_warp_under_another_is_done_first() {
        let image = disc(10.0);
        // Under: everything 8 px right. Over: a bloat at the disc's new place.
        let mut under = Field::new(200, 200);
        let right = [[0.0, 0.0], [200.0, 0.0], [0.0, 200.0]].map(|p| (p, [p[0] + 8.0, p[1]]));
        under.move_points(&right, [0, 0, 200, 200], 1, |_| 1.0);
        let moved = warped(&image, &under);
        assert_eq!((moved.get(117, 100), moved.get(91, 100)), (RED, GREY));
        let mut over = Field::new(200, 200);
        over.dab(Brush::Bloat, [108.0, 100.0], 40.0, 0.3, [0.0; 2]);
        let mut both = image.clone();
        warp_area(&image, &over, Some(&under), &mut both, [0, 0, 200, 200], 0);
        let in_turn = warped(&moved, &over);
        // The same as one after the other, but for resampling twice.
        for (x, y) in [(108, 100), (120, 100), (96, 100), (108, 112), (60, 60)] {
            assert_eq!(both.get(x, y), in_turn.get(x, y), "{x}, {y}");
        }
        assert_eq!(both.get(120, 100), RED, "bloated");
    }

    #[test]
    fn a_warp_put_over_another_is_both_as_one() {
        let image = disc(10.0);
        let mut under = Field::new(200, 200);
        let right = [[0.0, 0.0], [200.0, 0.0], [0.0, 200.0]].map(|p| (p, [p[0] + 8.0, p[1]]));
        under.move_points(&right, [0, 0, 200, 200], 1, |_| 1.0);
        let mut over = Field::new(200, 200);
        over.dab(Brush::Bloat, [108.0, 100.0], 40.0, 0.3, [0.0; 2]);
        let mut in_turn = image.clone();
        warp_area(&image, &over, Some(&under), &mut in_turn, [0, 0, 200, 200], 0);
        // At a grid point: its own offset, and the one under where that
        // reads from.
        let own = over.at(100.0, 92.0);
        let below = under.at(100.0 + own[0], 92.0 + own[1]);
        over.over(&under);
        assert_eq!(over.at(100.0, 92.0), [own[0] + below[0], own[1] + below[1]]);
        let both = warped(&image, &over);
        for (x, y) in [(108, 100), (120, 100), (96, 100), (108, 112), (60, 60)] {
            assert_eq!(both.get(x, y), in_turn.get(x, y), "{x}, {y}");
        }
    }

    #[test]
    fn a_dab_changes_only_the_area_it_reports() {
        let image = disc(10.0);
        let mut field = Field::new(200, 200);
        let area = field.dab(Brush::ForwardWarp, [50.0, 60.0], 10.0, 1.0, [5.0, 3.0]).unwrap();
        let mut dest = image.clone();
        let tiles = warp_area(&image, &field, None, &mut dest, area, 0);
        assert_eq!(tiles, [(0, 0)]);
        assert_eq!(dest.to_vec(), warped(&image, &field).to_vec());
    }
}
