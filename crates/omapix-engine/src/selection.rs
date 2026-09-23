//! Selections: which part of the image edits apply to.
//!
//! A selection is a greyscale coverage image (0 = unselected, 65535 =
//! fully selected), so feathered and anti-aliased edges are partial, as in
//! Photoshop. It also keeps the outlines of the shapes that made it, for
//! drawing "marching ants".

use rayon::prelude::*;

use crate::tiled::{TILE, TILE_PIXELS, Tiled};

const MAX: f32 = u16::MAX as f32;

/// Vertical sub-samples per pixel row when filling shapes, for smooth edges.
const SUBSAMPLES: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Combine {
    Replace,
    Add,
    Subtract,
    Intersect,
}

#[derive(Clone)]
pub struct Selection {
    pub coverage: Tiled<u16>,
    /// Closed outlines in image pixels, for display.
    pub outlines: Vec<Vec<(f32, f32)>>,
}

impl Selection {
    pub fn all(width: u32, height: u32) -> Self {
        let (w, h) = (width as f32, height as f32);
        Self {
            coverage: Tiled::new(width, height, u16::MAX),
            outlines: vec![vec![(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)]],
        }
    }

    /// A filled polygon (even-odd rule), with anti-aliased edges.
    pub fn polygon(width: u32, height: u32, points: &[(f32, f32)]) -> Self {
        let coverage = fill_polygon(width, height, points);
        Self {
            coverage,
            outlines: vec![points.to_vec()],
        }
    }

    pub fn rectangle(width: u32, height: u32, (x0, y0): (f32, f32), (x1, y1): (f32, f32)) -> Self {
        let (l, r) = (x0.min(x1), x0.max(x1));
        let (t, b) = (y0.min(y1), y0.max(y1));
        Self::polygon(width, height, &[(l, t), (r, t), (r, b), (l, b)])
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
        self.coverage.fill() == 0
            && (0..self.coverage.rows())
                .all(|r| (0..self.coverage.cols()).all(|c| self.coverage.tile(c, r).is_none()))
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
        let outlines = match how {
            Combine::Replace => other.outlines.clone(),
            _ => self
                .outlines
                .iter()
                .chain(&other.outlines)
                .cloned()
                .collect(),
        };
        Selection { coverage, outlines }
    }

    pub fn invert(&self) -> Selection {
        let c = &self.coverage;
        let coverage = Tiled::from_tiles(c.width(), c.height(), u16::MAX - c.fill(), |col, row| {
            c.tile(col, row)
                .map(|t| t.iter().map(|v| u16::MAX - v).collect())
        });
        let (w, h) = (self.width() as f32, self.height() as f32);
        let mut outlines = self.outlines.clone();
        outlines.push(vec![(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)]);
        Selection { coverage, outlines }
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
        Selection {
            coverage: Tiled::from_slice(self.width(), self.height(), 0, &values),
            outlines: self.outlines.clone(),
        }
    }
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
    fn lasso_triangle() {
        let s = Selection::polygon(100, 100, &[(10.0, 10.0), (90.0, 10.0), (50.0, 90.0)]);
        assert_eq!(s.at(50, 30), 1.0);
        assert_eq!(s.at(15, 80), 0.0);
    }
}
