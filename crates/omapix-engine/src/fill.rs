//! Filling a selection from its surroundings (Edit › Content-Aware Fill):
//! with an inpainting model (docs/AI.md), the square patch round the
//! selection that the model sees and its answer as a new layer, or with
//! none, by copying texture ([`crate::inpaint`]).

use rayon::prelude::*;

use crate::color::{self, ColorProfile};
use crate::inpaint;
use crate::layer::{Layer, Mask};
use crate::raster::Raster;
use crate::refine::{centre, sample};
use crate::selection::Selection;
use crate::tiled::{TILE, Tiled};
use crate::{Pixel, Result};

/// The part of the image a model fills.
#[derive(Debug)]
pub struct Patch {
    /// Where it is in the image, as `[x, y, width, height]`.
    pub rect: [u32; 4],
    /// The patch stretched to the model's size, `width` × `height`: sRGB
    /// from 0 to 1, red, green then blue planes.
    pub image: Vec<f32>,
    /// 1 where to fill, 0 where to keep, at the same size.
    pub mask: Vec<f32>,
    /// 1 where the image has nothing (it's transparent), 0 where it's
    /// solid, at the same size.
    pub empty: Vec<f32>,
    pub width: usize,
    pub height: usize,
}

impl Patch {
    /// For a model that works in cells `cell` pixels across: 1 for each
    /// cell with anything to fill in it, cell by cell in rows.
    pub fn cells(&self, cell: usize) -> Vec<f32> {
        self.cells_with(&self.mask, cell)
    }

    /// Likewise, 1 for each cell where any of the image is missing.
    pub fn empty_cells(&self, cell: usize) -> Vec<f32> {
        self.cells_with(&self.empty, cell)
    }

    fn cells_with(&self, plane: &[f32], cell: usize) -> Vec<f32> {
        let (columns, rows) = (self.width / cell, self.height / cell);
        (0..columns * rows)
            .map(|i| {
                let (x, y) = (i % columns * cell, i / columns * cell);
                let any = (y..y + cell).any(|py| plane[py * self.width + x..py * self.width + x + cell].iter().any(|&m| m > 0.01));
                if any { 1.0 } else { 0.0 }
            })
            .collect()
    }
}

/// The patch round what `selection` selects in `image` (in `profile`'s
/// colour space) that a model working at `size` pixels sees: twice the
/// selection's size, so there's as much around it as in it, and at least
/// `size` so small selections keep their detail. `None` with nothing
/// selected.
pub fn patch(image: &Raster, profile: &ColorProfile, selection: &Selection, size: usize) -> Result<Option<Patch>> {
    let Some([bx, by, bw, bh]) = selection.bounds() else {
        return Ok(None);
    };
    let (w, h) = (image.width(), image.height());
    let side = (bw.max(bh) * 2).max(size as u32);
    let (cw, ch) = (side.min(w), side.min(h));
    let x = (bx + bw / 2).saturating_sub(cw / 2).min(w - cw);
    let y = (by + bh / 2).saturating_sub(ch / 2).min(h - ch);
    cut(image, profile, selection, [x, y, cw, ch], size, size).map(Some)
}

/// The patch round what `selection` selects for a model that sees any
/// shape made of cells `cell` pixels across, up to `cells` of them: twice
/// the selection's width and height (no narrower than a third of its
/// length), so a tall selection gets a tall patch and keeps more of its
/// detail than in a square. Small selections get more of their
/// surroundings rather than being enlarged. `None` with nothing selected.
pub fn patch_of_cells(
    image: &Raster,
    profile: &ColorProfile,
    selection: &Selection,
    cell: usize,
    cells: usize,
) -> Result<Option<Patch>> {
    let Some([bx, by, bw, bh]) = selection.bounds() else {
        return Ok(None);
    };
    let (w, h) = (image.width(), image.height());
    let most = (cells * cell * cell) as f32;
    let (mut cw, mut ch) = ((bw * 2).max(bh * 2 / 3) as f32, (bh * 2).max(bw * 2 / 3) as f32);
    let grow = (most / (cw * ch)).sqrt().max(1.0);
    (cw, ch) = ((cw * grow).min(w as f32), (ch * grow).min(h as f32));
    // As many cells as fit the patch's shape, each at least a pixel of it.
    let scale = (most / (cw * ch)).sqrt().min(1.0);
    let whole = |length: f32| ((length * scale) as usize / cell).max(1) * cell;
    let (mut width, mut height) = (whole(cw), whole(ch));
    while width * height > cells * cell * cell {
        if width > height { width -= cell } else { height -= cell }
    }
    // Not shrunk: pixel for pixel, then.
    if scale > 0.99 {
        (cw, ch) = ((width as f32).min(cw), (height as f32).min(ch));
    }
    let (cw, ch) = ((cw as u32).max(1), (ch as u32).max(1));
    let x = (bx + bw / 2).saturating_sub(cw / 2).min(w - cw);
    let y = (by + bh / 2).saturating_sub(ch / 2).min(h - ch);
    cut(image, profile, selection, [x, y, cw, ch], width, height).map(Some)
}

