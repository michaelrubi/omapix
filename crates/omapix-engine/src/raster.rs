use crate::tiled::TILE;

/// One RGBA pixel, 16 bits per channel, straight (not premultiplied) alpha.
pub type Pixel = [u16; 4];

pub const OPAQUE: u16 = u16::MAX;

/// A 16-bit RGBA image held in one contiguous buffer, rows top to bottom.
#[derive(Clone)]
pub struct Raster {
    width: u32,
    height: u32,
    pixels: Vec<Pixel>,
}

impl Raster {
    pub fn new(width: u32, height: u32, pixels: Vec<Pixel>) -> Self {
        assert_eq!(
            pixels.len(),
            width as usize * height as usize,
            "pixel count must match size"
        );
        Self {
            width,
            height,
            pixels,
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn pixels(&self) -> &[Pixel] {
        &self.pixels
    }

    pub fn row(&self, y: u32) -> &[Pixel] {
        let start = y as usize * self.width as usize;
        &self.pixels[start..start + self.width as usize]
    }

    pub fn row_mut(&mut self, y: u32) -> &mut [Pixel] {
        let start = y as usize * self.width as usize;
        &mut self.pixels[start..start + self.width as usize]
    }

    pub fn get(&self, x: u32, y: u32) -> Pixel {
        self.pixels[y as usize * self.width as usize + x as usize]
    }

    /// Copy a whole 256 px tile (row-major, as in [`crate::tiled::Tiled`])
    /// into place at tile (col, row), leaving out what's past the edges.
    pub fn put_tile(&mut self, col: u32, row: u32, tile: &[Pixel]) {
        let (x0, y0) = (col * TILE, row * TILE);
        if x0 >= self.width || y0 >= self.height {
            return;
        }
        let tw = TILE.min(self.width - x0) as usize;
        for ty in 0..TILE.min(self.height - y0) {
            let src = &tile[(ty * TILE) as usize..(ty * TILE) as usize + tw];
            self.row_mut(y0 + ty)[x0 as usize..x0 as usize + tw].copy_from_slice(src);
        }
    }
}

/// Widen an 8-bit channel to 16 bits so that 255 maps to 65535.
pub fn widen(v: u8) -> u16 {
    u16::from(v) * 257
}
