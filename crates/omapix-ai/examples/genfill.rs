//! FLUX.2 klein on its own, timed, with the GPU memory it takes
//! (docs/AI.md, "Generative Fill"): an image from a prompt, or Generative
//! Fill of a box in a photo, three results saved as the model made them
//! (`fill-0.png`…) and as the layer shows them (`shown-0.png`…).
//!
//! ```
//! cargo run --release -p omapix-ai --example genfill -- image out.png 512 "a red fox in snow"
//! cargo run --release -p omapix-ai --example genfill -- fill out photo.jpg x0 y0 x1 y1 "remove the person"
//! ```
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use omapix_ai::flux::{self, CELL, CELLS, Progress, Request, STEPS, TextEncoder, Transformer, Vae};
use omapix_engine::fill;
use omapix_engine::selection::Selection;

/// This process's GPU memory, in MiB.
fn gpu_mib() -> u64 {
    let out = std::process::Command::new("nvidia-smi")
        .args(["--query-compute-apps=pid,used_memory", "--format=csv,noheader,nounits"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let pid = std::process::id().to_string();
    out.lines()
        .filter_map(|l| l.split_once(", "))
        .filter(|(p, _)| *p == pid)
        .filter_map(|(_, m)| m.trim().parse::<u64>().ok())
        .sum()
}

/// This process's memory in RAM, in MiB.
fn rss_mib() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|l| l.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
        .map_or(0, |kb| kb / 1024)
}

/// Runs `work`, and says how long it took and the most GPU memory held
/// meanwhile.
fn timed<T>(what: &str, work: impl FnOnce() -> T) -> T {
    let (peak, done) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicBool::new(false)));
    let watcher = {
        let (peak, done) = (Arc::clone(&peak), Arc::clone(&done));
        std::thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                peak.fetch_max(gpu_mib(), Ordering::Relaxed);
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        })
    };
    let started = Instant::now();
    let result = work();
    let took = started.elapsed();
    done.store(true, Ordering::Relaxed);
    watcher.join().unwrap();
    let now = gpu_mib();
    println!("{what}: {took:.2?}; GPU {now} MiB now, {} at most; RAM {} MiB", peak.load(Ordering::Relaxed).max(now), rss_mib());
    result
}

fn save(path: &str, planes: &[f32], width: usize, height: usize) {
    let mut png = image::RgbImage::new(width as u32, height as u32);
    for (x, y, p) in png.enumerate_pixels_mut() {
        let i = y as usize * width + x as usize;
        *p = image::Rgb([0, 1, 2].map(|c| (planes[c * width * height + i].clamp(0.0, 1.0) * 255.0).round() as u8));
    }
    png.save(path).unwrap();
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let seed: u64 = std::env::var("SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(1);
    let prompt = args.last().unwrap().as_str();
    match args[1].as_str() {
        "image" => {
            let size: usize = args[3].parse()?;
            let cells = size / CELL;
            // One at a time: no two of them fit on an 8 GB card.
            let mut text = timed("text encoder loaded", || TextEncoder::load(true))?;
            let embedding = timed("prompt read", || text.embed(prompt))?;
            timed("text encoder dropped", || drop(text));
            let mut transformer = timed("transformer loaded", Transformer::load)?;
            let request = Request { prompt: &embedding, columns: cells, rows: cells, seed, keep: None, reference: None };
            let latents = timed(&format!("{size} px, {STEPS} steps"), || transformer.generate(&request, |_| true))?.unwrap();
            timed("transformer dropped", || drop(transformer));
            let mut vae = timed("VAE loaded", || Vae::load(true))?;
            let image = timed("decoded", || vae.decode(&latents, cells, cells))?;
            timed("VAE dropped", || drop(vae));
            save(&args[2], &image, size, size);
        }
        "fill" => {
            let out = &args[2];
            let doc = omapix_engine::io::load(std::path::Path::new(&args[3]))?;
            let visible = doc.composite();
            let corner: Vec<f32> = args[4..8].iter().map(|a| a.parse()).collect::<Result<_, _>>()?;
            let selection = Selection::rectangle(visible.width(), visible.height(), (corner[0], corner[1]), (corner[2], corner[3]));
            let patch = fill::patch_of_cells(&visible, &doc.profile, &selection, CELL, CELLS)?.ok_or("nothing selected")?;
            let (width, height) = (patch.width, patch.height);
            println!("patch {:?} at {width} x {height}", patch.rect);
            save(&format!("{out}/before.png"), &patch.image, width, height);
            let (started, mut results) = (Instant::now(), 0);
            timed("filled", || {
                flux::fill(prompt, &patch.image, width, height, &patch.cells(CELL), &[seed, seed + 1, seed + 2], |progress| {
                    match progress {
                        Progress::Prompt => println!("{:.1?}: reading the prompt", started.elapsed()),
                        Progress::Loading => println!("{:.1?}: loading", started.elapsed()),
                        Progress::Step(result, 0) => println!("{:.1?}: making result {result}", started.elapsed()),
                        Progress::Step(..) => {}
                        Progress::Result(image) => {
                            println!("{:.1?}: result {results}", started.elapsed());
                            save(&format!("{out}/fill-{results}.png"), &image, width, height);
                            // As the layer shows it: only where selected.
                            let shown: Vec<f32> = (0..image.len())
                                .map(|i| if patch.mask[i % (width * height)] > 0.0 { image[i] } else { patch.image[i] })
                                .collect();
                            save(&format!("{out}/shown-{results}.png"), &shown, width, height);
                            results += 1;
                        }
                    }
                    true
                })
            })?;
        }
        other => return Err(format!("image or fill, not {other}").into()),
    }
    Ok(())
}
