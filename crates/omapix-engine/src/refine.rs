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
    coverage(logits, lw, lh, image, |v| if v > threshold { 1.0 } else { 0.0 })
}

/// `matte` (`lw` × `lh`, 0–1, stretched over the whole image: a matting
/// model's answer) as selection coverage the size of `image`, as
/// [`mask_coverage`] makes it, but kept soft, so hair comes out partly
/// selected.
pub fn matte_coverage(matte: &[f32], lw: usize, lh: usize, image: &Raster) -> Tiled<u16> {
    coverage(matte, lw, lh, image, |v| v)
}

/// `values`, interpolated and then made coverage by `inside`, snapped to
/// the image's edges and scaled up to its size.
fn coverage(values: &[f32], lw: usize, lh: usize, image: &Raster, inside: impl Fn(f32) -> f32 + Sync) -> Tiled<u16> {
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

    // Interpolated: a smooth outline rather than the model's blocky pixels.
    let inside: Vec<f32> = (0..gw * gh)
        .into_par_iter()
        .map(|i| inside(sample(values, lw, lh, centre(i % gw, gw, lw), centre(i / gw, gh, lh))))
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
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct EdgeOptions {
    /// Edge Detection: how far (pixels) either side of the edge it's
    /// snapped to the photo's own edges, for hair and fur.
    pub radius: f32,
    /// Automatically adapt the radius to the edge's sharpness: narrower on hard edges, wider on soft ones.
    pub smart_radius: bool,
    /// 0–100: rounds off a jagged outline.
    pub smooth: f32,
    /// Softens the edge: a Gaussian blur's standard deviation, in pixels.
    pub feather: f32,
    /// 0–100 %: hardens soft edges; 100 cuts them.
    pub contrast: f32,
    /// −100–100 %: moves soft edges in or out.
    pub shift_edge: f32,
    /// Replaces color fringing in edge transitions with nearby foreground colors.
    pub decontaminate: bool,
    /// 0–100 %: how strongly to replace fringe color with foreground color.
    #[serde(default = "default_decontaminate_amount")]
    pub decontaminate_amount: f32,
}

fn default_decontaminate_amount() -> f32 {
    100.0
}

impl Default for EdgeOptions {
    fn default() -> Self {
        Self {
            radius: 0.0,
            smart_radius: false,
            smooth: 0.0,
            feather: 0.0,
            contrast: 0.0,
            shift_edge: 0.0,
            decontaminate: false,
            decontaminate_amount: 100.0,
        }
    }
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
        coverage = snap(&coverage, guide, o.radius, o.smart_radius);
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

/// `coverage` with its edges found again in `guide`, within `radius` pixels of
/// where they are: a simple matting. What's selected (or not) all the way
/// round within `radius` is sure; each pixel nearer the edge than that is as
/// selected as its colour is along the way from the sure unselected
/// colour nearby to the sure selected one, so a strand of hair the colour of
/// the hair round it is selected and the gaps between strands aren't. Tiles
/// with no edge within reach stay as they are.
/// When `smart_radius` is true, the effective radius adapts to the sharpness
/// of the image edge: narrower on sharp transitions and wider on soft ones.
fn snap(coverage: &Tiled<u16>, guide: &[[f32; 3]], radius: f32, smart_radius: bool) -> Tiled<u16> {
    let (w, h) = (coverage.width() as usize, coverage.height() as usize);
    let tile = TILE as usize;
    let r = radius.round() as usize;
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

        let (dist, widths) = if smart_radius {
            let bin: Vec<bool> = input.iter().map(|&v| v >= 0.5).collect();
            let mut boundary = vec![false; ww * wh];
            let mut any = false;
            for y in 0..wh {
                for x in 0..ww {
                    let i = y * ww + x;
                    let v = bin[i];
                    let diff = (x > 0 && bin[i - 1] != v)
                        || (x + 1 < ww && bin[i + 1] != v)
                        || (y > 0 && bin[i - ww] != v)
                        || (y + 1 < wh && bin[i + ww] != v);
                    boundary[i] = diff;
                    any |= diff;
                }
            }
            if any {
                (Some(edt(&boundary, ww, wh)), Some(edge_width(&colour, ww, wh, r)))
            } else {
                (None, None)
            }
        } else {
            (None, None)
        };

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
                    let mat = if length < 4e-4 {
                        input[i]
                    } else {
                        (0..3).map(|c| (colour[i][c] - bc[c]) * d[c]).sum::<f32>() / length
                    };
                    if let (Some(dist), Some(widths)) = (&dist, &widths) {
                        let reff = widths[i].clamp(1.5, radius.max(1.5));
                        let wgt = (reff + 1.0 - dist[i]).clamp(0.0, 1.0);
                        input[i] + (mat - input[i]) * wgt
                    } else {
                        mat
                    }
                };
                out[(y - ty) * tile + (x - tx)] = unit(v);
            }
        }
        Some(out)
    })
}

