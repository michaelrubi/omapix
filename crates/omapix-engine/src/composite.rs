//! Flattening a layer stack into one image.

use rayon::prelude::*;

use crate::layer::Layer;
use crate::tiled::{TILE, TILE_PIXELS};
use crate::{Pixel, Raster};

const MAX: f32 = u16::MAX as f32;

/// Composite layers (bottom first) into a single image.
///
/// Each layer is blended onto everything below it with its blend mode, then
/// combined by its alpha × opacity × mask using source-over, as in
/// Photoshop and the W3C compositing spec. Tiles are processed in parallel.
pub fn composite(layers: &[Layer], width: u32, height: u32) -> Raster {
    let visible: Vec<&Layer> = layers
        .iter()
        .filter(|l| l.visible && l.opacity > 0.0)
        .collect();
    let w = width as usize;
    let mut out = vec![[0u16; 4]; w * height as usize];
    let cols = width.div_ceil(TILE);

    out.par_chunks_mut(w * TILE as usize)
        .enumerate()
        .for_each(|(row, band)| {
            let row = row as u32;
            let mut acc = vec![[0f32; 4]; TILE_PIXELS];
            for col in 0..cols {
                acc.fill([0.0; 4]);
                for layer in &visible {
                    blend_tile(&mut acc, layer, col, row);
                }
                let x0 = (col * TILE) as usize;
                let tw = (TILE as usize).min(w - x0);
                for (ty, line) in band.chunks_mut(w).enumerate() {
                    let src = &acc[ty * TILE as usize..ty * TILE as usize + tw];
                    for (dst, px) in line[x0..x0 + tw].iter_mut().zip(src) {
                        *dst = to_u16(*px);
                    }
                }
            }
        });
    Raster::new(width, height, out)
}

fn blend_tile(acc: &mut [[f32; 4]], layer: &Layer, col: u32, row: u32) {
    // An empty tile is transparent and changes nothing.
    let Some(src) = layer.pixels.tile(col, row) else {
        return;
    };
    let mask = layer.mask.as_ref().filter(|m| m.enabled).map(|m| &m.pixels);
    let mask_tile = mask.and_then(|m| m.tile(col, row));
    let mask_fill = mask.map_or(1.0, |m| f32::from(m.fill()) / MAX);
    if mask_tile.is_none() && mask_fill <= 0.0 {
        return;
    }
    let opacity = layer.opacity;
    let mode = layer.blend;

    for i in 0..TILE_PIXELS {
        let s = src[i];
        let m = mask_tile.map_or(mask_fill, |t| f32::from(t[i]) / MAX);
        let a_s = f32::from(s[3]) / MAX * opacity * m;
        if a_s <= 0.0 {
            continue;
        }
        let cs = [
            f32::from(s[0]) / MAX,
            f32::from(s[1]) / MAX,
            f32::from(s[2]) / MAX,
        ];
        let [br, bg, bb, a_b] = acc[i];
        let cb = [br, bg, bb];
        let blended = if a_b > 0.0 { mode.apply(cb, cs) } else { cs };
        let a_o = a_s + a_b * (1.0 - a_s);
        let mut out = [0.0; 4];
        for c in 0..3 {
            let co = a_s * (1.0 - a_b) * cs[c] + a_s * a_b * blended[c] + (1.0 - a_s) * a_b * cb[c];
            out[c] = co / a_o;
        }
        out[3] = a_o;
        acc[i] = out;
    }
}

fn to_u16(px: [f32; 4]) -> Pixel {
    px.map(|v| (v.clamp(0.0, 1.0) * MAX).round() as u16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blend::BlendMode;
    use crate::layer::Mask;
    use crate::tiled::Tiled;

    fn solid(id: u64, w: u32, h: u32, px: Pixel) -> Layer {
        let raster = Raster::new(w, h, vec![px; (w * h) as usize]);
        Layer::from_raster(id, "solid", &raster)
    }

    #[test]
    fn single_opaque_layer_is_unchanged() {
        let layer = solid(1, 300, 260, [1000, 30000, 65535, 65535]);
        let out = composite(&[layer], 300, 260);
        assert_eq!(out.get(299, 259), [1000, 30000, 65535, 65535]);
    }

    #[test]
    fn half_opacity_normal_mixes_evenly() {
        let bottom = solid(1, 10, 10, [0, 0, 0, 65535]);
        let mut top = solid(2, 10, 10, [65535, 65535, 65535, 65535]);
        top.opacity = 0.5;
        let out = composite(&[bottom, top], 10, 10);
        assert!(out.get(5, 5)[0].abs_diff(32768) <= 1);
        assert_eq!(out.get(5, 5)[3], 65535);
    }

    #[test]
    fn hidden_layers_and_black_masks_are_ignored() {
        let bottom = solid(1, 10, 10, [100, 200, 300, 65535]);
        let mut hidden = solid(2, 10, 10, [65535; 4]);
        hidden.visible = false;
        let mut masked = solid(3, 10, 10, [65535; 4]);
        let mut mask = Mask::white(10, 10);
        mask.invert();
        masked.mask = Some(mask);
        let out = composite(&[bottom, hidden, masked], 10, 10);
        assert_eq!(out.get(3, 3), [100, 200, 300, 65535]);
    }

    #[test]
    fn frequency_separation_reconstructs_the_image() {
        let (w, h) = (64, 64);
        let image: Vec<Pixel> = (0..w * h)
            .map(|i| {
                [
                    (i * 13 % 60000) as u16 + 2000,
                    (i * 7 % 50000) as u16 + 5000,
                    30000,
                    65535,
                ]
            })
            .collect();
        let low: Vec<Pixel> = image
            .iter()
            .map(|p| [p[0] / 2 + 15000, p[1] / 2 + 12000, 29000, 65535])
            .collect();
        let image = Raster::new(w, h, image);
        let low_layer = Layer::from_raster(1, "low", &Raster::new(w, h, low.clone()));

        // High = image grain-extract low.
        let mut extract = Layer::from_raster(2, "x", &Raster::new(w, h, low));
        extract.blend = BlendMode::GrainExtract;
        let high = composite(&[Layer::from_raster(3, "img", &image), extract], w, h);

        let mut high_layer = Layer::from_raster(4, "high", &high);
        high_layer.blend = BlendMode::GrainMerge;
        let rebuilt = composite(&[low_layer, high_layer], w, h);
        for (a, b) in rebuilt.pixels().iter().zip(image.pixels()) {
            for c in 0..3 {
                assert!(a[c].abs_diff(b[c]) <= 2, "{a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn empty_layer_costs_nothing_and_changes_nothing() {
        let bottom = solid(1, 10, 10, [5, 6, 7, 65535]);
        let empty = Layer::from_pixels(2, "empty", Tiled::new(10, 10, [0; 4]));
        assert_eq!(
            composite(&[bottom, empty], 10, 10).get(0, 0),
            [5, 6, 7, 65535]
        );
    }
}
