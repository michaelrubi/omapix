//! Selections: which part of the image edits apply to.
//!
//! A selection is a greyscale coverage image (0 = unselected, 65535 =
//! fully selected), so feathered and anti-aliased edges are partial, as in
//! Photoshop. It also keeps its outline, traced along the edges of the
//! pixels more than half selected, for drawing "marching ants".

use std::collections::HashMap;

use rayon::prelude::*;

use crate::tiled::{TILE, TILE_PIXELS, Tiled};
use crate::{Pixel, Raster};

const MAX: f32 = u16::MAX as f32;

/// Pixels with coverage above this are inside the outline.
const HALF: f32 = MAX / 2.0;

/// Vertical sub-samples per pixel row when filling shapes, for smooth edges.
const SUBSAMPLES: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Combine {
    Replace,
    Add,
    Subtract,
    Intersect,
}

impl Combine {
    pub fn from_modifiers(shift: bool, alt: bool) -> Self {
        match (shift, alt) {
            (true, true) => Self::Intersect,
            (true, false) => Self::Add,
            (false, true) => Self::Subtract,
            (false, false) => Self::Replace,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Red,
    Green,
    Blue,
    Luminosity,
}

impl Channel {
    /// Extract a channel's value (0–65535) from a pixel.
    #[inline]
    pub fn value(self, p: Pixel) -> u16 {
        match self {
            Channel::Red => p[0],
            Channel::Green => p[1],
            Channel::Blue => p[2],
            Channel::Luminosity => {
                // Luminance: 0.3 * R + 0.59 * G + 0.11 * B, matching the histogram.
                ((19661 * u64::from(p[0])
                    + 38666 * u64::from(p[1])
                    + 7209 * u64::from(p[2])
                    + 32768)
                    >> 16) as u16
            }
        }
    }
}

#[derive(Clone)]
pub struct Selection {
    pub coverage: Tiled<u16>,
    /// Closed outlines in image pixels, for display.
    pub outlines: Vec<Vec<(f32, f32)>>,
}

impl Selection {
    /// A selection with the given coverage, and its outline traced.
    pub fn from_coverage(coverage: Tiled<u16>) -> Self {
        let outlines = trace(&coverage);
        Self { coverage, outlines }
    }

    /// A selection from one channel of an image (e.g. the visible composite).
    pub fn from_channel(raster: &Raster, channel: Channel) -> Self {
        let (w, h) = (raster.width(), raster.height());
        let coverage = Tiled::from_tiles(w, h, 0, |col, row| {
            let mut tile = vec![0u16; TILE_PIXELS];
            let x0 = col * TILE;
            let y0 = row * TILE;
            let tw = TILE.min(w.saturating_sub(x0));
            let th = TILE.min(h.saturating_sub(y0));
            for ty in 0..th {
                let row_pixels = raster.row(y0 + ty);
                let src = &row_pixels[x0 as usize..(x0 + tw) as usize];
                let dst = &mut tile[(ty * TILE) as usize..(ty * TILE + tw) as usize];
                for (d, &p) in dst.iter_mut().zip(src) {
                    *d = channel.value(p);
                }
            }
            tile.iter().any(|&v| v != 0).then_some(tile)
        });
        Self::from_coverage(coverage)
    }

    /// A selection from a layer's alpha (transparency).
    pub fn from_alpha(pixels: &Tiled<Pixel>) -> Self {
        let fill = pixels.fill()[3];
        let coverage = Tiled::from_tiles(pixels.width(), pixels.height(), fill, |col, row| {
            let tile = pixels.tile(col, row)?;
            let mut alpha = vec![fill; TILE_PIXELS];
            for (dst, px) in alpha.iter_mut().zip(tile.iter()) {
                *dst = px[3];
            }
            alpha.iter().any(|&v| v != fill).then_some(alpha)
        });
        Self::from_coverage(coverage)
    }

    /// A selection from a layer mask.
    pub fn from_mask(mask: &Tiled<u16>) -> Self {
        Self::from_coverage(mask.clone())
    }

    pub fn all(width: u32, height: u32) -> Self {
        Self::from_coverage(Tiled::new(width, height, u16::MAX))
    }

    /// A filled polygon (even-odd rule), with anti-aliased edges.
    pub fn polygon(width: u32, height: u32, points: &[(f32, f32)]) -> Self {
        Self::from_coverage(fill_polygon(width, height, points))
    }

    pub fn rectangle(width: u32, height: u32, (x0, y0): (f32, f32), (x1, y1): (f32, f32)) -> Self {
        let (l, r) = (x0.min(x1), x0.max(x1));
        let (t, b) = (y0.min(y1), y0.max(y1));
        Self::polygon(width, height, &[(l, t), (r, t), (r, b), (l, b)])
    }

    pub fn ellipse(width: u32, height: u32, (x0, y0): (f32, f32), (x1, y1): (f32, f32)) -> Self {
        let (l, r) = (x0.min(x1), x0.max(x1));
        let (t, b) = (y0.min(y1), y0.max(y1));
        let rx = (r - l) * 0.5;
        let ry = (b - t) * 0.5;
        if rx <= 0.0 || ry <= 0.0 {
            return Self::from_coverage(Tiled::new(width, height, 0));
        }
        Self::from_coverage(fill_ellipse(width, height, l + rx, t + ry, rx, ry))
    }

    pub fn width(&self) -> u32 {
        self.coverage.width()
    }

    pub fn height(&self) -> u32 {
        self.coverage.height()
    }

    /// Coverage at a pixel, 0–1.
    pub fn at(&self, x: u32, y: u32) -> f32 {
        f32::from(self.coverage.get(x, y)) / MAX
    }

    /// True if nothing is selected.
    pub fn is_empty(&self) -> bool {
        let (w, h) = (self.coverage.width(), self.coverage.height());
        for row in 0..self.coverage.rows() {
            for col in 0..self.coverage.cols() {
                let x0 = col * TILE;
                let y0 = row * TILE;
                let tw = TILE.min(w.saturating_sub(x0));
                let th = TILE.min(h.saturating_sub(y0));
                if tw == 0 || th == 0 {
                    continue;
                }
                match self.coverage.tile(col, row) {
                    Some(tile) => {
                        for ty in 0..th {
                            for tx in 0..tw {
                                if tile[(ty * TILE + tx) as usize] != 0 {
                                    return false;
                                }
                            }
                        }
                    }
                    None => {
                        if self.coverage.fill() != 0 {
                            return false;
                        }
                    }
                }
            }
        }
        true
    }

    /// True if everything is selected (coverage is MAX everywhere).
    pub fn is_all(&self) -> bool {
        let (w, h) = (self.coverage.width(), self.coverage.height());
        for row in 0..self.coverage.rows() {
            for col in 0..self.coverage.cols() {
                let x0 = col * TILE;
                let y0 = row * TILE;
                let tw = TILE.min(w.saturating_sub(x0));
                let th = TILE.min(h.saturating_sub(y0));
                if tw == 0 || th == 0 {
                    continue;
                }
                match self.coverage.tile(col, row) {
                    Some(tile) => {
                        for ty in 0..th {
                            for tx in 0..tw {
                                if tile[(ty * TILE + tx) as usize] != u16::MAX {
                                    return false;
                                }
                            }
                        }
                    }
                    None => {
                        if self.coverage.fill() != u16::MAX {
                            return false;
                        }
                    }
                }
            }
        }
        true
    }

    /// The smallest rectangle holding everything selected, as `[x, y,
    /// width, height]`, or `None` if nothing is.
    pub fn bounds(&self) -> Option<[u32; 4]> {
        let c = &self.coverage;
        let (w, h) = (c.width(), c.height());
        if c.fill() != 0 {
            return Some([0, 0, w, h]);
        }
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
        for row in 0..c.rows() {
            for col in 0..c.cols() {
                let Some(tile) = c.tile(col, row) else {
                    continue;
                };
                for (i, _) in tile.iter().enumerate().filter(|&(_, &v)| v != 0) {
                    let x = col * TILE + i as u32 % TILE;
                    let y = row * TILE + i as u32 / TILE;
                    if x < w && y < h {
                        (x0, y0) = (x0.min(x), y0.min(y));
                        (x1, y1) = (x1.max(x), y1.max(y));
                    }
                }
            }
        }
        (x0 <= x1).then(|| [x0, y0, x1 - x0 + 1, y1 - y0 + 1])
    }

    pub fn combine(&self, other: &Selection, how: Combine) -> Selection {
        let op = |a: u16, b: u16| -> u16 {
            let (a, b) = (f32::from(a) / MAX, f32::from(b) / MAX);
            let v = match how {
                Combine::Replace => b,
                Combine::Add => a + b - a * b,
                Combine::Subtract => a * (1.0 - b),
                Combine::Intersect => a * b,
            };
            (v.clamp(0.0, 1.0) * MAX).round() as u16
        };
        let fill = op(self.coverage.fill(), other.coverage.fill());
        let (a, b) = (&self.coverage, &other.coverage);
        let coverage = Tiled::from_tiles(a.width(), a.height(), fill, |col, row| {
            let (ta, tb) = (a.tile(col, row), b.tile(col, row));
            if ta.is_none() && tb.is_none() {
                return None;
            }
            let tile: Vec<u16> = (0..TILE_PIXELS)
                .map(|i| op(ta.map_or(a.fill(), |t| t[i]), tb.map_or(b.fill(), |t| t[i])))
                .collect();
            tile.iter().any(|&v| v != fill).then_some(tile)
        });
        Selection::from_coverage(coverage)
    }

    pub fn invert(&self) -> Selection {
        let c = &self.coverage;
        let coverage = Tiled::from_tiles(c.width(), c.height(), u16::MAX - c.fill(), |col, row| {
            c.tile(col, row)
                .map(|t| t.iter().map(|v| u16::MAX - v).collect())
        });
        Selection::from_coverage(coverage)
    }

    /// The same selection moved by (`dx`, `dy`) pixels, as when the Move
    /// tool moves selected pixels.
    pub fn translated(&self, dx: i32, dy: i32) -> Selection {
        Selection::from_coverage(self.coverage.translated(dx, dy, 0))
    }

    /// The same selection transformed by `t`, as Free Transform does to
    /// selected pixels.
    pub fn transformed(&self, t: &crate::transform::Affine) -> Selection {
        let coverage = crate::transform::transformed(&self.coverage, t, 0, crate::transform::Resampling::Bilinear);
        Selection::from_coverage(coverage)
    }

    /// Soften the edge (Photoshop's Select › Modify › Feather), `radius`
    /// being the blur's standard deviation in pixels.
    pub fn feather(&self, radius: f32) -> Selection {
        let (w, h) = (self.width() as usize, self.height() as usize);
        let buf: Vec<[f32; 4]> = self
            .coverage
            .to_vec()
            .into_par_iter()
            .map(|v| [f32::from(v), 0.0, 0.0, 0.0])
            .collect();
        let blurred = crate::filters::blur_buffer(buf, w, h, radius);
        let values: Vec<u16> = blurred
            .into_par_iter()
            .map(|v| v[0].round().clamp(0.0, MAX) as u16)
            .collect();
        Selection::from_coverage(Tiled::from_slice(self.width(), self.height(), 0, &values))
    }

    /// Magic Wand: select similar colours starting from `start = (x, y)`.
    /// `tolerance` is in 16-bit channel units (e.g. `widen(32)` for Photoshop's default 32).
    /// If `contiguous`, only connected matching pixels are selected.
    /// If `anti_alias`, the boundary pixels get smooth anti-aliased coverage.
    pub fn magic_wand(
        width: u32,
        height: u32,
        start: (u32, u32),
        tolerance: u16,
        contiguous: bool,
        anti_alias: bool,
        pixel_at: impl Fn(u32, u32) -> Pixel + Sync,
    ) -> Self {
        if width == 0 || height == 0 || start.0 >= width || start.1 >= height {
            return Self::from_coverage(Tiled::new(width, height, 0));
        }

        let seed = pixel_at(start.0, start.1);
        let matches = |x: u32, y: u32| -> bool {
            let p = pixel_at(x, y);
            if seed[3] == 0 && p[3] == 0 {
                return true;
            }
            seed[0].abs_diff(p[0]) <= tolerance
                && seed[1].abs_diff(p[1]) <= tolerance
                && seed[2].abs_diff(p[2]) <= tolerance
                && seed[3].abs_diff(p[3]) <= tolerance
        };

        let mut visited = vec![false; (width * height) as usize];
        if contiguous {
            let mut stack = Vec::new();
            stack.push((start.0, start.1));

            while let Some((x, y)) = stack.pop() {
                let idx = (y * width + x) as usize;
                if visited[idx] || !matches(x, y) {
                    continue;
                }
                let mut x_left = x;
                while x_left > 0 && !visited[(y * width + x_left - 1) as usize] && matches(x_left - 1, y) {
                    x_left -= 1;
                }
                let mut x_right = x;
                while x_right + 1 < width && !visited[(y * width + x_right + 1) as usize] && matches(x_right + 1, y) {
                    x_right += 1;
                }
                for xi in x_left..=x_right {
                    visited[(y * width + xi) as usize] = true;
                }

                if y > 0 {
                    scan_row(y - 1, x_left, x_right, width, &visited, &matches, &mut stack);
                }
                if y + 1 < height {
                    scan_row(y + 1, x_left, x_right, width, &visited, &matches, &mut stack);
                }
            }
        } else {
            visited
                .par_chunks_mut(width as usize)
                .enumerate()
                .for_each(|(y, row)| {
                    for (x, v) in row.iter_mut().enumerate() {
                        if matches(x as u32, y as u32) {
                            *v = true;
                        }
                    }
                });
        }

        // Bounding box of visited pixels.
        let mut min_x = u32::MAX;
        let mut max_x = 0;
        let mut min_y = u32::MAX;
        let mut max_y = 0;
        let mut any = false;

        for y in 0..height {
            let row = &visited[(y * width) as usize..((y + 1) * width) as usize];
            if let Some(first) = row.iter().position(|&v| v) {
                any = true;
                min_y = min_y.min(y);
                max_y = max_y.max(y);
                min_x = min_x.min(first as u32);
                let last = row.iter().rposition(|&v| v).unwrap();
                max_x = max_x.max(last as u32);
            }
        }

        if !any {
            return Self::from_coverage(Tiled::new(width, height, 0));
        }

        let is_inside = |x: i32, y: i32| -> bool {
            let cx = x.clamp(0, width as i32 - 1) as u32;
            let cy = y.clamp(0, height as i32 - 1) as u32;
            visited[(cy * width + cx) as usize]
        };

        let bbox_x0 = min_x.saturating_sub(1);
        let bbox_y0 = min_y.saturating_sub(1);
        let bbox_x1 = (max_x + 1).min(width - 1);
        let bbox_y1 = (max_y + 1).min(height - 1);

        let weights = [
            [1, 2, 1],
            [2, 4, 2],
            [1, 2, 1],
        ];

        let coverage = Tiled::from_tiles(width, height, 0u16, |col, row| {
            let x0 = col * TILE;
            let y0 = row * TILE;
            let x1 = (x0 + TILE).min(width);
            let y1 = (y0 + TILE).min(height);

            if x0 > bbox_x1 || x1 <= bbox_x0 || y0 > bbox_y1 || y1 <= bbox_y0 {
                return None;
            }

            let mut tile = vec![0u16; TILE_PIXELS];
            let mut has_non_zero = false;

            for y in y0..y1 {
                let ty = y - y0;
                for x in x0..x1 {
                    let tx = x - x0;
                    let val = if !anti_alias {
                        if visited[(y * width + x) as usize] {
                            u16::MAX
                        } else {
                            0
                        }
                    } else {
                        let center_inside = is_inside(x as i32, y as i32);
                        let mut sum = 0u32;
                        for dy in -1..=1 {
                            for dx in -1..=1 {
                                if is_inside(x as i32 + dx, y as i32 + dy) {
                                    sum += weights[(dy + 1) as usize][(dx + 1) as usize];
                                }
                            }
                        }
                        if sum == 0 {
                            0
                        } else if sum == 16 {
                            u16::MAX
                        } else {
                            let n = sum as f32 / 16.0;
                            let c = if center_inside {
                                0.5 + 0.5 * n
                            } else {
                                0.5 * n
                            };
                            (c * MAX).round() as u16
                        }
                    };

                    if val > 0 {
                        tile[(ty * TILE + tx) as usize] = val;
                        has_non_zero = true;
                    }
                }
            }

            has_non_zero.then_some(tile)
        });

        Self::from_coverage(coverage)
    }

    pub fn magic_wand_raster(
        raster: &Raster,
        start: (u32, u32),
        tolerance: u16,
        contiguous: bool,
        anti_alias: bool,
    ) -> Self {
        Self::magic_wand(
            raster.width(),
            raster.height(),
            start,
            tolerance,
            contiguous,
            anti_alias,
            |x, y| raster.get(x, y),
        )
    }

    pub fn magic_wand_tiled(
        tiled: &Tiled<Pixel>,
        start: (u32, u32),
        tolerance: u16,
        contiguous: bool,
        anti_alias: bool,
    ) -> Self {
        Self::magic_wand(
            tiled.width(),
            tiled.height(),
            start,
            tolerance,
            contiguous,
            anti_alias,
            |x, y| tiled.get(x, y),
        )
    }
}

fn scan_row(
    y: u32,
    x1: u32,
    x2: u32,
    width: u32,
    visited: &[bool],
    matches: &impl Fn(u32, u32) -> bool,
    stack: &mut Vec<(u32, u32)>,
) {
    let mut in_run = false;
    for x in x1..=x2 {
        let idx = (y * width + x) as usize;
        if !visited[idx] && matches(x, y) {
            if !in_run {
                stack.push((x, y));
                in_run = true;
            }
        } else {
            in_run = false;
        }
    }
}

/// The outlines round the pixels more than half selected, along the pixel
/// edges as in Photoshop, by marching squares with cells centred on the
/// pixel corners. Everything outside the image counts as unselected, so
/// every outline closes. Only tiles that aren't all inside or all outside
/// are searched in full.
fn trace(coverage: &Tiled<u16>) -> Vec<Vec<(f32, f32)>> {
    let (w, h) = (coverage.width() as i32, coverage.height() as i32);
    let (cols, rows) = (coverage.cols(), coverage.rows());
    let value = |x: i32, y: i32| -> f32 {
        if x < 0 || y < 0 || x >= w || y >= h {
            0.0
        } else {
            f32::from(coverage.get(x as u32, y as u32))
        }
    };

    // Which side of the outline each tile's pixels are on: Some(inside)
    // if all on one side, None if mixed.
    let sides: Vec<Option<bool>> = (0..rows * cols)
        .into_par_iter()
        .map(|i| {
            let (col, row) = (i % cols, i / cols);
            let Some(tile) = coverage.tile(col, row) else {
                return Some(f32::from(coverage.fill()) > HALF);
            };
            let tw = (w as u32 - col * TILE).min(TILE) as usize;
            let th = (h as u32 - row * TILE).min(TILE) as usize;
            let first = f32::from(tile[0]) > HALF;
            (0..th)
                .flat_map(|y| &tile[y * TILE as usize..][..tw])
                .all(|&v| (f32::from(v) > HALF) == first)
                .then_some(first)
        })
        .collect();
    let side = |col: i64, row: i64| -> Option<bool> {
        if col < 0 || row < 0 || col >= cols as i64 || row >= rows as i64 {
            Some(false)
        } else {
            sides[(row * cols as i64 + col) as usize]
        }
    };

    // Cells are named by their bottom-right pixel (bx, by), with bx in
    // 0..=w and by in 0..=h. Tile (col, row) searches the cells whose
    // bottom-right pixel it holds, and the last column and row of tiles
    // also the cells along the right and bottom edges.
    let segments: Vec<Segment> = (0..rows * cols)
        .into_par_iter()
        .flat_map_iter(|i| {
            let (col, row) = (i % cols, i / cols);
            let (x0, y0) = ((col * TILE) as i32, (row * TILE) as i32);
            let x1 = if col + 1 == cols { w + 1 } else { x0 + TILE as i32 };
            let y1 = if row + 1 == rows { h + 1 } else { y0 + TILE as i32 };
            let own = side(col as i64, row as i64);
            let (c, r) = (col as i64, row as i64);
            let mut around = [side(c - 1, r - 1), side(c, r - 1), side(c - 1, r)].into_iter();
            let edge = (col + 1 == cols || row + 1 == rows).then_some(Some(false));
            let mut segments = Vec::new();
            if own.is_some() && around.all(|s| s == own) && edge.is_none_or(|s| s == own) {
                return segments;
            }
            for by in y0..y1 {
                // Inside a tile that's all on one side, only the cells
                // reaching into other tiles or past the image can cross.
                let whole_row = own.is_none() || by == y0 || by == h;
                let xs: Vec<i32> = if whole_row {
                    (x0..x1).collect()
                } else if x1 > w {
                    vec![x0, w]
                } else {
                    vec![x0]
                };
                for bx in xs {
                    cell(bx, by, &value, &mut segments);
                }
            }
            segments
        })
        .collect();

    // Join the segments into closed outlines. Each crossing is the start
    // of exactly one segment and the end of another.
    let mut next: HashMap<u64, &Segment> = segments.iter().map(|s| (s.from, s)).collect();
    let mut outlines = Vec::new();
    for s in &segments {
        let mut key = s.from;
        let mut outline = Vec::new();
        while let Some(s) = next.remove(&key) {
            outline.extend([s.point, s.corner]);
            key = s.to;
        }
        let outline = simplify(outline);
        if outline.len() >= 3 {
            outlines.push(outline);
        }
    }
    outlines
}

/// A piece of outline across one cell, between two crossings named by the
/// pixel edge they lie on, turning at the cell's middle (a pixel corner).
/// `point` is where it starts.
struct Segment {
    from: u64,
    to: u64,
    point: (f32, f32),
    corner: (f32, f32),
}

/// A crossing on the edge between pixel (x, y) and its right or lower
/// neighbour.
fn edge_key(x: i32, y: i32, down: bool) -> u64 {
    ((x + 1) as u64) << 32 | ((y + 1) as u64) << 1 | u64::from(down)
}

/// Add the segments crossing the cell whose bottom-right pixel is (bx,
/// by). They run with the selection on the same side, so neighbouring
/// cells' segments join end to start.
fn cell(bx: i32, by: i32, value: &impl Fn(i32, i32) -> f32, out: &mut Vec<Segment>) {
    let (l, t) = (bx - 1, by - 1);
    // Corners clockwise from the top left.
    let v = [value(l, t), value(bx, t), value(bx, by), value(l, by)];
    let inside = v.map(|v| v > HALF);
    if inside.iter().all(|&i| i == inside[0]) {
        return;
    }
    // The crossing between two neighbouring pixels, the first above or to
    // the left of the second, at the middle of the edge they share.
    let crossing = |x: i32, y: i32, down: bool| {
        let point = if down {
            (x as f32 + 0.5, y as f32 + 1.0)
        } else {
            (x as f32 + 1.0, y as f32 + 0.5)
        };
        (edge_key(x, y, down), point)
    };
    // Crossings clockwise round the cell, each marked if going clockwise
    // enters the selection there.
    let mut crossings = Vec::with_capacity(4);
    if inside[0] != inside[1] {
        crossings.push((crossing(l, t, false), inside[1]));
    }
    if inside[1] != inside[2] {
        crossings.push((crossing(bx, t, true), inside[2]));
    }
    if inside[2] != inside[3] {
        crossings.push((crossing(l, by, false), inside[3]));
    }
    if inside[3] != inside[0] {
        crossings.push((crossing(l, t, true), inside[0]));
    }
    // With two pixels in on opposite corners, the average decides whether
    // they join (the outline goes round the corners that are out) or not
    // (it goes round the ones that are in).
    let n = crossings.len();
    let middle_in = v.iter().sum::<f32>() / 4.0 > HALF;
    for (i, &((from, point), enters)) in crossings.iter().enumerate() {
        if enters {
            let j = if middle_in { (i + n - 1) % n } else { (i + 1) % n };
            out.push(Segment {
                from,
                to: crossings[j].0.0,
                point,
                corner: (bx as f32, by as f32),
            });
        }
    }
}

/// Drop points on the straight line from the last point kept to the next
/// one, so straight edges are single lines.
fn simplify(points: Vec<(f32, f32)>) -> Vec<(f32, f32)> {
    let straight = |prev: (f32, f32), p: (f32, f32), next: (f32, f32)| {
        let (dx, dy) = (next.0 - prev.0, next.1 - prev.1);
        let cross = (p.0 - prev.0) * dy - (p.1 - prev.1) * dx;
        let ahead = (p.0 - prev.0) * dx + (p.1 - prev.1) * dy > 0.0;
        ahead && cross == 0.0
    };
    let n = points.len();
    let mut out = Vec::with_capacity(n);
    for (i, &p) in points.iter().enumerate() {
        let next = points[(i + 1) % n];
        if !out.last().is_some_and(|&prev| straight(prev, p, next)) {
            out.push(p);
        }
    }
    // The first point is always kept above, but may be on a straight edge.
    if out.len() > 3 && straight(out[out.len() - 1], out[0], out[1]) {
        out.remove(0);
    }
    out
}

/// Rasterise a polygon with the even-odd rule. Each pixel row is sampled
/// on several sub-rows, and spans get fractional coverage at their ends.
fn fill_polygon(width: u32, height: u32, points: &[(f32, f32)]) -> Tiled<u16> {
    if points.len() < 3 {
        return Tiled::new(width, height, 0);
    }
    let min_y = points
        .iter()
        .map(|p| p.1)
        .fold(f32::MAX, f32::min)
        .floor()
        .max(0.0) as u32;
    let max_y = (points.iter().map(|p| p.1).fold(f32::MIN, f32::max).ceil() as u32).min(height);
    let edges: Vec<((f32, f32), (f32, f32))> = (0..points.len())
        .map(|i| (points[i], points[(i + 1) % points.len()]))
        .collect();

    // Coverage per row, computed in parallel.
    let rows: Vec<(u32, Vec<f32>)> = (min_y..max_y)
        .into_par_iter()
        .map(|y| {
            let mut row = vec![0f32; width as usize];
            let mut crossings = Vec::new();
            for s in 0..SUBSAMPLES {
                let sy = y as f32 + (s as f32 + 0.5) / SUBSAMPLES as f32;
                crossings.clear();
                for &((x0, y0), (x1, y1)) in &edges {
                    if (y0 <= sy && y1 > sy) || (y1 <= sy && y0 > sy) {
                        crossings.push(x0 + (sy - y0) / (y1 - y0) * (x1 - x0));
                    }
                }
                crossings.sort_by(f32::total_cmp);
                for &[left, right] in crossings.as_chunks::<2>().0 {
                    let (a, b) = (left.max(0.0), right.min(width as f32));
                    if a >= b {
                        continue;
                    }
                    let (ia, ib) = (a.floor() as usize, (b.ceil() as usize).min(width as usize));
                    for (x, cell) in row.iter_mut().enumerate().take(ib).skip(ia) {
                        let overlap = (b.min(x as f32 + 1.0) - a.max(x as f32)).clamp(0.0, 1.0);
                        *cell += overlap / SUBSAMPLES as f32;
                    }
                }
            }
            (y, row)
        })
        .collect();

    let mut out = Tiled::new(width, height, 0u16);
    for (y, row) in rows {
        for (x, &v) in row.iter().enumerate() {
            // Only store coverage that survives rounding, so a zero-area
            // shape (a lasso drawn as a line) selects nothing at all.
            let value = (v.min(1.0) * MAX).round() as u16;
            if value > 0 {
                let tile = out.tile_mut(x as u32 / TILE, y / TILE);
                tile[((y % TILE) * TILE + x as u32 % TILE) as usize] = value;
            }
        }
    }
    out
}

/// Rasterise an axis-aligned ellipse with smooth anti-aliased edges,
/// using the same sub-row sampling as `fill_polygon`.
fn fill_ellipse(width: u32, height: u32, cx: f32, cy: f32, rx: f32, ry: f32) -> Tiled<u16> {
    let min_y = (cy - ry).floor().max(0.0) as u32;
    let max_y = ((cy + ry).ceil() as u32).min(height);

    let rows: Vec<(u32, Vec<f32>)> = (min_y..max_y)
        .into_par_iter()
        .map(|y| {
            let mut row = vec![0f32; width as usize];
            for s in 0..SUBSAMPLES {
                let sy = y as f32 + (s as f32 + 0.5) / SUBSAMPLES as f32;
                let dy = (sy - cy).abs();
                if dy < ry {
                    let dx = rx * (1.0 - (dy / ry).powi(2)).sqrt();
                    let left = (cx - dx).max(0.0);
                    let right = (cx + dx).min(width as f32);
                    if left < right {
                        let (ia, ib) = (left.floor() as usize, (right.ceil() as usize).min(width as usize));
                        for (x, cell) in row.iter_mut().enumerate().take(ib).skip(ia) {
                            let overlap = (right.min(x as f32 + 1.0) - left.max(x as f32)).clamp(0.0, 1.0);
                            *cell += overlap / SUBSAMPLES as f32;
                        }
                    }
                }
            }
            (y, row)
        })
        .collect();

    let mut out = Tiled::new(width, height, 0u16);
    for (y, row) in rows {
        for (x, &v) in row.iter().enumerate() {
            let value = (v.min(1.0) * MAX).round() as u16;
            if value > 0 {
                let tile = out.tile_mut(x as u32 / TILE, y / TILE);
                tile[((y % TILE) * TILE + x as u32 % TILE) as usize] = value;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rectangle_selects_inside_only_with_soft_half_pixel_edges() {
        let s = Selection::rectangle(100, 100, (10.0, 10.0), (50.5, 40.0));
        assert_eq!(s.at(30, 20), 1.0);
        assert_eq!(s.at(5, 5), 0.0);
        assert!((s.at(50, 20) - 0.5).abs() < 0.01, "{}", s.at(50, 20));
        assert!(!s.is_empty());
    }

    #[test]
    fn ellipse_selects_interior_with_soft_edges() {
        // Circle centered at (50.5, 50.5) with radius 20 (bounding box 30.5 to 70.5).
        let s = Selection::ellipse(100, 100, (30.5, 30.5), (70.5, 70.5));
        assert_eq!(s.at(50, 50), 1.0);
        assert_eq!(s.at(10, 10), 0.0);
        assert_eq!(s.at(30, 30), 0.0); // Corner of bounding box is outside circle

        // Top edge at y = 30.5 cuts through pixel row 30.
        let edge = s.at(50, 30);
        assert!(edge > 0.0 && edge < 1.0, "edge: {edge}");
        assert!(!s.is_empty());
    }

    #[test]
    fn degenerate_ellipse_is_empty() {
        assert!(Selection::ellipse(100, 100, (20.0, 20.0), (20.0, 50.0)).is_empty());
        assert!(Selection::ellipse(100, 100, (20.0, 20.0), (50.0, 20.0)).is_empty());
    }

    #[test]
    fn combining_adds_subtracts_and_inverts() {
        let (w, h) = (100, 100);
        let a = Selection::rectangle(w, h, (0.0, 0.0), (50.0, 100.0));
        let b = Selection::rectangle(w, h, (25.0, 0.0), (75.0, 100.0));
        assert_eq!(a.combine(&b, Combine::Add).at(60, 50), 1.0);
        assert_eq!(a.combine(&b, Combine::Subtract).at(30, 50), 0.0);
        assert_eq!(a.combine(&b, Combine::Subtract).at(10, 50), 1.0);
        assert_eq!(a.combine(&b, Combine::Intersect).at(10, 50), 0.0);
        assert_eq!(a.invert().at(80, 50), 1.0);
        assert_eq!(a.invert().at(10, 50), 0.0);
        assert!(Selection::all(w, h).invert().is_empty());
    }

    #[test]
    fn feather_softens_the_edge() {
        let s = Selection::rectangle(200, 100, (50.0, 0.0), (150.0, 100.0)).feather(5.0);
        assert!(s.at(100, 50) > 0.99);
        let edge = s.at(50, 50);
        assert!(edge > 0.3 && edge < 0.7, "{edge}");
        assert!(s.at(40, 50) > 0.0 && s.at(40, 50) < s.at(60, 50));
    }

    #[test]
    fn a_lasso_drawn_as_a_line_selects_nothing() {
        let line: Vec<(f32, f32)> = (0..20)
            .map(|i| (10.0 + i as f32 * 3.7, 5.0 + i as f32 * 2.9))
            .collect();
        assert!(Selection::polygon(100, 100, &line).is_empty());
    }

    #[test]
    fn bounds_cover_just_what_is_selected() {
        let s = Selection::rectangle(600, 400, (300.0, 10.0), (310.5, 290.0));
        assert_eq!(s.bounds(), Some([300, 10, 11, 280]));
        assert_eq!(Selection::all(600, 400).bounds(), Some([0, 0, 600, 400]));
        assert_eq!(Selection::all(600, 400).invert().bounds(), None);
        // Tile padding past the image edge doesn't count.
        let mut coverage = Tiled::new(600, 400, 0);
        coverage.tile_mut(2, 1).fill(u16::MAX);
        let corner = Selection {
            coverage,
            outlines: Vec::new(),
        };
        assert_eq!(corner.bounds(), Some([512, 256, 88, 144]));
    }

    /// Check the outlines are the given polygons, in any order and
    /// starting anywhere.
    fn assert_traces(s: &Selection, polygons: &[&[(f32, f32)]]) {
        assert_eq!(s.outlines.len(), polygons.len(), "{:?}", s.outlines);
        for &poly in polygons {
            assert!(
                s.outlines
                    .iter()
                    .any(|o| o.len() == poly.len() && poly.iter().all(|c| o.contains(c))),
                "no outline {poly:?} in {:?}",
                s.outlines
            );
        }
    }

    #[test]
    fn a_rectangle_has_one_outline_round_its_edge() {
        let s = Selection::rectangle(100, 100, (10.0, 20.0), (60.0, 50.0));
        assert_traces(&s, &[&[(10.0, 20.0), (60.0, 20.0), (60.0, 50.0), (10.0, 50.0)]]);
        // Sub-pixel edges go round the pixels at least half selected.
        let left = |x| {
            let s = Selection::rectangle(100, 100, (x, 20.0), (60.0, 50.0));
            s.outlines[0].iter().map(|p| p.0).fold(f32::MAX, f32::min)
        };
        assert_eq!((left(10.25), left(10.5), left(10.75)), (10.0, 10.0, 11.0));
    }

    #[test]
    fn added_shapes_share_one_outline() {
        let a = Selection::rectangle(100, 100, (10.0, 10.0), (60.0, 60.0));
        let b = Selection::rectangle(100, 100, (40.0, 40.0), (90.0, 90.0));
        assert_traces(
            &a.combine(&b, Combine::Add),
            &[&[
                (10.0, 10.0),
                (60.0, 10.0),
                (60.0, 40.0),
                (90.0, 40.0),
                (90.0, 90.0),
                (40.0, 90.0),
                (40.0, 60.0),
                (10.0, 60.0),
            ]],
        );
        assert_traces(
            &a.combine(&b, Combine::Subtract),
            &[&[
                (10.0, 10.0),
                (60.0, 10.0),
                (60.0, 40.0),
                (40.0, 40.0),
                (40.0, 60.0),
                (10.0, 60.0),
            ]],
        );
        assert_traces(
            &a.combine(&b, Combine::Intersect),
            &[&[(40.0, 40.0), (60.0, 40.0), (60.0, 60.0), (40.0, 60.0)]],
        );
        // Apart, they keep an outline each.
        let c = Selection::rectangle(100, 100, (70.0, 10.0), (90.0, 20.0));
        assert_eq!(a.combine(&c, Combine::Add).outlines.len(), 2);
    }

    #[test]
    fn outlines_follow_the_image_edge_and_holes() {
        let border: &[(f32, f32)] = &[(0.0, 0.0), (100.0, 0.0), (100.0, 50.0), (0.0, 50.0)];
        assert_traces(&Selection::all(100, 50), &[border]);
        let hole = Selection::rectangle(100, 50, (10.0, 10.0), (20.0, 20.0)).invert();
        assert_traces(
            &hole,
            &[border, &[(10.0, 10.0), (20.0, 10.0), (20.0, 20.0), (10.0, 20.0)]],
        );
        assert!(Selection::all(100, 50).invert().outlines.is_empty());
        // Across tiles, and cut off at the image edge.
        let s = Selection::rectangle(600, 400, (200.0, 250.0), (700.0, 300.0));
        assert_traces(&s, &[&[(200.0, 250.0), (600.0, 250.0), (600.0, 300.0), (200.0, 300.0)]]);
    }

    #[test]
    fn pixels_touching_at_a_corner_get_an_outline_each() {
        let mut coverage = Tiled::new(20, 20, 0);
        coverage.tile_mut(0, 0)[5 * TILE as usize + 5] = u16::MAX;
        coverage.tile_mut(0, 0)[6 * TILE as usize + 6] = u16::MAX;
        assert_traces(
            &Selection::from_coverage(coverage),
            &[
                &[(5.0, 5.0), (6.0, 5.0), (6.0, 6.0), (5.0, 6.0)],
                &[(6.0, 6.0), (7.0, 6.0), (7.0, 7.0), (6.0, 7.0)],
            ],
        );
    }

    #[test]
    fn an_ellipse_outline_follows_the_pixel_edges() {
        let s = Selection::ellipse(300, 300, (50.0, 100.0), (250.0, 200.0));
        assert_eq!(s.outlines.len(), 1);
        for &(x, y) in &s.outlines[0] {
            assert_eq!((x.fract(), y.fract()), (0.0, 0.0));
            let r = ((x - 150.0) / 100.0).hypot((y - 150.0) / 50.0);
            assert!((r - 1.0).abs() < 0.02, "({x}, {y}) is off the ellipse");
        }
    }

    #[test]
    fn feathering_rounds_the_outline() {
        let s = Selection::rectangle(200, 200, (50.0, 50.0), (150.0, 150.0)).feather(10.0);
        assert_eq!(s.outlines.len(), 1);
        let near_corner = s.outlines[0]
            .iter()
            .map(|p| (p.0 - 50.0).hypot(p.1 - 50.0))
            .fold(f32::MAX, f32::min);
        assert!(near_corner > 2.0, "{near_corner}");
    }

    #[test]
    fn lasso_triangle() {
        let s = Selection::polygon(100, 100, &[(10.0, 10.0), (90.0, 10.0), (50.0, 90.0)]);
        assert_eq!(s.at(50, 30), 1.0);
        assert_eq!(s.at(15, 80), 0.0);
    }

    #[test]
    fn magic_wand_contiguous_stops_at_boundary() {
        let (w, h) = (50, 50);
        let red: Pixel = [65535, 0, 0, 65535];
        let green: Pixel = [0, 65535, 0, 65535];
        let mut pixels = vec![red; (w * h) as usize];
        // Vertical divider at x = 25.
        for y in 0..h {
            pixels[(y * w + 25) as usize] = green;
        }
        let raster = Raster::new(w, h, pixels);

        let sel = Selection::magic_wand_raster(&raster, (10, 10), 0, true, false);
        assert!(!sel.is_empty());
        assert_eq!(sel.at(10, 10), 1.0);
        assert_eq!(sel.at(20, 20), 1.0);
        assert_eq!(sel.at(25, 20), 0.0); // Divider not selected
        assert_eq!(sel.at(30, 20), 0.0); // Other side not reached
    }

    #[test]
    fn magic_wand_non_contiguous_selects_disconnected_islands() {
        let (w, h) = (50, 50);
        let red: Pixel = [65535, 0, 0, 65535];
        let green: Pixel = [0, 65535, 0, 65535];
        let mut pixels = vec![red; (w * h) as usize];
        for y in 0..h {
            pixels[(y * w + 25) as usize] = green;
        }
        let raster = Raster::new(w, h, pixels);

        let sel = Selection::magic_wand_raster(&raster, (10, 10), 0, false, false);
        assert!(!sel.is_empty());
        assert_eq!(sel.at(10, 10), 1.0);
        assert_eq!(sel.at(25, 20), 0.0); // Divider not selected
        assert_eq!(sel.at(35, 20), 1.0); // Other side selected when contiguous = false
    }

    #[test]
    fn magic_wand_tolerance_respects_threshold() {
        let (w, h) = (3, 1);
        let pixels: Vec<Pixel> = vec![
            [10000, 10000, 10000, 65535],
            [10500, 10000, 10000, 65535],
            [12000, 10000, 10000, 65535],
        ];
        let raster = Raster::new(w, h, pixels);

        let sel_narrow = Selection::magic_wand_raster(&raster, (0, 0), 100, true, false);
        assert_eq!(sel_narrow.at(0, 0), 1.0);
        assert_eq!(sel_narrow.at(1, 0), 0.0);
        assert_eq!(sel_narrow.at(2, 0), 0.0);

        let sel_mid = Selection::magic_wand_raster(&raster, (0, 0), 600, true, false);
        assert_eq!(sel_mid.at(0, 0), 1.0);
        assert_eq!(sel_mid.at(1, 0), 1.0);
        assert_eq!(sel_mid.at(2, 0), 0.0);

        let sel_wide = Selection::magic_wand_raster(&raster, (0, 0), 3000, true, false);
        assert_eq!(sel_wide.at(0, 0), 1.0);
        assert_eq!(sel_wide.at(1, 0), 1.0);
        assert_eq!(sel_wide.at(2, 0), 1.0);
    }

    #[test]
    fn magic_wand_anti_aliasing_softens_edges() {
        let (w, h) = (20, 20);
        let red: Pixel = [65535, 0, 0, 65535];
        let blue: Pixel = [0, 0, 65535, 65535];
        let mut pixels = vec![blue; (w * h) as usize];
        // 10x10 square in centre
        for y in 5..15 {
            for x in 5..15 {
                pixels[(y * w + x) as usize] = red;
            }
        }
        let raster = Raster::new(w, h, pixels);

        let aliased = Selection::magic_wand_raster(&raster, (10, 10), 0, true, false);
        assert_eq!(aliased.at(10, 10), 1.0);
        assert_eq!(aliased.at(5, 5), 1.0);
        assert_eq!(aliased.at(4, 5), 0.0);

        let aa = Selection::magic_wand_raster(&raster, (10, 10), 0, true, true);
        assert_eq!(aa.at(10, 10), 1.0);
        // On the edge, coverage is fractional
        let edge_inside = aa.at(5, 10);
        let edge_outside = aa.at(4, 10);
        assert!(edge_inside > 0.5 && edge_inside < 1.0, "edge inside: {edge_inside}");
        assert!(edge_outside > 0.0 && edge_outside < 0.5, "edge outside: {edge_outside}");
        assert!(!aa.outlines.is_empty());
    }

    #[test]
    fn magic_wand_out_of_bounds_is_empty() {
        let raster = Raster::new(10, 10, vec![[0, 0, 0, 65535]; 100]);
        let sel = Selection::magic_wand_raster(&raster, (20, 20), 0, true, false);
        assert!(sel.is_empty());
    }

    #[test]
    fn combine_from_modifiers() {
        assert_eq!(Combine::from_modifiers(false, false), Combine::Replace);
        assert_eq!(Combine::from_modifiers(true, false), Combine::Add);
        assert_eq!(Combine::from_modifiers(false, true), Combine::Subtract);
        assert_eq!(Combine::from_modifiers(true, true), Combine::Intersect);
    }

    #[test]
    fn from_channel_red_green_blue_luminosity() {
        let (w, h) = (2, 2);
        let pixels: Vec<Pixel> = vec![
            [10000, 20000, 30000, 65535],
            [65535, 0, 0, 65535],
            [0, 65535, 0, 65535],
            [0, 0, 65535, 65535],
        ];
        let raster = Raster::new(w, h, pixels);

        let red = Selection::from_channel(&raster, Channel::Red);
        assert_eq!(red.coverage.get(0, 0), 10000);
        assert_eq!(red.coverage.get(1, 0), 65535);
        assert_eq!(red.coverage.get(0, 1), 0);
        assert_eq!(red.coverage.get(1, 1), 0);

        let green = Selection::from_channel(&raster, Channel::Green);
        assert_eq!(green.coverage.get(0, 0), 20000);
        assert_eq!(green.coverage.get(1, 0), 0);
        assert_eq!(green.coverage.get(0, 1), 65535);
        assert_eq!(green.coverage.get(1, 1), 0);

        let blue = Selection::from_channel(&raster, Channel::Blue);
        assert_eq!(blue.coverage.get(0, 0), 30000);
        assert_eq!(blue.coverage.get(1, 0), 0);
        assert_eq!(blue.coverage.get(0, 1), 0);
        assert_eq!(blue.coverage.get(1, 1), 65535);

        let lum = Selection::from_channel(&raster, Channel::Luminosity);
        assert_eq!(lum.coverage.get(1, 0), 19661);
        assert_eq!(lum.coverage.get(0, 1), 38665);
        assert_eq!(lum.coverage.get(1, 1), 7209);
        let expected = ((19661u64 * 10000 + 38666 * 20000 + 7209 * 30000 + 32768) >> 16) as u16;
        assert_eq!(lum.coverage.get(0, 0), expected);
    }

    #[test]
    fn from_alpha_loads_layer_transparency() {
        let (w, h) = (20, 20);
        let mut pixels = Tiled::new(w, h, [0u16; 4]);
        pixels.tile_mut(0, 0)[0] = [1000, 2000, 3000, 48000];
        let sel = Selection::from_alpha(&pixels);
        assert_eq!(sel.coverage.get(0, 0), 48000);
        assert_eq!(sel.coverage.get(1, 0), 0);
        assert!(!sel.is_empty());
    }

    #[test]
    fn from_mask_loads_mask_coverage() {
        let (w, h) = (20, 20);
        let mut mask = Tiled::new(w, h, 0u16);
        mask.tile_mut(0, 0)[0] = 52000;
        let sel = Selection::from_mask(&mask);
        assert_eq!(sel.coverage.get(0, 0), 52000);
        assert_eq!(sel.coverage.get(1, 0), 0);
        assert!(!sel.is_empty());
    }

    #[test]
    fn is_all_and_is_empty_checks() {
        let (w, h) = (300, 300);
        let all = Selection::all(w, h);
        assert!(all.is_all());
        assert!(!all.is_empty());

        let empty = Selection::from_coverage(Tiled::new(w, h, 0));
        assert!(empty.is_empty());
        assert!(!empty.is_all());

        let rect = Selection::rectangle(w, h, (50.0, 50.0), (100.0, 100.0));
        assert!(!rect.is_empty());
        assert!(!rect.is_all());
    }
}
