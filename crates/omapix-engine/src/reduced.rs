//! Layers shrunk to a zoomed-out display level, for fast previews.
//!
//! Zoomed out, the canvas shows a pyramid level a half, a quarter… the size
//! of the image. Compositing layers shrunk to that size first, rather than
//! compositing at full size and shrinking the result, is 4× (16×…) less
//! work: close enough to show while a slider is dragged, before the exact
//! render catches up. Shrunk tiles are kept and only redone when their
//! layer's tiles change, so each step of a drag just composites.

use std::collections::HashMap;

use crate::Pixel;
use crate::layer::{Layer, Mask};
use crate::tiled::{TILE, TILE_PIXELS, Tiled};

/// Shrunk copies of layers at one level, reused while their tiles are
/// unchanged.
#[derive(Default)]
pub struct Reduced {
    level: u32,
    layers: HashMap<u64, Entry>,
}

/// A layer's pixels and mask, as last shrunk, and what they were shrunk
/// from.
struct Entry {
    pixels: (Tiled<Pixel>, Tiled<Pixel>),
    mask: Option<(Tiled<u16>, Tiled<u16>)>,
}

impl Reduced {
    /// `layers` shrunk by 2^`level` each way (box-filtered, like the display
    /// pyramid), with everything else about them unchanged.
    pub fn layers(&mut self, layers: &[Layer], level: u32) -> Vec<Layer> {
        if level != self.level {
            self.layers.clear();
            self.level = level;
        }
        self.layers
            .retain(|id, _| layers.iter().any(|l| l.id == *id));
        layers
            .iter()
            .map(|layer| {
                let old = self.layers.remove(&layer.id);
                let pixels = shrink(&layer.pixels, level, old.as_ref().map(|e| &e.pixels));
                let mask = layer.mask.as_ref().map(|m| {
                    let old = old.as_ref().and_then(|e| e.mask.as_ref());
                    (m.pixels.clone(), shrink(&m.pixels, level, old))
                });
                let mut small = layer.clone();
                small.pixels = pixels.clone();
                small.mask = layer
                    .mask
                    .as_ref()
                    .zip(mask.as_ref())
                    .map(|(m, (_, s))| Mask {
                        pixels: s.clone(),
                        enabled: m.enabled,
                    });
                let entry = Entry {
                    pixels: (layer.pixels.clone(), pixels),
                    mask,
                };
                self.layers.insert(layer.id, entry);
                small
            })
            .collect()
    }
}

/// Values that can be averaged.
trait Average: Copy + PartialEq + Send + Sync {
    type Sum: Copy + Default;
    fn add(sum: &mut Self::Sum, v: Self);
    fn average(sum: Self::Sum, n: u64) -> Self;
}

impl Average for u16 {
    type Sum = u64;

    fn add(sum: &mut u64, v: u16) {
        *sum += u64::from(v);
    }

    fn average(sum: u64, n: u64) -> u16 {
        ((sum + n / 2) / n) as u16
    }
}

/// Colours are weighted by alpha, so transparent pixels' colours don't
/// bleed into what's left.
impl Average for Pixel {
    type Sum = [u64; 4];

    fn add(sum: &mut [u64; 4], [r, g, b, a]: Pixel) {
        let a64 = u64::from(a);
        sum[0] += u64::from(r) * a64;
        sum[1] += u64::from(g) * a64;
        sum[2] += u64::from(b) * a64;
        sum[3] += a64;
    }

    fn average([r, g, b, a]: [u64; 4], n: u64) -> Pixel {
        if a == 0 {
            return [0; 4];
        }
        let colour = |c: u64| ((c + a / 2) / a) as u16;
        [colour(r), colour(g), colour(b), ((a + n / 2) / n) as u16]
    }
}

/// `src` shrunk by 2^`level` each way, each pixel the average of the block
/// it covers. `old` is what `src` was last shrunk from, and the result:
/// tiles made only from source tiles that haven't changed are reused.
fn shrink<T: Average>(src: &Tiled<T>, level: u32, old: Option<&(Tiled<T>, Tiled<T>)>) -> Tiled<T> {
    let (w, h) = (
        src.width().div_ceil(1 << level),
        src.height().div_ceil(1 << level),
    );
    let span = 1u32 << level;
    // Source tiles under output tile (col, row).
    let sources = move |col: u32, row: u32| {
        let cols = col * span..((col + 1) * span).min(src.cols());
        let rows = row * span..((row + 1) * span).min(src.rows());
        rows.flat_map(move |r| cols.clone().map(move |c| (c, r)))
    };
    let make = |col: u32, row: u32| -> Option<Vec<T>> {
        if sources(col, row).all(|(c, r)| src.tile(c, r).is_none()) {
            return None;
        }
        Some(shrink_tile(src, level, col, row))
    };
    match old {
        Some((was, small)) if was.width() == src.width() && was.height() == src.height() => {
            let mut out = small.clone();
            out.par_update(|col, row, _| {
                if sources(col, row).all(|(c, r)| src.same_tile(was, c, r)) {
                    return None;
                }
                Some(make(col, row).unwrap_or_else(|| vec![src.fill(); TILE_PIXELS]))
            });
            out
        }
        _ => Tiled::from_tiles(w, h, src.fill(), make),
    }
}