/// Where `image` has nothing (it's transparent), as a selection to extend
/// the picture into: it reaches `overlap` pixels into the picture and
/// fades out over as much again, so what fills it blends in rather than
/// meeting the picture at a line. `None` if the image is solid throughout.
pub fn empty(image: &Raster, overlap: f32) -> Option<Selection> {
    let clear: Vec<u16> = image.pixels().par_iter().map(|p| u16::MAX - p[3]).collect();
    if clear.iter().all(|&c| c == 0) {
        return None;
    }
    let (w, h) = (image.width(), image.height());
    let soft = Selection::from_coverage(Tiled::from_slice(w, h, 0, &clear)).expand(overlap).feather(overlap / 2.0);
    let coverage: Vec<u16> = soft.coverage.to_vec().into_iter().zip(clear).map(|(soft, clear)| soft.max(clear)).collect();
    Some(Selection::from_coverage(Tiled::from_slice(w, h, 0, &coverage)))
}

/// The part of `image` in `rect`, stretched to `width` × `height`.
fn cut(image: &Raster, profile: &ColorProfile, selection: &Selection, rect: [u32; 4], width: usize, height: usize) -> Result<Patch> {
    let [x, y, cw, ch] = rect;

    // The patch in sRGB, as planes.
    let crop: Vec<Pixel> = (0..ch).flat_map(|py| (0..cw).map(move |px| (px, py))).map(|(px, py)| image.get(x + px, y + py)).collect();
    let srgb = color::convert(&Tiled::from_slice(cw, ch, [0; 4], &crop), profile, &ColorProfile::srgb())?.to_raster();
    let (cw, ch) = (cw as usize, ch as usize);
    let planes: Vec<Vec<f32>> = (0..3)
        .map(|c| srgb.pixels().par_iter().map(|p| f32::from(p[c]) / 65535.0).collect())
        .collect();
    let image = planes.iter().flat_map(|plane| resample(plane, cw, ch, width, height)).collect();
    let clear: Vec<f32> = srgb.pixels().par_iter().map(|p| 1.0 - f32::from(p[3]) / 65535.0).collect();
    let empty = resample(&clear, cw, ch, width, height);

    // Anything selected under a model pixel, or next to it, is filled: the
    // model does best with a little of the object's surroundings too.
    let (kx, ky) = (cw as f32 / width as f32, ch as f32 / height as f32);
    let mask = (0..width * height)
        .into_par_iter()
        .map(|i| {
            let (mx, my) = ((i % width) as f32, (i / width) as f32);
            let x0 = ((mx - 1.0) * kx).floor().max(0.0) as usize;
            let y0 = ((my - 1.0) * ky).floor().max(0.0) as usize;
            let x1 = (((mx + 2.0) * kx).ceil() as usize).min(cw);
            let y1 = (((my + 2.0) * ky).ceil() as usize).min(ch);
            let selected = (y0..y1).any(|sy| (x0..x1).any(|sx| selection.at(x + sx as u32, y + sy as u32) > 0.0));
            if selected { 1.0 } else { 0.0 }
        })
        .collect();
    Ok(Patch {
        rect,
        image,
        mask,
        empty,
        width,
        height,
    })
}

