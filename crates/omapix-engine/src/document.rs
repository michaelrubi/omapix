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
    pub width: u32,
    pub height: u32,
    pub profile: ColorProfile,
    /// Bits per channel in the file it was loaded from (pixels are always held at 16).
    pub source_bits: u8,
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
            selection: None,
            channels: Vec::new(),
            width,
            height,
            profile,
            source_bits,
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
}

