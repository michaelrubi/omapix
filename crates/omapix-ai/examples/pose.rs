//! Body poses in photos, timed, drawn into `out/<photo>.png`: `cargo run
//! --release -p omapix-ai --example pose -- out photo.jpg…`. Each person's
//! matte is tinted, their crop is boxed, and their points are joined up
//! (their own left in green, right in red; faint where out of view). With
//! `SHAPE=waist=-100,legs=-50` (Body Reshape's sliders), `out/<photo>-shaped.png`
//! is the photo before and after.
use std::time::Instant;

use omapix_ai::face::Image;
use omapix_ai::pose::{Pose, Poses, SIZE};
use omapix_engine::DisplayTransform;
use omapix_engine::body::{Shape, reshape};
use omapix_engine::tiled::Tiled;
use omapix_engine::warp::{Field, warped};

/// The points joined up, and whether each bone is on the person's left.
const BONES: [(usize, usize, bool); 14] = [
    (11, 13, true),
    (13, 15, true),
    (12, 14, false),
    (14, 16, false),
    (11, 23, true),
    (12, 24, false),
    (23, 25, true),
    (25, 27, true),
    (24, 26, false),
    (26, 28, false),
    (27, 31, true),
    (28, 32, false),
    (11, 12, true),
    (23, 24, false),
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let out = std::path::Path::new(&args[1]);
    std::fs::create_dir_all(out)?;
    let t = Instant::now();
    let mut poses = Poses::load()?;
    println!("loaded in {:?}", t.elapsed());
    for path in &args[2..] {
        let path = std::path::Path::new(path);
        let doc = omapix_engine::io::load(path)?;
        let raster = doc.composite();
        let (w, h) = (raster.width() as usize, raster.height() as usize);
        let mut srgb = vec![[0u8; 4]; raster.pixels().len()];
        DisplayTransform::to_srgb(&doc.profile)?.convert(raster.pixels(), &mut srgb);
        let image = Image { pixels: &srgb, width: w, height: h };
        let t = Instant::now();
        let crops = poses.detect(&image)?;
        let detect = t.elapsed();
        let t = Instant::now();
        let found = poses.find(&image)?;
        println!("{}: {w}×{h}, {} detected in {detect:?}, {} found in {:?}", path.display(), crops.len(), found.len(), t.elapsed());

        let scale = w.max(h) as f32 / 1400.0;
        let (ow, oh) = ((w as f32 / scale) as u32, (h as f32 / scale) as u32);
        let mut img = image::RgbImage::new(ow, oh);
        for (x, y, pixel) in img.enumerate_pixels_mut() {
            let at = [(x as f32 + 0.5) * scale, (y as f32 + 0.5) * scale];
            let s = srgb[at[1] as usize * w + at[0] as usize];
            let k = found.iter().map(|pose| matte(pose, at)).fold(0.0, f32::max) * 0.4;
            *pixel = image::Rgb([0, 1, 2].map(|c| (f32::from(s[c]) * (1.0 - k) + [0.0, 120.0, 255.0][c] * k) as u8));
        }
        for pose in &found {
            let corners = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]].map(|[u, v]| pose.crop.to_image(u, v));
            for k in 0..4 {
                line(&mut img, corners[k], corners[(k + 1) % 4], scale, [255, 255, 0], 0);
            }
            for (a, b, left) in BONES {
                let (a, b) = (pose.points[a], pose.points[b]);
                let seen = a[3].min(b[3]) > 0.5;
                let colour = match (left, seen) {
                    (true, true) => [0, 255, 0],
                    (true, false) => [0, 90, 0],
                    (false, true) => [255, 0, 0],
                    (false, false) => [90, 0, 0],
                };
                line(&mut img, [a[0], a[1]], [b[0], b[1]], scale, colour, 1);
            }
            for p in &pose.points {
                dot(&mut img, [p[0] / scale, p[1] / scale], if p[3] > 0.5 { [255, 255, 255] } else { [90, 90, 90] }, 2);
            }
        }
        img.save(out.join(path.file_stem().unwrap()).with_extension("png"))?;

        // With `SHAPE` (as `waist=-100,legs=-50`), everyone given it: before
        // and after, side by side.
        let Ok(sliders) = std::env::var("SHAPE") else { continue };
        let mut shape = Shape::default();
        for slider in sliders.split(',') {
            let (name, value) = slider.split_once('=').ok_or("SHAPE is name=value,…")?;
            *match name {
                "level" => &mut shape.level,
                "head" => &mut shape.head,
                "neck" => &mut shape.neck,
                "shoulders" => &mut shape.shoulders,
                "waist" => &mut shape.waist,
                "hips" => &mut shape.hips,
                "arms" => &mut shape.arms,
                "legs" => &mut shape.legs,
                "leg_length" => &mut shape.leg_length,
                _ => return Err(format!("no slider called {name}").into()),
            } = value.parse()?;
        }
        let t = Instant::now();
        let bodies: Vec<_> = found.iter().filter_map(|pose| pose.body(&image)).collect();
        let lines = t.elapsed();
        let t = Instant::now();
        let mut field = Field::new(w as u32, h as u32);
        for body in &bodies {
            reshape(&mut field, body, &shape, 0);
        }
        let made = t.elapsed();
        let t = Instant::now();
        let before = Tiled::from_slice(w as u32, h as u32, [0; 4], raster.pixels());
        let mut after = vec![[0u8; 4]; srgb.len()];
        DisplayTransform::to_srgb(&doc.profile)?.convert(&warped(&before, &field).to_vec(), &mut after);
        println!("  {sliders}: the lines behind in {lines:?}, the warp in {made:?}, the image warped in {:?}", t.elapsed());
        let mut img = image::RgbImage::new(ow * 2, oh);
        for (x, y, pixel) in img.enumerate_pixels_mut() {
            let from = if x < ow { &srgb } else { &after };
            let s = from[((y as f32 + 0.5) * scale) as usize * w + (((x % ow) as f32 + 0.5) * scale) as usize];
            *pixel = image::Rgb([s[0], s[1], s[2]]);
        }
        img.save(out.join(format!("{}-shaped.png", path.file_stem().unwrap().to_string_lossy())))?;
    }
    Ok(())
}

