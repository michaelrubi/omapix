use std::path::PathBuf;

use crate::{ColorProfile, Raster};

/// An open image. Milestone 1 documents have a single raster; layers come
/// with the tile engine.
pub struct Document {
    pub path: PathBuf,
    pub raster: Raster,
    pub profile: ColorProfile,
    /// Bits per channel in the file it was loaded from (pixels are always held at 16).
    pub source_bits: u8,
}

impl Document {
    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled".into())
    }
}
