//! Display tiles: fixed-size pieces of a pyramid level, converted to 8-bit
//! display colour for upload to the GPU.

use crate::{DisplayTransform, Raster};

/// Edge length of a display tile's content, in pixels of its level.
pub const TILE_SIZE: u32 = 512;

/// Where a tile sits in its level. The texture covers the content plus a
/// 1 px border of neighbouring pixels (where they exist), so linear
/// filtering at tile edges blends with real neighbours instead of showing
/// seams.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileBounds {
    /// Content rectangle, in level pixels.
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    /// Texture rectangle including the border, in level pixels.
    pub tex_x: u32,
    pub tex_y: u32,
    pub tex_w: u32,
    pub tex_h: u32,
}

/// Number of tile columns and rows covering a level.
pub fn grid(width: u32, height: u32) -> (u32, u32) {
    (width.div_ceil(TILE_SIZE), height.div_ceil(TILE_SIZE))
}

pub fn bounds(width: u32, height: u32, col: u32, row: u32) -> TileBounds {
    let x = col * TILE_SIZE;
    let y = row * TILE_SIZE;
    let w = TILE_SIZE.min(width - x);
    let h = TILE_SIZE.min(height - y);
    let tex_x = x.saturating_sub(1);
    let tex_y = y.saturating_sub(1);
    let tex_w = (x + w + 1).min(width) - tex_x;
    let tex_h = (y + h + 1).min(height) - tex_y;
    TileBounds {
        x,
        y,
        w,
        h,
        tex_x,
        tex_y,
        tex_w,
        tex_h,
    }
}

/// Convert a tile's texture rectangle to display RGBA8, row-major.
pub fn render(raster: &Raster, transform: &DisplayTransform, b: TileBounds) -> Vec<u8> {
    let mut out = vec![[0u8; 4]; b.tex_w as usize * b.tex_h as usize];
    for (i, row) in out.chunks_exact_mut(b.tex_w as usize).enumerate() {
        let src = &raster.row(b.tex_y + i as u32)[b.tex_x as usize..(b.tex_x + b.tex_w) as usize];
        transform.convert(src, row);
    }
    out.into_iter().flatten().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interior_tile_has_border_on_all_sides() {
        let b = bounds(2000, 2000, 1, 1);
        assert_eq!((b.x, b.y, b.w, b.h), (512, 512, 512, 512));
        assert_eq!((b.tex_x, b.tex_y, b.tex_w, b.tex_h), (511, 511, 514, 514));
    }

    #[test]
    fn edge_tiles_clamp_to_image() {
        let first = bounds(1000, 600, 0, 0);
        assert_eq!(
            (first.tex_x, first.tex_y, first.tex_w, first.tex_h),
            (0, 0, 513, 513)
        );
        let last = bounds(1000, 600, 1, 1);
        assert_eq!((last.x, last.y, last.w, last.h), (512, 512, 488, 88));
        assert_eq!(
            (last.tex_x, last.tex_y, last.tex_w, last.tex_h),
            (511, 511, 489, 89)
        );
        assert_eq!(grid(1000, 600), (2, 2));
    }
}
