//! A spike for Regenerate Skin and an upscale that adds detail: FLUX.2
//! klein redrawing a photo from a little noise, in overlapping tiles, so
//! what's there stays and fine detail (pores, lashes) is made anew. Saves
//! `<photo>-before.png` and `<photo>-<strength>.png` for each strength.
//!
//! ```
//! cargo run --release -p omapix-ai --example regen -- out photo.jpg…
//! ```
//!
//! `STRENGTHS` (0.2,0.35,0.5), `PROMPT`, `REF` (1: the transformer also
//! sees the tile as it was), `TILE` (px), `SCALE` (1; 2 or 4 enlarges
//! with RealPLKSR first), `SEED`, `VAE_GPU` (1), `DETAIL` (a blur's σ in
//! px: also saves `<photo>-<strength>-detail.png`, the photo's colour and
//! shape with only the finer detail from the model's result).
use std::path::Path;
use std::time::Instant;

use omapix_ai::flux::{CELL, Request, TextEncoder, Transformer, Vae};
use omapix_ai::upscale::Upscaler;
use omapix_engine::{DisplayTransform, upscale};

const PROMPT: &str = "A high-resolution photograph, sharp focus, natural realistic skin texture with fine pores";
const OVERLAP: usize = 64;

fn save(path: &Path, planes: &[f32], width: usize, height: usize) {
    let mut png = image::RgbImage::new(width as u32, height as u32);
    for (x, y, p) in png.enumerate_pixels_mut() {
        let i = y as usize * width + x as usize;
        *p = image::Rgb([0, 1, 2].map(|c| (planes[c * width * height + i].clamp(0.0, 1.0) * 255.0).round() as u8));
    }
    png.save(path).unwrap();
}

/// Where tiles `size` long start along `length`, overlapping by OVERLAP.
fn starts(length: usize, size: usize) -> Vec<usize> {
    let n = (length - OVERLAP).div_ceil(size - OVERLAP);
    (0..n).map(|i| (i * (size - OVERLAP)).min(length - size)).collect()
}

/// How much a tile's pixel counts, fading out over OVERLAP to its edges.
fn weight(i: usize, size: usize) -> f32 {
    ((i + 1).min(size - i) as f32 / OVERLAP as f32).min(1.0)
}

