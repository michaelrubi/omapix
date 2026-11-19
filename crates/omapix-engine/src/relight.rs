//! Filter › Render › Relight: light a photo again with a lamp placed on it.
//!
//! Ported from PhotoCraft's `crates/algo/src/relight.rs` (MIT OR
//! Apache-2.0, [`b70bf524`](https://github.com/storytold/photocraft/commit/b70bf524e)).
//! The photo's brightness, blurred, stands in for the shape of what's in it
//! (bright as near, dark as far), and its slopes are the surfaces' normals.
//! A lamp hanging over the photo at (`x`, `y`), `reach` above it, lights
//! those surfaces as a matte one would be lit (Lambert), less with distance
//! (inverse square), as Photoshop's Lighting Effects' point light.
//!
//! Where PhotoCraft divides each pixel by its blurred brightness and
//! multiplies in the new light, which takes every area towards the same
//! tone, here each pixel is scaled by how much more (or less) light its
//! surface gets than a flat one right under the lamp, which keeps its tone.

use rayon::prelude::*;

use crate::Pixel;
use crate::adjust::luminance;
use crate::tiled::Tiled;

const MAX: f32 = 65535.0;
/// Softness maps onto this blur range, in pixels.
const SIGMA_MIN: f32 = 2.0;
const SIGMA_MAX: f32 = 96.0;
/// The slope's step is the blur's size, but never more than this.
const STEP_MAX: f32 = 32.0;
/// How far light wraps round a face, past where it stops reaching.
const SKIN_WRAP: f32 = 0.8;
/// How steep a slope of brightness makes a surface (PhotoCraft's).
const SLOPE: f32 = 2.0;

/// Settings for Relight, in the dialog's units.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct RelightOptions {
    /// Where the lamp is, across the image (0 its left edge, 1 its right);
    /// it can be off the image.
    pub x: f32,
    /// Where the lamp is, down the image (0 its top, 1 its bottom).
    pub y: f32,
    /// How high over the image the lamp hangs, which is how far its light
    /// reaches, as a percentage of the image's shorter side.
    pub reach: f32,
    /// How much light the lamp adds right under it, as a percentage of the
    /// light the photo had (0–100 %).
    pub intensity: f32,
    /// How much of the photo's own light is kept, which fills the shadows
    /// (0–100 %). 100 % with no Intensity leaves the image as it is.
    pub ambient: f32,
    /// How broad the shapes the light follows are (1–100 %).
    pub softness: f32,
    /// The lamp's colour, in sRGB.
    pub colour: [u8; 3],
    /// The faces' shapes to light, as [`keep_faces`] numbered them (0 for
    /// none: their brightness stands in for their shape). Not saved.
    #[serde(skip)]
    pub faces: u64,
}

impl Default for RelightOptions {
    fn default() -> Self {
        Self {
            // A key light up and to the left.
            x: 0.25,
            y: 0.2,
            reach: 75.0,
            intensity: 45.0,
            ambient: 70.0,
            softness: 25.0,
            colour: [255, 255, 255],
            faces: 0,
        }
    }
}

impl RelightOptions {
    /// The blur's size in pixels, for an image `whole` big.
    pub fn sigma(&self, (w, h): (u32, u32)) -> f32 {
        let side = w.min(h).max(1) as f32;
        (self.softness.clamp(1.0, 100.0) / 100.0 * 0.08 * side).clamp(SIGMA_MIN, SIGMA_MAX)
    }

    /// How far a pixel's result reaches, in pixels: the blur and the slope's
    /// step.
    pub fn margin(&self, whole: (u32, u32)) -> u32 {
        let sigma = self.sigma(whole);
        (sigma * 3.0).ceil() as u32 + 1 + step(sigma) as u32
    }

    /// Where the lamp is, in an image `whole` big: x right and y down, in
    /// pixels, and how high over it.
    pub fn lamp(&self, (w, h): (u32, u32)) -> [f32; 3] {
        let side = w.min(h).max(1) as f32;
        [self.x * w as f32, self.y * h as f32, (self.reach / 100.0 * side).max(1.0)]
    }

    /// The lamp's colour as a filter: white is 1 in each channel, and a
    /// coloured one holds back the others.
    fn filter(&self) -> [f32; 3] {
        let top = f32::from(*self.colour.iter().max().unwrap_or(&255)).max(1.0);
        self.colour.map(|c| f32::from(c) / top)
    }
}

/// Each 16-bit value, encoded with sRGB's curve, as linear light (0–1).
fn linear_table() -> Vec<f32> {
    (0..=u16::MAX)
        .map(|v| {
            let v = f32::from(v) / MAX;
            if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        })
        .collect()
}

/// `v` (linear) times `gain`, where light added eases into white rather
/// than clipping, as film's shoulder does.
fn lighten(v: f32, gain: f32) -> f32 {
    if gain <= 1.0 || v >= 1.0 {
        return v * gain;
    }
    let room = 1.0 - v;
    v + room * (1.0 - (-(v * (gain - 1.0)) / room).exp())
}

