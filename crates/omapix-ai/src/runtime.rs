//! ONNX Runtime, opened once from the system (`OMAPIX_ORT_LIBRARY`, or
//! `/usr/lib/libonnxruntime.so`), and sessions on the GPU when it can.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use ort::ep;
use ort::session::Session;

use crate::Result;

/// Open ONNX Runtime, once.
fn init() -> Result<()> {
    static INIT: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| {
        let library = std::env::var("OMAPIX_ORT_LIBRARY").unwrap_or_else(|_| "/usr/lib/libonnxruntime.so".into());
        // Arch's CUDA provider isn't linked against cuDNN, so it has to be
        // loaded already, where the provider can see it. Without it, CUDA
        // just isn't used.
        #[cfg(unix)]
        {
            use libloading::os::unix::{Library, RTLD_GLOBAL, RTLD_NOW};
            // SAFETY: cuDNN's initialisers only set up its own state.
            if let Ok(cudnn) = unsafe { Library::open(Some("libcudnn.so.9"), RTLD_NOW | RTLD_GLOBAL) } {
                std::mem::forget(cudnn);
            }
        }
        let builder = ort::init_from(&library).map_err(|e| format!("Couldn't open ONNX Runtime ({library}): {e}"))?;
        builder.with_name("omapix").commit();
        Ok(())
    })
    .clone()
}

/// A session for the model in `path`: on the GPU with CUDA if it works,
/// otherwise on the CPU.
pub(crate) fn session(path: &Path) -> Result<Session> {
    init()?;
    let open = |cuda: bool| -> ort::Result<Session> {
        let builder = Session::builder()?;
        let mut builder = if cuda {
            builder.with_execution_providers([ep::CUDA::default().build().error_on_failure()])?
        } else {
            builder
        };
        builder.commit_from_file(path)
    };
    open(true)
        .inspect(|_| log::info!("{}: on the GPU", path.display()))
        .or_else(|e| {
            log::info!("{}: on the CPU ({e})", path.display());
            open(false)
        })
        .map_err(|e| format!("Couldn't load {}: {e}", path.display()))
}

/// The folder of model `id` (a darktable-style id, such as
/// "mask-object-sam21-small"): Omapix's own models, then darktable's.
pub fn find_model(id: &str) -> Option<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    ["omapix/models", "darktable/models"]
        .iter()
        .map(|dir| data.join(dir).join(id))
        .find(|dir| dir.join("config.json").exists())
}
