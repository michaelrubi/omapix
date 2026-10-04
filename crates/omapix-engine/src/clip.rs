//! Cut, copy and paste: pixels lifted from a layer or the whole image, and
//! put back as a new layer.

use image::ImageEncoder;
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use rayon::prelude::*;

use crate::layer::Layer;
use crate::selection::Selection;
use crate::tiled::{TILE, TILE_PIXELS, Tiled};
use crate::{ColorProfile, DisplayTransform, Document, Pixel, Result};

/// Pixels on the clipboard.
#[derive(Clone)]
pub struct Clip {
    /// What was copied, transparent outside `bounds`.
    pub pixels: Tiled<Pixel>,
    /// `[x, y, width, height]` of the copied area within `pixels`.
    pub bounds: [u32; 4],
    pub profile: ColorProfile,
    /// Paste back where it was copied from (if it fits there) rather than
    /// centred. True for copies made in Omapix.
    pub in_place: bool,
}

impl Clip {
    /// Copy what `selection` covers of `pixels` (all of them without a
    /// selection). Partly selected pixels come out partly transparent, as
    /// in Photoshop. `None` if nothing is selected.
    pub fn copy(
        pixels: &Tiled<Pixel>,
        selection: Option<&Selection>,
        profile: &ColorProfile,
    ) -> Option<Clip> {
        let (w, h) = (pixels.width(), pixels.height());
        let (pixels, bounds) = match selection {
            None => (pixels.clone(), [0, 0, w, h]),
            Some(sel) => {
                let bounds = sel.bounds()?;
                let cov = &sel.coverage;
                let cut = Tiled::from_tiles(w, h, [0; 4], |col, row| {
                    let s = cov.tile(col, row);
                    if s.is_none() && cov.fill() == 0 {
                        return None;
                    }
                    let p = pixels.tile(col, row);
                    let tile: Vec<Pixel> = (0..TILE_PIXELS)
                        .map(|i| {
                            let mut px = p.map_or(pixels.fill(), |t| t[i]);
                            let k = u32::from(s.map_or(cov.fill(), |t| t[i]));
                            px[3] = ((u32::from(px[3]) * k + 32767) / 65535) as u16;
                            px
                        })
                        .collect();
                    tile.iter().any(|p| p[3] != 0).then_some(tile)
                });
                (cut, bounds)
            }
        };
        Some(Clip {
            pixels,
            bounds,
            profile: profile.clone(),
            in_place: true,
        })
    }

    /// Copy a layer mask as opaque grey pixels.
    pub fn copy_mask(
        mask: &Tiled<u16>,
        selection: Option<&Selection>,
        profile: &ColorProfile,
    ) -> Option<Clip> {
        let grey = Tiled::from_tiles(mask.width(), mask.height(), [0; 4], |col, row| {
            let t = mask.tile(col, row);
            Some(
                (0..TILE_PIXELS)
                    .map(|i| {
                        let v = t.map_or(mask.fill(), |t| t[i]);
                        [v, v, v, u16::MAX]
                    })
                    .collect(),
            )
        });
        Self::copy(&grey, selection, profile)
    }

    /// An image from another app, pasted centred.
    pub fn from_image(bytes: &[u8]) -> Result<Clip> {
        let (raster, profile) = crate::io::decode_image(bytes)?;
        Ok(Clip {
            bounds: [0, 0, raster.width(), raster.height()],
            pixels: Tiled::from_raster(&raster),
            profile,
            in_place: false,
        })
    }

    /// True if every copied pixel is transparent.
    pub fn is_empty(&self) -> bool {
        let [bx, by, bw, bh] = self.bounds;
        (by..by + bh)
            .into_par_iter()
            .all(|y| (bx..bx + bw).all(|x| self.pixels.get(x, y)[3] == 0))
    }

    /// The copied area as an 8-bit sRGB PNG, for other apps.
    pub fn to_png(&self) -> Result<Vec<u8>> {
        let [bx, by, bw, bh] = self.bounds;
        let transform = DisplayTransform::to_srgb(&self.profile)?;
        let mut rgba = vec![[0u8; 4]; bw as usize * bh as usize];
        rgba.par_chunks_mut(bw as usize)
            .enumerate()
            .for_each(|(y, out)| {
                let row: Vec<Pixel> = (0..bw)
                    .map(|x| self.pixels.get(bx + x, by + y as u32))
                    .collect();
                transform.convert(&row, out);
            });
        let mut png = Vec::new();
        PngEncoder::new_with_quality(&mut png, CompressionType::Fast, FilterType::Adaptive)
            .write_image(rgba.as_flattened(), bw, bh, image::ExtendedColorType::Rgba8)?;
        Ok(png)
    }