/// Per-pixel transition width (pixels) of the guide's luminance: local range / local max
/// gradient over a window of radius `r` (a sharp step gives ~2, a ramp of width L gives ~L).
/// Adapted from PhotoCraft (crates/algo/src/matting.rs), MIT / Apache-2.0.
pub fn edge_width(guide: &[[f32; 3]], w: usize, h: usize, r: usize) -> Vec<f32> {
    let y: Vec<f32> = guide.iter().map(|p| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]).collect();
    let mut g = vec![0.0f32; w * h];
    for yy in 0..h {
        for xx in 0..w {
            let at = |x: usize, y2: usize| y[y2 * w + x];
            let gx = (at((xx + 1).min(w - 1), yy) - at(xx.saturating_sub(1), yy)) / 2.0;
            let gy = (at(xx, (yy + 1).min(h - 1)) - at(xx, yy.saturating_sub(1))) / 2.0;
            g[yy * w + xx] = (gx * gx + gy * gy).sqrt();
        }
    }
    let gm = max_square(&g, w, h, r, true);
    let hi = max_square(&y, w, h, r, true);
    let lo = max_square(&y, w, h, r, false);
    (0..w * h).map(|i| (hi[i] - lo[i]) / gm[i].max(1e-4)).collect()
}

/// Decontaminates colours in the soft fringe of `coverage`: each fringe pixel
/// moves towards the average colour of nearby fully selected pixels (normalised
/// convolution with a box of radius `max(radius, 3)`, widened ×4 where no such pixel
/// is near), by `amount`% scaled by its transparency.
/// Adapted from PhotoCraft (crates/algo/src/matting.rs), MIT / Apache-2.0.
pub fn decontaminate_colours(
    pixels: &Tiled<[u16; 4]>,
    coverage: &Tiled<u16>,
    radius: f32,
    amount: f32,
) -> Tiled<[u16; 4]> {
    let (w, h) = (pixels.width() as usize, pixels.height() as usize);
    let tile = TILE as usize;
    let rad = (radius.max(3.0)).ceil() as usize;
    let halo = 4 * rad;
    let amount = (amount / 100.0).clamp(0.0, 1.0);
    if amount <= 0.0 || w == 0 || h == 0 {
        return pixels.clone();
    }
    let soft_cov = |v: u16| v > 1310 && v < 64224;

    Tiled::from_tiles(w as u32, h as u32, pixels.fill(), |col, row| {
        let own = || pixels.tile(col, row).map(<[[u16; 4]]>::to_vec);
        let (tx, ty) = (col as usize * tile, row as usize * tile);

        let has_soft = match coverage.tile(col, row) {
            Some(t) => t.iter().any(|&v| soft_cov(v)),
            None => soft_cov(coverage.fill()),
        };
        if !has_soft {
            return own();
        }

        let (x0, y0) = (tx.saturating_sub(halo), ty.saturating_sub(halo));
        let (x1, y1) = ((tx + tile + halo).min(w), (ty + tile + halo).min(h));
        let (ww, wh) = (x1 - x0, y1 - y0);
        let at = |x: usize, y: usize| (y - y0) * ww + (x - x0);

        let mut buf = Vec::with_capacity(ww * wh);
        let mut al = Vec::with_capacity(ww * wh);
        let mut orig_px = Vec::with_capacity(ww * wh);

        for y in y0..y1 {
            for x in x0..x1 {
                let p = pixels.get(x as u32, y as u32);
                let cov = coverage.get(x as u32, y as u32);
                let a = cov as f32 / MAX;
                let alpha = p[3] as f32 / MAX;
                let wts = if a >= 0.98 { alpha } else { 0.0 };
                let r = p[0] as f32 / MAX;
                let g = p[1] as f32 / MAX;
                let b = p[2] as f32 / MAX;
                buf.push([wts, wts * r, wts * g, wts * b]);
                al.push(a);
                orig_px.push([r, g, b, alpha]);
            }
        }

        let rad_c = rad.min(ww.saturating_sub(1)).min(wh.saturating_sub(1));
        let rad4_c = (rad * 4).min(ww.saturating_sub(1)).min(wh.saturating_sub(1));

        let filtered = box_filter_2d(buf.clone(), ww, wh, rad_c);
        let filtered2 = box_filter_2d(buf, ww, wh, rad4_c);

        let mut out = vec![pixels.fill(); TILE_PIXELS];
        for y in ty..(ty + tile).min(h) {
            for x in tx..(tx + tile).min(w) {
                let i = at(x, y);
                let a = al[i];
                let orig = orig_px[i];
                let out_px = if a > 0.02 && a < 0.98 {
                    let k = amount * ((1.0 - a) * 4.0).clamp(0.0, 1.0);
                    let den = filtered[i][0];
                    let den2 = filtered2[i][0];
                    let mut rgb = [0.0; 3];
                    for c in 0..3 {
                        let f = if den > 1e-4 {
                            filtered[i][c + 1] / den
                        } else if den2 > 1e-4 {
                            filtered2[i][c + 1] / den2
                        } else {
                            orig[c]
                        };
                        rgb[c] = orig[c] + (f - orig[c]) * k;
                    }
                    [
                        unit(rgb[0]),
                        unit(rgb[1]),
                        unit(rgb[2]),
                        unit(orig[3]),
                    ]
                } else {
                    [
                        unit(orig[0]),
                        unit(orig[1]),
                        unit(orig[2]),
                        unit(orig[3]),
                    ]
                };
                out[(y - ty) * tile + (x - tx)] = out_px;
            }
        }
        Some(out)
    })
}

