//! Free Transform: layers, masks and selections scaled, rotated and moved
//! by an affine transform, resampled as Photoshop does (bicubic, or
//! bilinear while dragging).

use rayon::prelude::*;

use crate::Pixel;
use crate::tiled::{TILE, Tiled};

/// Maps (x, y) to (a·x + b·y + c, d·x + e·y + f), in image pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine([f64; 6]);

impl Affine {
    pub const IDENTITY: Affine = Affine([1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);

    pub fn translate(dx: f64, dy: f64) -> Self {
        Affine([1.0, 0.0, dx, 0.0, 1.0, dy])
    }

    /// Scaled by (`sx`, `sy`) about `centre`.
    pub fn scale_about(sx: f64, sy: f64, (cx, cy): (f64, f64)) -> Self {
        Affine([sx, 0.0, cx - sx * cx, 0.0, sy, cy - sy * cy])
    }

    /// Turned by `angle` radians (clockwise on screen) about `centre`.
    pub fn rotate_about(angle: f64, (cx, cy): (f64, f64)) -> Self {
        let (s, c) = angle.sin_cos();
        Affine([c, -s, cx - c * cx + s * cy, s, c, cy - s * cx - c * cy])
    }

    /// This, then `next`.
    pub fn then(&self, next: &Affine) -> Affine {
        let [a, b, c, d, e, f] = self.0;
        let [p, q, r, s, t, u] = next.0;
        Affine([
            p * a + q * d,
            p * b + q * e,
            p * c + q * f + r,
            s * a + t * d,
            s * b + t * e,
            s * c + t * f + u,
        ])
    }

    pub fn apply(&self, (x, y): (f64, f64)) -> (f64, f64) {
        let [a, b, c, d, e, f] = self.0;
        (a * x + b * y + c, d * x + e * y + f)
    }

    pub fn inverse(&self) -> Option<Affine> {
        let [a, b, c, d, e, f] = self.0;
        let det = a * e - b * d;
        if det.abs() < 1e-12 {
            return None;
        }
        let (ia, ib, id, ie) = (e / det, -b / det, -d / det, a / det);
        Some(Affine([ia, ib, -(ia * c + ib * f), id, ie, -(id * c + ie * f)]))
    }

    /// How much it scales along x and y (the lengths of its columns), and
    /// the angle it turns the x axis by, in radians.
    pub fn decompose(&self) -> (f64, f64, f64) {
        let [a, b, _, d, e, _] = self.0;
        let sx = a.hypot(d);
        let det = a * e - b * d;
        (sx, det / sx, d.atan2(a))
    }

    /// (a, b, c, d, e, f): (x, y) goes to (a·x + b·y + c, d·x + e·y + f).
    pub fn coefficients(&self) -> [f64; 6] {
        self.0
    }

    /// The same transform in units `k` pixels wide (a pyramid level).
    pub fn in_units(&self, k: f64) -> Affine {
        let [a, b, c, d, e, f] = self.0;
        Affine([a, b, c / k, d, e, f / k])
    }

    /// The whole-pixel move it is, if that's all it is.
    fn whole_pixel_move(&self) -> Option<(i32, i32)> {
        let [a, b, c, d, e, f] = self.0;
        let whole = |v: f64| (v - v.round()).abs() < 1e-6;
        (a == 1.0 && b == 0.0 && d == 0.0 && e == 1.0 && whole(c) && whole(f))
            .then(|| (c.round() as i32, f.round() as i32))
    }
}

/// Bilinear is fast, for previews while dragging; bicubic is sharper, for
/// the result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resampling {
    Bilinear,
    Bicubic,
}

impl Resampling {
    /// How far the kernel reaches, in source pixels at full size.
    fn reach(self) -> f64 {
        match self {
            Self::Bilinear => 1.0,
            Self::Bicubic => 2.0,
        }
    }

