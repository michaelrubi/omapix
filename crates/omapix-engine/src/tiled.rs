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

/// Whole-document rotation or flip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Orientation {
    Rotate180,
    Rotate90Cw,
    Rotate90Ccw,
    FlipHorizontal,
    FlipVertical,
}

impl Orientation {
    pub fn dimensions(self, w: u32, h: u32) -> (u32, u32) {
        match self {
            Self::Rotate90Cw | Self::Rotate90Ccw => (h, w),
            Self::Rotate180 | Self::FlipHorizontal | Self::FlipVertical => (w, h),
        }
    }

    #[inline]
    pub fn source_coords(self, x: u32, y: u32, orig_w: u32, orig_h: u32) -> (u32, u32) {
        match self {
            Self::Rotate180 => (orig_w - 1 - x, orig_h - 1 - y),
            Self::Rotate90Cw => (y, orig_h - 1 - x),
            Self::Rotate90Ccw => (orig_w - 1 - y, x),
            Self::FlipHorizontal => (orig_w - 1 - x, y),
            Self::FlipVertical => (x, orig_h - 1 - y),
        }
    }
}

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

    /// Every value mapped by `f`, in parallel. Absent tiles stay absent.
    pub fn map<U: Copy + PartialEq + Send + Sync>(&self, f: impl Fn(T) -> U + Sync) -> Tiled<U> {
        Tiled::from_tiles(self.width, self.height, f(self.fill), |col, row| {
            self.tile(col, row).map(|t| t.iter().map(|&v| f(v)).collect())
        })
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
        self.reframed(self.width, self.height, dx, dy, uncovered)
    }

    /// A `width` × `height` copy with this image's top left corner at
    /// (`dx`, `dy`): Image › Canvas Size and Crop. Areas it doesn't cover
    /// read as `uncovered`; whatever falls outside is lost.
    pub fn reframed(&self, width: u32, height: u32, dx: i32, dy: i32, uncovered: T) -> Self {
        let fill = self.fill;
        if dx == 0 && dy == 0 && (width, height) == (self.width, self.height) {
            return self.clone();
        }
        let (w, h, t) = (
            i64::from(self.width),
            i64::from(self.height),
            i64::from(TILE),
        );
        let (dx, dy) = (i64::from(dx), i64::from(dy));
        Self::from_tiles(width, height, fill, |col, row| {
            let (x0, y0) = (i64::from(col) * t, i64::from(row) * t);
            let (x1, y1) = ((x0 + t).min(i64::from(width)), (y0 + t).min(i64::from(height)));
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

    /// A copy rotated or flipped by `orientation`. Untouched (fill-only) tiles stay empty.
    pub fn oriented(&self, orientation: Orientation) -> Self {
        let (orig_w, orig_h) = (self.width, self.height);
        let (new_w, new_h) = orientation.dimensions(orig_w, orig_h);
        let fill = self.fill;
        Self::from_tiles(new_w, new_h, fill, |col, row| {
            let x0 = col * TILE;
            let y0 = row * TILE;
            let x1 = (x0 + TILE).min(new_w);
            let y1 = (y0 + TILE).min(new_h);
            if x0 >= x1 || y0 >= y1 {
                return None;
            }
            let corners = [
                orientation.source_coords(x0, y0, orig_w, orig_h),
                orientation.source_coords(x1 - 1, y0, orig_w, orig_h),
                orientation.source_coords(x0, y1 - 1, orig_w, orig_h),
                orientation.source_coords(x1 - 1, y1 - 1, orig_w, orig_h),
            ];
            let sx0 = corners.iter().map(|c| c.0).min().unwrap();
            let sx1 = corners.iter().map(|c| c.0).max().unwrap();
            let sy0 = corners.iter().map(|c| c.1).min().unwrap();
            let sy1 = corners.iter().map(|c| c.1).max().unwrap();

            let empty = (sy0 / TILE..=sy1 / TILE).all(|r| {
                (sx0 / TILE..=sx1 / TILE).all(|c| self.tile(c, r).is_none())
            });
            if empty {
                return None;
            }

            let mut tile = vec![fill; TILE_PIXELS];
            for y in y0..y1 {
                let dst_offset = ((y - y0) * TILE) as usize;
                for x in x0..x1 {
                    let (sx, sy) = orientation.source_coords(x, y, orig_w, orig_h);
                    tile[dst_offset + (x - x0) as usize] = self.get(sx, sy);
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

    /// Copy of the area at (x0, y0), `w` × `h`, row-major. Parts outside
    /// the image read as the fill value.
    pub fn crop(&self, x0: u32, y0: u32, w: u32, h: u32) -> Vec<T> {
        let mut out = vec![self.fill; w as usize * h as usize];
        let x_end = (x0 + w).min(self.width);
        out.par_chunks_mut(w.max(1) as usize)
            .enumerate()
            .for_each(|(dy, line)| {
                let y = y0 + dy as u32;
                if y >= self.height {
                    return;
                }
                let (row, ty) = (y / TILE, (y % TILE) as usize);
                let mut x = x0;
                while x < x_end {
                    let col = x / TILE;
                    let end = ((col + 1) * TILE).min(x_end);
                    if let Some(tile) = self.tile(col, row) {
                        let at = ty * TILE as usize + (x % TILE) as usize;
                        let n = (end - x) as usize;
                        let o = (x - x0) as usize;
                        line[o..o + n].copy_from_slice(&tile[at..at + n]);
                    }
                    x = end;
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
    fn crop_copies_across_tiles_and_fills_outside() {
        let (w, h) = (600u32, 300u32);
        let px: Vec<u32> = (0..w * h).collect();
        let t = Tiled::from_slice(w, h, u32::MAX, &px);
        let out = t.crop(250, 240, 20, 30);
        for dy in 0..30 {
            for dx in 0..20 {
                assert_eq!(out[(dy * 20 + dx) as usize], (240 + dy) * w + 250 + dx);
            }
        }
        // Past the right and bottom edges: the fill value.
        let edge = t.crop(590, 290, 20, 20);
        assert_eq!(edge[0], 290 * w + 590);
        assert_eq!((edge[10], edge[19 * 20 + 19]), (u32::MAX, u32::MAX));
    }

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