/// One output tile of [`shrink`].
fn shrink_tile<T: Average>(src: &Tiled<T>, level: u32, col: u32, row: u32) -> Vec<T> {
    let span = 1u32 << level;
    let mut sums = vec![(T::Sum::default(), 0u64); TILE_PIXELS];
    // The part of the source image this tile covers.
    let (x0, y0) = (col * TILE * span, row * TILE * span);
    let x1 = ((col + 1) * TILE * span).min(src.width());
    let y1 = ((row + 1) * TILE * span).min(src.height());
    for y in y0..y1 {
        let line = ((y - y0) >> level) * TILE;
        let tile_row = y / TILE;
        let mut x = x0;
        while x < x1 {
            // One source tile's worth of this row at a time.
            let end = ((x / TILE + 1) * TILE).min(x1);
            let tile = src.tile(x / TILE, tile_row);
            let base = ((y % TILE) * TILE) as usize;
            for sx in x..end {
                let v = tile.map_or(src.fill(), |t| t[base + (sx % TILE) as usize]);
                let (sum, n) = &mut sums[(line + ((sx - x0) >> level)) as usize];
                T::add(sum, v);
                *n += 1;
            }
            x = end;
        }
    }
    sums.into_iter()
        .map(|(sum, n)| {
            if n == 0 {
                src.fill()
            } else {
                T::average(sum, n)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Raster;
    use crate::composite::composite;

    #[test]
    fn shrinks_by_averaging_blocks_weighted_by_alpha() {
        let (w, h) = (600, 300);
        // Opaque red columns alternating with transparent green ones.
        let pixels = (0..w * h)
            .map(|i| {
                if i % 2 == 0 {
                    [60000, 0, 0, 65535]
                } else {
                    [0, 60000, 0, 0]
                }
            })
            .collect();
        let layer = Layer::from_raster(1, "l", &Raster::new(w, h, pixels));
        let small = Reduced::default().layers(&[layer], 1).remove(0);
        assert_eq!((small.pixels.width(), small.pixels.height()), (300, 150));
        assert_eq!(small.pixels.get(299, 149), [60000, 0, 0, 32768]);
    }

    #[test]
    fn odd_edges_average_only_what_is_there() {
        let mut mask = Mask::white(3, 3);
        mask.pixels.tile_mut(0, 0)[2] = 0; // (2, 0)
        let mut layer = Layer::empty(1, "l", 3, 3);
        layer.mask = Some(mask);
        let small = Reduced::default().layers(&[layer], 1).remove(0);
        let mask = small.mask.unwrap().pixels;
        assert_eq!((mask.width(), mask.height()), (2, 2));
        assert_eq!(mask.get(1, 0), 32768);
        assert_eq!(mask.get(1, 1), 65535);
        assert!(small.pixels.tile(0, 0).is_none(), "empty stays empty");
    }

    #[test]
    fn only_changed_tiles_are_shrunk_again() {
        let (w, h) = (1100, 600);
        let image = Raster::new(w, h, vec![[1000, 2000, 3000, 65535]; (w * h) as usize]);
        let mut layer = Layer::from_raster(1, "l", &image);
        let mut reduced = Reduced::default();
        let first = reduced.layers(std::slice::from_ref(&layer), 1).remove(0);
        // Source tile (4, 1) is under output tile (2, 0).
        layer.pixels.tile_mut(4, 1)[0] = [9, 9, 9, 65535];
        let second = reduced.layers(std::slice::from_ref(&layer), 1).remove(0);
        assert!(second.pixels.same_tile(&first.pixels, 0, 0));
        assert!(!second.pixels.same_tile(&first.pixels, 2, 0));
        let fresh = Reduced::default().layers(&[layer], 1).remove(0);
        assert_eq!(second.pixels.to_vec(), fresh.pixels.to_vec());
    }

    #[test]
    fn a_shrunk_composite_is_close_to_a_shrunk_image() {
        let (w, h) = (520, 260);
        let ramp = (0..w * h)
            .map(|i| {
                let v = ((i % w) * 120) as u16;
                [v, 30000, 65535 - v, 65535]
            })
            .collect();
        let bottom = Layer::from_raster(1, "b", &Raster::new(w, h, ramp));
        let mut top = Layer::from_raster(
            2,
            "t",
            &Raster::new(w, h, vec![[65535, 0, 0, 40000]; (w * h) as usize]),
        );
        top.opacity = 0.5;
        let layers = [bottom, top];
        let full = composite(&layers, w, h);
        let small = Reduced::default().layers(&layers, 2);
        let preview = composite(&small, w.div_ceil(4), h.div_ceil(4));
        let exact = crate::pyramid::Pyramid::build(&full);
        let exact = exact.level(&full, 2);
        for (a, b) in preview.pixels().iter().zip(exact.pixels()) {
            for c in 0..4 {
                assert!(a[c].abs_diff(b[c]) <= 2, "{a:?} vs {b:?}");
            }
        }
    }
}
