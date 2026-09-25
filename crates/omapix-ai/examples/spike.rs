//! Object Selection end to end on a photo, timed, with the selection drawn
//! over a small copy: `cargo run --release -p omapix-ai --example spike --
//! photo.jpg x y out.png` (or `x0 y0 x1 y1` for a box).
use std::time::Instant;

use omapix_ai::sam::{MASK_SIZE, Prompt, Sam};
use omapix_engine::DisplayTransform;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let numbers: Vec<f32> = args[2..args.len() - 1].iter().map(|a| a.parse()).collect::<Result<_, _>>()?;
    let prompt = match numbers[..] {
        [x, y] => Prompt::Point(x, y),
        [x0, y0, x1, y1] => Prompt::Box(x0, y0, x1, y1),
        _ => return Err("give x y, or x0 y0 x1 y1".into()),
    };
    let doc = omapix_engine::io::load(std::path::Path::new(&args[1]))?;
    let image = doc.composite();
    let (w, h) = (image.width(), image.height());
    let mut srgb = vec![[0u8; 4]; image.pixels().len()];
    DisplayTransform::to_srgb(&doc.profile)?.convert(image.pixels(), &mut srgb);

    let t = Instant::now();
    let mut sam = Sam::load()?;
    println!("loaded in {:?}", t.elapsed());
    for _ in 0..2 {
        let t = Instant::now();
        let encoded = sam.encode(&srgb, w, h)?;
        let encode = t.elapsed();
        let t = Instant::now();
        let logits = sam.select(&encoded, &prompt, None)?;
        let select = t.elapsed();
        let t = Instant::now();
        let coverage = omapix_engine::refine::mask_coverage(&logits, MASK_SIZE, MASK_SIZE, 0.0, &image);
        println!("encode {encode:?}, select {select:?}, refine {:?}", t.elapsed());
        let (ow, oh) = (900u32, 900 * h / w);
        let mut img = image::RgbImage::new(ow, oh);
        for (x, y, pixel) in img.enumerate_pixels_mut() {
            let (sx, sy) = (x * w / ow, y * h / oh);
            let s = srgb[(sy * w + sx) as usize];
            let k = f32::from(coverage.get(sx, sy)) / 65535.0;
            *pixel = image::Rgb([0, 1, 2].map(|c| (f32::from(s[c]) * (1.0 - 0.5 * k) + [255.0, 0.0, 0.0][c] * 0.5 * k) as u8));
        }
        img.save(&args[args.len() - 1])?;
    }
    // Dropping CUDA sessions can crash the process as it exits (docs/AI.md).
    std::mem::forget(sam);
    Ok(())
}
