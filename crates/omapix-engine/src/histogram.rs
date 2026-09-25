//! Image histogram computation for tone adjustments (Curves, Levels).

use rayon::prelude::*;

use crate::composite;
use crate::layer::Layer;
use crate::raster::Raster;

/// A 256-bin histogram for red, green, blue, and luminance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Histogram {
    pub red: [u32; 256],
    pub green: [u32; 256],
    pub blue: [u32; 256],
    pub luminance: [u32; 256],
}

impl Default for Histogram {
    fn default() -> Self {
        Self {
            red: [0; 256],
            green: [0; 256],
            blue: [0; 256],
            luminance: [0; 256],
        }
    }
}

impl Histogram {
    /// Return the slice of 256 bins for the given channel:
    /// 0 = Luminance/RGB, 1 = Red, 2 = Green, 3 = Blue.
    pub fn channel(&self, ch: usize) -> &[u32; 256] {
        match ch {
            1 => &self.red,
            2 => &self.green,
            3 => &self.blue,
            _ => &self.luminance,
        }
    }

    /// Maximum count across bins in the given channel.
    pub fn max_count(&self, ch: usize) -> u32 {
        *self.channel(ch).iter().max().unwrap_or(&0)
    }

    /// Total number of pixels recorded.
    pub fn total(&self) -> u64 {
        self.luminance.iter().map(|&c| c as u64).sum()
    }

    /// True if no pixels are counted.
    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }

    /// Combine another histogram into this one.
    pub fn add(&mut self, other: &Self) {
        for i in 0..256 {
            self.red[i] += other.red[i];
            self.green[i] += other.green[i];
            self.blue[i] += other.blue[i];
            self.luminance[i] += other.luminance[i];
        }
    }

    /// Compute the histogram from a flattened raster image.
    pub fn from_raster(raster: &Raster) -> Self {
        let pixels = raster.pixels();
        if pixels.is_empty() {
            return Self::default();
        }

        // Parallel reduction over chunked pixels with Rayon
        pixels
            .par_chunks(32768)
            .map(|chunk| {
                let mut hist = Self::default();
                for &px in chunk {
                    // Skip fully transparent pixels
                    if px[3] == 0 {
                        continue;
                    }
                    let r = (px[0] >> 8) as usize;
                    let g = (px[1] >> 8) as usize;
                    let b = (px[2] >> 8) as usize;
                    // Luminance: 0.3 * R + 0.59 * G + 0.11 * B
                    let lum16 = (19661 * u64::from(px[0])
                        + 38666 * u64::from(px[1])
                        + 7209 * u64::from(px[2])
                        + 32768)
                        >> 16;
                    let lum = (lum16 >> 8).min(255) as usize;
                    hist.red[r] += 1;
                    hist.green[g] += 1;
                    hist.blue[b] += 1;
                    hist.luminance[lum] += 1;
                }
                hist
            })
            .reduce(Self::default, |mut a, b| {
                a.add(&b);
                a
            })
    }

    /// Compute the composite histogram of the given layers.
    pub fn from_layers(layers: &[Layer], width: u32, height: u32) -> Self {
        let visible = layers.iter().any(|l| l.visible && l.opacity > 0.0);
        if !visible || width == 0 || height == 0 {
            return Self::default();
        }
        let image = composite::composite(layers, width, height);
        Self::from_raster(&image)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Pixel;

    #[test]
    fn default_histogram_is_empty() {
        let h = Histogram::default();
        assert!(h.is_empty());
        assert_eq!(h.total(), 0);
        assert_eq!(h.max_count(0), 0);
    }

    #[test]
    fn transparent_pixels_are_ignored() {
        let raster = Raster::new(2, 2, vec![[0, 0, 0, 0]; 4]);
        let h = Histogram::from_raster(&raster);
        assert!(h.is_empty());
    }

    #[test]
    fn pure_colors_map_to_expected_bins() {
        // 4 red pixels (65535, 0, 0, 65535)
        let red_pixel: Pixel = [u16::MAX, 0, 0, u16::MAX];
        let raster = Raster::new(2, 2, vec![red_pixel; 4]);
        let h = Histogram::from_raster(&raster);
        assert_eq!(h.total(), 4);
        assert_eq!(h.red[255], 4);
        assert_eq!(h.green[0], 4);
        assert_eq!(h.blue[0], 4);
        // Luminance for red: 0.3 * 255 ~ 76
        let lum_bin = ((19661 * u64::from(u16::MAX) + 32768) >> 24) as usize;
        assert_eq!(h.luminance[lum_bin], 4);
    }

    #[test]
    fn layers_composite_and_histogram() {
        use crate::tiled::Tiled;

        let w = 10;
        let h = 10;
        let mut pixels = Tiled::new(w, h, [32768, 32768, 32768, u16::MAX]);
        // Set tile to grey
        for y in 0..h {
            for x in 0..w {
                pixels.tile_mut(0, 0)[(y * 256 + x) as usize] = [32768, 32768, 32768, u16::MAX];
            }
        }
        let layer = Layer::from_pixels(1, "Base", pixels);

        let hist = Histogram::from_layers(&[layer], w, h);
        assert_eq!(hist.total(), (w * h) as u64);
        // 32768 >> 8 = 128
        assert_eq!(hist.red[128], w * h);
        assert_eq!(hist.green[128], w * h);
        assert_eq!(hist.blue[128], w * h);
        assert_eq!(hist.luminance[128], w * h);
    }
}
