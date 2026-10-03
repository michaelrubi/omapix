//! FLUX.2 klein 4B (Black Forest Labs, Apache-2.0; hlhc's int4 ONNX
//! export), which makes an image from a prompt in four steps: Edit ›
//! Generative Fill. Four graphs: Qwen3 reads the prompt, the transformer
//! turns noise into the image's latents (a grid with one cell for each 16
//! pixels), and the VAE goes between latents and pixels. [`fill`] runs
//! them in turn; what it found out about them is in docs/AI.md
//! ("Generative Fill").

use std::sync::Mutex;

use ort::session::{RunOptions, Session};
use ort::value::Tensor;

use crate::Result;
use crate::find_model;
use crate::runtime::{cpu_session, gpu_session};
use crate::tokenizer::Tokenizer;

/// The model's id: its folder in Omapix's models
/// (`scripts/fetch-models.sh fill-flux2-klein-4b`).
pub const MODEL: &str = "fill-flux2-klein-4b";
/// A latent cell is this many pixels across.
pub const CELL: usize = 16;
/// Each cell holds this many numbers.
pub const CHANNELS: usize = 128;
/// The prompt is this many tokens, padded.
const TOKENS: usize = 512;
/// Each token's embedding is this long.
const EMBEDDING: usize = 7680;
/// The steps the model was distilled to.
pub const STEPS: usize = 4;
/// The most cells a fill is made of: with as many again for the image as
/// it was and the prompt's 512 tokens, about what fits in 8 GB.
pub const CELLS: usize = 768;
/// What an empty prompt asks for.
const REMOVE: &str = "Remove the object, leaving only the background";

fn file(name: &str) -> Result<std::path::PathBuf> {
    let files = find_model(MODEL)
        .ok_or_else(|| format!("Generative Fill needs the FLUX.2 klein model: run scripts/fetch-models.sh {MODEL}"))?;
    Ok(files[name].clone())
}

fn run(e: ort::Error) -> String {
    e.to_string()
}

/// For each run: on the GPU, what it needed while it ran is given back
/// afterwards rather than kept for the next, since the transformer needs
/// most of the card.
fn giving_back(gpu: bool) -> Result<RunOptions> {
    let mut options = RunOptions::new().map_err(run)?;
    if gpu {
        options.set("memory.enable_memory_arena_shrinkage", "gpu:0").map_err(run)?;
    }
    Ok(options)
}

/// Qwen3, which turns a prompt into what the transformer reads.
pub struct TextEncoder {
    session: Session,
    tokenizer: Tokenizer,
}

impl TextEncoder {
    pub fn load(gpu: bool) -> Result<Self> {
        let tokenizer = Tokenizer::load(&file("tokenizer.json")?)?;
        // The graph's weights are beside it, in text_encoder.onnx.data.
        file("text_encoder.onnx.data")?;
        let path = file("text_encoder.onnx")?;
        Ok(Self {
            session: if gpu { gpu_session(&path)? } else { cpu_session(&path)? },
            tokenizer,
        })
    }

    /// `prompt` as the transformer wants it.
    pub fn embed(&mut self, prompt: &str) -> Result<Vec<f32>> {
        // Qwen's chat template, with thinking off.
        let chat = format!("<|im_start|>user\n{prompt}<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n");
        let mut ids: Vec<i64> = self.tokenizer.ids(&chat).into_iter().map(i64::from).collect();
        ids.truncate(TOKENS);
        let mut mask = vec![1i64; ids.len()];
        let pad = self.tokenizer.id("<|endoftext|>").ok_or("The tokenizer has no padding token")?;
        ids.resize(TOKENS, i64::from(pad));
        mask.resize(TOKENS, 0);
        let shape = vec![1, TOKENS as i64];
        let ids = Tensor::from_array((shape.clone(), ids)).map_err(run)?;
        let mask = Tensor::from_array((shape, mask)).map_err(run)?;
        let outputs = self.session.run(ort::inputs!["input_ids" => ids, "attention_mask" => mask]).map_err(run)?;
        let (_, embedding) = outputs["prompt_embeds"].try_extract_tensor::<f32>().map_err(run)?;
        Ok(embedding.to_vec())
    }
}