    fn weight(self, t: f64) -> f64 {
        let t = t.abs();
        match self {
            Self::Bilinear => (1.0 - t).max(0.0),
            // Catmull-Rom, as Photoshop's Bicubic.
            Self::Bicubic if t < 1.0 => 1.5 * t * t * t - 2.5 * t * t + 1.0,
            Self::Bicubic if t < 2.0 => -0.5 * t * t * t + 2.5 * t * t - 4.0 * t + 2.0,
            Self::Bicubic => 0.0,
        }
    }
}

/// Values that can be resampled: as up to four premultiplied channels.
pub trait Resample: Copy + PartialEq + Send + Sync {
    fn to_f(self) -> [f32; 4];
    fn from_f(v: [f32; 4]) -> Self;
}

impl Resample for Pixel {
    fn to_f(self) -> [f32; 4] {
        let a = f32::from(self[3]);
        let k = a / (65535.0 * 65535.0);
        [f32::from(self[0]) * k, f32::from(self[1]) * k, f32::from(self[2]) * k, a / 65535.0]
    }

    fn from_f([r, g, b, a]: [f32; 4]) -> Self {
        let a = a.clamp(0.0, 1.0);
        if a <= 0.0 {
            return [0; 4];
        }
        let c = |v: f32| ((v / a).clamp(0.0, 1.0) * 65535.0).round() as u16;
        [c(r), c(g), c(b), (a * 65535.0).round() as u16]
    }
}

impl Resample for u16 {
    fn to_f(self) -> [f32; 4] {
        [f32::from(self), 0.0, 0.0, 0.0]
    }

    fn from_f(v: [f32; 4]) -> Self {
        v[0].round().clamp(0.0, 65535.0) as u16
    }
}

/// `image` transformed by `t`, reading as `outside` wherever nothing lands.
/// A whole-pixel move is exact. Only the tiles that can change are worked
/// out: where `image` isn't `outside` (all of it if its fill isn't).
pub fn transformed<T: Resample>(image: &Tiled<T>, t: &Affine, outside: T, resampling: Resampling) -> Tiled<T> {
    let (w, h) = (image.width(), image.height());
    if let Some((dx, dy)) = t.whole_pixel_move() {
        return image.translated(dx, dy, outside);
    }
    let (Some(inverse), Some((x0, y0, x1, y1))) = (t.inverse(), content_bounds(image, outside)) else {
        return Tiled::new(w, h, outside);
    };
    // The content, with a margin of `outside` for the kernel to read.
    let reach = resampling.reach();
    let (sx, sy, _) = inverse.decompose();
    // Shrinking, the kernel widens to cover the pixels that merge.
    let stretch = (sx.max(sy.abs())).max(1.0);
    let margin = (reach * stretch).ceil() as i64 + 1;
    let (bx, by) = (x0 as i64 - margin, y0 as i64 - margin);
    let (bw, bh) = ((x1 - x0) as i64 + 2 * margin, (y1 - y0) as i64 + 2 * margin);
    let source: Vec<[f32; 4]> = (0..bh)
        .into_par_iter()
        .flat_map_iter(|y| {
            (0..bw).map(move |x| {
                let (x, y) = (bx + x, by + y);
                let inside = x >= 0 && y >= 0 && x < i64::from(w) && y < i64::from(h);
                if inside { image.get(x as u32, y as u32) } else { outside }.to_f()
            })
        })
        .collect();
    let read = |x: i64, y: i64| {
        let (x, y) = (x - bx, y - by);
        if x < 0 || y < 0 || x >= bw || y >= bh {
            outside.to_f()
        } else {
            source[(y * bw + x) as usize]
        }
    };

    // Where the content lands.
    let corners = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)].map(|(x, y)| t.apply((f64::from(x), f64::from(y))));
    let lo = |i: usize| corners.iter().map(|c| if i == 0 { c.0 } else { c.1 }).fold(f64::MAX, f64::min);
    let hi = |i: usize| corners.iter().map(|c| if i == 0 { c.0 } else { c.1 }).fold(f64::MIN, f64::max);
    let (dx0, dy0) = ((lo(0) - 1.0).floor(), (lo(1) - 1.0).floor());
    let (dx1, dy1) = ((hi(0) + 1.0).ceil(), (hi(1) + 1.0).ceil());

    let r = reach * stretch;
    Tiled::from_tiles(w, h, outside, |col, row| {
        let (tx, ty) = (f64::from(col * TILE), f64::from(row * TILE));
        let (tw, th) = (f64::from(TILE.min(w - col * TILE)), f64::from(TILE.min(h - row * TILE)));
        if tx + tw <= dx0 || ty + th <= dy0 || tx >= dx1 || ty >= dy1 {
            return None;
        }
        let mut tile = vec![outside; (TILE * TILE) as usize];
        for j in 0..th as u32 {
            for i in 0..tw as u32 {
                let (u, v) = inverse.apply((tx + f64::from(i) + 0.5, ty + f64::from(j) + 0.5));
                let (u, v) = (u - 0.5, v - 0.5);
                let mut sum = [0.0f32; 4];
                let mut total = 0.0;
                for y in (v - r).floor() as i64 + 1..=(v + r).floor() as i64 {
                    let wy = resampling.weight((y as f64 - v) / stretch);
                    if wy == 0.0 {
                        continue;
                    }
                    for x in (u - r).floor() as i64 + 1..=(u + r).floor() as i64 {
                        let wxy = resampling.weight((x as f64 - u) / stretch) * wy;
                        if wxy == 0.0 {
                            continue;
                        }
                        let s = read(x, y);
                        for k in 0..4 {
                            sum[k] += s[k] * wxy as f32;
                        }
                        total += wxy;
                    }
                }
                if total != 0.0 {
                    tile[(j * TILE + i) as usize] = T::from_f(sum.map(|s| s / total as f32));
                }
            }
        }
        Some(tile)
    })
}

