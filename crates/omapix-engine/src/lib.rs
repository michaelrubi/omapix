//! Omapix image engine: pixel storage, colour management, file IO and
//! processing. Has no UI or GPU dependencies so it can be tested headless.

pub mod adjust;
pub mod blend;
pub mod brush;
pub mod clip;
pub mod color;
pub mod composite;
pub mod document;
pub mod export;
pub mod filters;
pub mod groups;
pub mod histogram;
pub mod io;
pub mod layer;
pub mod moving;
pub mod ops;
pub mod ora;
pub mod psd;
pub mod pyramid;
pub mod raster;
pub mod refine;
pub mod reduced;
pub mod selection;
pub mod tiled;
pub mod tiles;
pub mod transform;

pub use blend::BlendMode;
pub use clip::PasteKind;
pub use color::{ColorProfile, DisplayTransform};
pub use document::{AlphaChannel, Document};
pub use filters::{
    generate_grain, NoiseDistribution, NoiseOptions, ReduceNoiseOptions, SharpenRemove,
    SmartBlurMode, SmartBlurOptions, SmartBlurQuality, SmartSharpenOptions,
};
pub use histogram::{Histogram, HistogramStats};
pub use layer::{Layer, Locks, Mask};
pub use ops::{add_noise_layer, grain_layer};
pub use raster::{Pixel, Raster};
pub use selection::{Channel, Combine, Selection};

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