/// The VAE: pixels to latents and back.
pub struct Vae {
    encoder: Session,
    decoder: Session,
    gpu: bool,
}

impl Vae {
    pub fn load(gpu: bool) -> Result<Self> {
        let open = |name: &str| if gpu { gpu_session(&file(name)?) } else { cpu_session(&file(name)?) };
        Ok(Self {
            encoder: open("vae_encoder.onnx")?,
            decoder: open("vae_decoder.onnx")?,
            gpu,
        })
    }

    /// `image` (sRGB 0–1, red, green then blue planes, `width` × `height`,
    /// both multiples of [`CELL`]) as latents: [`CHANNELS`] numbers for
    /// each cell, cell by cell in rows.
    pub fn encode(&mut self, image: &[f32], width: usize, height: usize) -> Result<Vec<f32>> {
        let signed: Vec<f32> = image.iter().map(|v| v * 2.0 - 1.0).collect();
        let input = Tensor::from_array((vec![1, 3, height as i64, width as i64], signed)).map_err(run)?;
        let options = giving_back(self.gpu)?;
        let outputs = self.encoder.run_with_options(ort::inputs!["image" => input], &options).map_err(run)?;
        let (_, planes) = outputs["latents"].try_extract_tensor::<f32>().map_err(run)?;
        let cells = (width / CELL) * (height / CELL);
        Ok((0..cells * CHANNELS).map(|i| planes[(i % CHANNELS) * cells + i / CHANNELS]).collect())
    }

    /// Latents `columns` × `rows` cells as an image, in [`Vae::encode`]'s
    /// form.
    pub fn decode(&mut self, latents: &[f32], columns: usize, rows: usize) -> Result<Vec<f32>> {
        let cells = columns * rows;
        let planes: Vec<f32> = (0..cells * CHANNELS).map(|i| latents[(i % cells) * CHANNELS + i / cells]).collect();
        let input = Tensor::from_array((vec![1, CHANNELS as i64, rows as i64, columns as i64], planes)).map_err(run)?;
        let options = giving_back(self.gpu)?;
        let outputs = self.decoder.run_with_options(ort::inputs!["latents" => input], &options).map_err(run)?;
        let (_, image) = outputs["image"].try_extract_tensor::<f32>().map_err(run)?;
        Ok(image.iter().map(|v| (v + 1.0) / 2.0).collect())
    }
}

/// What the transformer is asked to make.
pub struct Request<'a> {
    /// The prompt, from [`TextEncoder::embed`].
    pub prompt: &'a [f32],
    /// The image's size in cells.
    pub columns: usize,
    pub rows: usize,
    /// Different seeds give different images.
    pub seed: u64,
    /// Latents of an image to keep, and how much of each cell to make
    /// anew (1) rather than keep (0): the rest of the image stays as it is
    /// while the new part is made to fit it.
    pub keep: Option<(&'a [f32], &'a [f32])>,
    /// Latents of an image for the prompt to speak of ("remove the
    /// chair"), `columns` × `rows` cells too.
    pub reference: Option<&'a [f32]>,
}

/// The transformer, which makes latents from noise.
pub struct Transformer {
    session: Session,
}

impl Transformer {
    pub fn load() -> Result<Self> {
        file("transformer.onnx.data")?;
        Ok(Self {
            session: gpu_session(&file("transformer.onnx")?)?,
        })
    }

