//! Auto-Align Layers: how photos of one scene line up.
//!
//! Each photo is shrunk to a working size in grey and its corners are
//! found at a few scales. A corner is described by which of 256 pairs of
//! points around it is the brighter, turned to the way the corner faces,
//! so it reads alike in another photo whatever that one's exposure, tilt
//! or size (as ORB does). Corners are paired between photos by those
//! descriptions, and the perspective transform most pairs agree on
//! (RANSAC) is the one that lines the two up: what moved between frames,
//! a person say, is outvoted by what didn't.

use rayon::prelude::*;

use crate::denoise::blur;
use crate::tiled::Tiled;
use crate::transform::{Projective, Resampling, projected};
use crate::{Document, Pixel};

/// The longer side of the working image, at most.
const WORK: u32 = 2048;
/// How many scales corners are found at, each √2 smaller than the last.
const SCALES: usize = 4;
/// How far from a corner the points describing it are, at most, and how
/// far from the edges corners are kept so those can be read.
const REACH: f64 = 15.0;
const BORDER: usize = 18;
/// Corners are kept evenly over the image: so many, the strongest, in
/// each square of this size.
const CELL: usize = 40;
const PER_CELL: usize = 3;
/// How far a pair may be from where a transform puts it and still agree
/// with it, in working pixels.
const TOLERANCE: f64 = 2.5;
/// Fewer pairs agreeing than this, and two photos don't line up.
const AGREEING: usize = 16;

type Point = (f64, f64);

/// A grey image, 0–1.
struct Plane {
    w: usize,
    h: usize,
    v: Vec<f32>,
}

impl Plane {
    fn at(&self, x: usize, y: usize) -> f32 {
        self.v[y * self.w + x]
    }

    fn blurred(&self, sigma: f32) -> Plane {
        Plane { w: self.w, h: self.h, v: blur(&self.v, self.w, self.h, sigma) }
    }

    /// Shrunk to `w` × `h`, somewhat smaller.
    fn shrunk(&self, w: usize, h: usize) -> Plane {
        let soft = self.blurred(0.8);
        let (sx, sy) = (self.w as f32 / w as f32, self.h as f32 / h as f32);
        let v = (0..w * h)
            .into_par_iter()
            .map(|i| {
                let x = ((i % w) as f32 + 0.5) * sx - 0.5;
                let y = ((i / w) as f32 + 0.5) * sy - 0.5;
                let (x0, y0) = (x.floor().max(0.0) as usize, y.floor().max(0.0) as usize);
                let (x1, y1) = ((x0 + 1).min(self.w - 1), (y0 + 1).min(self.h - 1));
                let (fx, fy) = ((x - x0 as f32).clamp(0.0, 1.0), (y - y0 as f32).clamp(0.0, 1.0));
                let top = soft.at(x0, y0) * (1.0 - fx) + soft.at(x1, y0) * fx;
                let bottom = soft.at(x0, y1) * (1.0 - fx) + soft.at(x1, y1) * fx;
                top * (1.0 - fy) + bottom * fy
            })
            .collect();
        Plane { w, h, v }
    }
}

/// A corner: where it is in the photo, and what's around it.
struct Feature {
    at: Point,
    bits: [u64; 4],
}

/// Numbers that look random and are the same every time.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    /// From −1 to 1, mostly near 0.
    fn near_zero(&mut self) -> f64 {
        (0..4).map(|_| (self.next() >> 11) as f64 / (1u64 << 53) as f64 - 0.5).sum::<f64>() / 2.0
    }
}

/// The pairs of points compared around a corner, within [`REACH`] of it.
fn pattern() -> Vec<[Point; 2]> {
    let mut random = Random(0x9e37_79b9_7f4a_7c15);
    let mut point = || loop {
        let p = (random.near_zero() * 2.0 * REACH, random.near_zero() * 2.0 * REACH);
        if p.0.hypot(p.1) <= REACH {
            return p;
        }
    };
    (0..256).map(|_| [point(), point()]).collect()
}

