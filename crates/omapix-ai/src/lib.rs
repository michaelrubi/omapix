//! AI models for Omapix, run with the system's ONNX Runtime (docs/AI.md).
//! No UI: the app runs these on background threads.

mod runtime;
pub mod face;
pub mod lama;
pub mod models;
pub mod sam;
pub mod subject;

pub use models::find_model;
pub use runtime::on_gpu;

/// What went wrong, in words for the status bar.
pub type Result<T> = std::result::Result<T, String>;