    /// The latents of an image for `request`, for [`Vae::decode`].
    /// `progress` hears each step as it starts, and stops it (with `None`
    /// for an answer) by answering `false`.
    pub fn generate(&mut self, request: &Request, mut progress: impl FnMut(usize) -> bool) -> Result<Option<Vec<f32>>> {
        let cells = request.columns * request.rows;
        let noise = noise(request.seed, cells * CHANNELS);
        let mut latents = noise.clone();
        let sigmas = sigmas(cells, STEPS);

        // Where each cell is: time (0 for the image, 10 for a reference),
        // row, column and 0; a prompt token only has its place in line.
        let place = |time: i64| (0..cells).flat_map(move |i| [time, (i / request.columns) as i64, (i % request.columns) as i64, 0]);
        let mut image_ids: Vec<i64> = place(0).collect();
        if request.reference.is_some() {
            image_ids.extend(place(10));
        }
        let all = image_ids.len() / 4;
        let text_ids: Vec<i64> = (0..TOKENS).flat_map(|i| [0, 0, 0, i as i64]).collect();

        let options = giving_back(true)?;
        for step in 0..STEPS {
            if !progress(step) {
                return Ok(None);
            }
            let (sigma, next) = (sigmas[step], sigmas[step + 1]);
            let mut hidden = latents.clone();
            hidden.extend(request.reference.unwrap_or_default());
            let outputs = self
                .session
                .run_with_options(
                    ort::inputs![
                    "hidden_states" => Tensor::from_array((vec![1, all as i64, CHANNELS as i64], hidden)).map_err(run)?,
                    "encoder_hidden_states" => Tensor::from_array((vec![1, TOKENS as i64, EMBEDDING as i64], request.prompt.to_vec())).map_err(run)?,
                    "timestep" => Tensor::from_array((vec![1], vec![sigma])).map_err(run)?,
                    "img_ids" => Tensor::from_array((vec![1, all as i64, 4], image_ids.clone())).map_err(run)?,
                    "txt_ids" => Tensor::from_array((vec![1, TOKENS as i64, 4], text_ids.clone())).map_err(run)?,
                    ],
                    &options,
                )
                .map_err(run)?;
            let (_, change) = outputs["noise_pred"].try_extract_tensor::<f32>().map_err(run)?;
            for (latent, change) in latents.iter_mut().zip(change) {
                *latent += (next - sigma) * change;
            }
            // What's kept is put back, as noisy as the rest still is.
            if let Some((kept, anew)) = request.keep {
                for (i, latent) in latents.iter_mut().enumerate() {
                    let known = (1.0 - next) * kept[i] + next * noise[i];
                    *latent = known + (*latent - known) * anew[i / CHANNELS];
                }
            }
        }
        Ok(Some(latents))
    }
}

/// What [`fill`] says as it goes.
pub enum Progress {
    /// Reading the prompt.
    Prompt,
    /// Loading the transformer.
    Loading,
    /// On this step (from 0, of [`STEPS`]) of this result (from 0).
    Step(usize, usize),
    /// A result, in `image`'s form.
    Result(Vec<f32>),
}

/// `image` (sRGB 0–1, red, green then blue planes, `width` × `height`,
/// both multiples of [`CELL`]) with the cells marked in `anew` (1 to make
/// anew, 0 to keep) changed as `prompt` asks, or with what's there removed
/// if it's empty: once for each of `seeds`. `tell` hears how it's going
/// and each result as it's ready, and stops it by answering `false`.
///
/// The transformer sees the image twice: as it is, for the prompt to
/// speak of, and with the kept cells held to it at every step, so what's
/// made fits what's round it. It needs the GPU nearly to itself, so the
/// prompt is read first and that model dropped, and the VAE runs on the
/// CPU, each result decoded while the next is made.
pub fn fill(
    prompt: &str,
    image: &[f32],
    width: usize,
    height: usize,
    anew: &[f32],
    seeds: &[u64],
    tell: impl FnMut(Progress) -> bool + Send,
) -> Result<()> {
    // One at a time: a fill told to stop may still be finishing its step,
    // with the card to itself.
    static ONE: Mutex<()> = Mutex::new(());
    let _one = ONE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let tell = Mutex::new(tell);
    let tell = |progress| tell.lock().is_ok_and(|mut tell| tell(progress));
    let (columns, rows) = (width / CELL, height / CELL);
    let prompt = if prompt.trim().is_empty() { REMOVE } else { prompt.trim() };

    // The last prompt's reading is kept: Generate again is usually the
    // same one.
    static READ: Mutex<Option<(String, Vec<f32>)>> = Mutex::new(None);
    let known = READ.lock().ok().and_then(|read| read.as_ref().filter(|(p, _)| p == prompt).map(|(_, e)| e.clone()));
    let embedding = match known {
        Some(embedding) => embedding,
        None => {
            if !tell(Progress::Prompt) {
                return Ok(());
            }
            let embedding = TextEncoder::load(true).and_then(|mut text| text.embed(prompt)).map_err(friendly)?;
            if let Ok(mut read) = READ.lock() {
                *read = Some((prompt.to_owned(), embedding.clone()));
            }
            embedding
        }
    };
    if !tell(Progress::Loading) {
        return Ok(());
    }
    let mut vae = Vae::load(false)?;
    let kept = vae.encode(image, width, height)?;
    let mut transformer = Transformer::load().map_err(friendly)?;

    std::thread::scope(|scope| {
        let (tx, rx) = std::sync::mpsc::channel::<Vec<f32>>();
        let tell = &tell;
        let decoding = scope.spawn(move || -> Result<()> {
            for latents in rx {
                if !tell(Progress::Result(vae.decode(&latents, columns, rows)?)) {
                    break;
                }
            }
            Ok(())
        });
        for (result, &seed) in seeds.iter().enumerate() {
            let request = Request {
                prompt: &embedding,
                columns,
                rows,
                seed,
                keep: Some((&kept, anew)),
                reference: Some(&kept),
            };
            let Some(latents) = transformer.generate(&request, |step| tell(Progress::Step(result, step))).map_err(friendly)? else {
                break;
            };
            if tx.send(latents).is_err() {
                break;
            }
        }
        drop(tx);
        decoding.join().map_err(|_| "Generative Fill stopped unexpectedly")?
    })
}