    /// The clip as a `width`×`height` layer's pixels in `profile`: centred
    /// on `centre` if given (Paste Into), or else back where it was copied
    /// from if it fits there, otherwise centred.
    pub fn place(
        &self,
        width: u32,
        height: u32,
        profile: &ColorProfile,
        centre: Option<(i64, i64)>,
    ) -> Result<Tiled<Pixel>> {
        let src = &self.pixels;
        let [bx, by, bw, bh] = self.bounds;
        let (x, y) = if let Some((cx, cy)) = centre {
            (cx - i64::from(bw) / 2, cy - i64::from(bh) / 2)
        } else if self.in_place && bx + bw <= width && by + bh <= height {
            (i64::from(bx), i64::from(by))
        } else {
            (
                (i64::from(width) - i64::from(bw)) / 2,
                (i64::from(height) - i64::from(bh)) / 2,
            )
        };
        let same_place = (x, y) == (i64::from(bx), i64::from(by));
        let placed = if same_place && (src.width(), src.height()) == (width, height) {
            // Copied from a document this size: reuse its tiles.
            src.clone()
        } else {
            Tiled::from_tiles(width, height, [0; 4], |col, row| {
                // The part of this tile the clip lands on.
                let (tx, ty) = (i64::from(col * TILE), i64::from(row * TILE));
                let x0 = x.max(tx);
                let y0 = y.max(ty);
                let x1 = (x + i64::from(bw))
                    .min(tx + i64::from(TILE))
                    .min(i64::from(width));
                let y1 = (y + i64::from(bh))
                    .min(ty + i64::from(TILE))
                    .min(i64::from(height));
                if x0 >= x1 || y0 >= y1 {
                    return None;
                }
                let mut tile = vec![[0; 4]; TILE_PIXELS];
                for py in y0..y1 {
                    let sy = (py - y) as u32 + by;
                    for px in x0..x1 {
                        let sx = (px - x) as u32 + bx;
                        tile[((py - ty) * i64::from(TILE) + px - tx) as usize] = src.get(sx, sy);
                    }
                }
                tile.iter().any(|p| p[3] != 0).then_some(tile)
            })
        };
        if self.profile.icc() == profile.icc() {
            Ok(placed)
        } else {
            crate::color::convert(&placed, &self.profile, profile)
        }
    }

    /// A new image with no file yet, the size of the copied area and in its
    /// colour space, with it as the only layer.
    pub fn document(&self) -> Result<Document> {
        let [_, _, w, h] = self.bounds;
        let centre = (i64::from(w) / 2, i64::from(h) / 2);
        let pixels = self.place(w, h, &self.profile, Some(centre))?;
        Ok(Document::new(
            std::path::PathBuf::new(),
            self.profile.clone(),
            16,
            w,
            h,
            vec![Layer::from_pixels(1, "Background", pixels)],
        ))
    }
}

/// What kind of paste to perform.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PasteKind {
    #[default]
    Normal,
    InPlace,
    Into,
}

