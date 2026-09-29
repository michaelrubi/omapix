use std::path::PathBuf;

use crate::layer::Layer;
use crate::selection::Selection;
use crate::tiled::Tiled;
use crate::{ColorProfile, Pixel, Raster};

/// A saved selection: Photoshop's alpha channel.
#[derive(Clone)]
pub struct AlphaChannel {
    pub id: u64,
    pub name: String,
    /// How selected each pixel is, 0 to 65535, like a mask.
    pub pixels: Tiled<u16>,
}

/// An open image: a stack of layers in one colour space.
///
/// Cloning is cheap (layer tiles are shared), which is how undo works: the
/// history keeps whole-document snapshots.
#[derive(Clone)]
pub struct Document {
    /// File the document was opened from.
    pub path: PathBuf,
    /// Where it was last saved in Omapix's own format, if ever.
    pub saved_path: Option<PathBuf>,
    /// A TIFF that saving also writes the flattened image to, for darktable
    /// to pick up (the round trip, see [`crate::io::load_round_trip`]).
    pub round_trip: Option<PathBuf>,
    pub width: u32,
    pub height: u32,
    pub profile: ColorProfile,
    /// Bits per channel in the file it was loaded from (pixels are always held at 16).
    pub source_bits: u8,
    /// Raw EXIF metadata blob preserved across saves and exports.
    pub exif: Option<Vec<u8>>,
    /// Bottom layer first.
    pub layers: Vec<Layer>,
    /// The active selection; `None` means everything (Photoshop's
    /// "nothing selected").
    pub selection: Option<Selection>,
    /// Saved selections, in the order they were made.
    pub channels: Vec<AlphaChannel>,
    next_id: u64,
}

impl Document {
    /// A document with the given layers, bottom first.
    pub fn new(
        path: PathBuf,
        profile: ColorProfile,
        source_bits: u8,
        width: u32,
        height: u32,
        layers: Vec<Layer>,
    ) -> Self {
        let next_id = layers.iter().map(|l| l.id).max().unwrap_or(0) + 1;
        Self {
            path,
            saved_path: None,
            round_trip: None,
            selection: None,
            channels: Vec::new(),
            width,
            height,
            profile,
            source_bits,
            exif: None,
            layers,
            next_id,
        }
    }

    /// A single-layer document from a flat image, like opening a TIFF in Photoshop.
    pub fn from_image(
        path: PathBuf,
        raster: &Raster,
        profile: ColorProfile,
        source_bits: u8,
    ) -> Self {
        let (w, h) = (raster.width(), raster.height());
        Self::new(
            path,
            profile,
            source_bits,
            w,
            h,
            vec![Layer::from_raster(1, "Background", raster)],
        )
    }

    pub fn file_name(&self) -> String {
        let path = self.saved_path.as_ref().unwrap_or(&self.path);
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled".into())
    }

    /// A fresh id for a new layer.
    pub fn next_layer_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Save the selection as a new alpha channel (Select › Save Selection),
    /// returning its id, or `None` if nothing is selected.
    pub fn save_selection(&mut self) -> Option<u64> {
        let pixels = self.selection.as_ref()?.coverage.clone();
        let name = (1..)
            .map(|n| format!("Alpha {n}"))
            .find(|name| self.channels.iter().all(|c| &c.name != name))?;
        let id = self.next_layer_id();
        self.channels.push(AlphaChannel { id, name, pixels });
        Some(id)
    }

    pub fn channel(&self, id: u64) -> Option<&AlphaChannel> {
        self.channels.iter().find(|c| c.id == id)
    }

    pub fn index_of(&self, id: u64) -> Option<usize> {
        self.layers.iter().position(|l| l.id == id)
    }

    pub fn layer(&self, id: u64) -> Option<&Layer> {
        self.layers.iter().find(|l| l.id == id)
    }