/// How much of the image at `at` is `pose`'s person.
fn matte(pose: &Pose, at: [f32; 2]) -> f32 {
    let crop = &pose.crop;
    let (dx, dy) = (at[0] - crop.centre[0], at[1] - crop.centre[1]);
    let (sin, cos) = crop.angle.sin_cos();
    let (u, v) = ((dx * cos + dy * sin) / crop.side + 0.5, (-dx * sin + dy * cos) / crop.side + 0.5);
    if !(0.0..1.0).contains(&u) || !(0.0..1.0).contains(&v) {
        return 0.0;
    }
    pose.matte[(v * SIZE as f32) as usize * SIZE + (u * SIZE as f32) as usize]
}

fn line(img: &mut image::RgbImage, a: [f32; 2], b: [f32; 2], scale: f32, colour: [u8; 3], r: i32) {
    for t in 0..=400 {
        let f = t as f32 / 400.0;
        dot(img, [(a[0] + (b[0] - a[0]) * f) / scale, (a[1] + (b[1] - a[1]) * f) / scale], colour, r);
    }
}

fn dot(img: &mut image::RgbImage, [x, y]: [f32; 2], colour: [u8; 3], r: i32) {
    for dy in -r..=r {
        for dx in -r..=r {
            let (px, py) = (x as i32 + dx, y as i32 + dy);
            if px >= 0 && py >= 0 && (px as u32) < img.width() && (py as u32) < img.height() {
                img.put_pixel(px as u32, py as u32, image::Rgb(colour));
            }
        }
    }
}