/// Running max (or min) over a window of radius `r` (edge-clipped), van Herk / Gil–Werman:
/// three comparisons per sample whatever the radius.
/// Adapted from PhotoCraft (crates/algo/src/matting.rs).
fn running_extreme(src: &[f32], out: &mut [f32], r: usize, max: bool, g: &mut Vec<f32>, hb: &mut Vec<f32>) {
    let n = src.len();
    if n == 0 {
        return;
    }
    let k = 2 * r + 1;
    let f = |a: f32, b: f32| if max { a.max(b) } else { a.min(b) };
    let pad = if max { f32::NEG_INFINITY } else { f32::INFINITY };
    let m = (n + 2 * r).div_ceil(k) * k;
    g.clear();
    g.resize(m, pad);
    hb.clear();
    hb.resize(m, pad);
    let p = |j: usize| if j >= r && j < r + n { src[j - r] } else { pad };
    for j in 0..m {
        g[j] = if j % k == 0 { p(j) } else { f(g[j - 1], p(j)) };
    }
    for j in (0..m).rev() {
        hb[j] = if j % k == k - 1 || j == m - 1 { p(j) } else { f(hb[j + 1], p(j)) };
    }
    for (i, o) in out.iter_mut().enumerate() {
        *o = f(hb[i], g[i + 2 * r]);
    }
}

