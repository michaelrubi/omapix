//! Each face's 478 points in a photo as "x y z" lines, faces split by blank
//! lines, for Relight's `relight_lamps` comparison:
//! `cargo run --release -p omapix-ai --example face_points -- photo.jpg out.points`
use omapix_ai::face::{Faces, Image};
use omapix_engine::DisplayTransform;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let doc = omapix_engine::io::load(std::path::Path::new(&args[1]))?;
    let raster = doc.composite();
    let mut srgb = vec![[0u8; 4]; raster.pixels().len()];
    DisplayTransform::to_srgb(&doc.profile)?.convert(raster.pixels(), &mut srgb);
    let image = Image {
        pixels: &srgb,
        width: raster.width() as usize,
        height: raster.height() as usize,
    };
    let mut faces = Faces::load()?;
    let mut out = String::new();
    for f in faces.detect(&image)? {
        if let Some((_, points)) = faces.landmarks(&image, &f)? {
            for p in points {
                out += &format!("{} {} {}\n", p[0], p[1], p[2]);
            }
            out += "\n";
        }
    }
    std::fs::write(&args[2], out)?;
    Ok(())
}
