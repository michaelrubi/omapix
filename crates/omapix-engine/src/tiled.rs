//! Tiled, copy-on-write pixel storage.
//!
//! Images are split into 256×256 tiles held behind `Arc`s. Cloning a
//! `Tiled` is cheap because tiles are shared, and writing to a tile copies
//! only that tile. Undo history keeps whole-document snapshots, so it only
//! costs memory for the tiles an edit actually touched.
//!
//! Tiles that were never written are absent and read as the fill value
//! (transparent for layers, white for masks), so empty layers cost nothing.

use std::sync::Arc;

use rayon::prelude::*;

use crate::Raster;

pub const TILE: u32 = 256;
pub const TILE_PIXELS: usize = (TILE * TILE) as usize;

#[derive(Clone)]
pub struct Tiled<T> {
    width: u32,
    height: u32,
    cols: u32,
    rows: u32,
    fill: T,
    tiles: Vec<Option<Arc<Vec<T>>>>,
}

impl<T: Copy + PartialEq + Send + Sync> Tiled<T> {
    pub fn new(width: u32, height: u32, fill: T) -> Self {
        let cols = width.div_ceil(TILE);
        let rows = height.div_ceil(TILE);
        Self {
            width,
            height,
            cols,
            rows,
            fill,
            tiles: vec![None; (cols * rows) as usize],
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn cols(&self) -> u32 {
        self.cols
    }

    pub fn rows(&self) -> u32 {
        self.rows
    }

    pub fn fill(&self) -> T {
        self.fill
    }

    fn index(&self, col: u32, row: u32) -> usize {
        (row * self.cols + col) as usize
    }

    /// A tile's pixels (row-major, always a full 256×256 even at image
    /// edges), or `None` if it was never written and reads as the fill value.
    pub fn tile(&self, col: u32, row: u32) -> Option<&[T]> {
        self.tiles[self.index(col, row)]
            .as_deref()
            .map(Vec::as_slice)
    }

    /// Writable pixels of a tile, copying it first if it is shared.
    pub fn tile_mut(&mut self, col: u32, row: u32) -> &mut [T] {
        let fill = self.fill;
        let i = self.index(col, row);
        let tile = self.tiles[i].get_or_insert_with(|| Arc::new(vec![fill; TILE_PIXELS]));
        Arc::make_mut(tile).as_mut_slice()
    }

    pub fn get(&self, x: u32, y: u32) -> T {
        match self.tile(x / TILE, y / TILE) {
            Some(t) => t[((y % TILE) * TILE + x % TILE) as usize],
            None => self.fill,
        }
    }

    /// Build every tile in parallel from a function of (col, row) that
    /// returns the tile's pixels, or `None` to leave it empty.
    pub fn from_tiles(
        width: u32,
        height: u32,
        fill: T,
        f: impl Fn(u32, u32) -> Option<Vec<T>> + Sync,
    ) -> Self {
        let mut out = Self::new(width, height, fill);
        let cols = out.cols;
        out.tiles.par_iter_mut().enumerate().for_each(|(i, slot)| {
            let (col, row) = (i as u32 % cols, i as u32 / cols);
            *slot = f(col, row).map(|v| {
                debug_assert_eq!(v.len(), TILE_PIXELS);
                Arc::new(v)
            });
        });
        out
    }

    /// Replace each tile with `f(col, row, tile)` in parallel. Tiles for
    /// which `f` returns `None` are left untouched (and stay shared).
    pub fn par_update(&mut self, f: impl Fn(u32, u32, Option<&[T]>) -> Option<Vec<T>> + Sync) {
        let cols = self.cols;
        self.tiles.par_iter_mut().enumerate().for_each(|(i, slot)| {
            let (col, row) = (i as u32 % cols, i as u32 / cols);
            if let Some(v) = f(col, row, slot.as_deref().map(Vec::as_slice)) {
                *slot = Some(Arc::new(v));
            }
        });
    }

    /// A copy moved by (`dx`, `dy`) pixels. Areas the move uncovers read as
    /// `uncovered`; whatever moves past the edges is lost.
    pub fn translated(&self, dx: i32, dy: i32, uncovered: T) -> Self {
        let fill = self.fill;
        if dx == 0 && dy == 0 {
            return self.clone();
        }
        let (w, h, t) = (
            i64::from(self.width),
            i64::from(self.height),
            i64::from(TILE),
        );
        let (dx, dy) = (i64::from(dx), i64::from(dy));
        Self::from_tiles(self.width, self.height, fill, |col, row| {
            let (x0, y0) = (i64::from(col) * t, i64::from(row) * t);
            let (x1, y1) = ((x0 + t).min(w), (y0 + t).min(h));
            // The source area, clipped to the image. If it covers the whole
            // tile and only unwritten tiles, this tile stays unwritten too.
            let (sx0, sy0) = ((x0 - dx).max(0), (y0 - dy).max(0));
            let (sx1, sy1) = ((x1 - dx).min(w), (y1 - dy).min(h));
            let whole = sx1 - sx0 == x1 - x0 && sy1 - sy0 == y1 - y0;
            let empty = sx0 >= sx1
                || sy0 >= sy1
                || (sy0 / t..=(sy1 - 1) / t).all(|r| {
                    (sx0 / t..=(sx1 - 1) / t).all(|c| self.tile(c as u32, r as u32).is_none())
                });
            if empty && (whole || uncovered == fill) {
                return None;
            }
            let mut tile = vec![fill; TILE_PIXELS];
            for y in y0..y1 {
                let line = &mut tile[((y - y0) * t) as usize..];
                let sy = y - dy;
                if sy < 0 || sy >= h {
                    line[..(x1 - x0) as usize].fill(uncovered);
                    continue;
                }
                // Copy runs that come from one source tile at a time.
                let mut x = x0;
                while x < x1 {
                    let sx = x - dx;
                    if sx < 0 || sx >= w {
                        line[(x - x0) as usize] = uncovered;
                        x += 1;
                        continue;
                    }
                    let run = (t - sx % t).min(x1 - x).min(w - sx);
                    let at = (x - x0) as usize..(x - x0 + run) as usize;
                    match self.tile((sx / t) as u32, (sy / t) as u32) {
                        Some(src) => {
                            let from = ((sy % t) * t + sx % t) as usize;
                            line[at].copy_from_slice(&src[from..from + run as usize]);
                        }
                        None => line[at].fill(fill),
                    }
                    x += run;
                }
            }
            tile.iter().any(|&p| p != fill).then_some(tile)
        })
    }

    /// True if tile (col, row) is the same shared tile in both images (or
    /// empty in both), so it is known to be equal without comparing pixels.
    pub fn same_tile(&self, other: &Self, col: u32, row: u32) -> bool {
        match (self.tile(col, row), other.tile(col, row)) {
            (None, None) => true,
            (Some(a), Some(b)) => std::ptr::eq(a, b),
            _ => false,
        }
    }

    /// True if two images share every tile, so they are known to be equal
    /// without comparing pixels.
    pub fn same_tiles(&self, other: &Self) -> bool {
        self.width == other.width
            && self.height == other.height
            && self
                .tiles
                .iter()
                .zip(&other.tiles)
                .all(|(a, b)| match (a, b) {
                    (None, None) => true,
                    (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                    _ => false,
                })
    }

    /// Copy of the whole image in one contiguous row-major buffer.
    pub fn to_vec(&self) -> Vec<T> {
        let (w, h) = (self.width as usize, self.height as usize);
        let mut out = vec![self.fill; w * h];
        out.par_chunks_mut(w * TILE as usize)
            .enumerate()
            .for_each(|(row, band)| {
                for col in 0..self.cols {
                    let Some(tile) = self.tile(col, row as u32) else {
                        continue;
                    };
                    let x0 = (col * TILE) as usize;
                    let tw = (TILE as usize).min(w - x0);
                    for (ty, line) in band.chunks_mut(w).enumerate() {
                        line[x0..x0 + tw]
                            .copy_from_slice(&tile[ty * TILE as usize..ty * TILE as usize + tw]);
                    }
                }
            });
        out
    }

    /// Tile the contents of a contiguous row-major buffer. Tiles that
    /// consist entirely of the fill value are left empty.
    pub fn from_slice(width: u32, height: u32, fill: T, pixels: &[T]) -> Self {
        assert_eq!(pixels.len(), width as usize * height as usize);
        let w = width as usize;
        Self::from_tiles(width, height, fill, |col, row| {
            let mut tile = vec![fill; TILE_PIXELS];
            let x0 = (col * TILE) as usize;
            let y0 = (row * TILE) as usize;
            let tw = (TILE as usize).min(w - x0);
            let th = (TILE as usize).min(height as usize - y0);
            for ty in 0..th {
                let src = &pixels[(y0 + ty) * w + x0..(y0 + ty) * w + x0 + tw];
                tile[ty * TILE as usize..ty * TILE as usize + tw].copy_from_slice(src);
            }
            tile.iter().any(|&p| p != fill).then_some(tile)
        })
    }
}

impl Tiled<crate::Pixel> {
    pub fn from_raster(raster: &Raster) -> Self {
        Self::from_slice(raster.width(), raster.height(), [0; 4], raster.pixels())
    }

    pub fn to_raster(&self) -> Raster {
        Raster::new(self.width, self.height, self.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_contiguous_buffer() {
        let (w, h) = (300, 520);
        let pixels: Vec<u16> = (0..w * h).map(|i| (i % 65521) as u16 + 1).collect();
        let tiled = Tiled::from_slice(w, h, 0, &pixels);
        assert_eq!((tiled.cols(), tiled.rows()), (2, 3));
        assert_eq!(tiled.to_vec(), pixels);
        assert_eq!(tiled.get(299, 519), pixels[(519 * w + 299) as usize]);
    }

    #[test]
    fn fill_only_tiles_stay_empty() {
        let tiled = Tiled::from_slice(512, 256, 7u16, &vec![7; 512 * 256]);
        assert!(tiled.tile(0, 0).is_none() && tiled.tile(1, 0).is_none());
        assert_eq!(tiled.get(10, 10), 7);
    }

    #[test]
    fn writes_copy_only_the_touched_tile() {
        let a = Tiled::from_slice(512, 256, 0u16, &vec![1; 512 * 256]);
        let mut b = a.clone();
        assert!(a.same_tiles(&b));
        b.tile_mut(1, 0)[0] = 9;
        assert!(!a.same_tiles(&b));
        assert_eq!(a.get(256, 0), 1);
        assert_eq!(b.get(256, 0), 9);
        // The untouched tile is still shared.
        assert!(std::ptr::eq(a.tile(0, 0).unwrap(), b.tile(0, 0).unwrap()));
    }
}