/// Linear light encoded with sRGB's curve as 16-bit values, in 65536
/// steps of its square root, so the shadows keep their precision.
fn encode_table() -> Vec<u16> {
    (0..=u16::MAX)
        .map(|v| {
            let root = f32::from(v) / MAX;
            (from_linear(root * root) * MAX).round() as u16
        })
        .collect()
}

/// Linear light `v` as a 16-bit value encoded with sRGB's curve.
fn encode_lookup(table: &[u16], v: f32) -> u16 {
    table[(v.max(0.0).sqrt() * MAX).round().min(MAX) as usize]
}

/// Linear light (0–1 and above) encoded with sRGB's curve.
fn from_linear(v: f32) -> f32 {
    if v <= 0.0031308 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}

/// Faces' shapes kept for Relight, newest last, by number: found by the
/// app's face models, which a filter's settings (copied about, and saved)
/// can't hold.
static FACES: std::sync::Mutex<Vec<(u64, std::sync::Arc<Vec<FaceShape>>)>> = std::sync::Mutex::new(Vec::new());

/// How many sets of faces are kept: enough for a filter still being
/// applied while the next image's are found.
const FACES_KEPT: usize = 4;

/// Keep `faces` for Relight, and the number its settings name them by.
pub fn keep_faces(faces: Vec<FaceShape>) -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut kept = FACES.lock().unwrap_or_else(|e| e.into_inner());
    kept.push((id, std::sync::Arc::new(faces)));
    let excess = kept.len().saturating_sub(FACES_KEPT);
    kept.drain(..excess);
    id
}

/// The faces kept as `id`, or none.
fn kept_faces(id: u64) -> std::sync::Arc<Vec<FaceShape>> {
    let kept = FACES.lock().unwrap_or_else(|e| e.into_inner());
    kept.iter().find(|(k, _)| *k == id).map(|(_, f)| f.clone()).unwrap_or_default()
}

/// A face's shape, from its landmarks: which way its surface faces, on a
/// coarse grid over it, and how much of each cell is face.
#[derive(Clone, Debug, PartialEq)]
pub struct FaceShape {
    /// The grid's top left, in image pixels, and the size of a cell.
    origin: [f32; 2],
    cell: f32,
    width: usize,
    height: usize,
    /// The surface's slope across and down (the normal is (x, y, 1)),
    /// and how much is face (0–1), per cell.
    normals: Vec<[f32; 3]>,
    /// How far the surface stands out from the image, in pixels, per cell.
    heights: Vec<f32>,
}

/// Cells across a face's grid.
const FACE_CELLS: f32 = 96.0;