/// `planes` (3 × `w` × `h`) blurred by a Gaussian of `sigma` px.
fn blur(planes: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    let r = (sigma * 3.0).ceil() as isize;
    let kernel: Vec<f32> = (-r..=r).map(|i| (-(i * i) as f32 / (2.0 * sigma * sigma)).exp()).collect();
    let total: f32 = kernel.iter().sum();
    let pass = |src: &[f32], horizontal: bool| -> Vec<f32> {
        let mut dst = vec![0f32; src.len()];
        for c in 0..3 {
            for y in 0..h {
                for x in 0..w {
                    let mut sum = 0.0;
                    for (k, weight) in kernel.iter().enumerate() {
                        let d = k as isize - r;
                        let (sx, sy) = if horizontal {
                            ((x as isize + d).clamp(0, w as isize - 1) as usize, y)
                        } else {
                            (x, (y as isize + d).clamp(0, h as isize - 1) as usize)
                        };
                        sum += weight * src[c * w * h + sy * w + sx];
                    }
                    dst[c * w * h + y * w + x] = sum / total;
                }
            }
        }
        dst
    };
    pass(&pass(planes, true), false)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let out = Path::new(&args[1]);
    std::fs::create_dir_all(out)?;
    let env = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_owned());
    let strengths: Vec<f32> = env("STRENGTHS", "0.2,0.35,0.5").split(',').map(str::parse).collect::<Result<_, _>>()?;
    let prompt = env("PROMPT", PROMPT);
    let reference = env("REF", "1") == "1";
    let tile: usize = env("TILE", if reference { "432" } else { "624" }).parse::<usize>()? / CELL * CELL;
    let scale: u32 = env("SCALE", "1").parse()?;
    let seed: u64 = env("SEED", "1").parse()?;
    let vae_gpu = env("VAE_GPU", "1") == "1";
    let detail: f32 = env("DETAIL", "0").parse()?;

    let mut photos = Vec::new();
    for path in &args[2..] {
        let doc = omapix_engine::io::load(Path::new(path))?;
        let mut image = doc.composite();
        if scale > 1 {
            let t = Instant::now();
            let mut up = Upscaler::load(if scale <= 2 { 2 } else { 4 })?;
            let square = (up.size, up.factor);
            let size = (image.width() * scale, image.height() * scale);
            image = upscale::upscale(&image, &doc.profile, size, square, |s| up.upscale(s), |_, _| ())?;
            println!("{path}: enlarged {scale}x in {:.1?}", t.elapsed());
        }
        let (w, h) = (image.width() as usize, image.height() as usize);
        let mut srgb = vec![[0u8; 4]; w * h];
        DisplayTransform::to_srgb(&doc.profile)?.convert(image.pixels(), &mut srgb);
        let mut planes = vec![0f32; 3 * w * h];
        for (i, p) in srgb.iter().enumerate() {
            for c in 0..3 {
                planes[c * w * h + i] = f32::from(p[c]) / 255.0;
            }
        }
        let name = Path::new(path).file_stem().unwrap().to_string_lossy().into_owned();
        save(&out.join(format!("{name}-before.png")), &planes, w, h);
        photos.push((name, planes, w, h));
    }

    let t = Instant::now();
    let embedding = TextEncoder::load(true)?.embed(&prompt)?;
    let mut transformer = Transformer::load()?;
    let mut vae = Vae::load(vae_gpu)?;
    println!("loaded in {:.1?}; tiles {tile} px, reference {reference}", t.elapsed());

    for (name, planes, w, h) in &photos {
        let (w, h) = (*w, *h);
        let (tw, th) = (tile.min(w / CELL * CELL), tile.min(h / CELL * CELL));
        let (columns, rows) = (tw / CELL, th / CELL);
        let shown = vec![1.0; columns * rows];
        let mut sums: Vec<(Vec<f32>, Vec<f32>)> = strengths.iter().map(|_| (vec![0.0; 3 * w * h], vec![0.0; w * h])).collect();
        let t = Instant::now();
        let (xs, ys) = (starts(w, tw), starts(h, th));
        for &y0 in &ys {
            for &x0 in &xs {
                let mut crop = vec![0f32; 3 * tw * th];
                for c in 0..3 {
                    for y in 0..th {
                        for x in 0..tw {
                            crop[c * tw * th + y * tw + x] = planes[c * w * h + (y0 + y) * w + x0 + x];
                        }
                    }
                }
                let latents = vae.encode(&crop, tw, th)?;
                for (k, &strength) in strengths.iter().enumerate() {
                    let request = Request {
                        prompt: &embedding,
                        columns,
                        rows,
                        seed,
                        keep: None,
                        reference: reference.then_some((&latents[..], &shown[..])),
                        from: Some((&latents, strength)),
                    };
                    let made = transformer.generate(&request, |_| true)?.unwrap();
                    let image = vae.decode(&made, columns, rows)?;
                    let (sum, weights) = &mut sums[k];
                    for y in 0..th {
                        for x in 0..tw {
                            let wt = weight(x, tw) * weight(y, th);
                            let i = (y0 + y) * w + x0 + x;
                            weights[i] += wt;
                            for c in 0..3 {
                                sum[c * w * h + i] += wt * image[c * tw * th + y * tw + x];
                            }
                        }
                    }
                }
            }
        }
        println!(
            "{name}: {w}x{h}, {} tiles x {} strengths in {:.1?}",
            xs.len() * ys.len(),
            strengths.len(),
            t.elapsed()
        );
        for (k, (sum, weights)) in sums.iter().enumerate() {
            let image: Vec<f32> = sum.iter().enumerate().map(|(i, v)| v / weights[i % (w * h)]).collect();
            save(&out.join(format!("{name}-{:.2}.png", strengths[k])), &image, w, h);
            if detail > 0.0 {
                let (low, made_low) = (blur(planes, w, h, detail), blur(&image, w, h, detail));
                let kept: Vec<f32> = (0..image.len()).map(|i| low[i] + image[i] - made_low[i]).collect();
                save(&out.join(format!("{name}-{:.2}-detail.png", strengths[k])), &kept, w, h);
            }
        }
    }
    Ok(())
}