/// `image` resampled to `width` × `height`, for Image › Image Size: in two
/// passes, across then down. Pixels past the edges repeat the edge, so an
/// opaque layer stays opaque to the border.
pub fn resized<T: Resample>(image: &Tiled<T>, width: u32, height: u32, resampling: Resampling) -> Tiled<T> {
    let (w, h) = (image.width(), image.height());
    if (width, height) == (w, h) {
        return image.clone();
    }
    let fill = image.fill();
    let written = (0..image.rows()).any(|r| (0..image.cols()).any(|c| image.tile(c, r).is_some()));
    if !written {
        return Tiled::new(width, height, fill);
    }
    let (across, down) = (taps(w, width, resampling), taps(h, height, resampling));
    let source = image.to_vec();
    let mut rows = vec![[0.0f32; 4]; width as usize * h as usize];
    rows.par_chunks_mut(width as usize).enumerate().for_each(|(y, out)| {
        let line = &source[y * w as usize..(y + 1) * w as usize];
        for (o, taps) in out.iter_mut().zip(&across) {
            *o = weighted(taps.iter().map(|&(x, k)| (line[x as usize].to_f(), k)));
        }
    });
    Tiled::from_tiles(width, height, fill, |col, row| {
        let (x0, y0) = (col * TILE, row * TILE);
        let (tw, th) = (TILE.min(width - x0), TILE.min(height - y0));
        let mut tile = vec![fill; (TILE * TILE) as usize];
        for j in 0..th {
            let taps = &down[(y0 + j) as usize];
            for i in 0..tw {
                let x = (x0 + i) as usize;
                let v = weighted(taps.iter().map(|&(y, k)| (rows[y as usize * width as usize + x], k)));
                tile[(j * TILE + i) as usize] = T::from_f(v);
            }
        }
        tile.iter().any(|&p| p != fill).then_some(tile)
    })
}

