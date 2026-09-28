//! Turning an AI model's low-resolution mask into a full-size selection
//! whose edges follow the photo's (docs/AI.md, "Getting images in and out
//! of models").

use rayon::prelude::*;

use crate::filters::{box_filter_2d, guided_filter};
use crate::raster::Raster;
use crate::selection::Selection;
use crate::tiled::{TILE, TILE_PIXELS, Tiled};

/// The longest side a mask is refined at. Refining a 24 MP image at full
/// size would cost far more and look the same once scaled up.
const WORKING: usize = 2048;

/// `logits` (`lw` × `lh`, stretched over the whole image, above
/// `threshold` inside the object; the model's own cut-off is 0) as
/// selection coverage the size of `image`: its outline interpolated
/// smoothly, snapped to the edges of the image's luminance with a guided
/// filter, then scaled up to full size.
pub fn mask_coverage(logits: &[f32], lw: usize, lh: usize, threshold: f32, image: &Raster) -> Tiled<u16> {
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
            if v > threshold { 1.0 } else { 0.0 }
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

/// Select and Mask's settings, as in Photoshop.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct EdgeOptions {
    /// Edge Detection: how far (pixels) either side of the edge it's
    /// snapped to the photo's own edges, for hair and fur.
    pub radius: f32,
    /// 0–100: rounds off a jagged outline.
    pub smooth: f32,
    /// Softens the edge: a Gaussian blur's standard deviation, in pixels.
    pub feather: f32,
    /// 0–100 %: hardens soft edges; 100 cuts them.
    pub contrast: f32,
    /// −100–100 %: moves soft edges in or out.
    pub shift_edge: f32,
}

/// `image`'s colours, 0–1: what [`refine_edge`] finds edges in.
pub fn colours(image: &Raster) -> Vec<[f32; 3]> {
    image.pixels().par_iter().map(|p| [0, 1, 2].map(|c| f32::from(p[c]) / MAX)).collect()
}

/// `selection` refined as Select and Mask does, in Photoshop's order: its
/// edge found again in `guide` (the image's [`colours`]), smoothed,
/// feathered, hardened, then shifted.
pub fn refine_edge(selection: &Selection, guide: &[[f32; 3]], o: &EdgeOptions) -> Selection {
    let mut coverage = selection.coverage.clone();
    if o.radius >= 0.5 {
        coverage = snap(&coverage, guide, o.radius.round() as usize);
    }
    if o.smooth > 0.0 {
        // Blurred, then cut again with a narrow ramp: corners round off
        // and specks go, and the edge stays about as crisp.
        let blurred = Selection::from_coverage(coverage).feather(o.smooth / 10.0).coverage;
        coverage = blurred.map(|v| unit(ramp(f32::from(v) / MAX, 4.0)));
    }
    if o.feather > 0.05 {
        coverage = Selection::from_coverage(coverage).feather(o.feather).coverage;
    }
    let slope = 1.0 / (1.0 - (o.contrast / 100.0).clamp(0.0, 0.999));
    let shift = (o.shift_edge / 100.0).clamp(-0.99, 0.99);
    if slope > 1.0 || shift != 0.0 {
        coverage = coverage.map(|v| {
            let v = ramp(f32::from(v) / MAX, slope);
            // Outwards, what's partly selected becomes more so, and inwards
            // less, so a soft edge's middle moves.
            unit(if shift > 0.0 { v / (1.0 - shift) } else { (v + shift) / (1.0 + shift) })
        });
    }
    Selection::from_coverage(coverage)
}

const MAX: f32 = 65535.0;

/// `v` steepened by `slope` about the middle.
fn ramp(v: f32, slope: f32) -> f32 {
    ((v - 0.5) * slope + 0.5).clamp(0.0, 1.0)
}

/// 0–1 as a coverage value.
fn unit(v: f32) -> u16 {
    (v.clamp(0.0, 1.0) * MAX).round() as u16
}

