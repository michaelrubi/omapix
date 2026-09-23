use std::path::PathBuf;

use crate::layer::Layer;
use crate::{ColorProfile, Raster};

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

    /// A name like "Layer 3" that isn't already taken.
    pub fn unused_name(&self, base: &str) -> String {
        (1..)
            .map(|n| format!("{base} {n}"))
            .find(|name| !self.layers.iter().any(|l| &l.name == name))
            .expect("some number is free")
    }
}
