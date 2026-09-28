//! Turning an AI model's low-resolution mask into a full-size selection
//! whose edges follow the photo's (docs/AI.md, "Getting images in and out
//! of models").

use rayon::prelude::*;

use crate::filters::guided_filter;
use crate::raster::Raster;
use crate::tiled::{TILE, Tiled};

/// The longest side a mask is refined at. Refining a 24 MP image at full
/// size would cost far more and look the same once scaled up.
const WORKING: usize = 2048;

/// `logits` (`lw` × `lh`, stretched over the whole image, positive inside
/// the object) as selection coverage the size of `image`: its outline
/// interpolated smoothly, snapped to the edges of the image's luminance
/// with a guided filter, then scaled up to full size.
pub fn mask_coverage(logits: &[f32], lw: usize, lh: usize, image: &Raster) -> Tiled<u16> {
    let (width, height) = (image.width() as usize, image.height() as usize);
    let k = (width.max(height) as f32 / WORKING as f32).max(1.0);
    let (gw, gh) = (((width as f32 / k).round() as usize).max(1), ((height as f32 / k).round() as usize).max(1));

    // The image's luminance at the working size, averaged over each
    // working pixel's footprint.
    let guide: Vec<f32> = (0..gw * gh)
        .into_par_iter()
        .map(|i| {
            let (x, y) = (i % gw, i / gw);
            let (x0, x1) = (x * width / gw, ((x + 1) * width / gw).max(x * width / gw + 1));
            let (y0, y1) = (y * height / gh, ((y + 1) * height / gh).max(y * height / gh + 1));
            let mut sum = 0.0;
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let p = image.get(sx as u32, sy as u32);
                    sum += 0.2126 * f32::from(p[0]) + 0.7152 * f32::from(p[1]) + 0.0722 * f32::from(p[2]);
                }
            }
            sum / ((x1 - x0) * (y1 - y0)) as f32 / 65535.0
        })
        .collect();

    // Inside where the logits, interpolated, are positive: a smooth outline
    // rather than the model's blocky pixels.
    let inside: Vec<f32> = (0..gw * gh)
        .into_par_iter()
        .map(|i| {
            let v = sample(logits, lw, lh, centre(i % gw, gw, lw), centre(i / gw, gh, lh));
            if v > 0.0 { 1.0 } else { 0.0 }
        })
        .collect();

    // The model's pixels are this many working pixels across; the guided
    // filter reaches about that far to find the real edge.
    let r = ((gw.max(gh) as f32 / lw.max(lh) as f32) * 1.5).ceil() as usize;
    let refined = guided_filter(&guide, &inside, gw, gh, r.max(2), 1e-3);

    Tiled::from_tiles(width as u32, height as u32, 0, |col, row| {
        let (tx, ty) = ((col * TILE) as usize, (row * TILE) as usize);
        let mut tile = vec![0u16; (TILE * TILE) as usize];
        let mut any = false;
        for y in ty..(ty + TILE as usize).min(height) {
            for x in tx..(tx + TILE as usize).min(width) {
                let v = sample(&refined, gw, gh, centre(x, width, gw), centre(y, height, gh));
                let v = (v.clamp(0.0, 1.0) * 65535.0).round() as u16;
                any |= v > 0;
                tile[(y - ty) * TILE as usize + (x - tx)] = v;
            }
        }
        any.then_some(tile)
    })
}

/// Where the centre of pixel `i` of `n` falls on a grid `m` pixels across.
fn centre(i: usize, n: usize, m: usize) -> f32 {
    (i as f32 + 0.5) * m as f32 / n as f32
}

/// `values` (`w` × `h`) at `(x, y)` in its own pixels, interpolated
/// between the four nearest pixel centres (edges extend outwards).
fn sample(values: &[f32], w: usize, h: usize, x: f32, y: f32) -> f32 {
    let (sx, sy) = (x - 0.5, y - 0.5);
    let (x0, y0) = (sx.floor(), sy.floor());
    let (fx, fy) = (sx - x0, sy - y0);
    let at = |x: f32, y: f32| values[(y.clamp(0.0, h as f32 - 1.0) as usize) * w + x.clamp(0.0, w as f32 - 1.0) as usize];
    let top = at(x0, y0) * (1.0 - fx) + at(x0 + 1.0, y0) * fx;
    let bottom = at(x0, y0 + 1.0) * (1.0 - fx) + at(x0 + 1.0, y0 + 1.0) * fx;
    top * (1.0 - fy) + bottom * fy
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rough_mask_snaps_to_the_objects_edge() {
        // A bright square from x = 200 to 400 on a dark 600 × 400 image.
        let (w, h) = (600u32, 400u32);
        let px = (0..w * h)
            .map(|i| if (200..400).contains(&(i % w)) && (100..300).contains(&(i / w)) { [60000; 4] } else { [8000, 8000, 8000, 65535] })
            .collect();
        let image = Raster::new(w, h, px);
        // The model's 64 × 64 guess is a little too wide on the left: its
        // edge is at x = 190.
        let (lw, lh) = (64, 64);
        let logits: Vec<f32> = (0..lw * lh)
            .map(|i| {
                let (x, y) = ((i % lw) as f32 * 600.0 / 64.0, (i / lw) as f32 * 400.0 / 64.0);
                if (190.0..400.0).contains(&x) && (100.0..300.0).contains(&y) { 5.0 } else { -5.0 }
            })
            .collect();
        let coverage = mask_coverage(&logits, lw, lh, &image);
        assert_eq!(coverage.get(300, 200), 65535);
        assert_eq!(coverage.get(50, 50), 0);
        // Between the guess's edge and the square's, it follows the square.
        assert!(coverage.get(194, 200) < 16384, "{}", coverage.get(194, 200));
        assert!(coverage.get(205, 200) > 49000, "{}", coverage.get(205, 200));
        // Tiles nowhere near it stay empty.
        assert!(coverage.tile(2, 1).is_none());
    }
}
