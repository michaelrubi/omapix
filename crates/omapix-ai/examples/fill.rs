//! Content-Aware Fill both ways on a photo, timed: the box from (x0, y0)
//! to (x1, y1) as it was, filled by LaMa and filled by copying texture,
//! side by side at full size: `cargo run --release -p omapix-ai --example
//! fill -- out photo.jpg x0 y0 x1 y1`.
use std::time::Instant;

use omapix_ai::lama::{Lama, SIZE};
use omapix_engine::{DisplayTransform, Layer, Raster, Selection, fill};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let out = std::path::Path::new(&args[1]);
    std::fs::create_dir_all(out)?;
    let doc = omapix_engine::io::load(std::path::Path::new(&args[2]))?;
    let image = doc.composite();
    let [x0, y0, x1, y1] = [3, 4, 5, 6].map(|i| args[i].parse::<f32>().unwrap());
    let selection = Selection::rectangle(image.width(), image.height(), (x0, y0), (x1, y1));

    let mut lama = Lama::load()?;
    let patch = fill::patch(&image, &doc.profile, &selection, SIZE)?.unwrap();
    lama.fill(&patch.image, &patch.mask)?;
    let t = Instant::now();
    let filled = lama.fill(&patch.image, &patch.mask)?;
    let model = fill::layer(0, "Content-Aware Fill", &patch, &filled, &doc.profile, &selection)?;
    let scale = patch.rect[2] as f32 / SIZE as f32;
    println!("LaMa: {:?}, each of its pixels {scale:.2} of the photo's", t.elapsed());
    let t = Instant::now();
    let copied = fill::copied(0, "Content-Aware Fill", &image, &selection).unwrap();
    println!("copied: {:?}", t.elapsed());

    // The box and half as much again round it, three times over.
    let (w, h) = (x1 - x0, y1 - y0);
    let (cx, cy) = ((x0 - w / 2.0).max(0.0) as u32, (y0 - h / 2.0).max(0.0) as u32);
    let (cw, ch) = (((w * 2.0) as u32).min(image.width() - cx), ((h * 2.0) as u32).min(image.height() - cy));
    let to_srgb = DisplayTransform::to_srgb(&doc.profile)?;
    let mut png = image::RgbImage::new(cw * 3 + 16, ch);
    for (n, layer) in [None, Some(&model), Some(&copied)].into_iter().enumerate() {
        let shown = with(&image, layer, [cx, cy, cw, ch]);
        let mut srgb = vec![[0u8; 4]; shown.len()];
        to_srgb.convert(&shown, &mut srgb);
        for (i, p) in srgb.iter().enumerate() {
            png.put_pixel(n as u32 * (cw + 8) + i as u32 % cw, i as u32 / cw, image::Rgb([p[0], p[1], p[2]]));
        }
    }
    let name = std::path::Path::new(&args[2]).file_stem().unwrap().to_string_lossy().into_owned();
    png.save(out.join(format!("{name}-{x0}-{y0}.png")))?;
    Ok(())
}

/// `rect` of `image` with `layer` over it, through its mask.
fn with(image: &Raster, layer: Option<&Layer>, [x, y, w, h]: [u32; 4]) -> Vec<[u16; 4]> {
    (0..w * h)
        .map(|i| {
            let (px, py) = (x + i % w, y + i / w);
            let under = image.get(px, py);
            let Some(layer) = layer else { return under };
            let over = layer.pixels.get(px, py);
            let k = f32::from(layer.mask.as_ref().unwrap().pixels.get(px, py)) / 65535.0 * f32::from(over[3]) / 65535.0;
            [0, 1, 2, 3].map(|c| (f32::from(under[c]) + (f32::from(over[c]) - f32::from(under[c])) * k).round() as u16)
        })
        .collect()
}