/// Running max (or min) over a square window of radius `r` (edge-clipped), separable.
/// Adapted from PhotoCraft (crates/algo/src/matting.rs).
fn max_square(src: &[f32], w: usize, h: usize, r: usize, max: bool) -> Vec<f32> {
    if w == 0 || h == 0 {
        return Vec::new();
    }
    let (mut g, mut hb) = (Vec::new(), Vec::new());
    let mut tmp = vec![0.0f32; w * h];
    for y in 0..h {
        running_extreme(&src[y * w..(y + 1) * w], &mut tmp[y * w..(y + 1) * w], r, max, &mut g, &mut hb);
    }
    let mut out = vec![0.0f32; w * h];
    let (mut col, mut res) = (vec![0.0f32; h], vec![0.0f32; h]);
    for x in 0..w {
        for y in 0..h {
            col[y] = tmp[y * w + x];
        }
        running_extreme(&col, &mut res, r, max, &mut g, &mut hb);
        for y in 0..h {
            out[y * w + x] = res[y];
        }
    }
    out
}

/// Euclidean distance transform for binary seeds.
/// Adapted from PhotoCraft (crates/algo/src/selection.rs).
fn edt(seeds: &[bool], w: usize, h: usize) -> Vec<f32> {
    let mut g: Vec<f32> = seeds.iter().map(|&b| if b { 0.0 } else { 1e20 }).collect();
    let n = w.max(h).max(1);
    let (mut f, mut o, mut v, mut z) = (vec![0.0; n], vec![0.0; n], vec![0usize; n], vec![0.0f32; n + 1]);
    for x in 0..w {
        for y in 0..h {
            f[y] = g[y * w + x];
        }
        dt1(&f[..h], &mut o[..h], &mut v, &mut z);
        for y in 0..h {
            g[y * w + x] = o[y];
        }
    }
    for y in 0..h {
        f[..w].copy_from_slice(&g[y * w..(y + 1) * w]);
        dt1(&f[..w], &mut o[..w], &mut v, &mut z);
        for x in 0..w {
            g[y * w + x] = o[x].sqrt();
        }
    }
    g
}