/// `coverage` with its edges found again in `guide`, within `r` pixels of
/// where they are: a simple matting. What's selected (or not) all the way
/// round within `r` is sure; each pixel nearer the edge than that is as
/// selected as its colour is along the way from the sure unselected
/// colour nearby to the sure selected one, so a strand of hair the colour of
/// the hair round it is selected and the gaps between strands aren't. Tiles
/// with no edge within reach stay as they are.
fn snap(coverage: &Tiled<u16>, guide: &[[f32; 3]], r: usize) -> Tiled<u16> {
    let (w, h) = (coverage.width() as usize, coverage.height() as usize);
    let tile = TILE as usize;
    // Sure pixels are found `r` away, then their colours averaged over
    // `2 * r` more.
    let reach = 3 * r;
    Tiled::from_tiles(w as u32, h as u32, coverage.fill(), |col, row| {
        let own = || coverage.tile(col, row).map(<[u16]>::to_vec);
        let (tx, ty) = (col as usize * tile, row as usize * tile);
        let near = |m: usize| {
            (tx.saturating_sub(m), ty.saturating_sub(m), (tx + tile + m).min(w), (ty + tile + m).min(h))
        };
        // Nothing to find unless the coverage changes somewhere within `r`.
        let (x0, y0, x1, y1) = near(r);
        let first = coverage.get(x0 as u32, y0 as u32);
        let uniform = (y0 / tile..=(y1 - 1) / tile).all(|r| {
            (x0 / tile..=(x1 - 1) / tile).all(|c| {
                coverage
                    .tile(c as u32, r as u32)
                    .map_or(coverage.fill() == first, |t| t.iter().all(|&v| v == first))
            })
        });
        if uniform {
            return own();
        }
        let (x0, y0, x1, y1) = near(reach);
        let (ww, wh) = (x1 - x0, y1 - y0);
        let at = |x: usize, y: usize| (y - y0) * ww + (x - x0);
        let input: Vec<f32> = (y0..y1)
            .flat_map(|y| (x0..x1).map(move |x| f32::from(coverage.get(x as u32, y as u32)) / MAX))
            .collect();
        let colour: Vec<[f32; 3]> = (y0..y1).flat_map(|y| guide[y * w + x0..y * w + x1].iter().copied()).collect();
        let around = box_filter_2d(input.iter().map(|&v| [v, 0.0, 0.0, 0.0]).collect(), ww, wh, r);
        let inside: Vec<bool> = around.iter().map(|m| m[0] > 0.9999).collect();
        let outside: Vec<bool> = around.iter().map(|m| m[0] < 0.0001).collect();
        // The sure selected, then unselected, colours nearby: their count,
        // then red, green and blue, averaged.
        let mean = |sure: &[bool]| {
            let sums = colour.iter().zip(sure).map(|(c, &s)| if s { [1.0, c[0], c[1], c[2]] } else { [0.0; 4] }).collect();
            box_filter_2d(sums, ww, wh, 2 * r)
        };
        let (f, b) = (mean(&inside), mean(&outside));
        let mut out = vec![0; TILE_PIXELS];
        for y in ty..(ty + tile).min(h) {
            for x in tx..(tx + tile).min(w) {
                let i = at(x, y);
                let v = if inside[i] || outside[i] || f[i][0] <= 0.0 || b[i][0] <= 0.0 {
                    input[i]
                } else {
                    // How far along from the unselected colour to the selected.
                    let (fc, bc) = ([1, 2, 3].map(|c| f[i][c] / f[i][0]), [1, 2, 3].map(|c| b[i][c] / b[i][0]));
                    let d = [0, 1, 2].map(|c| fc[c] - bc[c]);
                    let length = d.iter().map(|v| v * v).sum::<f32>();
                    if length < 4e-4 {
                        input[i]
                    } else {
                        (0..3).map(|c| (colour[i][c] - bc[c]) * d[c]).sum::<f32>() / length
                    }
                };
                out[(y - ty) * tile + (x - tx)] = unit(v);
            }
        }
        Some(out)
    })
}

/// Where the centre of pixel `i` of `n` falls on a grid `m` pixels across.
pub(crate) fn centre(i: usize, n: usize, m: usize) -> f32 {
    (i as f32 + 0.5) * m as f32 / n as f32
}

/// `values` (`w` × `h`) at `(x, y)` in its own pixels, interpolated
/// between the four nearest pixel centres (edges extend outwards).
pub(crate) fn sample(values: &[f32], w: usize, h: usize, x: f32, y: f32) -> f32 {
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
        let coverage = mask_coverage(&logits, lw, lh, 0.0, &image);
        assert_eq!(coverage.get(300, 200), 65535);
        assert_eq!(coverage.get(50, 50), 0);
        // Between the guess's edge and the square's, it follows the square.
        assert!(coverage.get(194, 200) < 16384, "{}", coverage.get(194, 200));
        assert!(coverage.get(205, 200) > 49000, "{}", coverage.get(205, 200));
        // Tiles nowhere near it stay empty.
        assert!(coverage.tile(2, 1).is_none());
    }

    #[test]
    fn select_and_mask_snaps_a_rough_selection_then_softens_hardens_and_shifts_it() {
        // The bright square again, from x = 200 to 400, and a selection
        // that's too wide on the left, from x = 185.
        let (w, h) = (600u32, 400u32);
        let px = (0..w * h)
            .map(|i| if (200..400).contains(&(i % w)) && (100..300).contains(&(i / w)) { [60000; 4] } else { [8000, 8000, 8000, 65535] })
            .collect();
        let guide = colours(&Raster::new(w, h, px));
        let rough = Selection::rectangle(w, h, (185.0, 100.0), (400.0, 300.0));
        let refine = |o: EdgeOptions| refine_edge(&rough, &guide, &o);

        assert_eq!(refine(EdgeOptions::default()).coverage.get(190, 200), 65535);
        let snapped = refine(EdgeOptions { radius: 20.0, ..Default::default() });
        assert!(snapped.at(190, 200) < 0.25, "{}", snapped.at(190, 200));
        assert!(snapped.at(210, 200) > 0.75, "{}", snapped.at(210, 200));
        assert_eq!((snapped.at(300, 200), snapped.at(50, 50)), (1.0, 0.0));

        // Feathered, the edge is soft; hardened again, it isn't.
        let soft = refine(EdgeOptions { feather: 5.0, ..Default::default() });
        let edge = soft.at(183, 200);
        assert!(edge > 0.2 && edge < 0.5, "{edge}");
        let hard = refine(EdgeOptions { feather: 5.0, contrast: 100.0, ..Default::default() });
        assert_eq!((hard.at(183, 200), hard.at(187, 200)), (0.0, 1.0));
        // Shifted out, the soft edge reaches further; in, less far.
        let out = refine(EdgeOptions { feather: 5.0, shift_edge: 50.0, ..Default::default() });
        let inwards = refine(EdgeOptions { feather: 5.0, shift_edge: -50.0, ..Default::default() });
        assert!(out.at(183, 200) > edge && inwards.at(183, 200) < edge);

        // Smooth clears a speck and keeps the rest.
        let speck = rough.combine(&Selection::rectangle(w, h, (50.0, 50.0), (52.0, 52.0)), crate::selection::Combine::Add);
        let smoothed = refine_edge(&speck, &guide, &EdgeOptions { smooth: 30.0, ..Default::default() });
        assert_eq!((smoothed.at(51, 51), smoothed.at(300, 200)), (0.0, 1.0));
    }
}