/// For each of `to` pixels along one axis, the `from` pixels it reads and
/// their weights (summing to 1), with the edge pixels repeated.
fn taps(from: u32, to: u32, resampling: Resampling) -> Vec<Vec<(u32, f32)>> {
    let scale = f64::from(from) / f64::from(to);
    // Shrinking, the kernel widens to cover the pixels that merge.
    let stretch = scale.max(1.0);
    let r = resampling.reach() * stretch;
    (0..to)
        .map(|i| {
            let c = (f64::from(i) + 0.5) * scale - 0.5;
            let near = (c - r).floor() as i64 + 1..=(c + r).floor() as i64;
            let taps: Vec<_> = near
                .map(|s| (s.clamp(0, i64::from(from) - 1) as u32, resampling.weight((s as f64 - c) / stretch)))
                .filter(|&(_, k)| k != 0.0)
                .collect();
            let total: f64 = taps.iter().map(|&(_, k)| k).sum();
            taps.into_iter().map(|(s, k)| (s, (k / total) as f32)).collect()
        })
        .collect()
}

fn weighted(samples: impl Iterator<Item = ([f32; 4], f32)>) -> [f32; 4] {
    let mut sum = [0.0f32; 4];
    for (s, k) in samples {
        for (a, b) in sum.iter_mut().zip(s) {
            *a += b * k;
        }
    }
    sum
}