/// A layer `width` × `height` holding the model's answer `filled` (planes
/// like [`Patch::image`]) over the patch, in `profile`'s colour space,
/// masked to `selection`.
pub fn layer(
    id: u64,
    name: &str,
    patch: &Patch,
    filled: &[f32],
    profile: &ColorProfile,
    selection: &Selection,
) -> Result<Layer> {
    let [x, y, cw, ch] = patch.rect;
    let planes: Vec<Vec<f32>> = filled
        .chunks(patch.width * patch.height)
        .map(|plane| resample(plane, patch.width, patch.height, cw as usize, ch as usize))
        .collect();
    let srgb: Vec<Pixel> = (0..cw as usize * ch as usize)
        .into_par_iter()
        .map(|i| {
            let [r, g, b] = [0, 1, 2].map(|c| (planes[c][i].clamp(0.0, 1.0) * 65535.0).round() as u16);
            [r, g, b, u16::MAX]
        })
        .collect();
    let fill = color::convert(&Tiled::from_slice(cw, ch, [0; 4], &srgb), &ColorProfile::srgb(), profile)?;
    let (w, h) = (selection.width(), selection.height());
    let pixels = Tiled::from_tiles(w, h, [0; 4], |col, row| {
        let (tx, ty) = (col * TILE, row * TILE);
        if tx >= x + cw || ty >= y + ch || tx + TILE <= x || ty + TILE <= y {
            return None;
        }
        let mut tile = vec![[0; 4]; (TILE * TILE) as usize];
        for py in ty.max(y)..(ty + TILE).min(y + ch).min(h) {
            for px in tx.max(x)..(tx + TILE).min(x + cw).min(w) {
                tile[((py - ty) * TILE + (px - tx)) as usize] = fill.get(px - x, py - y);
            }
        }
        Some(tile)
    });
    let mut layer = Layer::from_pixels(id, name, pixels);
    layer.mask = Some(Mask {
        pixels: selection.coverage.clone(),
        enabled: true,
    });
    Ok(layer)
}

/// The least `image` round the selection that [`copied`] copies from.
const SURROUNDINGS: u32 = 256;

/// A layer holding what `selection` selects in `image`, filled with texture
/// copied from round it, masked to `selection`. It looks as far from the
/// selection as the selection is long, on every side, and works on the
/// image's own values at its own size, so the fill is as sharp as what's
/// round it. `None` with nothing selected.
pub fn copied(id: u64, name: &str, image: &Raster, selection: &Selection) -> Option<Layer> {
    let [bx, by, bw, bh] = selection.bounds()?;
    let (w, h) = (image.width(), image.height());
    let pad = bw.max(bh).max(SURROUNDINGS);
    let (x, y) = (bx.saturating_sub(pad), by.saturating_sub(pad));
    let (cw, ch) = ((bx + bw + pad).min(w) - x, (by + bh + pad).min(h) - y);
    let window = move || (0..ch).flat_map(move |py| (0..cw).map(move |px| (x + px, y + py)));
    let pixels: Vec<Pixel> = window().map(|(px, py)| image.get(px, py)).collect();
    let hole: Vec<bool> = window().map(|(px, py)| selection.at(px, py) > 0.0).collect();
    let empty: Vec<bool> = pixels.iter().map(|p| p[3] < u16::MAX).collect();
    let rgb: Vec<[f32; 3]> = pixels.iter().map(|p| [0, 1, 2].map(|c| f32::from(p[c]) / 65535.0)).collect();
    let filled = inpaint::fill(cw as usize, ch as usize, &rgb, &hole, &empty);

    // Only the selection's pixels, so the layer has no more tiles than
    // the selection touches.
    let at = |px: u32, py: u32| ((py - y) * cw + (px - x)) as usize;
    let pixels = Tiled::from_tiles(w, h, [0; 4], |col, row| {
        let (tx, ty) = (col * TILE, row * TILE);
        if tx >= bx + bw || ty >= by + bh || tx + TILE <= bx || ty + TILE <= by {
            return None;
        }
        let mut tile = vec![[0; 4]; (TILE * TILE) as usize];
        for py in ty.max(by)..(ty + TILE).min(by + bh) {
            for px in (tx.max(bx)..(tx + TILE).min(bx + bw)).filter(|&px| hole[at(px, py)]) {
                let [r, g, b] = filled[at(px, py)].map(|v| (v * 65535.0).round() as u16);
                tile[((py - ty) * TILE + (px - tx)) as usize] = [r, g, b, u16::MAX];
            }
        }
        Some(tile)
    });
    let mut layer = Layer::from_pixels(id, name, pixels);
    layer.mask = Some(Mask {
        pixels: selection.coverage.clone(),
        enabled: true,
    });
    Some(layer)
}

