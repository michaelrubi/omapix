//! Denoise on photos, timed, with 100 % crops of the largest face (or the
//! middle) before and after, side by side: `cargo run --release -p
//! omapix-ai --example denoise -- out photo.jpg…`. `DENOISE_AMOUNTS` sets
//! luminance and colour ("100,100" by default).
use std::time::Instant;

use omapix_ai::denoise::{Nind, SIZE};
use omapix_ai::face::{Faces, Image};
use omapix_engine::denoise::{self, Amounts};
use omapix_engine::DisplayTransform;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let out = std::path::Path::new(&args[1]);
    std::fs::create_dir_all(out)?;
    let amounts: Vec<f32> = std::env::var("DENOISE_AMOUNTS")
        .unwrap_or("100,100".into())
        .split(',')
        .map(|v| v.parse::<f32>().unwrap() / 100.0)
        .collect();
    let amounts = Amounts { luminance: amounts[0], color: amounts[1] };
    let t = Instant::now();
    let mut nind = Nind::load()?;
    let mut faces = Faces::load()?;
    println!("loaded in {:?}", t.elapsed());
    for path in &args[2..] {
        let doc = omapix_engine::io::load(std::path::Path::new(path))?;
        let image = doc.composite();
        let (w, h) = (image.width(), image.height());
        let t = Instant::now();
        let mut model = std::time::Duration::ZERO;
        let mut squares = 0;
        let denoised = denoise::denoise(&image, &doc.profile, SIZE, amounts, |s| {
            let t = Instant::now();
            let out = nind.denoise(s);
            model += t.elapsed();
            out
        }, |_, n| squares = n)?;
        println!("{path}: {w}×{h} in {:?}, {squares} squares, model {model:?}", t.elapsed());

        let to_srgb = DisplayTransform::to_srgb(&doc.profile)?;
        let srgb = |r: &omapix_engine::Raster| {
            let mut v = vec![[0u8; 4]; r.pixels().len()];
            to_srgb.convert(r.pixels(), &mut v);
            v
        };
        let (before, after) = (srgb(&image), srgb(&denoised));
        let mean = |v: &[[u8; 4]]| [0, 1, 2].map(|c| v.iter().map(|p| f64::from(p[c])).sum::<f64>() / v.len() as f64);
        let (b, a) = (mean(&before), mean(&after));
        println!("   mean sRGB before {b:.1?}, after {a:.1?}");
        let found = faces.detect(&Image { pixels: &before, width: w as usize, height: h as usize })?;
        let face = found.iter().max_by(|a, b| (a.bounds[2] - a.bounds[0]).total_cmp(&(b.bounds[2] - b.bounds[0])));
        let (cx, cy) = face.map_or((w / 2, h / 2), |f| (((f.bounds[0] + f.bounds[2]) / 2.0) as u32, ((f.bounds[1] + f.bounds[3]) / 2.0) as u32));
        let side = 600.min(w).min(h);
        let (x0, y0) = (cx.saturating_sub(side / 2).min(w - side), cy.saturating_sub(side / 2).min(h - side));
        let mut png = image::RgbImage::new(side * 2 + 8, side);
        for y in 0..side {
            for x in 0..side {
                let i = ((y0 + y) * w + x0 + x) as usize;
                png.put_pixel(x, y, image::Rgb([before[i][0], before[i][1], before[i][2]]));
                png.put_pixel(side + 8 + x, y, image::Rgb([after[i][0], after[i][1], after[i][2]]));
            }
        }
        let name = std::path::Path::new(path).file_stem().unwrap().to_string_lossy();
        png.save(out.join(format!("{name}.png")))?;
    }
    Ok(())
}