/// Paste `clip` as a new layer above layer index `above` (at the top of it,
/// for a group), and deselect, as Photoshop does. Returns the new layer's
/// id.
pub fn paste(doc: &mut Document, clip: &Clip, above: usize, kind: PasteKind) -> Result<u64> {
    let centre = if kind == PasteKind::Into {
        doc.selection
            .as_ref()
            .and_then(|s| s.bounds())
            .map(|[bx, by, bw, bh]| {
                (
                    i64::from(bx) + i64::from(bw) / 2,
                    i64::from(by) + i64::from(bh) / 2,
                )
            })
    } else {
        None
    };
    let pixels = clip.place(doc.width, doc.height, &doc.profile, centre)?;
    let id = doc.next_layer_id();
    let name = doc.unused_name("Layer");
    let above = above.min(doc.layers.len() - 1);
    let mut layer = Layer::from_pixels(id, name, pixels);
    if kind == PasteKind::Into
        && let Some(sel) = &doc.selection
    {
        layer.mask = Some(crate::Mask {
            pixels: sel.coverage.clone(),
            enabled: true,
        });
    }
    doc.insert_above(above, layer);
    doc.selection = None;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Raster;

    const GREY: Pixel = [30000, 30000, 30000, 65535];

    fn doc(w: u32, h: u32) -> Document {
        let px = (0..w * h)
            .map(|i| [(i % w) as u16 * 100, (i / w) as u16 * 100, 20000, 65535])
            .collect();
        Document::from_image(
            "t.tif".into(),
            &Raster::new(w, h, px),
            ColorProfile::srgb(),
            16,
        )
    }

    #[test]
    fn copying_the_selection_crops_to_it_and_feathers_alpha() {
        let d = doc(600, 400);
        let sel = Selection::rectangle(600, 400, (300.0, 100.0), (400.5, 200.0));
        let clip = Clip::copy(&d.layers[0].pixels, Some(&sel), &d.profile).unwrap();
        assert_eq!(clip.bounds, [300, 100, 101, 100]);
        assert_eq!(clip.pixels.get(350, 150), d.layers[0].pixels.get(350, 150));
        assert_eq!(clip.pixels.get(250, 150)[3], 0);
        // Half selected, half transparent.
        assert!(clip.pixels.get(400, 150)[3].abs_diff(32768) < 300);
        assert!(!clip.is_empty());
    }

    #[test]
    fn copying_without_a_selection_shares_the_layer_tiles() {
        let d = doc(600, 400);
        let clip = Clip::copy(&d.layers[0].pixels, None, &d.profile).unwrap();
        assert!(clip.pixels.same_tiles(&d.layers[0].pixels));
        assert_eq!(clip.bounds, [0, 0, 600, 400]);
        // Pasting back into a document this size shares them too.
        let placed = clip.place(600, 400, &d.profile, None).unwrap();
        assert!(placed.same_tiles(&d.layers[0].pixels));
    }

    #[test]
    fn a_clip_becomes_a_new_image_its_size() {
        let d = doc(600, 400);
        let sel = Selection::rectangle(600, 400, (300.0, 100.0), (400.0, 200.0));
        let clip = Clip::copy(&d.layers[0].pixels, Some(&sel), &d.profile).unwrap();
        let new = clip.document().unwrap();
        assert_eq!((new.width, new.height, new.layers.len()), (100, 100, 1));
        assert_eq!(new.file_name(), "Untitled");
        let pixels = &new.layers[0].pixels;
        assert_eq!((pixels.width(), pixels.height()), (100, 100));
        assert_eq!(pixels.get(0, 0), d.layers[0].pixels.get(300, 100));
        assert_eq!(pixels.get(99, 99), d.layers[0].pixels.get(399, 199));
        // A whole image shares its tiles.
        let clip = Clip::copy(&d.layers[0].pixels, None, &d.profile).unwrap();
        assert!(clip.document().unwrap().layers[0].pixels.same_tiles(&d.layers[0].pixels));
    }

    #[test]
    fn nothing_to_copy_from_transparent_pixels_or_an_empty_selection() {
        let d = doc(300, 300);
        let empty = Tiled::new(300, 300, [0; 4]);
        assert!(Clip::copy(&empty, None, &d.profile).unwrap().is_empty());
        let nothing = Selection::all(300, 300).invert();
        assert!(Clip::copy(&d.layers[0].pixels, Some(&nothing), &d.profile).is_none());
    }

    #[test]
    fn paste_puts_it_back_in_place_as_a_new_layer_and_deselects() {
        let mut d = doc(600, 400);
        let sel = Selection::rectangle(600, 400, (500.0, 300.0), (550.0, 350.0));
        let clip = Clip::copy(&d.layers[0].pixels, Some(&sel), &d.profile).unwrap();
        d.selection = Some(sel);
        let id = paste(&mut d, &clip, 0, PasteKind::Normal).unwrap();
        assert_eq!(d.index_of(id), Some(1));
        assert_eq!(d.layer(id).unwrap().name, "Layer 1");
        assert!(d.selection.is_none());
        let pasted = &d.layer(id).unwrap().pixels;
        assert_eq!(pasted.get(520, 320), d.layers[0].pixels.get(520, 320));
        assert_eq!(pasted.get(450, 320)[3], 0);
    }

    #[test]
    fn paste_centres_what_does_not_fit_in_place() {
        let big = doc(600, 400);
        let sel = Selection::rectangle(600, 400, (500.0, 300.0), (600.0, 400.0));
        let clip = Clip::copy(&big.layers[0].pixels, Some(&sel), &big.profile).unwrap();
        let mut small = doc(300, 200);
        small.layers[0].pixels = Tiled::new(300, 200, [0; 4]);
        let id = paste(&mut small, &clip, 0, PasteKind::Normal).unwrap();
        let pasted = &small.layer(id).unwrap().pixels;
        // 100×100 centred in 300×200: from (100, 50).
        assert_eq!(pasted.get(100, 50), big.layers[0].pixels.get(500, 300));
        assert_eq!(pasted.get(199, 149), big.layers[0].pixels.get(599, 399));
        assert_eq!(pasted.get(99, 50)[3], 0);
        assert_eq!(pasted.get(200, 50)[3], 0);

        // Bigger than the document: centred and cropped.
        let whole = Clip::copy(&big.layers[0].pixels, None, &big.profile).unwrap();
        let placed = whole.place(300, 200, &big.profile, None).unwrap();
        assert_eq!(placed.get(0, 0), big.layers[0].pixels.get(150, 100));
    }

    #[test]
    fn place_centres_foreign_image_on_given_point_and_clips_at_edges() {
        let (w, h) = (100, 100);
        let px: Vec<Pixel> = (0..w * h)
            .map(|i| [(i % w) as u16 * 100, (i / w) as u16 * 100, 10000, 65535])
            .collect();
        let clip = Clip {
            pixels: Tiled::from_raster(&Raster::new(w, h, px)),
            bounds: [0, 0, 100, 100],
            profile: ColorProfile::srgb(),
            in_place: false,
        };

        // 100×100 centred on (20, 20) in a 200×200 canvas: top-left is (-30, -30).
        // Negative coordinates clip at the top/left canvas edges.
        let placed = clip.place(200, 200, &ColorProfile::srgb(), Some((20, 20))).unwrap();
        assert_eq!(placed.get(0, 0), clip.pixels.get(30, 30));
        assert_eq!(placed.get(69, 69), clip.pixels.get(99, 99));
        assert_eq!(placed.get(70, 70), [0; 4]);
        assert_eq!(placed.get(100, 100), [0; 4]);

        // Centred at (190, 190): top-left is (140, 140).
        // Extends to (240, 240), clipped at right/bottom edges.
        let placed_br = clip.place(200, 200, &ColorProfile::srgb(), Some((190, 190))).unwrap();
        assert_eq!(placed_br.get(140, 140), clip.pixels.get(0, 0));
        assert_eq!(placed_br.get(199, 199), clip.pixels.get(59, 59));
        assert_eq!(placed_br.get(139, 139), [0; 4]);
    }

    #[test]
    fn paste_into_centres_foreign_clip_on_selection_and_masks_to_coverage() {
        let mut d = doc(600, 400);
        let sel = Selection::rectangle(600, 400, (100.0, 100.0), (300.0, 300.0));
        d.selection = Some(sel.clone());

        let mut px = vec![[0; 4]; 100 * 100];
        px[0] = [50000, 50000, 50000, 65535];
        let foreign = Clip {
            pixels: Tiled::from_raster(&Raster::new(100, 100, px)),
            bounds: [0, 0, 100, 100],
            profile: ColorProfile::srgb(),
            in_place: false,
        };

        let id = paste(&mut d, &foreign, 0, PasteKind::Into).unwrap();
        let layer = d.layer(id).unwrap();
        assert!(layer.mask.as_ref().unwrap().pixels.same_tiles(&sel.coverage));
        assert!(d.selection.is_none());
        // Selection bounds [100, 100, 201, 201] has centre (200, 200).
        // 100×100 centred on (200, 200) has top-left (150, 150).
        assert_eq!(layer.pixels.get(150, 150), [50000, 50000, 50000, 65535]);

        // A copy from elsewhere in the image lands in the selection too,
        // not back where it came from.
        let copied = Selection::rectangle(600, 400, (400.0, 0.0), (500.0, 100.0));
        let own = Clip::copy(&d.layers[0].pixels, Some(&copied), &d.profile).unwrap();
        d.selection = Some(sel);
        let id = paste(&mut d, &own, 0, PasteKind::Into).unwrap();
        assert_eq!(d.layer(id).unwrap().pixels.get(160, 160), d.layers[0].pixels.get(410, 10));
    }

    #[test]
    fn masks_copy_as_grey() {
        let d = doc(300, 300);
        let mut mask = Tiled::new(300, 300, u16::MAX);
        mask.tile_mut(0, 0)[0] = 30000;
        let clip = Clip::copy_mask(&mask, None, &d.profile).unwrap();
        assert_eq!(clip.pixels.get(0, 0), GREY);
        assert_eq!(clip.pixels.get(299, 299), [65535; 4]);
    }

    #[test]
    fn round_trips_through_png_for_other_apps() {
        let d = doc(300, 200);
        let sel = Selection::rectangle(300, 200, (10.0, 20.0), (110.0, 70.0));
        let clip = Clip::copy(&d.layers[0].pixels, Some(&sel), &d.profile).unwrap();
        let back = Clip::from_image(&clip.to_png().unwrap()).unwrap();
        assert!(!back.in_place);
        assert_eq!(back.bounds, [0, 0, 100, 50]);
        let (a, b) = (clip.pixels.get(60, 40), back.pixels.get(50, 20));
        for c in 0..4 {
            assert!(a[c].abs_diff(b[c]) <= 257, "{a:?} vs {b:?}");
        }
    }
}