    /// The topmost layer showing pixels of its own at (x, y): what the Move
    /// tool's Ctrl+click picks (Photoshop's Auto-Select). Hidden layers,
    /// those in hidden groups, and where a mask hides them don't count.
    pub fn layer_at(&self, x: u32, y: u32) -> Option<u64> {
        let shown = |layer: &Layer| {
            let mut next = Some(layer);
            while let Some(l) = next {
                if !l.visible {
                    return false;
                }
                next = l.parent.and_then(|p| self.layer(p));
            }
            true
        };
        self.layers
            .iter()
            .rev()
            .find(|l| {
                l.has_pixels()
                    && x < self.width
                    && y < self.height
                    && l.pixels.get(x, y)[3] > 0
                    && l.mask.as_ref().is_none_or(|m| !m.enabled || m.pixels.get(x, y) > 0)
                    && shown(l)
            })
            .map(|l| l.id)
    }

    pub fn layer_mut(&mut self, id: u64) -> Option<&mut Layer> {
        self.layers.iter_mut().find(|l| l.id == id)
    }

    /// The visible image, flattened.
    pub fn composite(&self) -> Raster {
        crate::composite::composite(&self.layers, self.width, self.height)
    }

    /// The tonal histogram of the composite of all visible layers below `layer_id`.
    /// Used by adjustment layers (such as Curves and Levels) to show their input distribution.
    pub fn histogram_below(&self, layer_id: u64) -> crate::Histogram {
        let Some(idx) = self.index_of(layer_id) else {
            return crate::Histogram::default();
        };
        crate::Histogram::from_layers(&self.layers[..idx], self.width, self.height)
    }

