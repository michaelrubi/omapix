//! Omapix image engine: pixel storage, colour management, file IO and
//! processing. Has no UI or GPU dependencies so it can be tested headless.

pub mod adjust;
pub mod blend;
pub mod brush;
pub mod color;
pub mod composite;
pub mod document;
pub mod export;
pub mod filters;
pub mod io;
pub mod layer;
pub mod moving;
pub mod ops;
pub mod ora;
pub mod pyramid;
pub mod raster;
pub mod selection;
pub mod tiled;
pub mod tiles;

pub use blend::BlendMode;
pub use color::{ColorProfile, DisplayTransform};
pub use document::Document;
pub use layer::{Layer, Mask};
pub use raster::{Pixel, Raster};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not read {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("TIFF: {0}")]
    Tiff(#[from] tiff::TiffError),
    #[error("{0}")]
    Image(#[from] image::ImageError),
    #[error("colour management: {0}")]
    Color(#[from] lcms2::Error),
    #[error("unsupported image: {0}")]
    Unsupported(String),
}

pub type Result<T> = std::result::Result<T, Error>;
