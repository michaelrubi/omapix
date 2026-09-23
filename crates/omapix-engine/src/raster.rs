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
}

/// Widen an 8-bit channel to 16 bits so that 255 maps to 65535.
pub fn widen(v: u8) -> u16 {
    u16::from(v) * 257
}