impl FaceShape {
    /// The shape from a face's landmarks: x and y in image pixels, and depth
    /// in pixels too, nearer the camera less (MediaPipe's). The points are
    /// joined into triangles, heights filled in across each, then smoothed.
    pub fn new(points: &[[f32; 3]]) -> Option<Self> {
        if points.len() < 3 {
            return None;
        }
        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
        for p in points {
            for i in 0..2 {
                lo[i] = lo[i].min(p[i]);
                hi[i] = hi[i].max(p[i]);
            }
        }
        let size = (hi[0] - lo[0]).max(hi[1] - lo[1]).max(1.0);
        let margin = size * 0.15;
        let cell = size / FACE_CELLS;
        let origin = [lo[0] - margin, lo[1] - margin];
        let width = ((hi[0] - lo[0] + 2.0 * margin) / cell).ceil() as usize + 1;
        let height = ((hi[1] - lo[1] + 2.0 * margin) / cell).ceil() as usize + 1;
        // Heights (nearer is higher) across each triangle; NaN off the face.
        let mut heights = vec![f32::NAN; width * height];
        let flat: Vec<[f32; 2]> = points.iter().map(|p| [(p[0] - origin[0]) / cell, (p[1] - origin[1]) / cell]).collect();
        for [a, b, c] in delaunay(&flat) {
            let (pa, pb, pc) = (flat[a], flat[b], flat[c]);
            let area = (pb[0] - pa[0]) * (pc[1] - pa[1]) - (pc[0] - pa[0]) * (pb[1] - pa[1]);
            if area.abs() < 1e-6 {
                continue;
            }
            let x0 = pa[0].min(pb[0]).min(pc[0]).floor().max(0.0) as usize;
            let x1 = (pa[0].max(pb[0]).max(pc[0]).ceil() as usize).min(width - 1);
            let y0 = pa[1].min(pb[1]).min(pc[1]).floor().max(0.0) as usize;
            let y1 = (pa[1].max(pb[1]).max(pc[1]).ceil() as usize).min(height - 1);
            for y in y0..=y1 {
                for x in x0..=x1 {
                    let (px, py) = (x as f32, y as f32);
                    let wa = ((pb[0] - px) * (pc[1] - py) - (pc[0] - px) * (pb[1] - py)) / area;
                    let wb = ((pc[0] - px) * (pa[1] - py) - (pa[0] - px) * (pc[1] - py)) / area;
                    let wc = 1.0 - wa - wb;
                    if wa >= -1e-4 && wb >= -1e-4 && wc >= -1e-4 {
                        heights[y * width + x] = -(wa * points[a][2] + wb * points[b][2] + wc * points[c][2]);
                    }
                }
            }
        }
        // Off the face, its edge carried on smoothly (no cliff to show as an
        // outline); then smoothed. How much is face fades out inside its
        // edge, where the brightness's shape takes over.
        let inside: Vec<f32> = heights.iter().map(|h| if h.is_nan() { 0.0 } else { 1.0 }).collect();
        if inside.iter().all(|&v| v == 0.0) {
            return None;
        }
        let known: Vec<f32> = heights.iter().map(|h| if h.is_nan() { 0.0 } else { *h }).collect();
        let reach = FACE_CELLS / 8.0;
        let (sum, weight) = (
            crate::denoise::blur(&known, width, height, reach),
            crate::denoise::blur(&inside, width, height, reach),
        );
        let filled: Vec<f32> = (0..width * height)
            .map(|i| {
                if inside[i] > 0.0 {
                    known[i]
                } else if weight[i] > 1e-6 {
                    sum[i] / weight[i]
                } else {
                    0.0
                }
            })
            .collect();
        let smooth = crate::denoise::blur(&filled, width, height, 2.0);
        let cover: Vec<f32> = crate::denoise::blur(&inside, width, height, 4.0)
            .iter()
            .map(|c| ((c - 0.5) * 2.0).clamp(0.0, 1.0))
            .collect();
        let at = |x: usize, y: usize| smooth[y.min(height - 1) * width + x.min(width - 1)];
        let normals = (0..width * height)
            .map(|i| {
                let (x, y) = (i % width, i / width);
                let (xl, xr, yu, yd) = (x.saturating_sub(1), (x + 1).min(width - 1), y.saturating_sub(1), (y + 1).min(height - 1));
                let dx = (at(xr, y) - at(xl, y)) / (cell * (xr - xl).max(1) as f32);
                let dy = (at(x, yd) - at(x, yu)) / (cell * (yd - yu).max(1) as f32);
                [-dx, -dy, cover[i]]
            })
            .collect();
        // Standing out from the furthest of it.
        let floor = smooth.iter().copied().fold(f32::MAX, f32::min);
        let heights = smooth.iter().map(|h| h - floor).collect();
        Some(Self {
            origin,
            cell,
            width,
            height,
            normals,
            heights,
        })
    }

    /// How much of each cell is in the shadow the face casts from a lamp at
    /// `lamp` (image pixels, and height over the image): the surface
    /// between the cell and the lamp standing above the line between them.
    fn shadows(&self, lamp: [f32; 3]) -> Vec<f32> {
        let h = |x: f32, y: f32| {
            let (u, v) = (x.clamp(0.0, (self.width - 1) as f32), y.clamp(0.0, (self.height - 1) as f32));
            self.heights[v as usize * self.width + u as usize]
        };
        let (lu, lv) = ((lamp[0] - self.origin[0]) / self.cell, (lamp[1] - self.origin[1]) / self.cell);
        // Soft over a few pixels' height, as a lamp has some size.
        let soft = self.cell * 3.0;
        let raw: Vec<f32> = (0..self.width * self.height)
            .into_par_iter()
            .map(|i| {
                let (u, v) = ((i % self.width) as f32, (i / self.width) as f32);
                let from = self.heights[i];
                let (du, dv) = (lu - u, lv - v);
                let steps = du.abs().max(dv.abs()).ceil().min((self.width + self.height) as f32);
                let mut shade: f32 = 0.0;
                for s in 1..steps as usize {
                    let t = s as f32 / steps;
                    let (qu, qv) = (u + du * t, v + dv * t);
                    if qu < 0.0 || qv < 0.0 || qu > (self.width - 1) as f32 || qv > (self.height - 1) as f32 {
                        break;
                    }
                    // The line to the lamp's height there.
                    let line = from + (lamp[2] - from) * t;
                    shade = shade.max(((h(qu, qv) - line) / soft).clamp(0.0, 1.0));
                }
                shade
            })
            .collect();
        crate::denoise::blur(&raw, self.width, self.height, 1.5)
    }

    /// `grid`, one value a cell, at (`x`, `y`), bilinearly (0 off it).
    fn sample(&self, grid: &[f32], x: f32, y: f32) -> f32 {
        let (u, v) = ((x - self.origin[0]) / self.cell, (y - self.origin[1]) / self.cell);
        if u < 0.0 || v < 0.0 || u >= (self.width - 1) as f32 || v >= (self.height - 1) as f32 {
            return 0.0;
        }
        let (x0, y0) = (u as usize, v as usize);
        let (fx, fy) = (u - x0 as f32, v - y0 as f32);
        let g = |x: usize, y: usize| grid[y * self.width + x];
        (g(x0, y0) * (1.0 - fx) + g(x0 + 1, y0) * fx) * (1.0 - fy) + (g(x0, y0 + 1) * (1.0 - fx) + g(x0 + 1, y0 + 1) * fx) * fy
    }

