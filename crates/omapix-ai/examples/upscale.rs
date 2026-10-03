//! AI upscaling on photos, timed, with crops at the new size of the
//! largest face (or the middle), enlarged the ordinary way (bicubic) and by
//! the model, side by side: `cargo run --release -p omapix-ai --example
//! upscale -- out photo.jpg…`. `UPSCALE` sets how many times larger (2 by
//! default; over 2 uses the ×4 model).
use std::time::Instant;

use omapix_ai::face::{Faces, Image};
use omapix_ai::upscale::Upscaler;
use omapix_engine::transform::{Resampling::Bicubic, resized};
use omapix_engine::tiled::Tiled;
use omapix_engine::{DisplayTransform, upscale};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let out = std::path::Path::new(&args[1]);
    std::fs::create_dir_all(out)?;
    let scale: f64 = std::env::var("UPSCALE").map_or(2.0, |v| v.parse().unwrap());
    let t = Instant::now();
    let mut upscaler = Upscaler::load(if scale <= 2.0 { 2 } else { 4 })?;
    let mut faces = Faces::load()?;
    let file = &omapix_ai::find_model(omapix_ai::upscale::MODEL).unwrap()[if upscaler.factor == 2 { "model_x2.onnx" } else { "model_x4.onnx" }];
    println!("loaded in {:?}, on the {}", t.elapsed(), if omapix_ai::on_gpu(file) == Some(true) { "GPU" } else { "CPU" });
    for path in &args[2..] {
        let doc = omapix_engine::io::load(std::path::Path::new(path))?;
        let image = doc.composite();
        let (w, h) = (image.width(), image.height());
        let size = ((f64::from(w) * scale).round() as u32, (f64::from(h) * scale).round() as u32);
        let t = Instant::now();
        let mut model = std::time::Duration::ZERO;
        let mut squares = 0;
        let enlarged = upscale::upscale(&image, &doc.profile, size, (upscaler.size, upscaler.factor), |s| {
            let t = Instant::now();
            let out = upscaler.upscale(s);
            model += t.elapsed();
            out
        }, |_, n| squares = n)?;
        println!("{path}: {w}×{h} to {}×{} in {:?}, {squares} squares, model {model:?}", size.0, size.1, t.elapsed());
        let plain = resized(&Tiled::from_raster(&image), size.0, size.1, Bicubic).to_raster();

        let to_srgb = DisplayTransform::to_srgb(&doc.profile)?;
        let srgb = |r: &omapix_engine::Raster| {
            let mut v = vec![[0u8; 4]; r.pixels().len()];
            to_srgb.convert(r.pixels(), &mut v);
            v
        };
        let (before, after) = (srgb(&plain), srgb(&enlarged));
        let mean = |v: &[[u8; 4]]| [0, 1, 2].map(|c| v.iter().map(|p| f64::from(p[c])).sum::<f64>() / v.len() as f64);
        let (b, a) = (mean(&before), mean(&after));
        println!("   mean sRGB bicubic {b:.2?}, model {a:.2?}");
        let small = srgb(&image);
        let found = faces.detect(&Image { pixels: &small, width: w as usize, height: h as usize })?;
        let face = found.iter().max_by(|a, b| (a.bounds[2] - a.bounds[0]).total_cmp(&(b.bounds[2] - b.bounds[0])));
        let (cx, cy) = face.map_or((0.5, 0.5), |f| ((f.bounds[0] + f.bounds[2]) / 2.0 / w as f32, (f.bounds[1] + f.bounds[3]) / 2.0 / h as f32));
        let (cx, cy) = ((cx * size.0 as f32) as u32, (cy * size.1 as f32) as u32);
        let side = 700.min(size.0).min(size.1);
        let (x0, y0) = (cx.saturating_sub(side / 2).min(size.0 - side), cy.saturating_sub(side / 2).min(size.1 - side));
        let mut png = image::RgbImage::new(side * 2 + 8, side);
        for y in 0..side {
            for x in 0..side {
                let i = ((y0 + y) * size.0 + x0 + x) as usize;
                png.put_pixel(x, y, image::Rgb([before[i][0], before[i][1], before[i][2]]));
                png.put_pixel(side + 8 + x, y, image::Rgb([after[i][0], after[i][1], after[i][2]]));
            }
        }
        let name = std::path::Path::new(path).file_stem().unwrap().to_string_lossy();
        png.save(out.join(format!("{name}.png")))?;
    }
    Ok(())
}
