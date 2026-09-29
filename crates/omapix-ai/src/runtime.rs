//! ONNX Runtime, opened once from the system (`OMAPIX_ORT_LIBRARY`, or
//! `/usr/lib/libonnxruntime.so`), and sessions on the GPU when it can.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use ort::ep;
use ort::session::Session;
use rayon::prelude::*;

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
    let (session, gpu) = match open(true) {
        Ok(session) => {
            log::info!("{}: on the GPU", path.display());
            (session, true)
        }
        Err(e) => {
            log::info!("{}: on the CPU ({e})", path.display());
            (open(false).map_err(|e| format!("Couldn't load {}: {e}", path.display()))?, false)
        }
    };
    if let Ok(mut loaded) = LOADED.lock() {
        loaded.push((path.to_owned(), gpu));
    }
    Ok(session)
}

/// The models loaded so far, and whether each is on the GPU.
static LOADED: Mutex<Vec<(PathBuf, bool)>> = Mutex::new(Vec::new());

/// Whether the model file at `path` runs on the GPU (`Some(true)`) or the
/// CPU, once it's been loaded.
pub fn on_gpu(path: &Path) -> Option<bool> {
    LOADED.lock().ok()?.iter().find(|(p, _)| p == path).map(|&(_, gpu)| gpu)
}

/// ImageNet's mean and spread, which most image models were trained with.
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];

/// An image given as 8-bit sRGB, `width` × `height`, as a model trained on
/// ImageNet wants it: averaged down (or stretched) to `size` × `size`,
/// normalised, channels first.
pub(crate) fn imagenet_input(srgb: &[[u8; 4]], width: u32, height: u32, size: usize) -> Vec<f32> {
    let (w, h) = (width as usize, height as usize);
    let pixels: Vec<[f32; 3]> = (0..size * size)
        .into_par_iter()
        .map(|i| {
            let (x, y) = (i % size, i / size);
            let (x0, y0) = (x * w / size, y * h / size);
            let (x1, y1) = (((x + 1) * w / size).max(x0 + 1), ((y + 1) * h / size).max(y0 + 1));
            let mut sum = [0u32; 3];
            for row in srgb[y0 * w..y1 * w].chunks(w) {
                for p in &row[x0..x1] {
                    for c in 0..3 {
                        sum[c] += u32::from(p[c]);
                    }
                }
            }
            let n = ((x1 - x0) * (y1 - y0)) as f32 * 255.0;
            [0, 1, 2].map(|c| (sum[c] as f32 / n - MEAN[c]) / STD[c])
        })
        .collect();
    (0..3).flat_map(|c| pixels.iter().map(move |p| p[c])).collect()
}