    /// The surface's slope and how much is face at (`x`, `y`), bilinearly.
    fn at(&self, x: f32, y: f32) -> [f32; 3] {
        let (u, v) = ((x - self.origin[0]) / self.cell, (y - self.origin[1]) / self.cell);
        if u < 0.0 || v < 0.0 || u >= (self.width - 1) as f32 || v >= (self.height - 1) as f32 {
            return [0.0; 3];
        }
        let (x0, y0) = (u as usize, v as usize);
        let (fx, fy) = (u - x0 as f32, v - y0 as f32);
        let n = |x: usize, y: usize| self.normals[y * self.width + x];
        let (a, b, c, d) = (n(x0, y0), n(x0 + 1, y0), n(x0, y0 + 1), n(x0 + 1, y0 + 1));
        std::array::from_fn(|k| (a[k] * (1.0 - fx) + b[k] * fx) * (1.0 - fy) + (c[k] * (1.0 - fx) + d[k] * fx) * fy)
    }
}

/// The Delaunay triangles of `points` (Bowyer–Watson), as indices.
fn delaunay(points: &[[f32; 2]]) -> Vec<[usize; 3]> {
    let p: Vec<[f64; 2]> = points.iter().map(|p| [f64::from(p[0]), f64::from(p[1])]).collect();
    let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
    for q in &p {
        for i in 0..2 {
            lo[i] = lo[i].min(q[i]);
            hi[i] = hi[i].max(q[i]);
        }
    }
    let span = (hi[0] - lo[0]).max(hi[1] - lo[1]).max(1.0) * 20.0;
    let mid = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0];
    // A triangle round them all, its corners after the points.
    let mut all = p.clone();
    let n = p.len();
    all.extend([[mid[0] - span, mid[1] - span], [mid[0] + span, mid[1] - span], [mid[0], mid[1] + span]]);
    let circle = |t: [usize; 3]| {
        let ([ax, ay], [bx, by], [cx, cy]) = (all[t[0]], all[t[1]], all[t[2]]);
        let d = 2.0 * (ax * (by - cy) + bx * (cy - ay) + cx * (ay - by));
        let (a2, b2, c2) = (ax * ax + ay * ay, bx * bx + by * by, cx * cx + cy * cy);
        let ux = (a2 * (by - cy) + b2 * (cy - ay) + c2 * (ay - by)) / d;
        let uy = (a2 * (cx - bx) + b2 * (ax - cx) + c2 * (bx - ax)) / d;
        ([ux, uy], (ax - ux).powi(2) + (ay - uy).powi(2))
    };
    let mut triangles: Vec<([usize; 3], [f64; 2], f64)> = vec![{
        let t = [n, n + 1, n + 2];
        let (c, r) = circle(t);
        (t, c, r)
    }];
    for (i, &[x, y]) in all.iter().enumerate().take(n) {
        let (bad, good): (Vec<_>, Vec<_>) = triangles.into_iter().partition(|(_, c, r)| (x - c[0]).powi(2) + (y - c[1]).powi(2) < *r);
        triangles = good;
        // The hole's edges: those of one bad triangle only.
        let mut edges: Vec<[usize; 2]> = Vec::new();
        for (t, ..) in &bad {
            for e in [[t[0], t[1]], [t[1], t[2]], [t[2], t[0]]] {
                let key = [e[0].min(e[1]), e[0].max(e[1])];
                match edges.iter().position(|f| *f == key) {
                    Some(k) => {
                        edges.swap_remove(k);
                    }
                    None => edges.push(key),
                }
            }
        }
        for [a, b] in edges {
            let t = [a, b, i];
            let (c, r) = circle(t);
            if r.is_finite() {
                triangles.push((t, c, r));
            }
        }
    }
    triangles.into_iter().map(|(t, ..)| t).filter(|t| t.iter().all(|&v| v < n)).collect()
}

fn step(sigma: f32) -> usize {
    sigma.round().clamp(1.0, STEP_MAX) as usize
}

/// `image` lit again with `options`, as if it were the part at `origin` of
/// an image `whole` big (so a part, previewed, comes out as it would in
/// the whole). Alpha is kept.
/// The faces' shapes are those its settings name, if they're still kept.
pub fn relight(image: &Tiled<Pixel>, options: &RelightOptions, origin: (u32, u32), whole: (u32, u32)) -> Tiled<Pixel> {
    relight_faces(image, options, origin, whole, &kept_faces(options.faces))
}