    /// Flattened value of a pixel from the composite of all visible layers below `layer_id`.
    pub fn sample_below(&self, layer_id: u64, x: u32, y: u32) -> Option<Pixel> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let idx = self.index_of(layer_id)?;
        let col = x / crate::tiled::TILE;
        let row = y / crate::tiled::TILE;
        let tiles = crate::composite::composite_tiles(&self.layers[..idx], &[(col, row)], None);
        let tile = tiles.into_iter().next()?;
        let offset = (y % crate::tiled::TILE) * crate::tiled::TILE + (x % crate::tiled::TILE);
        tile.get(offset as usize).copied()
    }

    /// A name like "Layer 3" that isn't already taken.
    pub fn unused_name(&self, base: &str) -> String {
        (1..)
            .map(|n| format!("{base} {n}"))
            .find(|name| !self.layers.iter().any(|l| &l.name == name))
            .expect("some number is free")
    }

    /// Rotate or flip the whole document: every layer's pixels (including groups
    /// and adjustment layers' masks), every layer mask, the selection (rebuilding outlines),
    /// and saved alpha channels. Rotating 90° swaps the document's width and height.
    pub fn apply_orientation(&mut self, orientation: crate::tiled::Orientation) {
        (self.width, self.height) = orientation.dimensions(self.width, self.height);
        for layer in &mut self.layers {
            layer.pixels = layer.pixels.oriented(orientation);
            if let Some(mask) = &mut layer.mask {
                mask.pixels = mask.pixels.oriented(orientation);
            }
        }
        if let Some(sel) = &self.selection {
            self.selection = Some(Selection::from_coverage(sel.coverage.oriented(orientation)));
        }
        for ch in &mut self.channels {
            ch.pixels = ch.pixels.oriented(orientation);
        }
    }

    /// Change the canvas to `width` × `height`, with the old top left corner
    /// at (`dx`, `dy`): Image › Canvas Size, and Crop with the offsets
    /// negative. Whatever falls outside is deleted. New areas are transparent
    /// (unselected in the selection and alpha channels, and as masks read
    /// by default), except on the bottom layer when `extension` gives them
    /// a colour, as Photoshop's Background layer.
    pub fn resize_canvas(&mut self, width: u32, height: u32, dx: i32, dy: i32, extension: Option<Pixel>) {
        (self.width, self.height) = (width, height);
        for (i, layer) in self.layers.iter_mut().enumerate() {
            let colour = extension.filter(|_| i == 0 && layer.has_pixels() && layer.parent.is_none());
            let uncovered = colour.unwrap_or(layer.pixels.fill());
            layer.pixels = layer.pixels.reframed(width, height, dx, dy, uncovered);
            if let Some(mask) = &mut layer.mask {
                mask.pixels = mask.pixels.reframed(width, height, dx, dy, mask.pixels.fill());
            }
        }
        if let Some(sel) = &self.selection {
            let coverage = sel.coverage.reframed(width, height, dx, dy, 0);
            self.selection = Some(Selection::from_coverage(coverage));
        }
        for ch in &mut self.channels {
            ch.pixels = ch.pixels.reframed(width, height, dx, dy, 0);
        }
    }

    /// Resample the whole document to `width` × `height` (Image › Image
    /// Size): every layer, mask, the selection and alpha channels.
    pub fn resize_image(&mut self, width: u32, height: u32) {
        use crate::transform::{Resampling::Bicubic, resized};
        (self.width, self.height) = (width, height);
        for layer in &mut self.layers {
            layer.pixels = resized(&layer.pixels, width, height, Bicubic);
            if let Some(mask) = &mut layer.mask {
                mask.pixels = resized(&mask.pixels, width, height, Bicubic);
            }
        }
        if let Some(sel) = &self.selection {
            self.selection = Some(Selection::from_coverage(resized(&sel.coverage, width, height, Bicubic)));
        }
        for ch in &mut self.channels {
            ch.pixels = resized(&ch.pixels, width, height, Bicubic);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_below_samples_layers_below_layer_id() {
        let (w, h) = (10, 10);
        let image = Raster::new(w, h, vec![[10000, 20000, 30000, 65535]; (w * h) as usize]);
        let mut doc = Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16);
        let top_layer = Layer::empty(2, "Top", w, h);
        doc.layers.push(top_layer);

        let sampled = doc.sample_below(2, 5, 5);
        assert_eq!(sampled, Some([10000, 20000, 30000, 65535]));

        // Out of bounds returns None
        assert_eq!(doc.sample_below(2, 10, 5), None);
        // Non-existent layer returns None
        assert_eq!(doc.sample_below(999, 5, 5), None);
    }

    #[test]
    fn saving_the_selection_makes_numbered_alpha_channels() {
        let image = Raster::new(8, 8, vec![[0, 0, 0, 65535]; 64]);
        let mut doc = Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16);
        assert_eq!(doc.save_selection(), None, "nothing selected");
        doc.selection = Some(Selection::rectangle(8, 8, (0.0, 0.0), (4.0, 8.0)));
        let first = doc.save_selection().unwrap();
        let second = doc.save_selection().unwrap();
        let names: Vec<_> = doc.channels.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["Alpha 1", "Alpha 2"]);
        assert_ne!(first, second);
        let saved = &doc.channel(first).unwrap().pixels;
        assert_eq!((saved.get(1, 1), saved.get(6, 1)), (65535, 0));
    }

    #[test]
    fn small_asymmetric_image_rotation_and_flips() {
        use crate::tiled::Orientation;
        let (w, h) = (3, 2);
        let pixels: Vec<Pixel> = vec![
            [10, 0, 0, 65535], [20, 0, 0, 65535], [30, 0, 0, 65535],
            [40, 0, 0, 65535], [50, 0, 0, 65535], [60, 0, 0, 65535],
        ];
        let image = Raster::new(w, h, pixels);
        let mut doc = Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16);
        let mask_vals: Vec<u16> = vec![100, 200, 300, 400, 500, 600];
        doc.layers[0].mask = Some(crate::layer::Mask {
            pixels: Tiled::from_slice(w, h, 0, &mask_vals),
            enabled: true,
        });
        let sel_vals: Vec<u16> = vec![1000, 2000, 3000, 4000, 5000, 6000];
        doc.selection = Some(Selection::from_coverage(Tiled::from_slice(w, h, 0, &sel_vals)));
        doc.channels.push(AlphaChannel {
            id: 99,
            name: "Alpha 1".into(),
            pixels: Tiled::from_slice(w, h, 0, &sel_vals),
        });

        let orientations = [
            (
                Orientation::Rotate180,
                3, 2,
                vec![60, 50, 40, 30, 20, 10],
            ),
            (
                Orientation::Rotate90Cw,
                2, 3,
                vec![40, 10, 50, 20, 60, 30],
            ),
            (
                Orientation::Rotate90Ccw,
                2, 3,
                vec![30, 60, 20, 50, 10, 40],
            ),
            (
                Orientation::FlipHorizontal,
                3, 2,
                vec![30, 20, 10, 60, 50, 40],
            ),
            (
                Orientation::FlipVertical,
                3, 2,
                vec![40, 50, 60, 10, 20, 30],
            ),
        ];

        for (orient, ew, eh, expected_r) in orientations {
            let mut d = doc.clone();
            d.apply_orientation(orient);
            assert_eq!((d.width, d.height), (ew, eh));
            for y in 0..eh {
                for x in 0..ew {
                    let idx = (y * ew + x) as usize;
                    let exp = expected_r[idx];
                    assert_eq!(d.layers[0].pixels.get(x, y)[0], exp);
                    assert_eq!(d.layers[0].mask.as_ref().unwrap().pixels.get(x, y), exp * 10);
                    assert_eq!(d.selection.as_ref().unwrap().coverage.get(x, y), exp * 100);
                    assert_eq!(d.channels[0].pixels.get(x, y), exp * 100);
                }
            }
        }
    }

    #[test]
    fn multi_tile_image_rotation_and_round_trip() {
        use crate::tiled::Orientation;
        let (w, h) = (600, 500);
        let px: Vec<Pixel> = (0..w * h)
            .map(|i| {
                let x = (i % w) as u16;
                let y = (i / w) as u16;
                [x, y, (x * 7 + y) % 65535, 65535]
            })
            .collect();
        let image = Raster::new(w, h, px);
        let mut doc = Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16);
        let mask_px: Vec<u16> = (0..w * h).map(|i| (i % 65521) as u16).collect();
        doc.layers[0].mask = Some(crate::layer::Mask {
            pixels: Tiled::from_slice(w, h, 0, &mask_px),
            enabled: true,
        });
        doc.selection = Some(Selection::rectangle(w, h, (100.0, 100.0), (400.0, 350.0)));
        doc.channels.push(AlphaChannel {
            id: 1,
            name: "Alpha 1".into(),
            pixels: Tiled::from_slice(w, h, 0, &mask_px),
        });

        for orient in [
            Orientation::Rotate180,
            Orientation::Rotate90Cw,
            Orientation::Rotate90Ccw,
            Orientation::FlipHorizontal,
            Orientation::FlipVertical,
        ] {
            let mut d = doc.clone();
            d.apply_orientation(orient);
            let (ew, eh) = orient.dimensions(w, h);
            assert_eq!((d.width, d.height), (ew, eh));
            for &(x, y) in &[(0, 0), (ew - 1, 0), (0, eh - 1), (ew - 1, eh - 1), (150, 200), (300, 400)] {
                let (sx, sy) = orient.source_coords(x, y, w, h);
                assert_eq!(d.layers[0].pixels.get(x, y), doc.layers[0].pixels.get(sx, sy));
                assert_eq!(
                    d.layers[0].mask.as_ref().unwrap().pixels.get(x, y),
                    doc.layers[0].mask.as_ref().unwrap().pixels.get(sx, sy)
                );
                assert_eq!(
                    d.selection.as_ref().unwrap().coverage.get(x, y),
                    doc.selection.as_ref().unwrap().coverage.get(sx, sy)
                );
                assert_eq!(
                    d.channels[0].pixels.get(x, y),
                    doc.channels[0].pixels.get(sx, sy)
                );
            }
            assert!(!d.selection.as_ref().unwrap().outlines.is_empty());
        }

        let mut d = doc.clone();
        for _ in 0..4 {
            d.apply_orientation(Orientation::Rotate90Cw);
        }
        assert_eq!((d.width, d.height), (w, h));
        assert_eq!(d.layers[0].pixels.to_vec(), doc.layers[0].pixels.to_vec());
        assert_eq!(
            d.layers[0].mask.as_ref().unwrap().pixels.to_vec(),
            doc.layers[0].mask.as_ref().unwrap().pixels.to_vec()
        );
        assert_eq!(
            d.selection.as_ref().unwrap().coverage.to_vec(),
            doc.selection.as_ref().unwrap().coverage.to_vec()
        );
        assert_eq!(
            d.channels[0].pixels.to_vec(),
            doc.channels[0].pixels.to_vec()
        );
    }

    #[test]
    fn canvas_size_extends_and_crops_everything_together() {
        let grey = [9000, 9000, 9000, 65535];
        let mut doc = Document::from_image("t.tif".into(), &Raster::new(300, 200, vec![grey; 60000]), ColorProfile::srgb(), 16);
        let mut top = Layer::empty(doc.next_layer_id(), "top", 300, 200);
        top.pixels.tile_mut(0, 0)[0] = [1, 2, 3, 65535];
        top.mask = Some(crate::layer::Mask::white(300, 200));
        doc.layers.push(top);
        doc.selection = Some(Selection::rectangle(300, 200, (0.0, 0.0), (10.0, 10.0)));
        doc.channels.push(AlphaChannel { id: 99, name: "Alpha 1".into(), pixels: Tiled::new(300, 200, 65535) });

        // 100 px more on the left and 50 on top, filled white on the bottom layer only.
        let mut big = doc.clone();
        big.resize_canvas(400, 250, 100, 50, Some([65535; 4]));
        assert_eq!((big.width, big.height), (400, 250));
        assert_eq!((big.layers[0].pixels.get(0, 0), big.layers[0].pixels.get(100, 50)), ([65535; 4], grey));
        assert_eq!((big.layers[1].pixels.get(0, 0)[3], big.layers[1].pixels.get(100, 50)), (0, [1, 2, 3, 65535]));
        assert_eq!(big.layers[1].mask.as_ref().unwrap().pixels.get(0, 0), 65535);
        let sel = &big.selection.as_ref().unwrap().coverage;
        assert_eq!((sel.get(105, 55), sel.get(5, 5)), (65535, 0));
        assert_eq!((big.channels[0].pixels.get(0, 0), big.channels[0].pixels.get(399, 249)), (0, 65535));

        // Cropping is the same with the offsets negative.
        let mut crop = doc.clone();
        crop.resize_canvas(50, 40, -1, -1, None);
        assert_eq!((crop.width, crop.height, crop.layers[1].pixels.get(0, 0)[3]), (50, 40, 0));
        assert_eq!(crop.layers[0].pixels.width(), 50);
    }

    #[test]
    fn image_size_resamples_everything_together() {
        let grey = [9000, 9000, 9000, 65535];
        let mut doc = Document::from_image("t.tif".into(), &Raster::new(300, 200, vec![grey; 60000]), ColorProfile::srgb(), 16);
        doc.layers[0].mask = Some(crate::layer::Mask::white(300, 200));
        doc.selection = Some(Selection::rectangle(300, 200, (0.0, 0.0), (150.0, 200.0)));
        doc.resize_image(150, 100);
        assert_eq!((doc.width, doc.height), (150, 100));
        assert_eq!(doc.layers[0].pixels.get(149, 99), grey);
        assert_eq!(doc.layers[0].mask.as_ref().unwrap().pixels.width(), 150);
        let sel = &doc.selection.as_ref().unwrap().coverage;
        assert_eq!((sel.get(10, 50), sel.get(140, 50)), (65535, 0));
    }

    #[test]
    fn auto_select_picks_the_topmost_layer_showing_pixels_there() {
        let mut doc = Document::from_image("t.tif".into(), &Raster::new(20, 10, vec![[9000; 4]; 200]), ColorProfile::srgb(), 16);
        let background = doc.layers[0].id;
        let mut top = Layer::empty(doc.next_layer_id(), "top", 20, 10);
        top.pixels.tile_mut(0, 0)[5] = [1, 2, 3, 65535];
        let top_id = top.id;
        doc.layers.push(top);
        assert_eq!((doc.layer_at(5, 0), doc.layer_at(6, 0)), (Some(top_id), Some(background)));
        doc.layer_mut(top_id).unwrap().visible = false;
        assert_eq!(doc.layer_at(5, 0), Some(background));
        doc.layer_mut(background).unwrap().visible = false;
        assert_eq!((doc.layer_at(5, 0), doc.layer_at(30, 0)), (None, None));
    }
}