/// The smallest tile-aligned area outside of which `image` is `outside`,
/// as (x0, y0, x1, y1), or `None` if it's all `outside`.
fn content_bounds<T: Resample>(image: &Tiled<T>, outside: T) -> Option<(u32, u32, u32, u32)> {
    let (w, h) = (image.width(), image.height());
    if image.fill() != outside {
        return Some((0, 0, w, h));
    }
    let tiles = (0..image.rows()).flat_map(|row| (0..image.cols()).map(move |col| (col, row)));
    let present: Vec<_> = tiles.filter(|&(c, r)| image.tile(c, r).is_some()).collect();
    let (c0, r0) = present.iter().fold((u32::MAX, u32::MAX), |(c, r), &(x, y)| (c.min(x), r.min(y)));
    let (c1, r1) = present.iter().fold((0, 0), |(c, r), &(x, y)| (c.max(x + 1), r.max(y + 1)));
    (!present.is_empty()).then(|| (c0 * TILE, r0 * TILE, (c1 * TILE).min(w), (r1 * TILE).min(h)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Pixel = [65535, 0, 0, 65535];

    /// A red square from (100, 100) to (200, 200) on a transparent 600 × 400 layer.
    fn square() -> Tiled<Pixel> {
        let mut t = Tiled::new(600, 400, [0; 4]);
        for y in 100..200 {
            for x in 100..200 {
                t.tile_mut(x / TILE, y / TILE)[((y % TILE) * TILE + x % TILE) as usize] = RED;
            }
        }
        t
    }

    #[test]
    fn affine_composes_and_inverts() {
        let t = Affine::scale_about(2.0, 3.0, (10.0, 20.0)).then(&Affine::rotate_about(0.7, (5.0, 5.0)));
        let p = t.apply((33.0, -4.0));
        let back = t.inverse().unwrap().apply(p);
        assert!((back.0 - 33.0).abs() < 1e-9 && (back.1 + 4.0).abs() < 1e-9);
        let (sx, sy, angle) = Affine::scale_about(2.0, 3.0, (0.0, 0.0))
            .then(&Affine::rotate_about(0.5, (0.0, 0.0)))
            .decompose();
        assert!((sx - 2.0).abs() < 1e-9 && (sy - 3.0).abs() < 1e-9 && (angle - 0.5).abs() < 1e-9);
    }

    #[test]
    fn resizing_keeps_edges_opaque_and_flat_colour_exact() {
        let grey = [30000, 40000, 50000, 65535];
        let image = Tiled::from_slice(600, 400, [0; 4], &vec![grey; 600 * 400]);
        for (w, h) in [(300, 200), (1000, 700), (599, 401)] {
            let out = resized(&image, w, h, Resampling::Bicubic);
            assert_eq!((out.width(), out.height()), (w, h));
            for (x, y) in [(0, 0), (w - 1, h - 1), (w / 2, h / 2), (w - 1, 0)] {
                assert_eq!(out.get(x, y), grey, "{w}×{h} at ({x}, {y})");
            }
        }
        // Nothing written stays nothing written, at any size.
        let empty = resized(&Tiled::new(600, 400, [0u16; 4]), 50, 70, Resampling::Bicubic);
        assert!((empty.cols(), empty.rows()) == (1, 1) && empty.tile(0, 0).is_none());
    }

    #[test]
    fn resizing_down_averages_and_keeps_the_square_in_place() {
        let px: Vec<Pixel> =
            (0..600 * 400).map(|i| if i % 2 == 0 { [65535; 4] } else { [0, 0, 0, 65535] }).collect();
        let out = resized(&Tiled::from_slice(600, 400, [0; 4], &px), 150, 100, Resampling::Bicubic);
        assert!((out.get(75, 50)[0] as i32 - 32768).abs() < 2000, "{:?}", out.get(75, 50));
        // The square from 100 to 200 lands from 50 to 100 at half size.
        let half = resized(&square(), 300, 200, Resampling::Bicubic);
        assert_eq!((half.get(52, 52), half.get(97, 97)), (RED, RED));
        assert_eq!((half.get(47, 75)[3], half.get(103, 75)[3]), (0, 0));
    }

    #[test]
    fn whole_pixel_moves_are_exact() {
        let moved = transformed(&square(), &Affine::translate(7.0, -3.0), [0; 4], Resampling::Bicubic);
        assert_eq!(moved.to_vec(), square().translated(7, -3, [0; 4]).to_vec());
    }

    #[test]
    fn scaling_up_doubles_the_square_about_its_corner() {
        let t = Affine::scale_about(2.0, 2.0, (100.0, 100.0));
        for resampling in [Resampling::Bilinear, Resampling::Bicubic] {
            let out = transformed(&square(), &t, [0; 4], resampling);
            // Now (100, 100) to (300, 300): solid inside, clear outside.
            assert_eq!(out.get(110, 110), RED);
            assert_eq!(out.get(290, 290), RED);
            assert_eq!(out.get(310, 200)[3], 0);
            assert_eq!(out.get(200, 90)[3], 0);
            // No colour bleeds in from the transparent pixels.
            let edge = out.get(299, 200);
            assert!(edge[3] > 0 && edge[1] == 0 && edge[2] == 0 && edge[0] == 65535, "{edge:?}");
        }
    }

    #[test]
    fn shrinking_averages_rather_than_skipping() {
        // Alternate black and white columns shrunk to a quarter come out
        // mid grey, not all one or the other.
        let px: Vec<Pixel> =
            (0..600 * 400).map(|i| if i % 2 == 0 { [65535; 4] } else { [0, 0, 0, 65535] }).collect();
        let image = Tiled::from_slice(600, 400, [0; 4], &px);
        let out = transformed(&image, &Affine::scale_about(0.25, 0.25, (0.0, 0.0)), [0; 4], Resampling::Bicubic);
        let v = out.get(50, 50)[0];
        assert!(v.abs_diff(32768) < 3000, "{v}");
    }

    #[test]
    fn rotating_a_quarter_turn_about_the_centre_keeps_the_square() {
        let t = Affine::rotate_about(std::f64::consts::FRAC_PI_2, (150.0, 150.0));
        let out = transformed(&square(), &t, [0; 4], Resampling::Bicubic);
        assert_eq!(out.get(150, 150), RED);
        assert_eq!(out.get(102, 102), RED);
        assert_eq!(out.get(90, 150)[3], 0);
    }

    #[test]
    fn masks_read_as_their_fill_outside() {
        // A white mask with a black square: moved by half a pixel, it stays
        // white where the square wasn't.
        let mut mask = Tiled::new(600, 400, u16::MAX);
        for y in 100..200 {
            for x in 100..200 {
                mask.tile_mut(x / TILE, y / TILE)[((y % TILE) * TILE + x % TILE) as usize] = 0;
            }
        }
        let out = transformed(&mask, &Affine::translate(0.5, 0.0), u16::MAX, Resampling::Bilinear);
        assert_eq!(out.get(50, 50), u16::MAX);
        assert_eq!(out.get(150, 150), 0);
        assert_eq!(out.get(100, 150), u16::MAX / 2 + 1);
    }
}