/// [`relight`], with the shapes of the faces in it from their landmarks
/// rather than from their brightness.
pub fn relight_faces(image: &Tiled<Pixel>, options: &RelightOptions, origin: (u32, u32), whole: (u32, u32), faces: &[FaceShape]) -> Tiled<Pixel> {
    let (w, h) = (image.width() as usize, image.height() as usize);
    if w == 0 || h == 0 || options.intensity.is_nan() || options.ambient.is_nan() {
        return image.clone();
    }
    let pixels = image.to_vec();
    let sigma = options.sigma(whole);
    let d = step(sigma) as f32;
    // The brightness's shape, blurred, on a grid `f` pixels a cell: the
    // blur is broad enough not to need every pixel. Cells are aligned to
    // the whole image, so a part comes out as in it.
    let f = ((sigma / 3.0) as usize).clamp(1, 8);
    let (ox0, oy0) = (origin.0 as usize, origin.1 as usize);
    let (cx0, cy0) = (ox0 / f, oy0 / f);
    let (sw, sh) = ((ox0 + w - 1) / f - cx0 + 1, (oy0 + h - 1) / f - cy0 + 1);
    let small: Vec<f32> = (0..sw * sh)
        .into_par_iter()
        .map(|i| {
            let (gx, gy) = ((cx0 + i % sw) * f, (cy0 + i / sw) * f);
            let (x0, y0) = (gx.max(ox0) - ox0, gy.max(oy0) - oy0);
            let (x1, y1) = ((gx + f).min(ox0 + w) - ox0, (gy + f).min(oy0 + h) - oy0);
            let mut sum = 0.0;
            for y in y0..y1 {
                for p in &pixels[y * w + x0..y * w + x1] {
                    sum += luminance([p[0], p[1], p[2]].map(|v| f32::from(v) / MAX));
                }
            }
            sum / ((x1 - x0) * (y1 - y0)).max(1) as f32
        })
        .collect();
    let shape = crate::denoise::blur(&small, sw, sh, sigma / f as f32);
    // Its slopes per cell, over the same step as at full size.
    let k = ((d / f as f32).round() as usize).max(1);
    let s = |x: usize, y: usize| shape[y.min(sh - 1) * sw + x.min(sw - 1)];
    let scale = d * SLOPE / (k * f) as f32;
    let slopes: Vec<[f32; 2]> = (0..sw * sh)
        .map(|i| {
            let (x, y) = (i % sw, i / sw);
            [
                (s(x.saturating_sub(k), y) - s(x + k, y)) * scale,
                (s(x, y.saturating_sub(k)) - s(x, y + k)) * scale,
            ]
        })
        .collect();
    // A pixel's slopes, between the cells' middles.
    let half = (f as f32 - 1.0) / 2.0;
    let slope_at = |x: usize, y: usize| {
        let u = (((ox0 + x) as f32 - half) / f as f32 - cx0 as f32).clamp(0.0, (sw - 1) as f32);
        let v = (((oy0 + y) as f32 - half) / f as f32 - cy0 as f32).clamp(0.0, (sh - 1) as f32);
        let (x0, y0) = (u as usize, v as usize);
        let (x1, y1) = ((x0 + 1).min(sw - 1), (y0 + 1).min(sh - 1));
        let (fx, fy) = (u - x0 as f32, v - y0 as f32);
        let g = |x: usize, y: usize, c: usize| slopes[y * sw + x][c];
        let mix = |c| (g(x0, y0, c) * (1.0 - fx) + g(x1, y0, c) * fx) * (1.0 - fy) + (g(x0, y1, c) * (1.0 - fx) + g(x1, y1, c) * fx) * fy;
        (mix(0), mix(1))
    };

    let [lx, ly, lz] = options.lamp(whole);
    // What each face shades from the lamp.
    let shadows: Vec<Vec<f32>> = faces.iter().map(|f| f.shadows([lx, ly, lz])).collect();
    let (ox, oy) = (origin.0 as f32, origin.1 as f32);
    let filter = options.filter();
    let ambient = options.ambient.clamp(0.0, 100.0) / 100.0;
    let lamp = options.intensity.clamp(0.0, 100.0) / 100.0;
    let decode = linear_table();
    let encode = encode_table();
    let out: Vec<Pixel> = pixels
        .par_iter()
        .enumerate()
        .map(|(i, p)| {
            let (x, y) = ((i % w) as isize, (i / w) as isize);
            let (mut nx, mut ny) = slope_at(x as usize, y as usize);
            let (mut skin, mut lit_by): (f32, f32) = (0.0, 1.0);
            for (face, shadow) in faces.iter().zip(&shadows) {
                let (px, py) = (ox + x as f32, oy + y as f32);
                let [fx, fy, cover] = face.at(px, py);
                if cover > 0.0 {
                    (nx, ny) = (nx + (fx - nx) * cover, ny + (fy - ny) * cover);
                    skin = skin.max(cover);
                    lit_by = lit_by.min(1.0 - face.sample(shadow, px, py) * cover);
                }
            }
            let len = (nx * nx + ny * ny + 1.0).sqrt();
            // Towards the lamp, and its light falling off with the square of
            // the distance: 1 on a flat surface right under it.
            let (vx, vy) = (lx - ox - x as f32, ly - oy - y as f32);
            let dist = (vx * vx + vy * vy + lz * lz).sqrt();
            let cos = (nx * vx + ny * vy + lz) / (len * dist);
            let falloff = lz * lz / (dist * dist) * lit_by;
            // Light adds up in linear values, not the encoded ones.
            let c = |k: usize| {
                // On a face, light wraps round past where it stops reaching,
                // red most (skin scatters it), so its shadows' edges are soft
                // and warm rather than grey.
                let wrap = SKIN_WRAP * skin * [1.0, 0.5, 0.3][k];
                let lit = ((cos + wrap) / (1.0 + wrap)).max(0.0) * falloff;
                let gain = ambient + lamp * lit * filter[k];
                encode_lookup(&encode, lighten(decode[p[k] as usize], gain))
            };
            [c(0), c(1), c(2), p[3]]
        })
        .collect();
    Tiled::from_slice(image.width(), image.height(), [0; 4], &out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A grey dome, brightest in the middle, on a darker grey.
    fn dome(size: u32) -> Tiled<Pixel> {
        let c = size as f32 / 2.0;
        let px: Vec<Pixel> = (0..size * size)
            .map(|i| {
                let (x, y) = ((i % size) as f32 - c, (i / size) as f32 - c);
                let t = (1.0 - (x * x + y * y).sqrt() / (c * 0.8)).clamp(0.0, 1.0);
                let v = (20000.0 + 30000.0 * t) as u16;
                [v, v, v, u16::MAX]
            })
            .collect();
        Tiled::from_slice(size, size, [0; 4], &px)
    }

    fn mean(img: &Tiled<Pixel>, x0: u32, y0: u32, x1: u32, y1: u32, k: usize) -> f32 {
        let mut sum = 0.0;
        for y in y0..y1 {
            for x in x0..x1 {
                sum += f32::from(img.get(x, y)[k]);
            }
        }
        sum / ((x1 - x0) * (y1 - y0)) as f32
    }

    /// A lamp to the left of the dome, low enough to rake it.
    fn from_the_left() -> RelightOptions {
        RelightOptions {
            x: 0.15,
            y: 0.5,
            reach: 8.0,
            intensity: 80.0,
            ambient: 5.0,
            ..Default::default()
        }
    }

    #[test]
    fn a_lamp_to_the_left_brightens_the_left_and_darkens_the_right() {
        let img = dome(256);
        let out = relight(&img, &from_the_left(), (0, 0), (256, 256));
        // How much the light changes each side, in linear light, against a
        // flat surface there.
        let flat_in = Tiled::new(256, 256, [30000, 30000, 30000, u16::MAX]);
        let flat = relight(&flat_in, &from_the_left(), (0, 0), (256, 256));
        let lin = linear_table();
        let light = |a: &Tiled<Pixel>, b: &Tiled<Pixel>, (x0, y0, x1, y1): (u32, u32, u32, u32)| {
            let mut sum = 0.0;
            for y in y0..y1 {
                for x in x0..x1 {
                    sum += lin[a.get(x, y)[0] as usize] / lin[b.get(x, y)[0] as usize];
                }
            }
            sum / ((x1 - x0) * (y1 - y0)) as f32
        };
        let gain = |r| light(&out, &img, r) / light(&flat, &flat_in, r);
        let (left, right) = ((60, 120, 90, 136), (166, 120, 196, 136));
        assert!(gain(left) > 1.05, "the side facing the lamp: {}", gain(left));
        assert!(gain(right) < 1.0, "the side away from it: {}", gain(right));
        // To the right, the other way round.
        let out = relight(&img, &RelightOptions { x: 0.85, ..from_the_left() }, (0, 0), (256, 256));
        assert!(mean(&out, right.0, right.1, right.2, right.3, 0) > mean(&out, left.0, left.1, left.2, left.3, 0));
    }

    #[test]
    fn flat_under_the_lamp_keeps_its_tone_and_falls_off_away_from_it() {
        let colour = [12000, 30000, 50000, 40000];
        let flat = Tiled::new(400, 400, colour);
        let options = RelightOptions {
            x: 0.25,
            y: 0.25,
            reach: 25.0,
            intensity: 35.0,
            ambient: 65.0,
            ..Default::default()
        };
        let out = relight(&flat, &options, (0, 0), (400, 400));
        let under = out.get(100, 100);
        assert!((0..4).all(|k| under[k].abs_diff(colour[k]) <= 3), "right under it: {under:?}");
        let (near, far) = (out.get(150, 100)[2], out.get(399, 399)[2]);
        assert!(near < 50000 && far < near, "darker further away: {near}, {far}");
        // Out of reach, only the ambient light is left.
        let ambient = options.ambient / 100.0;
        let lin = linear_table();
        assert!((lin[far as usize] / lin[50000] - ambient).abs() < 0.03, "{far}");
        assert_eq!(out.get(399, 399)[3], 40000, "alpha kept");

        let img = dome(128);
        let none = RelightOptions {
            intensity: 0.0,
            ambient: 100.0,
            ..Default::default()
        };
        let same = relight(&img, &none, (0, 0), (128, 128));
        assert!(same.to_vec().iter().zip(img.to_vec()).all(|(a, b)| (0..4).all(|k| a[k].abs_diff(b[k]) <= 3)));
    }

    #[test]
    fn a_part_previewed_comes_out_as_in_the_whole() {
        let img = dome(400);
        let filter = crate::filters::LayerFilter::Relight(RelightOptions {
            softness: 60.0,
            ..from_the_left()
        });
        let whole = filter.apply(&img);
        let part = filter.preview(&img, 150, 120, 60, 40);
        let want = whole.crop(150, 120, 60, 40);
        // The blur's running sums round a little differently from another
        // start, and the linear tables round too.
        let worst = part
            .iter()
            .zip(&want)
            .map(|(a, b)| (0..4).map(|k| a[k].abs_diff(b[k])).max().unwrap_or(0))
            .max();
        assert!(worst <= Some(3), "{worst:?} levels off");
    }

    /// A face as landmarks: a dome of points, nearest in the middle.
    fn face_points(cx: f32, cy: f32, r: f32) -> Vec<[f32; 3]> {
        let mut points = Vec::new();
        for ring in 0..=8 {
            let rr = r * ring as f32 / 8.0;
            for k in 0..(ring * 6).max(1) {
                let a = k as f32 / (ring * 6).max(1) as f32 * std::f32::consts::TAU;
                // MediaPipe's depth: nearer the camera less.
                let depth = -(r * r - rr * rr).max(0.0).sqrt();
                points.push([cx + rr * a.cos(), cy + rr * a.sin(), depth]);
            }
        }
        points
    }

    #[test]
    fn a_face_is_lit_by_its_shape_not_its_brightness() {
        // Flat grey, so brightness says nothing of the face's shape.
        let flat = Tiled::new(256, 256, [30000, 30000, 30000, u16::MAX]);
        let face = FaceShape::new(&face_points(128.0, 128.0, 80.0)).unwrap();
        let options = RelightOptions {
            x: 0.0,
            y: 0.5,
            reach: 30.0,
            intensity: 100.0,
            ambient: 20.0,
            ..Default::default()
        };
        let without = relight_faces(&flat, &options, (0, 0), (256, 256), &[]);
        let with = relight_faces(&flat, &options, (0, 0), (256, 256), std::slice::from_ref(&face));
        // Without its shape the face's two sides differ only by distance;
        // with it, the side turned away from the lamp is much darker.
        let ratio = |img: &Tiled<Pixel>| f32::from(img.get(80, 128)[0]) / f32::from(img.get(176, 128)[0]);
        assert!(ratio(&with) > ratio(&without) * 1.1, "{} against {}", ratio(&with), ratio(&without));
        // Away from the face, as before.
        assert_eq!(with.get(250, 10), without.get(250, 10));
        // The edge of its shadow is warm, as skin's are, not grey.
        let edge = with.get(150, 128);
        assert!(edge[0] > edge[1] && edge[1] > edge[2], "{edge:?}");

        // Kept and named by number, as the app hands them over.
        let id = keep_faces(vec![face]);
        let kept = relight(&flat, &RelightOptions { faces: id, ..options }, (0, 0), (256, 256));
        assert_eq!(kept.to_vec(), with.to_vec());
        // A part previewed comes out as in the whole.
        let filter = crate::filters::LayerFilter::Relight(RelightOptions { faces: id, ..options });
        let part = filter.preview(&flat, 60, 90, 40, 40);
        assert_eq!(part, with.crop(60, 90, 40, 40));
    }

    #[test]
    fn a_face_casts_a_shadow_away_from_a_low_lamp() {
        let face = FaceShape::new(&face_points(128.0, 128.0, 80.0)).unwrap();
        // Low, to the left: the dome's right side is behind its top.
        let shadows = face.shadows([0.0, 128.0, 20.0]);
        let at = |x: f32| face.sample(&shadows, x, 128.0);
        assert!(at(80.0) < 0.1, "the side facing it is lit: {}", at(80.0));
        assert!(at(185.0) > 0.5, "the far side is shaded: {}", at(185.0));
        // High overhead, nothing is.
        let overhead = face.shadows([128.0, 128.0, 5000.0]);
        assert!(overhead.iter().all(|&s| s < 0.05));
    }

    #[test]
    fn added_light_eases_into_white() {
        let bright = Tiled::new(64, 64, [60000, 60000, 60000, u16::MAX]);
        let options = RelightOptions {
            x: 0.5,
            y: 0.5,
            intensity: 100.0,
            ambient: 100.0,
            ..Default::default()
        };
        let v = relight(&bright, &options, (0, 0), (64, 64)).get(32, 32)[0];
        assert!(v > 60000 && v < u16::MAX, "brighter but short of white: {v}");
        // Dimmer, it simply scales.
        assert_eq!(lighten(0.2, 0.5), 0.1);
        assert!((lighten(0.01, 2.0) - 0.02).abs() < 1e-3, "barely bent in the shadows");
    }

    #[test]
    fn a_coloured_lamp_tints_what_it_lights() {
        let img = dome(256);
        let warm = RelightOptions {
            colour: [255, 160, 80],
            ..from_the_left()
        };
        let out = relight(&img, &warm, (0, 0), (256, 256));
        let lit = out.get(75, 128);
        assert!(lit[0] > lit[1] && lit[1] > lit[2], "orange where it's lit: {lit:?}");
    }
}

#[cfg(test)]
mod timing {
    use super::*;

    /// Relight a photo from the left and the right, for looking at:
    /// `OMAPIX_RELIGHT=photo.tif cargo test --release -p omapix-engine relight_a_photo -- --ignored`
    /// writes `target/relight/{before,left,right}.jpg`.
    #[test]
    #[ignore]
    fn relight_a_photo() {
        let Ok(path) = std::env::var("OMAPIX_RELIGHT") else {
            return;
        };
        let mut doc = crate::io::load(std::path::Path::new(&path)).expect("the photo");
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/relight");
        std::fs::create_dir_all(&out).unwrap();
        let before = doc.layers[0].pixels.clone();
        crate::export::jpeg(&doc, &out.join("before.jpg"), 85).unwrap();
        for (name, x) in [("left", 0.1), ("right", 0.9)] {
            let options = RelightOptions { x, ..Default::default() };
            doc.layers[0].pixels = relight(&before, &options, (0, 0), (doc.width, doc.height));
            crate::export::jpeg(&doc, &out.join(format!("{name}.jpg")), 85).unwrap();
        }
    }

    /// Relight a photo with lamps given as `name:x,y,reach,intensity,ambient`
    /// separated by `;`, for comparing versions by eye, its faces' shapes
    /// from `OMAPIX_RELIGHT_POINTS` if given (`omapix-ai`'s `face_points`
    /// example writes them):
    /// `OMAPIX_RELIGHT=photo.jpg OMAPIX_RELIGHT_LAMPS="left:0.4,0.25,40,50,50"
    /// OMAPIX_RELIGHT_OUT=dir cargo test --release -p omapix-engine relight_lamps -- --ignored`
    #[test]
    #[ignore]
    fn relight_lamps() {
        let (Ok(path), Ok(lamps), Ok(out)) = (
            std::env::var("OMAPIX_RELIGHT"),
            std::env::var("OMAPIX_RELIGHT_LAMPS"),
            std::env::var("OMAPIX_RELIGHT_OUT"),
        ) else {
            return;
        };
        let out = std::path::Path::new(&out);
        std::fs::create_dir_all(out).unwrap();
        let mut doc = crate::io::load(std::path::Path::new(&path)).expect("the photo");
        let before = doc.layers[0].pixels.clone();
        let faces: Vec<FaceShape> = std::env::var("OMAPIX_RELIGHT_POINTS")
            .map(|f| std::fs::read_to_string(f).unwrap())
            .unwrap_or_default()
            .split("\n\n")
            .filter_map(|face| {
                let points: Vec<[f32; 3]> = face
                    .lines()
                    .map(|l| {
                        let v: Vec<f32> = l.split(' ').map(|v| v.parse().unwrap()).collect();
                        [v[0], v[1], v[2]]
                    })
                    .collect();
                FaceShape::new(&points)
            })
            .collect();
        for lamp in lamps.split(';') {
            let (name, values) = lamp.split_once(':').unwrap();
            let v: Vec<f32> = values.split(',').map(|v| v.trim().parse().unwrap()).collect();
            let options = RelightOptions {
                x: v[0],
                y: v[1],
                reach: v[2],
                intensity: v[3],
                ambient: v[4],
                ..Default::default()
            };
            doc.layers[0].pixels = relight_faces(&before, &options, (0, 0), (doc.width, doc.height), &faces);
            crate::export::jpeg(&doc, &out.join(format!("{name}.jpg")), 90).unwrap();
        }
    }

    /// `cargo test --release -p omapix-engine relight_at_24_mp -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn relight_at_24_mp() {
        let (w, h) = (6000, 4000);
        let px: Vec<Pixel> = (0..w * h).map(|i| [(i % 65536) as u16, ((i / 7) % 65536) as u16, 30000, u16::MAX]).collect();
        let img = Tiled::from_slice(w, h, [0; 4], &px);
        for softness in [25.0, 100.0] {
            let start = std::time::Instant::now();
            let out = relight(
                &img,
                &RelightOptions {
                    softness,
                    ..Default::default()
                },
                (0, 0),
                (w, h),
            );
            eprintln!("softness {softness}: {:.2} s", start.elapsed().as_secs_f32());
            assert_eq!(out.width(), w);
        }
    }
}