/// ONNX Runtime's errors that have a plain reason, in plain words.
fn friendly(error: String) -> String {
    if error.contains("Failed to allocate memory") {
        "Not enough free GPU memory for Generative Fill: it needs about 7 GB".into()
    } else if error.contains("on the GPU") {
        format!("Generative Fill needs an NVIDIA GPU. {error}")
    } else {
        error
    }
}

/// How noisy the image is before each of `steps` steps and after the last
/// (1 is all noise): even steps down, shifted towards noise by more the
/// larger the image, as the model was trained (Black Forest Labs'
/// `compute_empirical_mu`).
fn sigmas(cells: usize, steps: usize) -> Vec<f32> {
    let cells = cells as f64;
    let at_200 = 0.00016927 * cells + 0.45666666;
    let mu = if cells > 4300.0 {
        at_200
    } else {
        let at_10 = 8.73809524e-05 * cells + 1.89833333;
        let slope = (at_200 - at_10) / 190.0;
        slope * steps as f64 + at_200 - 200.0 * slope
    };
    (0..steps)
        .map(|i| 1.0 - i as f64 * (1.0 - 1.0 / steps as f64) / (steps - 1).max(1) as f64)
        .map(|s| (mu.exp() / (mu.exp() + (1.0 / s - 1.0))) as f32)
        .chain([0.0])
        .collect()
}

/// `count` numbers from a bell curve, the same for the same `seed`.
fn noise(seed: u64, count: usize) -> Vec<f32> {
    let mut state = seed;
    // SplitMix64, to a number in (0, 1].
    let mut uniform = move || {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        (((z ^ (z >> 31)) >> 11) as f64 + 1.0) / (1u64 << 53) as f64
    };
    // Box and Muller's.
    (0..count)
        .map(|_| ((-2.0 * uniform().ln()).sqrt() * (std::f64::consts::TAU * uniform()).cos()) as f32)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_steps_run_from_all_noise_to_none_and_larger_images_stay_noisy_longer() {
        let small = sigmas(32 * 32, STEPS);
        assert_eq!(small.len(), STEPS + 1);
        assert_eq!((small[0], small[STEPS]), (1.0, 0.0));
        assert!(small.windows(2).all(|w| w[0] > w[1]), "{small:?}");
        let large = sigmas(64 * 64, STEPS);
        assert!((1..STEPS).all(|i| large[i] > small[i]), "{large:?} {small:?}");
    }

    #[test]
    fn noise_is_a_bell_curve_and_the_same_for_the_same_seed() {
        let a = noise(7, 100_000);
        assert_eq!(a, noise(7, 100_000));
        assert_ne!(a[..8], noise(8, 8)[..]);
        let mean = a.iter().sum::<f32>() / a.len() as f32;
        let spread = (a.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / a.len() as f32).sqrt();
        assert!(mean.abs() < 0.02 && (spread - 1.0).abs() < 0.02, "{mean} {spread}");
    }
}