/// `image` in grey with the longer side at most [`WORK`], each pixel the
/// average of a square `k` pixels wide, and how much of each is there.
fn working(image: &Tiled<Pixel>) -> (Plane, Vec<f32>, u32) {
    let k = image.width().max(image.height()).div_ceil(WORK).max(1);
    let (w, h) = ((image.width() / k) as usize, (image.height() / k) as usize);
    let rows: Vec<(Vec<f32>, Vec<f32>)> = (0..h)
        .into_par_iter()
        .map(|y| {
            let band = image.crop(0, y as u32 * k, w as u32 * k, k);
            let mut grey = vec![0.0; w];
            let mut alpha = vec![0.0; w];
            for (i, p) in band.iter().enumerate() {
                let x = i % (w * k as usize) / k as usize;
                let a = f32::from(p[3]) / 65535.0;
                grey[x] += (0.299 * f32::from(p[0]) + 0.587 * f32::from(p[1]) + 0.114 * f32::from(p[2])) / 65535.0 * a;
                alpha[x] += a;
            }
            let n = (k * k) as f32;
            (grey.into_iter().map(|v| v / n).collect(), alpha.into_iter().map(|v| v / n).collect())
        })
        .collect();
    let (mut grey, mut alpha) = (Vec::with_capacity(w * h), Vec::with_capacity(w * h));
    for (g, a) in rows {
        grey.extend(g);
        alpha.extend(a);
    }
    (Plane { w, h, v: grey }, alpha, k)
}

/// The corners of `image`, at every scale.
fn features(image: &Tiled<Pixel>) -> Vec<Feature> {
    let (base, alpha, k) = working(image);
    let (w0, h0) = (base.w, base.h);
    let pattern = pattern();
    // All of what describes a corner has to be in the picture.
    let whole = |x: f64, y: f64, r: f64| {
        (0..9).all(|i| {
            let (dx, dy) = ((i % 3) as f64 - 1.0, (i / 3) as f64 - 1.0);
            let (x, y) = ((x + dx * r).round(), (y + dy * r).round());
            x >= 0.0 && y >= 0.0 && x < w0 as f64 && y < h0 as f64 && alpha[y as usize * w0 + x as usize] > 0.999
        })
    };
    let mut found = Vec::new();
    let mut level = base;
    for scale in 0..SCALES {
        if scale > 0 {
            let shrink = 2f64.sqrt().powi(scale as i32);
            level = level.shrunk((w0 as f64 / shrink).round() as usize, (h0 as f64 / shrink).round() as usize);
        }
        let (w, h) = (level.w, level.h);
        if w <= 2 * BORDER + CELL || h <= 2 * BORDER + CELL {
            break;
        }
        // Level pixels to the photo's, pixel centres on the halves.
        let (sx, sy) = (w0 as f64 / w as f64, h0 as f64 / h as f64);
        let smooth = level.blurred(2.0);
        let corners = corners(&level);
        found.par_extend(corners.into_par_iter().filter_map(|(x, y, dx, dy)| {
            if !whole((x as f64 + 0.5) * sx - 0.5, (y as f64 + 0.5) * sy - 0.5, REACH * sx.max(sy)) {
                return None;
            }
            // The way it faces: from its middle to where it's brightest.
            let (mut mx, mut my) = (0.0, 0.0);
            let r = REACH as i32;
            for j in -r..=r {
                for i in -r..=r {
                    if i * i + j * j <= r * r {
                        let v = smooth.at((x as i32 + i) as usize, (y as i32 + j) as usize);
                        mx += i as f32 * v;
                        my += j as f32 * v;
                    }
                }
            }
            let (sin, cos) = f64::from(my.atan2(mx)).sin_cos();
            let read = |p: Point| {
                let (px, py) = (cos * p.0 - sin * p.1, sin * p.0 + cos * p.1);
                smooth.at((x as f64 + px).round() as usize, (y as f64 + py).round() as usize)
            };
            let mut bits = [0u64; 4];
            for (i, pair) in pattern.iter().enumerate() {
                bits[i / 64] |= u64::from(read(pair[0]) < read(pair[1])) << (i % 64);
            }
            let at = ((x as f64 + 0.5 + dx) * sx * f64::from(k), (y as f64 + 0.5 + dy) * sy * f64::from(k));
            Some(Feature { at, bits })
        }));
    }
    found
}