/// `plane` (`sw` × `sh`) at `dw` × `dh`: averaged over each pixel's
/// footprint when shrinking, interpolated when growing.
fn resample(plane: &[f32], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<f32> {
    (0..dw * dh)
        .into_par_iter()
        .map(|i| {
            let (x, y) = (i % dw, i / dw);
            if dw > sw || dh > sh {
                return sample(plane, sw, sh, centre(x, dw, sw), centre(y, dh, sh));
            }
            let (x0, x1) = (x * sw / dw, ((x + 1) * sw / dw).max(x * sw / dw + 1));
            let (y0, y1) = (y * sh / dh, ((y + 1) * sh / dh).max(y * sh / dh + 1));
            let sum: f32 = (y0..y1).map(|sy| plane[sy * sw + x0..sy * sw + x1].iter().sum::<f32>()).sum();
            sum / ((x1 - x0) * (y1 - y0)) as f32
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A grey image with a red square where the selection is.
    fn scene(w: u32, h: u32, square: [u32; 4]) -> (Raster, Selection) {
        let [sx, sy, sw, sh] = square;
        let inside = |x: u32, y: u32| x >= sx && x < sx + sw && y >= sy && y < sy + sh;
        let pixels = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .map(|(x, y)| if inside(x, y) { [65535, 0, 0, 65535] } else { [32768, 32768, 32768, 65535] })
            .collect();
        let selection = Selection::rectangle(w, h, (sx as f32, sy as f32), ((sx + sw) as f32, (sy + sh) as f32));
        (Raster::new(w, h, pixels), selection)
    }

    #[test]
    fn the_patch_surrounds_the_selection_and_marks_it_for_filling() {
        let (image, selection) = scene(2000, 1000, [900, 400, 200, 100]);
        let patch = super::patch(&image, &ColorProfile::srgb(), &selection, 64).unwrap().unwrap();
        // Twice the selection's size, centred on it.
        assert_eq!(patch.rect, [800, 250, 400, 400]);
        assert_eq!(patch.image.len(), 3 * 64 * 64);
        // The square's in the middle (red, and to fill), grey round it.
        let at = |x: usize, y: usize| y * 64 + x;
        assert_eq!(patch.mask[at(32, 32)], 1.0);
        assert!(patch.image[at(32, 32)] > 0.99 && patch.image[64 * 64 + at(32, 32)] < 0.01);
        assert_eq!(patch.mask[at(2, 2)], 0.0);
        assert!((patch.image[at(2, 2)] - 0.5).abs() < 0.01);

        // Near the edge the patch stays inside the image.
        let (image, selection) = scene(500, 300, [0, 0, 50, 50]);
        let edge = super::patch(&image, &ColorProfile::srgb(), &selection, 256).unwrap().unwrap();
        assert_eq!(edge.rect, [0, 0, 256, 256]);
        assert!(super::patch(&image, &ColorProfile::srgb(), &Selection::from_coverage(Tiled::new(500, 300, 0)), 256).unwrap().is_none());
    }

    #[test]
    fn a_patch_of_cells_takes_the_selections_shape() {
        // A tall selection: a tall patch, twice its height and a third of
        // that across, in whole cells.
        let (image, selection) = scene(4000, 3000, [1800, 500, 400, 1600]);
        let patch = patch_of_cells(&image, &ColorProfile::srgb(), &selection, 16, 1024).unwrap().unwrap();
        assert_eq!(patch.rect, [1467, 0, 1066, 3000]);
        assert_eq!((patch.width, patch.height), (19 * 16, 53 * 16));
        assert_eq!(patch.image.len(), 3 * patch.width * patch.height);
        // The cells the selection touches are filled, and no others.
        let cells = patch.cells(16);
        let at = |column: usize, row: usize| cells[row * 19 + column];
        assert_eq!(cells.len(), 19 * 53);
        assert_eq!((at(9, 20), at(0, 20), at(9, 3), at(9, 45)), (1.0, 0.0, 0.0, 0.0));

        // A small selection isn't enlarged: it gets more round it instead.
        let (image, selection) = scene(4000, 3000, [2000, 1500, 60, 40]);
        let small = patch_of_cells(&image, &ColorProfile::srgb(), &selection, 16, 1024).unwrap().unwrap();
        assert_eq!((small.rect[2], small.rect[3]), (small.width as u32, small.height as u32));
        assert_eq!((small.width, small.height), (39 * 16, 26 * 16));

        // A long thin one keeps some surroundings across it.
        let (image, selection) = scene(4000, 3000, [500, 1500, 3000, 20]);
        let wide = patch_of_cells(&image, &ColorProfile::srgb(), &selection, 16, 1024).unwrap().unwrap();
        assert_eq!(wide.rect[2], 4000);
        assert!(wide.rect[3] >= 1000 && wide.height >= 8 * 16, "{:?} {}", wide.rect, wide.height);

        // An image smaller than the model could take: all of it that
        // makes whole cells.
        let (image, selection) = scene(300, 200, [100, 50, 50, 50]);
        let whole = patch_of_cells(&image, &ColorProfile::srgb(), &selection, 16, 1024).unwrap().unwrap();
        assert_eq!((whole.rect, whole.width, whole.height), ([0, 0, 288, 192], 288, 192));
    }

    #[test]
    fn a_canvas_with_room_round_the_picture_selects_the_room_and_a_soft_overlap() {
        // 200 × 100 with the right-hand 60 columns transparent.
        let pixels = (0..200 * 100).map(|i| if i % 200 < 140 { [30000, 30000, 30000, 65535] } else { [0; 4] }).collect();
        let image = Raster::new(200, 100, pixels);
        let selection = empty(&image, 10.0).unwrap();
        let at = |x: u32| selection.coverage.get(x, 50);
        // All of the room, nearly all at the picture's edge, half of it 10
        // pixels in, and nothing beyond 20.
        assert_eq!((at(199), at(140)), (65535, 65535));
        assert!(at(139) > 60000, "{}", at(139));
        assert!((25000..40000).contains(&at(130)), "{}", at(130));
        assert!(at(118) < 1500 && at(100) == 0, "{} {}", at(118), at(100));
        // The model's told which of its cells have nothing in them.
        let patch = patch_of_cells(&image, &ColorProfile::srgb(), &selection, 16, 1024).unwrap().unwrap();
        assert_eq!((patch.rect, patch.width, patch.height), ([8, 2, 192, 96], 192, 96));
        let (anew, clear) = (patch.cells(16), patch.empty_cells(16));
        // In the middle row: solid and kept, solid and made anew (the
        // overlap), part empty, and empty.
        let row = |cells: &[f32]| [5, 7, 8, 11].map(|column| cells[3 * 12 + column]);
        assert_eq!(row(&anew), [0.0, 1.0, 1.0, 1.0]);
        assert_eq!(row(&clear), [0.0, 0.0, 1.0, 1.0]);

        let solid = Raster::new(20, 20, vec![[1, 2, 3, 65535]; 400]);
        assert!(empty(&solid, 10.0).is_none());
    }

    #[test]
    fn copying_fills_the_selection_from_round_it_on_a_layer_of_its_own() {
        // The red square on grey, in a corner of a bigger image.
        let (image, selection) = scene(1200, 900, [200, 200, 100, 40]);
        let layer = copied(7, "Content-Aware Fill", &image, &selection).unwrap();
        // Grey where the square was, and nothing anywhere else.
        for (x, y) in [(200, 200), (250, 220), (299, 239)] {
            let p = layer.pixels.get(x, y);
            assert!(p[0].abs_diff(32768) < 300 && p[1].abs_diff(32768) < 300 && p[3] == 65535, "{x} {y} {p:?}");
        }
        assert_eq!(layer.pixels.get(199, 220)[3], 0);
        assert_eq!(layer.pixels.get(250, 240)[3], 0);
        // Only the tiles the selection touches.
        let tiles = (0..4).flat_map(|row| (0..5).map(move |col| (col, row))).filter(|&(col, row)| layer.pixels.tile(col, row).is_some());
        assert_eq!(tiles.collect::<Vec<_>>(), [(0, 0), (1, 0)]);
        let mask = &layer.mask.as_ref().unwrap().pixels;
        assert_eq!((mask.get(250, 220), mask.get(199, 220)), (65535, 0));

        // At the image's edge, and with part of the image empty.
        let (mut image, selection) = scene(300, 200, [0, 0, 40, 40]);
        for y in 0..200 {
            image.row_mut(y)[200..].fill([0; 4]);
        }
        let layer = copied(7, "Content-Aware Fill", &image, &selection).unwrap();
        let p = layer.pixels.get(20, 20);
        assert!(p[0].abs_diff(32768) < 300 && p[3] == 65535, "{p:?}");
        assert!(copied(7, "x", &image, &Selection::from_coverage(Tiled::new(300, 200, 0))).is_none());
    }

    #[test]
    fn the_answer_becomes_a_layer_over_the_patch_masked_to_the_selection() {
        let (image, selection) = scene(2000, 1000, [900, 400, 200, 100]);
        let patch = super::patch(&image, &ColorProfile::srgb(), &selection, 64).unwrap().unwrap();
        // A model that paints everything grey.
        let filled = vec![0.5; 3 * 64 * 64];
        let layer = layer(7, "Content-Aware Fill", &patch, &filled, &ColorProfile::srgb(), &selection).unwrap();
        let grey = layer.pixels.get(1000, 450);
        assert!(grey[0].abs_diff(32768) < 200 && grey[3] == 65535, "{grey:?}");
        assert_eq!(layer.pixels.get(100, 100)[3], 0);
        let mask = &layer.mask.as_ref().unwrap().pixels;
        assert_eq!((mask.get(1000, 450), mask.get(850, 450)), (65535, 0));
    }
}
