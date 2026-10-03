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
    match open(path, Some(cuda()), true) {
        Ok(session) => Ok(loaded(path, session, true)),
        Err(e) => {
            log::info!("{}: not on the GPU ({e})", path.display());
            cpu_session(path)
        }
    }
}

/// A session for the model in `path` on the GPU, or nothing: for models
/// that would take minutes on the CPU. These nearly fill the card, so a
/// run's memory isn't planned as one block from the run before.
pub(crate) fn gpu_session(path: &Path) -> Result<Session> {
    init()?;
    let session = open(path, Some(cuda()), false).map_err(|e| format!("Couldn't load {} on the GPU: {e}", path.display()))?;
    Ok(loaded(path, session, true))
}

/// How sessions use the GPU: its memory grows by what's asked for rather
/// than doubling, and convolutions aren't each tried every way first.
/// Either is easier on the card's memory, and with either, dropping a
/// session no longer crashes Omapix as it quits (docs/AI.md, Runtime).
fn cuda() -> ep::CUDA {
    ep::CUDA::default()
        .with_arena_extend_strategy(ep::ArenaExtendStrategy::SameAsRequested)
        .with_conv_algorithm_search(ep::cuda::ConvAlgorithmSearch::Heuristic)
}

/// A session for the model in `path` on the CPU.
pub(crate) fn cpu_session(path: &Path) -> Result<Session> {
    init()?;
    let session = open(path, None, true).map_err(|e| format!("Couldn't load {}: {e}", path.display()))?;
    Ok(loaded(path, session, false))
}

/// With `cuda`, on the GPU or not at all. `planned`: each run's memory
/// is laid out as one block from the run before, which is quicker and
/// takes more.
fn open(path: &Path, cuda: Option<ep::CUDA>, planned: bool) -> ort::Result<Session> {
    let builder = Session::builder()?.with_memory_pattern(planned)?;
    let mut builder = match cuda {
        Some(cuda) => builder.with_execution_providers([cuda.build().error_on_failure()])?,
        None => builder,
    };
    builder.commit_from_file(path)
}

fn loaded(path: &Path, session: Session, gpu: bool) -> Session {
    log::info!("{}: on the {}", path.display(), if gpu { "GPU" } else { "CPU" });
    if let Ok(mut loaded) = LOADED.lock() {
        loaded.push((path.to_owned(), gpu));
    }
    session
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
