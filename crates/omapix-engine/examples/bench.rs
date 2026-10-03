//! Times core operations on a real image:
//! `cargo run --release -p omapix-engine --example bench -- photo.tif`

use std::time::Instant;

use omapix_engine::{export, filters, io, ops, ora, tiled::Tiled};

fn main() {
    let path = std::env::args().nth(1).expect("usage: bench <image>");
    let t = Instant::now();
    let mut doc = io::load(path.as_ref()).expect("load");
    println!(
        "load            {:>8.0?}  {}x{}",
        t.elapsed(),
        doc.width,
        doc.height
    );

    let t = Instant::now();
    let _ = doc.composite();
    println!("composite x1    {:>8.0?}", t.elapsed());

    let t = Instant::now();
    let _ = filters::gaussian_blur(&doc.layers[0].pixels, 9.0);
    println!("gaussian r9     {:>8.0?}", t.elapsed());

    let t = Instant::now();
    ops::frequency_separation(&mut doc, 0, 9.0);
    println!("freq sep        {:>8.0?}", t.elapsed());

    let t = Instant::now();
    ops::dodge_and_burn_layer(&mut doc, 2);
    println!("dodge&burn      {:>8.0?}", t.elapsed());

    let t = Instant::now();
    let _ = doc.composite();
    println!("composite x4    {:>8.0?}", t.elapsed());

    let t = Instant::now();
    let snapshot = doc.clone();
    println!("undo snapshot   {:>8.0?}", t.elapsed());
    assert!(snapshot.layers[0].pixels.same_tiles(&doc.layers[0].pixels));

    let t = Instant::now();
    let _ = Tiled::from_raster(&doc.composite());
    println!("stamp visible   {:>8.0?}", t.elapsed());

    let t = Instant::now();
    let mut rot = doc.clone();
    rot.apply_orientation(omapix_engine::tiled::Orientation::Rotate90Cw);
    println!("rotate 90cw     {:>8.0?}", t.elapsed());

    let t = Instant::now();
    doc.clone().resize_image(doc.width / 2, doc.height / 2);
    println!("image size 50%  {:>8.0?}", t.elapsed());
    let t = Instant::now();
    doc.clone().resize_image(doc.width * 3 / 2, doc.height * 3 / 2);
    println!("image size 150% {:>8.0?}", t.elapsed());

    for radius in [50.0, 250.0] {
        let mut field = omapix_engine::warp::Field::new(doc.width, doc.height);
        let mut layer = doc.layers[0].pixels.clone();
        let t = Instant::now();
        let c = [doc.width as f32 / 2.0, doc.height as f32 / 2.0];
        let area = field.dab(omapix_engine::warp::Brush::ForwardWarp, c, radius, 1.0, [10.0, 0.0]).unwrap();
        omapix_engine::warp::warp_area(&doc.layers[0].pixels, &field, None, &mut layer, area);
        println!("liquify dab r{radius:<4} {:>8.1?}", t.elapsed());
    }
    {
        // Restore All re-warps everything warped, 1500 px across, each time
        // its slider moves.
        let mut field = omapix_engine::warp::Field::new(doc.width, doc.height);
        let c = [doc.width as f32 / 2.0, doc.height as f32 / 2.0];
        field.dab(omapix_engine::warp::Brush::Bloat, c, 750.0, 0.2, [0.0; 2]);
        let mut layer = doc.layers[0].pixels.clone();
        let t = Instant::now();
        let mut shown = field.clone();
        shown.scale(0.5);
        let area = field.extent().unwrap();
        omapix_engine::warp::warp_area(&doc.layers[0].pixels, &shown, None, &mut layer, area);
        println!("liquify restore {:>8.1?}", t.elapsed());
    }

    let dir = std::env::temp_dir().join(format!("omapix-bench-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let t = Instant::now();
    ora::save(&doc, &dir.join("bench.ora")).expect("save");
    let size = std::fs::metadata(dir.join("bench.ora"))
        .map(|m| m.len())
        .unwrap_or(0);
    println!(
        "save .ora       {:>8.0?}  {} MB, {} layers",
        t.elapsed(),
        size / 1_000_000,
        doc.layers.len()
    );
    let t = Instant::now();
    let _ = ora::load(&dir.join("bench.ora")).expect("load");
    println!("open .ora       {:>8.0?}", t.elapsed());
    let t = Instant::now();
    export::tiff(&doc, &dir.join("bench.tif")).expect("tiff");
    println!("export tiff     {:>8.0?}", t.elapsed());
    let t = Instant::now();
    export::jpeg(&doc, &dir.join("bench.jpg"), 92).expect("jpeg");
    println!("export jpeg     {:>8.0?}", t.elapsed());
    let t = Instant::now();
    let sharpen = filters::LayerFilter::UnsharpMask { amount: 1.0, radius: 1.0, threshold: 0.0 };
    let grain = omapix_engine::NoiseOptions::default();
    export::batch_file(path.as_ref(), &dir, Some(2048), Some(&sharpen), Some(&grain), Some(92)).expect("batch");
    println!("batch export    {:>8.0?}", t.elapsed());
    std::fs::remove_dir_all(&dir).ok();
}