fn dt1(f: &[f32], out: &mut [f32], v: &mut [usize], z: &mut [f32]) {
    let n = f.len();
    if n == 0 {
        return;
    }
    let mut k = 0;
    v[0] = 0;
    z[0] = f32::NEG_INFINITY;
    z[1] = f32::INFINITY;
    for q in 1..n {
        loop {
            let p = v[k];
            let s = ((f[q] + (q * q) as f32) - (f[p] + (p * p) as f32)) / (2.0 * (q as f32 - p as f32));
            if s <= z[k] && k > 0 {
                k -= 1;
                continue;
            }
            if s <= z[k] {
                v[0] = q;
                z[0] = f32::NEG_INFINITY;
                z[1] = f32::INFINITY;
                break;
            }
            k += 1;
            v[k] = q;
            z[k] = s;
            z[k + 1] = f32::INFINITY;
            break;
        }
    }
    k = 0;
    for (q, o) in out.iter_mut().enumerate() {
        while z[k + 1] < q as f32 {
            k += 1;
        }
        let d = q as f32 - v[k] as f32;
        *o = d * d + f[v[k]];
    }
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

    #[test]
    fn max_square_matches_naive() {
        let (w, h) = (17, 11);
        let src: Vec<f32> = (0..w * h).map(|i| ((i * 37) % 23) as f32).collect();
        for r in [1usize, 2, 5] {
            for max in [true, false] {
                let fast = max_square(&src, w, h, r, max);
                for y in 0..h {
                    for x in 0..w {
                        let mut v = if max { f32::MIN } else { f32::MAX };
                        for yy in y.saturating_sub(r)..(y + r + 1).min(h) {
                            for xx in x.saturating_sub(r)..(x + r + 1).min(w) {
                                v = if max { v.max(src[yy * w + xx]) } else { v.min(src[yy * w + xx]) };
                            }
                        }
                        assert_eq!(fast[y * w + x], v, "r={r} max={max} ({x},{y})");
                    }
                }
            }
        }
    }

    #[test]
    fn edge_width_sharp_step_and_soft_ramp() {
        let (w, h) = (100, 20);
        // Sharp step image: 0.0 on left, 1.0 on right (step at x=50).
        let step_guide: Vec<[f32; 3]> = (0..w * h)
            .map(|i| {
                let x = i % w;
                let v = if x >= 50 { 1.0 } else { 0.0 };
                [v, v, v]
            })
            .collect();
        let step_widths = edge_width(&step_guide, w, h, 10);
        // At the step, transition width should be around 2.0 (sharp).
        let step_w = step_widths[10 * w + 50];
        assert!(step_w >= 1.5 && step_w <= 2.5, "step width: {step_w}");

        // Linear ramp image over 20 pixels from x=40 to x=60.
        let ramp_guide: Vec<[f32; 3]> = (0..w * h)
            .map(|i| {
                let x = i % w;
                let v = if x < 40 { 0.0 } else if x >= 60 { 1.0 } else { (x - 40) as f32 / 20.0 };
                [v, v, v]
            })
            .collect();
        let ramp_widths = edge_width(&ramp_guide, w, h, 10);
        // On the ramp, transition width should be much wider (~20.0).
        let ramp_w = ramp_widths[10 * w + 50];
        assert!(ramp_w >= 15.0, "ramp width: {ramp_w}");
    }

    #[test]
    fn smart_radius_limits_spread_on_sharp_edges() {
        let (w, h) = (600u32, 400u32);
        // Sharp square from x=200 to 400.
        let px = (0..w * h)
            .map(|i| if (200..400).contains(&(i % w)) && (100..300).contains(&(i / w)) { [60000; 4] } else { [8000, 8000, 8000, 65535] })
            .collect();
        let guide = colours(&Raster::new(w, h, px));
        // A selection starting at x=198 (2 pixels outside the square edge at 200).
        let rough = Selection::rectangle(w, h, (198.0, 100.0), (400.0, 300.0));

        let standard = refine_edge(&rough, &guide, &EdgeOptions { radius: 20.0, smart_radius: false, ..Default::default() });
        let smart = refine_edge(&rough, &guide, &EdgeOptions { radius: 20.0, smart_radius: true, ..Default::default() });

        // Both refine the edge inside the square:
        assert!(standard.at(205, 200) > 0.8, "{}", standard.at(205, 200));
        assert!(smart.at(205, 200) > 0.8, "{}", smart.at(205, 200));

        // Smart radius leaves pixels outside its ~2px reach untouched:
        assert_eq!(rough.coverage.get(190, 200), 0);
        assert_eq!(smart.coverage.get(190, 200), 0);
    }

    #[test]
    fn decontaminate_colours_replaces_fringe_with_foreground() {
        let (w, h) = (20u32, 8u32);
        // Foreground red on the left (x < 10), background blue on the right (x > 10).
        // Fringe pixel at x = 10 is a 50/50 mix (purple).
        let mut px = vec![[0u16; 4]; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let idx = (y * w + x) as usize;
                px[idx] = if x < 10 {
                    [65535, 0, 0, 65535] // Red
                } else if x == 10 {
                    [32768, 0, 32768, 65535] // Purple fringe
                } else {
                    [0, 0, 65535, 65535] // Blue
                };
            }
        }
        let pixels = Tiled::from_slice(w, h, [0; 4], &px);

        // Mask: 65535 for x < 10, 32768 for x == 10, 0 for x > 10.
        let mut cov = vec![0u16; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let idx = (y * w + x) as usize;
                cov[idx] = if x < 10 {
                    65535
                } else if x == 10 {
                    32768
                } else {
                    0
                };
            }
        }
        let coverage = Tiled::from_slice(w, h, 0, &cov);

        let result = decontaminate_colours(&pixels, &coverage, 3.0, 100.0);

        // At x = 10 (the fringe), color should have moved strongly towards red (foreground).
        let fringe = result.get(10, 4);
        assert!(fringe[0] > 55000, "expected red > 55000, got {}", fringe[0]);
        assert!(fringe[2] < 10000, "expected blue < 10000, got {}", fringe[2]);

        // At x = 5 (deep inside foreground), color is untouched red.
        assert_eq!(result.get(5, 4), [65535, 0, 0, 65535]);

        // At x = 15 (deep outside), color is untouched blue.
        assert_eq!(result.get(15, 4), [0, 0, 65535, 65535]);
    }
}