/// The strongest corners of `plane` (Harris's measure) in each square of
/// it: the pixel each is in, and how far from that pixel's middle it is.
fn corners(plane: &Plane) -> Vec<(usize, usize, f64, f64)> {
    let (w, h) = (plane.w, plane.h);
    let soft = plane.blurred(1.0);
    // How fast it changes across and down, squared and multiplied, averaged
    // around each pixel.
    let gradient = |f: fn(f32, f32) -> f32| -> Vec<f32> {
        let v: Vec<f32> = (0..w * h)
            .into_par_iter()
            .map(|i| {
                let (x, y) = (i % w, i / w);
                if x == 0 || y == 0 || x == w - 1 || y == h - 1 {
                    return 0.0;
                }
                f((soft.at(x + 1, y) - soft.at(x - 1, y)) / 2.0, (soft.at(x, y + 1) - soft.at(x, y - 1)) / 2.0)
            })
            .collect();
        blur(&v, w, h, 2.0)
    };
    let (xx, yy, xy) = (gradient(|x, _| x * x), gradient(|_, y| y * y), gradient(|x, y| x * y));
    let strength: Vec<f32> = (0..w * h)
        .into_par_iter()
        .map(|i| xx[i] * yy[i] - xy[i] * xy[i] - 0.04 * (xx[i] + yy[i]) * (xx[i] + yy[i]))
        .collect();
    let s = |x: usize, y: usize| strength[y * w + x];
    let (cols, rows) = ((w - 2 * BORDER).div_ceil(CELL), (h - 2 * BORDER).div_ceil(CELL));
    (0..cols * rows)
        .into_par_iter()
        .flat_map_iter(|cell| {
            let (x0, y0) = (BORDER + cell % cols * CELL, BORDER + cell / cols * CELL);
            let mut peaks = Vec::new();
            for y in y0..(y0 + CELL).min(h - BORDER) {
                for x in x0..(x0 + CELL).min(w - BORDER) {
                    let v = s(x, y);
                    let peak = v > 1e-10
                        && (0..9).all(|i| i == 4 || v > s(x + i % 3 - 1, y + i / 3 - 1) || (i > 4 && v == s(x + i % 3 - 1, y + i / 3 - 1)));
                    if peak {
                        peaks.push((v, x, y));
                    }
                }
            }
            peaks.sort_by(|a, b| b.0.total_cmp(&a.0));
            peaks.truncate(PER_CELL);
            // Between pixels: the top of the curve through its neighbours.
            let top = |before: f32, v: f32, after: f32| {
                let bend = 2.0 * v - before - after;
                if bend > 0.0 { f64::from((after - before) / (2.0 * bend)).clamp(-0.5, 0.5) } else { 0.0 }
            };
            peaks
                .into_iter()
                .map(|(v, x, y)| (x, y, top(s(x - 1, y), v, s(x + 1, y)), top(s(x, y - 1), v, s(x, y + 1))))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Corners of `a` and `b` that look most like each other, and clearly
/// more than like any other: indices into each.
fn pairs(a: &[Feature], b: &[Feature]) -> Vec<(usize, usize)> {
    let nearest = |from: &[Feature], to: &[Feature]| -> Vec<Option<usize>> {
        from.par_iter()
            .map(|f| {
                let (mut best, mut least, mut next) = (0, u32::MAX, u32::MAX);
                for (j, t) in to.iter().enumerate() {
                    let d: u32 = (0..4).map(|k| (f.bits[k] ^ t.bits[k]).count_ones()).sum();
                    if d < least {
                        (best, next, least) = (j, least, d);
                    } else if d < next {
                        next = d;
                    }
                }
                (least < 80 && least * 5 < next * 4).then_some(best)
            })
            .collect()
    };
    let (ab, ba) = (nearest(a, b), nearest(b, a));
    ab.iter().enumerate().filter_map(|(i, j)| j.filter(|&j| ba[j] == Some(i)).map(|j| (i, j))).collect()
}

/// The perspective transform that best takes each pair's first point to
/// its second (four pairs or more).
fn fit(pairs: &[(Point, Point)]) -> Option<Projective> {
    // Both sets of points about their middle and of size 1, for the sums'
    // sake.
    let unit = |pick: fn(&(Point, Point)) -> Point| {
        let n = pairs.len() as f64;
        let (cx, cy) = pairs.iter().map(pick).fold((0.0, 0.0), |a, p| (a.0 + p.0 / n, a.1 + p.1 / n));
        let size = pairs.iter().map(pick).map(|p| (p.0 - cx).hypot(p.1 - cy)).sum::<f64>() / n;
        (size > 1e-9).then(|| (cx, cy, 1.0 / size))
    };
    let (ax, ay, ak) = unit(|p| p.0)?;
    let (bx, by, bk) = unit(|p| p.1)?;
    let mut m = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];
    // Least squares in what's multiplied through by the divisor, then again
    // dividing by the divisor it found, so the distances it makes least
    // are the ones in the picture.
    for round in 0..if pairs.len() > 4 { 3 } else { 1 } {
        let mut sums = [[0.0f64; 9]; 8];
        for &((x, y), (u, v)) in pairs {
            let (x, y, u, v) = ((x - ax) * ak, (y - ay) * ak, (u - bx) * bk, (v - by) * bk);
            let weight = if round == 0 { 1.0 } else { 1.0 / (m[6] * x + m[7] * y + 1.0).powi(2) };
            for row in [[x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y, u], [0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y, v]] {
                for i in 0..8 {
                    for j in 0..9 {
                        sums[i][j] += weight * row[i] * row[j];
                    }
                }
            }
        }
        m = solve(sums)?;
    }
    let to_unit = Projective::new([ak, 0.0, -ak * ax, 0.0, ak, -ak * ay, 0.0, 0.0, 1.0]);
    let from_unit = Projective::new([1.0 / bk, 0.0, bx, 0.0, 1.0 / bk, by, 0.0, 0.0, 1.0]);
    let fitted = Projective::new([m[0], m[1], m[2], m[3], m[4], m[5], m[6], m[7], 1.0]);
    Some(to_unit.then(&fitted).then(&from_unit))
}

/// Eight equations in eight unknowns, each row its coefficients then what
/// they add up to.
fn solve(mut m: [[f64; 9]; 8]) -> Option<[f64; 8]> {
    for i in 0..8 {
        let pivot = (i..8).max_by(|&a, &b| m[a][i].abs().total_cmp(&m[b][i].abs()))?;
        if m[pivot][i].abs() < 1e-12 {
            return None;
        }
        m.swap(i, pivot);
        for r in 0..8 {
            if r != i {
                let (k, row) = (m[r][i] / m[i][i], m[i]);
                for (v, pivot) in m[r].iter_mut().zip(row).skip(i) {
                    *v -= k * pivot;
                }
            }
        }
    }
    Some(std::array::from_fn(|i| m[i][8] / m[i][i]))
}

/// The transform most of `pairs` agree on, to within `tolerance`, and how
/// many do (RANSAC).
fn consensus(pairs: &[(Point, Point)], tolerance: f64) -> Option<(Projective, usize)> {
    if pairs.len() < AGREEING {
        return None;
    }
    let agreeing = |t: &Projective, tolerance: f64| -> Vec<(Point, Point)> {
        let agrees = |&(a, b): &(Point, Point)| t.apply(a).is_some_and(|p| (p.0 - b.0).hypot(p.1 - b.1) < tolerance);
        pairs.iter().copied().filter(agrees).collect()
    };
    let mut random = Random(0x2545_f491_4f6c_dd1d);
    let mut best: Option<(Projective, usize)> = None;
    let (mut tries, mut tried) = (2000.0, 0.0);
    while tried < tries {
        tried += 1.0;
        let mut four = [0; 4];
        for i in 0..4 {
            four[i] = loop {
                let pick = random.below(pairs.len());
                if !four[..i].contains(&pick) {
                    break pick;
                }
            };
        }
        let Some(t) = fit(&four.map(|i| pairs[i])) else { continue };
        let count = agreeing(&t, tolerance).len();
        if best.is_none_or(|(_, most)| count > most) {
            best = Some((t, count));
            // Enough tries to have very likely drawn four that agree.
            let share = count as f64 / pairs.len() as f64;
            tries = ((0.001f64).ln() / (1.0 - share.powi(4)).max(1e-12).ln()).clamp(50.0, tries);
        }
    }
    // Fitted again to all that agree, a few times, more strictly each time.
    let (mut t, _) = best?;
    let mut count = 0;
    for tolerance in [tolerance, tolerance * 0.8, tolerance * 0.6, tolerance * 0.6] {
        let agree = agreeing(&t, tolerance);
        if agree.len() < AGREEING {
            return None;
        }
        (t, count) = (fit(&agree)?, agree.len());
    }
    Some((t, count))
}

/// Whether `t` could be one photo's view of another's scene: `width` ×
/// `height` stays a four-sided shape the same way up, of a like size.
fn plausible(t: &Projective, width: f64, height: f64) -> bool {
    let corners = [(0.0, 0.0), (width, 0.0), (width, height), (0.0, height)].map(|p| t.apply(p));
    let Some(c) = corners.into_iter().collect::<Option<Vec<_>>>() else {
        return false;
    };
    let turn = |i: usize| {
        let (a, b, d) = (c[i], c[(i + 1) % 4], c[(i + 2) % 4]);
        (b.0 - a.0) * (d.1 - b.1) - (b.1 - a.1) * (d.0 - b.0)
    };
    let area = (0..4).map(|i| c[i].0 * c[(i + 1) % 4].1 - c[(i + 1) % 4].0 * c[i].1).sum::<f64>() / 2.0;
    (0..4).all(|i| turn(i) > 0.0) && (1.0 / 16.0..16.0).contains(&(area / (width * height)))
}

/// How each of `images` (all one size) moves to line up with `reference`,
/// or with the one that has most in common with the others. That one
/// stays where it is; `None` is for those nothing lines up with.
pub fn align(images: &[&Tiled<Pixel>], reference: Option<usize>) -> Vec<Option<Projective>> {
    let n = images.len();
    let Some(first) = images.first() else {
        return Vec::new();
    };
    let (width, height) = (f64::from(first.width()), f64::from(first.height()));
    let tolerance = TOLERANCE * f64::from(first.width().max(first.height()).div_ceil(WORK).max(1));
    let features: Vec<Vec<Feature>> = images.iter().map(|image| features(image)).collect();
    // What takes each to each other, and how many corners say so.
    let mut links: Vec<Vec<Option<(Projective, usize)>>> = vec![vec![None; n]; n];
    for i in 0..n {
        for j in i + 1..n {
            let (a, b) = (&features[i], &features[j]);
            let pairs: Vec<_> = pairs(a, b).into_iter().map(|(i, j)| (a[i].at, b[j].at)).collect();
            let found = consensus(&pairs, tolerance).filter(|(t, _)| plausible(t, width, height));
            if let Some((t, count)) = found
                && let Some(back) = t.inverse()
            {
                links[i][j] = Some((t, count));
                links[j][i] = Some((back, count));
            }
        }
    }
    let shared = |i: usize| links[i].iter().flatten().map(|l| l.1).sum::<usize>();
    let reference = reference.unwrap_or_else(|| (0..n).max_by_key(|&i| (shared(i), n - i)).unwrap_or(0));
    // Outwards from the reference, each by its strongest link to one
    // that's already placed.
    let mut placed: Vec<Option<Projective>> = vec![None; n];
    placed[reference] = Some(Projective::IDENTITY);
    loop {
        let ways = (0..n).flat_map(|to| (0..n).map(move |from| (from, to)));
        let open = ways.filter(|&(from, to)| placed[from].is_none() && placed[to].is_some());
        let strongest = open.filter_map(|(from, to)| links[from][to].map(|(t, count)| (count, from, to, t)));
        let Some((_, from, to, t)) = strongest.max_by_key(|l| l.0) else {
            return placed;
        };
        placed[from] = placed[to].map(|onwards| t.then(&onwards));
    }
}

/// Photoshop's Edit › Auto-Align Layers: the layers of `ids` that have
/// pixels, moved to line up with one of them, which stays as it is: the
/// one whose position is locked, or else the bottom one. Each is resampled
/// where it lands, its mask with it, and what leaves the canvas is lost.
/// Returns the names of those nothing lined up with, or `None` if there
/// weren't two layers to line up.
pub fn auto_align(doc: &mut Document, ids: &[u64]) -> Option<Vec<String>> {
    let layers: Vec<usize> = (0..doc.layers.len())
        .filter(|&i| ids.contains(&doc.layers[i].id) && doc.layers[i].has_pixels())
        .collect();
    let reference = layers.iter().position(|&i| !doc.layers[i].can_move()).unwrap_or(0);
    // Others that are locked stay out of it.
    let layers: Vec<usize> =
        layers.iter().enumerate().filter(|&(n, &i)| n == reference || doc.layers[i].can_move()).map(|(_, &i)| i).collect();
    if layers.len() < 2 {
        return None;
    }
    let reference = layers.iter().position(|&i| !doc.layers[i].can_move()).unwrap_or(0);
    let images: Vec<&Tiled<Pixel>> = layers.iter().map(|&i| &doc.layers[i].pixels).collect();
    let moves = align(&images, Some(reference));
    let mut left = Vec::new();
    for (&i, t) in layers.iter().zip(moves) {
        let layer = &mut doc.layers[i];
        let Some(t) = t else {
            left.push(layer.name.clone());
            continue;
        };
        if t != Projective::IDENTITY {
            layer.pixels = projected(&layer.pixels, &t, [0; 4], Resampling::Bicubic);
            if let Some(mask) = &mut layer.mask {
                mask.pixels = projected(&mask.pixels, &t, mask.pixels.fill(), Resampling::Bicubic);
            }
        }
    }
    Some(left)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layer::{Layer, Mask};
    use crate::ColorProfile;

    const W: u32 = 640;
    const H: u32 = 480;

    /// A scene of overlapping rectangles in greys, different for each `seed`.
    fn scene(seed: u64) -> Tiled<Pixel> {
        let mut random = Random(seed);
        let mut px = vec![[30000, 30000, 30000, 65535]; (W * H) as usize];
        for _ in 0..400 {
            let (x, y) = (random.below(W as usize), random.below(H as usize));
            let (w, h) = (8 + random.below(50), 8 + random.below(50));
            let v = (5000 + random.below(55000)) as u16;
            for j in y..(y + h).min(H as usize) {
                for i in x..(x + w).min(W as usize) {
                    px[j * W as usize + i] = [v, v, v, 65535];
                }
            }
        }
        Tiled::from_slice(W, H, [0; 4], &px)
    }

    /// A handheld camera's move between frames: a turn, a shift and a tilt.
    fn shake() -> Projective {
        let turn = crate::transform::Affine::rotate_about(0.03, (300.0, 200.0));
        Projective::from(turn).then(&Projective::new([1.02, 0.0, 14.0, 0.0, 1.02, -9.0, 2e-5, -1e-5, 1.0]))
    }

    /// How far `t` puts the image's corners from where `truth` does, at most.
    fn error(t: &Projective, truth: &Projective) -> f64 {
        let corners = [(0.0, 0.0), (f64::from(W), 0.0), (0.0, f64::from(H)), (f64::from(W), f64::from(H))];
        let far = |p| {
            let (a, b) = (t.apply(p).unwrap(), truth.apply(p).unwrap());
            (a.0 - b.0).hypot(a.1 - b.1)
        };
        corners.into_iter().map(far).fold(0.0, f64::max)
    }

    #[test]
    fn fits_the_transform_its_points_were_moved_by() {
        let truth = shake();
        let points = [(10.0, 20.0), (600.0, 40.0), (580.0, 400.0), (30.0, 450.0), (300.0, 200.0), (100.0, 300.0)];
        let pairs: Vec<_> = points.iter().map(|&p| (p, truth.apply(p).unwrap())).collect();
        assert!(error(&fit(&pairs[..4]).unwrap(), &truth) < 1e-6);
        assert!(error(&fit(&pairs).unwrap(), &truth) < 1e-6);
        // All in a line, there's no telling.
        let line: Vec<_> = (0..6).map(|i| ((f64::from(i), 0.0), (f64::from(i), 1.0))).collect();
        assert!(fit(&line).is_none());
    }

    #[test]
    fn a_shaken_darker_frame_lines_up_to_within_a_pixel() {
        let reference = scene(1);
        // The same scene from where the camera moved to, a stop darker.
        let back = shake();
        let moved = projected(&reference, &back.inverse().unwrap(), [0; 4], Resampling::Bicubic);
        let moved = moved.map(|p| [p[0] / 2, p[1] / 2, p[2] / 2, p[3]]);
        let found = align(&[&reference, &moved], Some(0));
        assert_eq!(found[0], Some(Projective::IDENTITY));
        let e = error(&found[1].expect("lined up"), &back);
        assert!(e < 0.5, "{e} pixels out");
    }

    #[test]
    fn what_moved_in_the_scene_is_outvoted() {
        let reference = scene(1);
        let back = Projective::new([1.0, 0.0, 12.5, 0.0, 1.0, -7.25, 0.0, 0.0, 1.0]);
        let mut moved = projected(&reference, &back.inverse().unwrap(), [0; 4], Resampling::Bicubic).to_vec();
        // Someone walked across: a fifth of the frame is another scene.
        let other = scene(2).to_vec();
        for y in 150..350 {
            for x in 200..400 {
                moved[y * W as usize + x] = other[y * W as usize + x - 90];
            }
        }
        let moved = Tiled::from_slice(W, H, [0; 4], &moved);
        let found = align(&[&reference, &moved], Some(0));
        let e = error(&found[1].expect("lined up"), &back);
        assert!(e < 0.5, "{e} pixels out");
    }

    #[test]
    fn frames_line_up_through_the_ones_between_them() {
        // Three frames of a wide scene, the outer two not overlapping: the
        // middle one is what both line up with.
        let wide = scene(3);
        let frame = |dx: f64| {
            let t = Projective::new([1.0, 0.0, -dx, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
            projected(&wide, &t, [0; 4], Resampling::Bicubic)
        };
        let (left, middle, right) = (frame(-330.0), frame(0.0), frame(330.0));
        let found = align(&[&left, &middle, &right], None);
        assert_eq!(found[1], Some(Projective::IDENTITY), "the middle one has most in common");
        for (t, dx) in [(found[0], -330.0), (found[2], 330.0)] {
            let truth = Projective::new([1.0, 0.0, dx, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
            let e = error(&t.expect("lined up"), &truth);
            assert!(e < 0.5, "{e} pixels out");
        }
        // With the left one to stay, the right one is reached through the middle.
        let found = align(&[&left, &middle, &right], Some(0));
        let truth = Projective::new([1.0, 0.0, 660.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
        let e = error(&found[2].expect("lined up"), &truth);
        assert!(e < 1.0, "{e} pixels out");
    }

    #[test]
    fn auto_align_moves_layers_and_masks_to_the_locked_one_and_names_what_it_cant() {
        let reference = scene(1);
        let back = Projective::new([1.0, 0.0, 20.0, 0.0, 1.0, 10.0, 0.0, 0.0, 1.0]);
        let moved = projected(&reference, &back.inverse().unwrap(), [0; 4], Resampling::Bicubic);
        let mut doc = Document::from_image("t.tif".into(), &moved.to_raster(), ColorProfile::srgb(), 16);
        doc.layers[0].mask = Some(Mask::white(W, H));
        let bottom = doc.layers[0].id;
        let (top, unrelated, empty) = (doc.next_layer_id(), doc.next_layer_id(), doc.next_layer_id());
        doc.layers.push(Layer::from_pixels(unrelated, "Elsewhere", scene(9)));
        doc.layers.push(Layer::empty(empty, "Empty", W, H));
        doc.layers.push(Layer::from_pixels(top, "Reference", reference.clone()));
        assert_eq!(auto_align(&mut doc, &[top]), None, "one layer has nothing to line up with");

        doc.layer_mut(top).unwrap().locks.position = true;
        let left = auto_align(&mut doc, &[bottom, unrelated, empty, top]);
        assert_eq!(left, Some(vec!["Elsewhere".to_owned(), "Empty".to_owned()]));
        // The locked layer stayed and the bottom one moved onto it, its
        // mask now hiding what the move uncovered.
        assert!(doc.layer(top).unwrap().pixels.same_tiles(&reference));
        let layer = doc.layer(bottom).unwrap();
        let (was, now) = (reference.get(300, 200), layer.pixels.get(300, 200));
        assert!(was[0].abs_diff(now[0]) < 700, "{was:?} {now:?}");
        assert_eq!(layer.pixels.get(5, 5)[3], 0);
        let mask = &layer.mask.as_ref().unwrap().pixels;
        assert_eq!((mask.get(300, 200), mask.get(5, 5)), (u16::MAX, u16::MAX));
        assert_eq!(doc.layer(unrelated).unwrap().pixels.to_vec(), scene(9).to_vec());
    }
}
