//! AI models for Omapix, run with the system's ONNX Runtime (docs/AI.md).
//! No UI: the app runs these on background threads.

mod runtime;
pub mod face;
pub mod lama;
pub mod sam;

pub use runtime::find_model;

/// What went wrong, in words for the status bar.
pub type Result<T> = std::result::Result<T, String>;
