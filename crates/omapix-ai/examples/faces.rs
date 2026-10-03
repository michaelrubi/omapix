//! Face analysis on a photo, timed, drawn into PNGs in `out`: `cargo run
//! --release -p omapix-ai --example faces -- photo.jpg out`.
//! - `out/overview.png`: the faces found, their points, and the whole
//!   image's segmentation (hair yellow, face skin pink, body skin red)
//! - `out/face<n>.png`: each face's crop with its points, then the same
//!   segmentation run on a crop round that person
//! - `out/skin.png`: face and body skin as a refined full-size mask
use std::time::Instant;

use omapix_ai::face::{BODY_SKIN, Crop, FACE_SKIN, Faces, Image, SEGMENT_SIZE};
use omapix_engine::DisplayTransform;

const TINTS: [[f32; 3]; 6] = [
    [0.0; 3],
    [255.0, 200.0, 0.0],
    [255.0, 0.0, 0.0],
    [255.0, 128.0, 160.0],
    [0.0, 0.0, 255.0],
    [0.0, 255.0, 0.0],
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let out = std::path::Path::new(&args[2]);
    std::fs::create_dir_all(out)?;
    let doc = omapix_engine::io::load(std::path::Path::new(&args[1]))?;
    let raster = doc.composite();
    let (w, h) = (raster.width() as usize, raster.height() as usize);
    let mut srgb = vec![[0u8; 4]; raster.pixels().len()];
    DisplayTransform::to_srgb(&doc.profile)?.convert(raster.pixels(), &mut srgb);
    let image = Image {
        pixels: &srgb,
        width: w,
        height: h,
    };

    let t = Instant::now();
    let mut faces = Faces::load()?;
    println!("{w}×{h}, loaded in {:?}", t.elapsed());
    let side = w.max(h) as f32;
    let whole = Crop {
        centre: [w as f32 / 2.0, h as f32 / 2.0],
        side,
        angle: 0.0,
    };
    let (mut found, mut marks, mut seg) = (Vec::new(), Vec::new(), Vec::new());
    for run in 0..3 {
        let t = Instant::now();
        found = faces.detect(&image)?;
        let detect = t.elapsed();
        let t = Instant::now();
        marks = found
            .iter()
            .map(|f| faces.landmarks(&image, f))
            .collect::<Result<Vec<_>, _>>()?;
        let landmarks = t.elapsed();
        let t = Instant::now();
        seg = faces.segment(&image, &whole)?;
        println!(
            "run {run}: {} faces, detect {detect:?}, landmarks {landmarks:?}, segment {:?}",
            found.len(),
            t.elapsed()
        );
    }
    for (i, (d, m)) in found.iter().zip(&marks).enumerate() {
        println!(
            "face {i}: score {:.2}, bounds {:?}, points {}",
            d.score,
            d.bounds.map(|v| v as i32),
            m.is_some()
        );
    }

    // Overview: the segmentation tinted over a small copy, boxes, points.
    let (ow, oh) = (1000usize, 1000 * h / w);
    let scale = w as f32 / ow as f32;
    let mut img = image::RgbImage::new(ow as u32, oh as u32);
    for (x, y, pixel) in img.enumerate_pixels_mut() {
        let (sx, sy) = ((x as f32 + 0.5) * scale, (y as f32 + 0.5) * scale);
        let s = srgb[sy as usize * w + sx as usize];
        // The whole image's segmentation covers the square `whole`.
        let (u, v) = (
            (sx - (w as f32 - side) / 2.0) / side,
            (sy - (h as f32 - side) / 2.0) / side,
        );
        let p = seg[(v * SEGMENT_SIZE as f32) as usize * SEGMENT_SIZE + (u * SEGMENT_SIZE as f32) as usize];
        *pixel = image::Rgb(tint(s, &p));
    }
    for (d, m) in found.iter().zip(&marks) {
        let [x0, y0, x1, y1] = d.bounds.map(|v| v / scale);
        for t in 0..=100 {
            let f = t as f32 / 100.0;
            for p in [
                [x0 + (x1 - x0) * f, y0],
                [x0 + (x1 - x0) * f, y1],
                [x0, y0 + (y1 - y0) * f],
                [x1, y0 + (y1 - y0) * f],
            ] {
                dot(&mut img, p, [0, 255, 0], 0);
            }
        }
        for p in d.points {
            dot(&mut img, [p[0] / scale, p[1] / scale], [255, 0, 255], 1);
        }
        if let Some((_, points)) = m {
            for p in points {
                dot(&mut img, [p[0] / scale, p[1] / scale], [0, 255, 255], 0);
            }
        }
    }
    img.save(out.join("overview.png"))?;

    // Each face: its landmark crop with the points, beside a segmentation
    // of a crop round the person (three face crops wide, a little low).
    for (i, m) in marks.iter().enumerate() {
        let Some((crop, points)) = m else { continue };
        let n = 512u32;
        let mut img = image::RgbImage::new(2 * n, n);
        let face = image.sample(crop, n as usize);
        let body = Crop {
            centre: crop.to_image(0.5, 0.9),
            side: crop.side * 3.0,
            angle: 0.0,
        };
        let t = Instant::now();
        let seg = faces.segment(&image, &body)?;
        println!("face {i}: segmented its person in {:?}", t.elapsed());
        let people = image.sample(&body, n as usize);
        for y in 0..n {
            for x in 0..n {
                let to8 = |p: [f32; 3]| [p[0], p[1], p[2], 1.0].map(|v| (v * 255.0) as u8);
                img.put_pixel(x, y, image::Rgb(to8(face[(y * n + x) as usize])[..3].try_into()?));
                let s = seg
                    [(y as usize * SEGMENT_SIZE / n as usize) * SEGMENT_SIZE + x as usize * SEGMENT_SIZE / n as usize];
                img.put_pixel(n + x, y, image::Rgb(tint(to8(people[(y * n + x) as usize]), &s)));
            }
        }
        // The points, back in the crop's frame, and the features' outlines.
        let (sin, cos) = crop.angle.sin_cos();
        let at = |p: [f32; 3]| {
            let (dx, dy) = (p[0] - crop.centre[0], p[1] - crop.centre[1]);
            [
                ((dx * cos + dy * sin) / crop.side + 0.5) * n as f32,
                ((-dx * sin + dy * cos) / crop.side + 0.5) * n as f32,
            ]
        };
        for &p in points {
            dot(&mut img, at(p), [0, 255, 255], 0);
        }
        use omapix_ai::face::outline::*;
        let rings: [(&[usize], [u8; 3]); 9] = [
            (&LEFT_EYE, [0, 255, 0]),
            (&RIGHT_EYE, [0, 255, 0]),
            (&LEFT_BROW, [255, 160, 0]),
            (&RIGHT_BROW, [255, 160, 0]),
            (&LIPS, [255, 0, 255]),
            (&MOUTH, [255, 255, 0]),
            (&FACE, [255, 255, 255]),
            (&LEFT_IRIS[1..], [0, 128, 255]),
            (&RIGHT_IRIS[1..], [0, 128, 255]),
        ];
        for (ring, colour) in rings {
            for (k, &a) in ring.iter().enumerate() {
                let (a, b) = (at(points[a]), at(points[ring[(k + 1) % ring.len()]]));
                for t in 0..=40 {
                    let f = t as f32 / 40.0;
                    dot(
                        &mut img,
                        [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f],
                        colour,
                        0,
                    );
                }
            }
        }
        img.save(out.join(format!("face{i}.png")))?;
    }

    // Skin (face and body) from the whole image's segmentation, refined at
    // full size, as Select › Skin does.
    let t = Instant::now();
    let (logits, lw, lh) = faces.analyse(&image)?.logits(&[BODY_SKIN, FACE_SKIN]);
    let coverage = omapix_engine::refine::mask_coverage(&logits, lw, lh, 0.0, &raster);
    println!("analysed again, skin refined at full size in {:?}", t.elapsed());
    let mut img = image::RgbImage::new(ow as u32, oh as u32);
    for (x, y, pixel) in img.enumerate_pixels_mut() {
        let (sx, sy) = (((x as f32 + 0.5) * scale) as u32, ((y as f32 + 0.5) * scale) as u32);
        let s = srgb[sy as usize * w + sx as usize];
        let k = f32::from(coverage.get(sx, sy)) / 65535.0;
        *pixel =
            image::Rgb([0, 1, 2].map(|c| (f32::from(s[c]) * (1.0 - 0.6 * k) + [255.0, 0.0, 80.0][c] * 0.6 * k) as u8));
    }
    img.save(out.join("skin.png"))?;
    Ok(())
}

/// `s` tinted by the most likely class in `p`, as strongly as it's likely.
fn tint(s: [u8; 4], p: &[f32; 6]) -> [u8; 3] {
    let best = (0..6).max_by(|&a, &b| p[a].total_cmp(&p[b])).unwrap_or(0);
    let k = if best == 0 { 0.0 } else { 0.5 * p[best] };
    [0, 1, 2].map(|c| (f32::from(s[c]) * (1.0 - k) + TINTS[best][c] * k) as u8)
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
